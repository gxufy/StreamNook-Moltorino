// Run with: node --test src/utils/moltorinoRuntimeGate.test.ts
//
// Covers the pure availability gates for the chat-runtime integration. Availability
// is the backend resolver's `available` result (the source of truth) — NOT any
// executable-path string. These tests therefore never construct or mock a path;
// they feed the resolved boolean|null status directly, exactly as the components
// receive it from `chat_runtime_status`.

import test from 'node:test';
import assert from 'node:assert/strict';

import { isChatRuntimeEmbedCandidate, canOpenInChatRuntime } from './moltorinoRuntimeGate.ts';

// --- Embed candidate (embedded-chat surface) ---
//
// Signature: isChatRuntimeEmbedCandidate(embeddedEnabled, runtimeAvailable, surfaceVisible).
// The primary surface is visible in these baseline cases (surfaceVisible=true)
// unless the test is specifically about the surface being hidden.

test('embedded enabled + bundled runtime available + surface visible -> embed candidate true', () => {
  assert.equal(isChatRuntimeEmbedCandidate(true, true, true), true);
});

test('embedded enabled + runtime unavailable + surface visible -> embed candidate false', () => {
  assert.equal(isChatRuntimeEmbedCandidate(true, false, true), false);
});

test('embedded enabled + status loading (null) + surface visible -> embed candidate false', () => {
  assert.equal(isChatRuntimeEmbedCandidate(true, null, true), false);
});

test('embedded disabled + runtime available + surface visible -> embed candidate false', () => {
  assert.equal(isChatRuntimeEmbedCandidate(false, true, true), false);
});

// --- Surface visibility (Settings overlay) ---
//
// The embedded chat is a native Win32 child window that composites above the
// WebView, so a DOM overlay like Settings cannot cover it. When the primary
// surface is hidden (Settings open) the candidate must be false regardless of the
// other terms, so the host unmounts and its teardown restores the WebView region.

test('surface hidden (Settings open) + enabled + runtime available -> embed candidate false', () => {
  assert.equal(isChatRuntimeEmbedCandidate(true, true, false), false);
});

test('returning surface visibility to true permits embedding again', () => {
  // Hidden while Settings is open...
  assert.equal(isChatRuntimeEmbedCandidate(true, true, false), false);
  // ...and re-enabled the moment the surface is visible again (Settings closed),
  // with no other state having changed. This is what lets the host remount
  // automatically on close without an app restart.
  assert.equal(isChatRuntimeEmbedCandidate(true, true, true), true);
});

// --- Open-in-chat-runtime button ---
//
// The launch button is independent of surface visibility (it lives in the chat
// toolbar and just spawns/attaches the external client), so its gate keeps the
// two-term shape: show-button setting + runtime available.

test('show-button enabled + bundled runtime available -> button available', () => {
  assert.equal(canOpenInChatRuntime(true, true), true);
});

test('show-button enabled + runtime unavailable -> button unavailable', () => {
  assert.equal(canOpenInChatRuntime(true, false), false);
});

test('show-button enabled + status loading (null) -> button unavailable', () => {
  assert.equal(canOpenInChatRuntime(true, null), false);
});

test('show-button disabled + runtime available -> button unavailable', () => {
  assert.equal(canOpenInChatRuntime(false, true), false);
});

// --- Availability abstracts away *why* the resolver said yes ---
//
// The resolver reports a single `available` boolean regardless of which source it
// picked; the frontend gate must not care. These two cases assert that the exact
// scenarios the bundled-runtime work was about — an invalid/blank custom override
// that falls through to a present bundle — reach the gates as `available === true`
// and therefore enable both features, with no path string ever involved.

test('invalid custom override falling through to a valid bundle (available=true) enables both gates', () => {
  // The backend resolver rejected the custom override but found the bundle, so it
  // reports available=true. Both gates must open on that alone (surface visible).
  assert.equal(isChatRuntimeEmbedCandidate(true, true, true), true);
  assert.equal(canOpenInChatRuntime(true, true), true);
});

test('empty custom path with a valid bundle (available=true) enables both gates', () => {
  // The custom field is blank; the resolver still finds the bundled runtime and
  // reports available=true. Both gates must open — no non-empty path required.
  assert.equal(isChatRuntimeEmbedCandidate(true, true, true), true);
  assert.equal(canOpenInChatRuntime(true, true), true);
});
