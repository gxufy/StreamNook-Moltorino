// Pure boolean gates for the optional Moltorino integration.
//
// Whether Moltorino features are offered is a two-part decision: the user has to
// have opted in (a per-feature setting), AND the backend resolver has to report a
// launchable runtime. Crucially, availability is decided by the resolver — NOT by
// whether the custom executable-path field is filled in — because a bundled
// Moltorino ships with the app and works with that field left blank. These helpers
// deliberately never see a path string; they take only the settings toggle and the
// resolver's `available` result (from `chat_runtime_status`).
//
// `runtimeAvailable` mirrors the fetch state at the call site:
//   • `null`  -> status not yet loaded. Treat as not-a-candidate so nothing is
//                launched or embedded speculatively while we're still checking.
//   • `true`  -> resolver found a runtime (bundled or custom). Feature may proceed.
//   • `false` -> no runtime. Feature is unavailable.

/// Whether the embedded main-chat surface (MoltorinoChatHost) is even a candidate:
/// the embedded-chat setting is on, a runtime is confirmed available, AND the
/// primary stream/chat surface is actually visible.
///
/// `surfaceVisible` is the crucial third term. The embedded chat is a native Win32
/// child window (an HWND that Rust overlays on the chat rectangle), NOT a DOM node,
/// so it composites *above* the WebView and cannot be covered by any React overlay.
/// When a full-window overlay like Settings opens, the only way to stop Moltorino
/// drawing through it is to drop the candidate to false so the host unmounts and
/// its teardown hides the native window / restores the WebView region. Pass
/// `false` whenever the primary surface is occluded (e.g. Settings open); pass
/// `true` when it's showing normally.
export function isChatRuntimeEmbedCandidate(
  embeddedEnabled: boolean,
  runtimeAvailable: boolean | null,
  surfaceVisible: boolean,
): boolean {
  return embeddedEnabled && runtimeAvailable === true && surfaceVisible;
}

/// Whether the "Open chat in Moltorino" button should be offered: the show-button
/// setting is on AND a runtime is confirmed available.
export function canOpenInChatRuntime(
  showButtonEnabled: boolean,
  runtimeAvailable: boolean | null,
): boolean {
  return showButtonEnabled && runtimeAvailable === true;
}
