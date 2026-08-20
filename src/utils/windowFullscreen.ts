import { Logger } from './logger.ts';

export interface PlayerFullscreenSnapshot {
  active: boolean;
  transitioning: boolean;
  owner: string | null;
}

type PlayerFullscreenListener = (snapshot: PlayerFullscreenSnapshot) => void;
type FullscreenTransition = (entering: boolean) => Promise<void>;

export interface PlayerFullscreenCoordinator {
  getSnapshot: () => PlayerFullscreenSnapshot;
  subscribe: (listener: PlayerFullscreenListener) => () => void;
  request: (entering: boolean, owner?: string | null) => Promise<void>;
}

const INITIAL_SNAPSHOT: PlayerFullscreenSnapshot = Object.freeze({
  active: false,
  transitioning: false,
  owner: null,
});

/**
 * Small serialized state machine shared by the real window bridge and its tests.
 * State changes before the transition promise runs, so native surfaces and resize
 * handlers stand down in the same turn as Plyr's fullscreen event.
 */
export const createPlayerFullscreenCoordinator = (
  performTransition: FullscreenTransition,
): PlayerFullscreenCoordinator => {
  let snapshot = INITIAL_SNAPSHOT;
  let transitionQueue = Promise.resolve();
  let requestVersion = 0;
  let settledActive = false;
  let settledOwner: string | null = null;
  const listeners = new Set<PlayerFullscreenListener>();

  const publish = (next: PlayerFullscreenSnapshot) => {
    snapshot = Object.freeze(next);
    for (const listener of Array.from(listeners)) listener(snapshot);
  };

  const request = (entering: boolean, owner: string | null = null): Promise<void> => {
    const version = ++requestVersion;
    publish({
      active: entering,
      transitioning: true,
      // Retain the owner through exit so observers know which surface is leaving.
      owner: entering ? owner : (owner ?? snapshot.owner),
    });

    const transition = transitionQueue.then(() => performTransition(entering));
    // A failed operation must not poison the queue; the next opposite request still
    // needs to run. The caller still receives this operation's original rejection.
    transitionQueue = transition.catch(() => {});

    const settleSuccess = () => {
      settledActive = entering;
      settledOwner = entering ? owner : null;
      // A newer request has already published its desired state. Never let this
      // older operation's completion clear that transition prematurely.
      if (version !== requestVersion) return;
      publish({
        active: entering,
        transitioning: false,
        owner: settledOwner,
      });
    };

    const settleFailure = () => {
      if (version !== requestVersion) return;
      // Reflect the last OS transition that actually succeeded rather than
      // claiming the failed request took effect.
      publish({
        active: settledActive,
        transitioning: false,
        owner: settledOwner,
      });
    };

    return transition.then(
      () => settleSuccess(),
      (error) => {
        settleFailure();
        throw error;
      },
    );
  };

  return {
    getSnapshot: () => snapshot,
    subscribe: (listener) => {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
    request,
  };
};

// Win32 quirk: a borderless window (decorations: false) that is WS_MAXIMIZE
// keeps its maximized chrome/taskbar visible even after setFullscreen(true).
// Track whether the window was maximized going in so we can restore it on exit.
let restoreMaximizedAfterFullscreen = false;

const performTauriWindowFullscreenTransition = async (entering: boolean): Promise<void> => {
  const { getCurrentWindow, currentMonitor, PhysicalPosition } = await import('@tauri-apps/api/window');
  const win = getCurrentWindow();
  if (entering) {
    restoreMaximizedAfterFullscreen = await win.isMaximized();
    if (restoreMaximizedAfterFullscreen) {
      await win.unmaximize();
    }
    await win.setFullscreen(true);
  } else {
    // If the app is in its own borderless full-screen mode, the player only
    // borrowed an already-fullscreen window. Leave the window fullscreen on
    // exit so closing the video doesn't kick the whole app back to windowed.
    const { useAppStore } = await import('../stores/AppStore');
    if (useAppStore.getState().isWindowFullscreen) return;
    await win.setFullscreen(false);
    if (restoreMaximizedAfterFullscreen) {
      // After repeated fullscreen→exit cycles, Win32's saved restore
      // placement can drift, leaving the next maximize() bound to the
      // wrong rect (window ends up partially off-screen). Anchor to the
      // current monitor's origin first so maximize() snaps to its work area.
      const monitor = await currentMonitor();
      if (monitor) {
        await win.setPosition(new PhysicalPosition(monitor.position.x, monitor.position.y));
      }
      await win.maximize();
      restoreMaximizedAfterFullscreen = false;
    }
  }
};

const playerFullscreenCoordinator = createPlayerFullscreenCoordinator(
  performTauriWindowFullscreenTransition,
);

export const getPlayerFullscreenSnapshot = (): PlayerFullscreenSnapshot =>
  playerFullscreenCoordinator.getSnapshot();

export const subscribePlayerFullscreen = (listener: PlayerFullscreenListener): (() => void) =>
  playerFullscreenCoordinator.subscribe(listener);

/**
 * Promote (or demote) the Tauri window to true OS fullscreen in lockstep with
 * Plyr's CSS fullscreen. Calls are serialized, while the shared snapshot changes
 * synchronously so native surfaces and geometry writers can react immediately.
 */
export const syncTauriWindowFullscreen = (
  entering: boolean,
  owner: string | null = null,
): Promise<void> => {
  return playerFullscreenCoordinator.request(entering, owner).catch((err) => {
    Logger.error('[Fullscreen] Failed to sync Tauri window:', err);
  });
};
