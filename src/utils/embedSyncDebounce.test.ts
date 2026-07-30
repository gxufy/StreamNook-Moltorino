// Run with: node --test src/utils/embedSyncDebounce.test.ts
//
// Drives the debouncer with node:test's mock timers so debounce timing, dedup,
// and stale-update cancellation are all deterministic.

import test from 'node:test';
import assert from 'node:assert/strict';

import { createDedupingDebouncer } from './embedSyncDebounce.ts';

// A tiny deterministic timer harness. We don't rely on node's mock timers so the
// test reads the same on every runtime: a single pending callback + a manual
// clock is all the debouncer needs.
function makeClock() {
  let seq = 0;
  const timers = new Map<number, { fireAt: number; cb: () => void }>();
  let now = 0;
  return {
    setTimeoutFn: (cb: () => void, ms: number) => {
      const id = ++seq;
      timers.set(id, { fireAt: now + ms, cb });
      return id as unknown as ReturnType<typeof setTimeout>;
    },
    clearTimeoutFn: (h: ReturnType<typeof setTimeout>) => {
      timers.delete(h as unknown as number);
    },
    advance: (ms: number) => {
      now += ms;
      for (const [id, t] of [...timers.entries()]) {
        if (t.fireAt <= now) {
          timers.delete(id);
          t.cb();
        }
      }
    },
  };
}

test('coalesces rapid pushes into a single fire after the delay', () => {
  const clock = makeClock();
  const fired: string[] = [];
  const d = createDedupingDebouncer<string>({
    delayMs: 300,
    equals: (a, b) => a === b,
    onFire: (v) => fired.push(v),
    setTimeoutFn: clock.setTimeoutFn,
    clearTimeoutFn: clock.clearTimeoutFn,
  });

  d.push('a');
  clock.advance(100);
  d.push('b');
  clock.advance(100);
  d.push('c');
  // Only 200ms of the 300ms window has elapsed since the last push; nothing yet.
  clock.advance(100);
  assert.deepEqual(fired, []);
  // Cross the threshold for the final value.
  clock.advance(200);
  assert.deepEqual(fired, ['c']);
});

test('the final destination wins during fast switching (stale updates cancelled)', () => {
  const clock = makeClock();
  const fired: string[] = [];
  const d = createDedupingDebouncer<string>({
    delayMs: 300,
    equals: (a, b) => a === b,
    onFire: (v) => fired.push(v),
    setTimeoutFn: clock.setTimeoutFn,
    clearTimeoutFn: clock.clearTimeoutFn,
  });

  // Rapidly flip through channels faster than the debounce window.
  d.push('forsen');
  clock.advance(50);
  d.push('xqc');
  clock.advance(50);
  d.push('nmplol');
  clock.advance(50);
  d.push('sodapoppin');
  clock.advance(300);
  assert.deepEqual(fired, ['sodapoppin']);
});

test('does not re-send a value equal to the last committed one', () => {
  const clock = makeClock();
  const fired: string[] = [];
  const d = createDedupingDebouncer<string>({
    delayMs: 300,
    equals: (a, b) => a === b,
    onFire: (v) => fired.push(v),
    setTimeoutFn: clock.setTimeoutFn,
    clearTimeoutFn: clock.clearTimeoutFn,
  });

  d.push('forsen');
  clock.advance(300);
  assert.deepEqual(fired, ['forsen']);

  // Pushing the same value again commits nothing.
  d.push('forsen');
  clock.advance(300);
  assert.deepEqual(fired, ['forsen']);
});

test('A -> B -> A with A already committed sends nothing further', () => {
  const clock = makeClock();
  const fired: string[] = [];
  const d = createDedupingDebouncer<string>({
    delayMs: 300,
    equals: (a, b) => a === b,
    onFire: (v) => fired.push(v),
    setTimeoutFn: clock.setTimeoutFn,
    clearTimeoutFn: clock.clearTimeoutFn,
  });

  d.push('a');
  clock.advance(300);
  assert.deepEqual(fired, ['a']);

  // Start heading to B, then snap back to A before the window elapses. Because A
  // is already committed, the pending B is cancelled and no fire happens.
  d.push('b');
  clock.advance(100);
  d.push('a');
  clock.advance(300);
  assert.deepEqual(fired, ['a']);
  assert.equal(d.hasPending, false);
});

test('flush commits the pending value immediately', () => {
  const clock = makeClock();
  const fired: string[] = [];
  const d = createDedupingDebouncer<string>({
    delayMs: 300,
    equals: (a, b) => a === b,
    onFire: (v) => fired.push(v),
    setTimeoutFn: clock.setTimeoutFn,
    clearTimeoutFn: clock.clearTimeoutFn,
  });

  d.push('a');
  assert.equal(d.hasPending, true);
  d.flush();
  assert.deepEqual(fired, ['a']);
  assert.equal(d.hasPending, false);
});

test('cancel drops the pending value without firing', () => {
  const clock = makeClock();
  const fired: string[] = [];
  const d = createDedupingDebouncer<string>({
    delayMs: 300,
    equals: (a, b) => a === b,
    onFire: (v) => fired.push(v),
    setTimeoutFn: clock.setTimeoutFn,
    clearTimeoutFn: clock.clearTimeoutFn,
  });

  d.push('a');
  d.cancel();
  clock.advance(300);
  assert.deepEqual(fired, []);
  assert.equal(d.hasPending, false);
});

test('lastFired reflects the most recent commit', () => {
  const clock = makeClock();
  const d = createDedupingDebouncer<string>({
    delayMs: 300,
    equals: (a, b) => a === b,
    onFire: () => {},
    setTimeoutFn: clock.setTimeoutFn,
    clearTimeoutFn: clock.clearTimeoutFn,
  });

  assert.equal(d.lastFired, undefined);
  d.push('a');
  clock.advance(300);
  assert.equal(d.lastFired, 'a');
  d.push('b');
  clock.advance(300);
  assert.equal(d.lastFired, 'b');
});
