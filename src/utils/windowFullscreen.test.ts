// Run with: node --test src/utils/windowFullscreen.test.ts

import test from 'node:test';
import assert from 'node:assert/strict';

import { createPlayerFullscreenCoordinator } from './windowFullscreen.ts';

const deferred = () => {
  let resolve!: () => void;
  const promise = new Promise<void>((done) => {
    resolve = done;
  });
  return { promise, resolve };
};

const flushMicrotasks = async () => {
  await Promise.resolve();
  await Promise.resolve();
};

test('enter publishes active and transitioning synchronously, then settles', async () => {
  const operation = deferred();
  const coordinator = createPlayerFullscreenCoordinator(() => operation.promise);

  const entered = coordinator.request(true, 'tile-a');
  assert.deepEqual(coordinator.getSnapshot(), {
    active: true,
    transitioning: true,
    owner: 'tile-a',
  });

  operation.resolve();
  await entered;
  assert.deepEqual(coordinator.getSnapshot(), {
    active: true,
    transitioning: false,
    owner: 'tile-a',
  });
});

test('exit clears active state and owner after it settles', async () => {
  const coordinator = createPlayerFullscreenCoordinator(async () => {});

  await coordinator.request(true, 'main-player');
  const exited = coordinator.request(false, 'main-player');
  assert.deepEqual(coordinator.getSnapshot(), {
    active: false,
    transitioning: true,
    owner: 'main-player',
  });

  await exited;
  assert.deepEqual(coordinator.getSnapshot(), {
    active: false,
    transitioning: false,
    owner: null,
  });
});

test('opposite transitions are serialized without stale state settling', async () => {
  const enterOperation = deferred();
  const exitOperation = deferred();
  const started: boolean[] = [];
  const coordinator = createPlayerFullscreenCoordinator((entering) => {
    started.push(entering);
    return entering ? enterOperation.promise : exitOperation.promise;
  });

  const entered = coordinator.request(true, 'tile-a');
  const exited = coordinator.request(false, 'tile-a');
  assert.deepEqual(coordinator.getSnapshot(), {
    active: false,
    transitioning: true,
    owner: 'tile-a',
  });

  await flushMicrotasks();
  assert.deepEqual(started, [true]);

  enterOperation.resolve();
  await entered;
  await flushMicrotasks();
  assert.deepEqual(started, [true, false]);
  assert.equal(coordinator.getSnapshot().transitioning, true);

  exitOperation.resolve();
  await exited;
  assert.deepEqual(coordinator.getSnapshot(), {
    active: false,
    transitioning: false,
    owner: null,
  });
});

test('subscribers receive each publication and can unsubscribe', async () => {
  const coordinator = createPlayerFullscreenCoordinator(async () => {});
  const snapshots: Array<ReturnType<typeof coordinator.getSnapshot>> = [];
  const unsubscribe = coordinator.subscribe((snapshot) => snapshots.push(snapshot));

  await coordinator.request(true, 'tile-a');
  unsubscribe();
  await coordinator.request(false, 'tile-a');

  assert.deepEqual(snapshots, [
    { active: true, transitioning: true, owner: 'tile-a' },
    { active: true, transitioning: false, owner: 'tile-a' },
  ]);
});
