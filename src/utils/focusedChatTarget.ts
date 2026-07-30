// Centralized "focused chat target" abstraction — the single source of truth for
// what the *embedded* Moltorino chat surface should follow, and when it must step
// aside for StreamNook's native chat.
//
// Everything here is pure (no React, no Tauri, no DOM) so the decision logic can
// be unit-tested directly with `node --test`. The React layer feeds in the raw
// store values and consumes a single resolved decision.

import type { ProviderId } from '../types/providers';

/// Where a resolved target came from — useful for logging and for the UI to
/// reason about MultiNook vs the main player without re-deriving it.
export type FocusedChatSource = 'main-player' | 'multinook' | 'none';

/// A resolved chat target. `provider` is always present; `channelLogin` is the
/// normalized (lowercased) Twitch login when — and only when — the target is a
/// live Twitch channel that the embedded surface can actually follow.
export interface FocusedChatTarget {
  provider: ProviderId;
  /// Normalized Twitch login, or null when there is nothing embeddable to show
  /// (non-Twitch, VOD replay, offline, whisper, unsupported context, ...).
  channelLogin: string | null;
  source: FocusedChatSource;
}

/// Twitch login rule, mirrored from the Rust side
/// (commands::moltorino::is_valid_twitch_login): 1-25 chars of ASCII letters,
/// digits, or underscore. Kept byte-identical so the frontend never asks the
/// backend to follow a channel the backend would reject.
export function isValidTwitchLogin(channel: string): boolean {
  if (channel.length === 0 || channel.length > 25) return false;
  for (let i = 0; i < channel.length; i++) {
    const c = channel.charCodeAt(i);
    const isDigit = c >= 48 && c <= 57; // 0-9
    const isUpper = c >= 65 && c <= 90; // A-Z
    const isLower = c >= 97 && c <= 122; // a-z
    const isUnderscore = c === 95; // _
    if (!isDigit && !isUpper && !isLower && !isUnderscore) return false;
  }
  return true;
}

/// Normalize a raw login to the canonical form the backend expects: trimmed and
/// lowercased. Returns null when the trimmed value isn't a valid Twitch login,
/// so callers can treat "invalid" and "empty" identically.
export function normalizeTwitchLogin(raw: string | null | undefined): string | null {
  if (!raw) return null;
  const normalized = raw.trim().toLowerCase();
  return isValidTwitchLogin(normalized) ? normalized : null;
}

/// Inputs the resolver needs, pulled straight from the stores. Deliberately a
/// flat plain-data bag so the resolver stays pure and trivially testable.
export interface FocusedChatInputs {
  /// True when the main video player currently has a Twitch stream loaded.
  /// (The main player is Twitch-only; non-Twitch only appears in MultiChat
  /// popouts, which never use the embedded surface.)
  mainProvider: ProviderId | null;
  mainChannelLogin: string | null;
  /// The main player's media type; only 'live' is embeddable. 'video'/'clip'
  /// are historical, 'offline_chat' has no live chat, null means nothing loaded.
  mainMediaType: 'live' | 'clip' | 'video' | 'offline_chat' | null;
  /// True while a VOD is being replayed — historical, must use native chat.
  isVodReplay: boolean;
  /// MultiNook state. When active, its focused channel wins over the main player.
  isMultiNookActive: boolean;
  multiNookChannelLogin: string | null;
}

const NATIVE: FocusedChatTarget = { provider: 'twitch', channelLogin: null, source: 'none' };

/// Resolve the single focused chat target from the current app state.
///
/// Precedence: MultiNook's focused channel (when MultiNook is active) outranks
/// the main player, matching how ChatWidget already resolves `currentStream`.
/// Any condition that isn't a live, valid Twitch channel resolves to the native
/// fallback (`channelLogin: null`), so the embedded surface is only ever asked to
/// follow something it can actually display.
export function resolveFocusedChatTarget(input: FocusedChatInputs): FocusedChatTarget {
  // VOD replay is historical chat and must never be embedded, regardless of
  // which surface is in front.
  if (input.isVodReplay) return NATIVE;

  // MultiNook takes priority when active: its focused pane is the channel the
  // user is attending to. MultiNook is Twitch-only for chat purposes here.
  if (input.isMultiNookActive) {
    const login = normalizeTwitchLogin(input.multiNookChannelLogin);
    if (login) {
      return { provider: 'twitch', channelLogin: login, source: 'multinook' };
    }
    // MultiNook active but no valid Twitch channel focused -> native, and
    // crucially source is still 'multinook' so the caller knows why it's empty.
    return { provider: 'twitch', channelLogin: null, source: 'multinook' };
  }

  // Main player: only a *live* *Twitch* channel is embeddable.
  if (input.mainProvider === 'twitch' && input.mainMediaType === 'live') {
    const login = normalizeTwitchLogin(input.mainChannelLogin);
    if (login) {
      return { provider: 'twitch', channelLogin: login, source: 'main-player' };
    }
  }

  return NATIVE;
}

/// Whether two targets are equivalent for embedding purposes. Used to dedupe
/// rapid, redundant updates so we don't re-send an unchanged channel to
/// Moltorino. Source is intentionally ignored: following forsen from the main
/// player vs from MultiNook is the same channel to Moltorino.
export function sameEmbedTarget(a: FocusedChatTarget, b: FocusedChatTarget): boolean {
  return a.channelLogin === b.channelLogin;
}

/// Whether the embedded Moltorino surface should be shown at all for a target.
/// (Only when there's a concrete Twitch channel to follow.)
export function isEmbeddable(target: FocusedChatTarget): boolean {
  return target.channelLogin !== null;
}
