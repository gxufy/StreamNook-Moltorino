// Run with: node --test src/utils/focusedChatTarget.test.ts
//
// Covers the pure decision logic behind the embedded Moltorino surface: Twitch
// login normalization/validation, target resolution precedence, native-fallback
// decisions, and the dedupe predicate.

import test from 'node:test';
import assert from 'node:assert/strict';

import {
  isValidTwitchLogin,
  normalizeTwitchLogin,
  resolveFocusedChatTarget,
  sameEmbedTarget,
  isEmbeddable,
  type FocusedChatInputs,
} from './focusedChatTarget.ts';

// A baseline "nothing loaded" input the individual tests tweak.
const EMPTY: FocusedChatInputs = {
  mainProvider: null,
  mainChannelLogin: null,
  mainMediaType: null,
  isVodReplay: false,
  isMultiNookActive: false,
  multiNookChannelLogin: null,
};

test('isValidTwitchLogin accepts real logins', () => {
  assert.equal(isValidTwitchLogin('forsen'), true);
  assert.equal(isValidTwitchLogin('some_user_123'), true);
  assert.equal(isValidTwitchLogin('a'), true);
  assert.equal(isValidTwitchLogin('a'.repeat(25)), true);
});

test('isValidTwitchLogin rejects empty, overlong, and injection shapes', () => {
  assert.equal(isValidTwitchLogin(''), false);
  assert.equal(isValidTwitchLogin('a'.repeat(26)), false);
  assert.equal(isValidTwitchLogin('has space'), false);
  assert.equal(isValidTwitchLogin('semi;colon'), false);
  assert.equal(isValidTwitchLogin('--flag'), false);
  assert.equal(isValidTwitchLogin('t:prefixed'), false);
  assert.equal(isValidTwitchLogin('quote"mark'), false);
});

test('normalizeTwitchLogin trims and lowercases', () => {
  assert.equal(normalizeTwitchLogin('  Forsen  '), 'forsen');
  assert.equal(normalizeTwitchLogin('XQC'), 'xqc');
});

test('normalizeTwitchLogin returns null for empty/invalid', () => {
  assert.equal(normalizeTwitchLogin(''), null);
  assert.equal(normalizeTwitchLogin(null), null);
  assert.equal(normalizeTwitchLogin(undefined), null);
  assert.equal(normalizeTwitchLogin('   '), null);
  assert.equal(normalizeTwitchLogin('bad name'), null);
});

test('resolves a live main-player Twitch channel', () => {
  const target = resolveFocusedChatTarget({
    ...EMPTY,
    mainProvider: 'twitch',
    mainChannelLogin: 'Forsen',
    mainMediaType: 'live',
  });
  assert.deepEqual(target, { provider: 'twitch', channelLogin: 'forsen', source: 'main-player' });
});

test('VOD replay always falls back to native, even with a live Twitch channel', () => {
  const target = resolveFocusedChatTarget({
    ...EMPTY,
    mainProvider: 'twitch',
    mainChannelLogin: 'forsen',
    mainMediaType: 'live',
    isVodReplay: true,
  });
  assert.equal(target.channelLogin, null);
  assert.equal(target.source, 'none');
});

test('non-live media types fall back to native', () => {
  for (const mediaType of ['clip', 'video', 'offline_chat', null] as const) {
    const target = resolveFocusedChatTarget({
      ...EMPTY,
      mainProvider: 'twitch',
      mainChannelLogin: 'forsen',
      mainMediaType: mediaType,
    });
    assert.equal(target.channelLogin, null, `media type ${mediaType} should not embed`);
  }
});

test('MultiNook active outranks the main player', () => {
  const target = resolveFocusedChatTarget({
    ...EMPTY,
    mainProvider: 'twitch',
    mainChannelLogin: 'mainguy',
    mainMediaType: 'live',
    isMultiNookActive: true,
    multiNookChannelLogin: 'NookChannel',
  });
  assert.deepEqual(target, {
    provider: 'twitch',
    channelLogin: 'nookchannel',
    source: 'multinook',
  });
});

test('MultiNook active with no valid channel is native but tagged multinook', () => {
  const target = resolveFocusedChatTarget({
    ...EMPTY,
    isMultiNookActive: true,
    multiNookChannelLogin: null,
  });
  assert.equal(target.channelLogin, null);
  assert.equal(target.source, 'multinook');
});

test('MultiNook active with an invalid channel does not fall through to the main player', () => {
  // A stale/invalid MultiNook channel must not "leak" the main player's channel
  // into the embedded surface — MultiNook owns the decision while it is active.
  const target = resolveFocusedChatTarget({
    ...EMPTY,
    mainProvider: 'twitch',
    mainChannelLogin: 'mainguy',
    mainMediaType: 'live',
    isMultiNookActive: true,
    multiNookChannelLogin: 'not a login',
  });
  assert.equal(target.channelLogin, null);
  assert.equal(target.source, 'multinook');
});

test('nothing loaded resolves to native', () => {
  const target = resolveFocusedChatTarget(EMPTY);
  assert.equal(target.channelLogin, null);
  assert.equal(target.source, 'none');
});

test('sameEmbedTarget dedupes by channel regardless of source', () => {
  const a = { provider: 'twitch' as const, channelLogin: 'forsen', source: 'main-player' as const };
  const b = { provider: 'twitch' as const, channelLogin: 'forsen', source: 'multinook' as const };
  const c = { provider: 'twitch' as const, channelLogin: 'xqc', source: 'main-player' as const };
  assert.equal(sameEmbedTarget(a, b), true);
  assert.equal(sameEmbedTarget(a, c), false);
});

test('isEmbeddable only when a concrete channel is present', () => {
  assert.equal(isEmbeddable({ provider: 'twitch', channelLogin: 'forsen', source: 'main-player' }), true);
  assert.equal(isEmbeddable({ provider: 'twitch', channelLogin: null, source: 'none' }), false);
});
