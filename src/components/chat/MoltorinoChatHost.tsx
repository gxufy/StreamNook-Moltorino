// Embedded chat-runtime host (Phase 2).
//
// Renders a React *placeholder* that reserves the chat area, then asks the Rust
// backend to position a StreamNook-owned Win32 host window over that exact
// rectangle. The chat runtime reparents its split into that host window, so the
// external client appears to live inside StreamNook's chat panel.
//
// This component owns only the *frontend* side of the lifecycle:
//   • report the placeholder's bounds (DPI-aware) and visibility to Rust,
//   • keep the followed channel in sync (debounced + deduped upstream),
//   • surface embedding failures / unexpected runtime exit as a fallback so the
//     parent (ChatSurface) can swap in the native chat panel.
//
// It never kills the shared chat-runtime process on its own unmount — the process
// is launched once and reused (see the Rust `moltorino_embed` module). On unmount
// it merely hides the host window so returning to an embeddable channel is
// instant. The process is torn down only when the feature is turned off
// (ChatSurface) or the app exits (Rust `RunEvent::Exit`).

import { useEffect, useLayoutEffect, useRef } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { Logger } from '../../utils/logger';
import {
  getPlayerFullscreenSnapshot,
  subscribePlayerFullscreen,
} from '../../utils/windowFullscreen';

/// Event Rust emits when the embedded surface can't continue (the runtime exited,
/// or a create/attach failure) and the UI must fall back to native chat.
const FALLBACK_EVENT = 'chat-runtime-embed-fallback';

export interface MoltorinoChatHostProps {
  /// The normalized (lowercased, validated) Twitch login to follow.
  channel: string;
  /// Called when the embedded surface fails and native chat must take over.
  /// The parent decides how to recover (typically: force native for this
  /// channel, retry on the next channel change).
  onFallback: (reason: string) => void;
}

/// Physical-pixel bounds of the placeholder, relative to the webview's top-left
/// (which is the main window's client-area origin — where a WS_CHILD host sits).
interface Bounds {
  x: number;
  y: number;
  w: number;
  h: number;
  visible: boolean;
}

function boundsEqual(a: Bounds | null, b: Bounds): boolean {
  return (
    a !== null &&
    a.x === b.x &&
    a.y === b.y &&
    a.w === b.w &&
    a.h === b.h &&
    a.visible === b.visible
  );
}

/// Measure the placeholder in physical pixels. The Win32 child window is
/// positioned in the parent's client coordinates, and the webview fills that
/// client area, so CSS coords from getBoundingClientRect map 1:1 after scaling
/// by devicePixelRatio (which also tracks per-monitor DPI changes on move).
function measure(el: HTMLElement): Bounds {
  const rect = el.getBoundingClientRect();
  const dpr = window.devicePixelRatio || 1;
  const w = Math.round(rect.width * dpr);
  const h = Math.round(rect.height * dpr);
  // A collapsed/clipped placeholder (0 in either axis) means "don't show": the
  // auto-hide dock animates width to 0, and a hidden panel has no box. Reporting
  // visible=false hides the host rather than leaving a stray strip on screen.
  const visible = w > 0 && h > 0;
  return {
    x: Math.round(rect.left * dpr),
    y: Math.round(rect.top * dpr),
    w,
    h,
    visible,
  };
}

const MoltorinoChatHost = ({ channel, onFallback }: MoltorinoChatHostProps) => {
  const placeholderRef = useRef<HTMLDivElement | null>(null);
  const lastBounds = useRef<Bounds | null>(null);
  // Whether we've issued the initial `start` for this mount. Guards against
  // double-start under StrictMode's mount/unmount/mount and rapid remounts.
  const started = useRef(false);
  // Keep the latest channel in a ref so the bounds effect (which we don't want to
  // re-run on every channel change) can start with the right channel.
  const channelRef = useRef(channel);
  channelRef.current = channel;
  // First channel-effect run corresponds to the mount, whose channel was already
  // sent by `start`. Skip it so we only issue `set_channel` on real changes.
  const firstChannelRun = useRef(true);

  // --- Start + bounds tracking (runs once per mount) ---------------------------
  useLayoutEffect(() => {
    const el = placeholderRef.current;
    if (!el) return;

    let disposed = false;
    const initialFullscreen = getPlayerFullscreenSnapshot();
    let suppressed = initialFullscreen.active || initialFullscreen.transitioning;

    const pushBounds = (b: Bounds, visible = b.visible) => {
      lastBounds.current = b;
      invoke('chat_runtime_embed_set_bounds', {
        x: b.x,
        y: b.y,
        width: b.w,
        height: b.h,
        visible,
      }).catch((e) => Logger.error('[ChatRuntime] set_bounds failed:', e));
    };

    const hideHost = () => {
      const b = lastBounds.current ?? measure(el);
      pushBounds(b, false);
    };

    // Measure and, only if something actually changed, report. Cheap enough to
    // call from a ResizeObserver, window resize, and a low-frequency safety poll
    // (which covers layout shifts that don't resize the element itself). While
    // player fullscreen is active or transitioning, only the coordinator may
    // touch visibility so a resize tick cannot re-show the native child.
    const syncBounds = (force = false) => {
      if (disposed || suppressed || !placeholderRef.current) return;
      const b = measure(placeholderRef.current);
      if (force || !boundsEqual(lastBounds.current, b)) pushBounds(b);
    };

    // Initial start: create/reuse the host at the current bounds, following the
    // current channel. A rejection means the embed can't run (bad path, non-
    // Windows, spawn failure) — fall back to native immediately.
    const initial = measure(el);
    lastBounds.current = initial;
    if (!started.current) {
      started.current = true;
      invoke('chat_runtime_embed_start', {
        channel: channelRef.current,
        x: initial.x,
        y: initial.y,
        width: initial.w,
        height: initial.h,
        visible: initial.visible && !suppressed,
      }).catch((e) => {
        Logger.error('[ChatRuntime] embed start failed:', e);
        if (!disposed) onFallback(String(e));
      });
    } else {
      // Remounted while the process still lives: update its rectangle, but keep it
      // hidden if a player-fullscreen transition already owns the window.
      pushBounds(initial, initial.visible && !suppressed);
    }

    const unsubscribeFullscreen = subscribePlayerFullscreen((next) => {
      if (disposed) return;
      const nextSuppressed = next.active || next.transitioning;
      if (nextSuppressed) {
        suppressed = true;
        hideHost();
      } else if (suppressed) {
        suppressed = false;
        // Fullscreen exit can restore a different window rectangle. Ignore all
        // intermediate resize ticks above, then publish exactly one fresh box.
        syncBounds(true);
      }
    });
    if (suppressed) hideHost();

    const ro = new ResizeObserver(() => syncBounds());
    ro.observe(el);
    const onWindowResize = () => syncBounds();
    window.addEventListener('resize', onWindowResize);
    // Safety net for moves that don't change the element's own size (dock
    // switches, sibling panels opening, Framer layout animations). Dedup makes
    // this a no-op whenever the box is static, so the IPC cost is only paid on
    // real change.
    const poll = window.setInterval(() => syncBounds(), 250);

    return () => {
      disposed = true;
      unsubscribeFullscreen();
      ro.disconnect();
      window.removeEventListener('resize', onWindowResize);
      window.clearInterval(poll);
      // Hide (don't kill) the shared host so a later embeddable channel reuses it
      // instantly, and so an unsupported context never leaves the old channel on
      // screen.
      invoke('chat_runtime_embed_set_bounds', {
        x: lastBounds.current?.x ?? 0,
        y: lastBounds.current?.y ?? 0,
        width: lastBounds.current?.w ?? 0,
        height: lastBounds.current?.h ?? 0,
        visible: false,
      }).catch(() => {});
    };
    // Intentionally mount-only: channel changes are handled by the effect below,
    // bounds by the observers above. onFallback is stable (parent memoizes it).
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // --- Channel sync (runs when the followed channel changes) -------------------
  useEffect(() => {
    // Skip the very first run: the initial channel was already sent by `start`.
    if (firstChannelRun.current) {
      firstChannelRun.current = false;
      return;
    }
    invoke('chat_runtime_embed_set_channel', { channel }).catch((e) =>
      Logger.error('[ChatRuntime] set_channel failed:', e)
    );
  }, [channel]);

  // --- Unexpected-exit / attach-failure fallback from Rust ---------------------
  useEffect(() => {
    let unlisten: (() => void) | null = null;
    let active = true;
    listen<string>(FALLBACK_EVENT, (event) => {
      Logger.warn('[ChatRuntime] embed fallback event:', event.payload);
      onFallback(event.payload || 'embed-fallback');
    })
      .then((fn) => {
        if (active) unlisten = fn;
        else fn();
      })
      .catch(() => {});
    return () => {
      active = false;
      if (unlisten) unlisten();
    };
  }, [onFallback]);

  // The placeholder simply reserves the chat rectangle. The actual chat pixels
  // are the native Win32 window overlaid by Rust; we render a themed backdrop so
  // there's never a flash of nothing before the runtime paints / on a slow attach.
  return (
    <div
      ref={placeholderRef}
      data-moltorino-embed-host="true"
      className="h-full w-full bg-secondary"
      aria-label="Embedded Bluzyrino chat"
    />
  );
};

export default MoltorinoChatHost;
