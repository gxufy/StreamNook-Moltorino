// Pure boolean gates for the optional Moltorino integration.
//
// Whether Moltorino features are offered is a two-part decision: the user has to
// have opted in (a per-feature setting), AND the backend resolver has to report a
// launchable runtime. Crucially, availability is decided by the resolver — NOT by
// whether the custom executable-path field is filled in — because a bundled
// Moltorino ships with the app and works with that field left blank. These helpers
// deliberately never see a path string; they take only the settings toggle and the
// resolver's `available` result (from `moltorino_runtime_status`).
//
// `runtimeAvailable` mirrors the fetch state at the call site:
//   • `null`  -> status not yet loaded. Treat as not-a-candidate so nothing is
//                launched or embedded speculatively while we're still checking.
//   • `true`  -> resolver found a runtime (bundled or custom). Feature may proceed.
//   • `false` -> no runtime. Feature is unavailable.

/// Whether the embedded main-chat surface (MoltorinoChatHost) is even a candidate:
/// the embedded-chat setting is on AND a runtime is confirmed available.
export function isMoltorinoEmbedCandidate(
  embeddedEnabled: boolean,
  runtimeAvailable: boolean | null,
): boolean {
  return embeddedEnabled && runtimeAvailable === true;
}

/// Whether the "Open chat in Moltorino" button should be offered: the show-button
/// setting is on AND a runtime is confirmed available.
export function canOpenInMoltorino(
  showButtonEnabled: boolean,
  runtimeAvailable: boolean | null,
): boolean {
  return showButtonEnabled && runtimeAvailable === true;
}
