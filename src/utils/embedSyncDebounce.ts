// A small, framework-agnostic debouncer purpose-built for syncing the embedded
// chat-runtime surface to the focused chat target. It coalesces rapid updates,
// deduplicates against what was last committed, and cancels superseded (stale)
// updates so that fast channel switching only ever sends the final destination.
//
// Kept pure (no React, no timers of its own beyond the injected setTimeout) so
// the debounce/stale-cancellation behavior can be unit-tested directly with
// node:test's mock timers.

export interface DedupingDebouncer<T> {
  /// Feed the latest desired value. Reschedules the fire; supersedes any pending
  /// value. If `value` equals the last committed value, any pending fire is
  /// cancelled instead (we're already there — nothing to send).
  push(value: T): void;
  /// Fire the pending value immediately (if any), skipping the remaining delay.
  flush(): void;
  /// Drop any pending fire without committing it.
  cancel(): void;
  /// The value most recently committed via `onFire`, or undefined if none yet.
  readonly lastFired: T | undefined;
  /// Whether a fire is currently scheduled.
  readonly hasPending: boolean;
}

export interface DebouncerOptions<T> {
  /// Delay before a pushed value is committed, in milliseconds.
  delayMs: number;
  /// Equality used for deduplication: an incoming value equal to the last
  /// committed one is a no-op (and cancels any pending fire).
  equals: (a: T, b: T) => boolean;
  /// Called with the committed value once the delay elapses.
  onFire: (value: T) => void;
  /// Injectable timer functions so tests can drive them deterministically.
  /// Defaults to the global setTimeout/clearTimeout.
  setTimeoutFn?: (cb: () => void, ms: number) => ReturnType<typeof setTimeout>;
  clearTimeoutFn?: (handle: ReturnType<typeof setTimeout>) => void;
}

export function createDedupingDebouncer<T>(opts: DebouncerOptions<T>): DedupingDebouncer<T> {
  const setT = opts.setTimeoutFn ?? ((cb, ms) => setTimeout(cb, ms));
  const clearT = opts.clearTimeoutFn ?? ((h) => clearTimeout(h));

  let timer: ReturnType<typeof setTimeout> | null = null;
  let pending: { value: T } | null = null;
  let committed: { value: T } | null = null;

  const clearTimer = () => {
    if (timer !== null) {
      clearT(timer);
      timer = null;
    }
  };

  const fire = () => {
    timer = null;
    if (!pending) return;
    const value = pending.value;
    pending = null;
    committed = { value };
    opts.onFire(value);
  };

  return {
    push(value: T) {
      // Deduplicate against the committed value: if the desired state is what we
      // already sent, cancel any in-flight (now-stale) fire and do nothing. This
      // is what makes A -> B -> A (with A committed) send nothing at all.
      if (committed && opts.equals(committed.value, value)) {
        pending = null;
        clearTimer();
        return;
      }
      // Otherwise (re)schedule. Replacing the timer is the stale-cancellation:
      // an earlier pending value is dropped in favor of this newest one.
      pending = { value };
      clearTimer();
      timer = setT(fire, opts.delayMs);
    },
    flush() {
      if (timer !== null || pending) {
        clearTimer();
        fire();
      }
    },
    cancel() {
      pending = null;
      clearTimer();
    },
    get lastFired() {
      return committed?.value;
    },
    get hasPending() {
      return pending !== null;
    },
  };
}
