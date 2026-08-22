// Main-chat surface selector (Phase 2).
//
// This is the single wrapper that decides, for the *normal main chat area only*,
// whether to render StreamNook's native chat (`ChatWidget`) or the embedded
// chat-runtime surface (`MoltorinoChatHost`). It is deliberately the ONLY place that
// swaps the two, so every other ChatWidget placement (MultiChat popouts, provider
// surfaces, the header pop-out) keeps rendering native chat untouched.
//
// Behavior:
//   • Native chat is the default. The embedded surface is used only when the user
//     has turned it on (settings.moltorino.embedded_chat), pointed the integration
//     at a real executable, AND the focused target is a live Twitch channel we can
//     actually follow.
//   • The followed channel is resolved from the same stores ChatWidget reads
//     (main player + MultiNook), normalized, and debounced so rapid switching only
//     sends the chat runtime the final destination.
//   • Any unsupported context (VOD replay, non-Twitch, offline, no valid channel)
//     falls back to native chat immediately — and because the host is unmounted in
//     that case, a previous channel is never left on screen.
//   • If the embed fails at runtime (spawn failure, unexpected runtime exit,
//     attach failure) we latch native chat for the current channel until the
//     target changes, so the user always has a working chat.

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import ChatWidget from '../ChatWidget';
import MoltorinoChatHost from './MoltorinoChatHost';
import { useAppStore } from '../../stores/AppStore';
import { usemultiNookStore } from '../../stores/multiNookStore';
import { useVodReplayStore } from '../../stores/vodReplayStore';
import {
  resolveFocusedChatTarget,
  isEmbeddable,
  type FocusedChatTarget,
} from '../../utils/focusedChatTarget';
import { createDedupingDebouncer } from '../../utils/embedSyncDebounce';
import { isChatRuntimeEmbedCandidate } from '../../utils/moltorinoRuntimeGate';
import { Logger } from '../../utils/logger';

/// Delay before a resolved channel change is pushed to the runtime. Long enough to
/// swallow the burst of intermediate targets during rapid switching, short enough
/// to feel instant on a deliberate switch.
const CHANNEL_DEBOUNCE_MS = 300;

/// Resolve the current focused chat target from the live stores. Mirrors the
/// precedence ChatWidget already uses for `currentStream` (MultiNook active slot
/// outranks the main player), but funnels everything through the pure resolver so
/// the embeddable/native decision lives in one tested place.
function useFocusedChatTarget(): FocusedChatTarget {
  // Main player (Twitch-only). A loaded stream means provider === 'twitch'.
  const currentStream = useAppStore((s) => s.currentStream);
  const currentMediaType = useAppStore((s) => s.currentMediaType);
  // MultiNook.
  const isMultiNookActive = usemultiNookStore((s) => s.isMultiNookActive);
  const activeChatChannelId = usemultiNookStore((s) => s.activeChatChannelId);
  const slots = usemultiNookStore((s) => s.slots);
  // VOD replay -> historical chat, never embeddable.
  const isVodReplay = useVodReplayStore((s) => s.active);

  // The MultiNook active id can be either a channel id or a login; map it to the
  // slot's login exactly as ChatWidget does.
  const multiNookChannelLogin = useMemo(() => {
    if (!isMultiNookActive || !activeChatChannelId) return null;
    const slot = slots.find(
      (s) => s.channelId === activeChatChannelId || s.channelLogin === activeChatChannelId,
    );
    return slot?.channelLogin ?? null;
  }, [isMultiNookActive, activeChatChannelId, slots]);

  return useMemo(
    () =>
      resolveFocusedChatTarget({
        // The main player only ever hosts Twitch; a loaded stream implies twitch.
        mainProvider: currentStream ? 'twitch' : null,
        mainChannelLogin: currentStream?.user_login ?? null,
        mainMediaType: currentMediaType,
        isVodReplay,
        isMultiNookActive,
        multiNookChannelLogin,
      }),
    [currentStream, currentMediaType, isVodReplay, isMultiNookActive, multiNookChannelLogin],
  );
}

const ChatSurface = () => {
  const embeddedEnabled = useAppStore((s) => s.settings?.moltorino?.embedded_chat ?? true);
  const executablePath = useAppStore((s) => s.settings?.moltorino?.executable_path ?? '');
  // The embedded chat is a native Win32 child window Rust overlays on the chat
  // rectangle; it composites ABOVE the WebView, so a DOM overlay like the Settings
  // dialog (fixed inset-0) can never cover it — the runtime would draw straight
  // through Settings. We therefore treat "Settings open" as "primary surface not
  // visible" and drop the embed candidate, which unmounts the host and runs its
  // teardown (hide native window + restore the WebView region). Closing Settings
  // flips this back and the host remounts automatically.
  const isSettingsOpen = useAppStore((s) => s.isSettingsOpen);
  const surfaceVisible = !isSettingsOpen;
  const target = useFocusedChatTarget();

  // Whether the backend resolver would actually find a launchable runtime
  // (bundled or custom). This is the real availability signal — NOT whether the
  // custom path field is filled in — because bundled Bluzyrino ships with the
  // app and works with the custom field left blank.
  //
  //   • `null`  -> status not yet loaded; treat as "not a candidate" so we never
  //                launch prematurely and native chat stays on screen.
  //   • `true`  -> resolver found a runtime; embedding may proceed.
  //   • `false` -> no runtime; native chat, no host, no WebView cutout.
  //
  // Read-only command (never spawns the runtime). Fetched once on mount and
  // refreshed when the saved custom path changes, so an unavailable->available
  // transition (e.g. the user points at a valid exe, or clears an invalid one so
  // the bundle takes over) starts the embedded host without an app restart.
  const [runtimeAvailable, setRuntimeAvailable] = useState<boolean | null>(null);
  useEffect(() => {
    let alive = true;
    invoke<{ available: boolean }>('chat_runtime_status')
      .then((status) => {
        if (alive) setRuntimeAvailable(status.available);
      })
      .catch((err) => {
        Logger.warn('[ChatRuntime] runtime status check failed:', err);
        if (alive) setRuntimeAvailable(false);
      });
    return () => {
      alive = false;
    };
  }, [executablePath]);

  // The channel actually handed to the embedded host. Driven by the debouncer so
  // rapid switching collapses to the final channel; null means "nothing embeddable
  // right now" (render native).
  const [embedChannel, setEmbedChannel] = useState<string | null>(null);

  // Runtime failure latch: once the embed fails for a channel we force native for
  // THAT channel until the target changes (so we don't thrash retrying a broken
  // embed on every bounds tick). Cleared whenever the resolved channel changes.
  const [failedChannel, setFailedChannel] = useState<string | null>(null);

  // Whether the embedded surface is even a candidate: feature on + the backend
  // resolver reports a launchable runtime + the primary surface is visible. We gate
  // on real availability (not a non-empty custom path) so the bundled runtime works
  // with the field blank. While status is still loading (`null`) this is false, so
  // native chat stays up and we never spin up the host or a WebView cutout
  // speculatively. `surfaceVisible` is false while Settings is open, so the native
  // chat-runtime window can't draw through the Settings overlay.
  const embedCandidate = isChatRuntimeEmbedCandidate(embeddedEnabled, runtimeAvailable, surfaceVisible);

  // One debouncer for the lifetime of the surface. It commits the resolved
  // channel (or null) into React state; dedup means an unchanged channel never
  // re-commits, so the host doesn't remount on incidental store ticks.
  const debouncerRef = useRef<ReturnType<typeof createDedupingDebouncer<string | null>> | null>(
    null,
  );
  if (debouncerRef.current === null) {
    debouncerRef.current = createDedupingDebouncer<string | null>({
      delayMs: CHANNEL_DEBOUNCE_MS,
      equals: (a, b) => a === b,
      onFire: (value) => setEmbedChannel(value),
    });
  }

  // The channel we'd embed right now (or null when nothing is embeddable). When
  // the embed isn't a candidate this is null, which collapses the host and shows
  // native chat.
  const resolvedChannel = embedCandidate && isEmbeddable(target) ? target.channelLogin : null;

  // Clear a stale failure latch as soon as the *channel* changes: a new channel
  // deserves a fresh attempt at embedding. This is a "reset state when a value
  // changes" case, so we adjust state during render guarded by a ref of the
  // previous channel — React's supported pattern — rather than in an effect
  // (which would trigger a cascading re-render).
  const prevResolvedChannel = useRef<string | null>(resolvedChannel);
  if (prevResolvedChannel.current !== resolvedChannel) {
    prevResolvedChannel.current = resolvedChannel;
    if (failedChannel !== null) setFailedChannel(null);
  }

  // Feed the resolved channel into the debouncer (the actual side effect). Dedup
  // means an unchanged channel never re-commits, so the host doesn't remount on
  // incidental store ticks.
  useEffect(() => {
    debouncerRef.current!.push(resolvedChannel);
  }, [resolvedChannel]);

  // Flush + tear down the debouncer on unmount so a pending commit can't fire into
  // an unmounted tree.
  useEffect(() => {
    const d = debouncerRef.current!;
    return () => d.cancel();
  }, []);

  // Tear down the reused chat-runtime process when the feature is turned OFF (or its
  // path is cleared) — the one moment we own the process and should release it.
  //
  // This is deliberately keyed on a true->false transition of `embedCandidate`, NOT
  // on component unmount: the two <ChatSurface /> placements in App.tsx are
  // mutually-exclusive ternary branches, so a layout swap (bottom<->side dock) or a
  // MultiNook toggle unmounts one and mounts the other WHILE the feature stays on.
  // Stopping on unmount would kill and respawn the single reused process on every
  // such swap, defeating "launch ONE embedded process and reuse it". App exit is
  // handled separately by Rust's `RunEvent::Exit`.
  const prevEmbedCandidate = useRef(embedCandidate);
  useEffect(() => {
    if (prevEmbedCandidate.current && !embedCandidate) {
      invoke('chat_runtime_embed_stop').catch((err) => {
        Logger.warn('[ChatRuntime] embed stop failed:', err);
      });
    }
    prevEmbedCandidate.current = embedCandidate;
  }, [embedCandidate]);

  const handleFallback = useCallback(
    (reason: string) => {
      Logger.warn('[ChatRuntime] falling back to native chat:', reason);
      // Latch the channel that failed so we don't immediately re-mount the host
      // for the same broken target. A channel change clears this latch above.
      setFailedChannel(embedChannel);
    },
    [embedChannel],
  );

  // Decide what to render. The embedded host is shown only when: it's a candidate,
  // we have a debounced embeddable channel, and that channel hasn't just failed.
  const showEmbed =
    embedCandidate && embedChannel !== null && embedChannel !== failedChannel;

  if (showEmbed) {
    return <MoltorinoChatHost channel={embedChannel} onFallback={handleFallback} />;
  }

  // Native chat: the default and the universal fallback.
  return <ChatWidget />;
};

export default ChatSurface;
