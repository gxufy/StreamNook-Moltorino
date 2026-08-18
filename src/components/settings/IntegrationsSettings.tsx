import { useCallback, useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Plug, MessagesSquare, Loader2, Check, AlertCircle } from 'lucide-react';
import { useAppStore } from '../../stores/AppStore';
import { usePluginUiRegistry, selectSlot } from '../../plugins-ui/registry';
import { DiscordGlyph } from '../ui/DiscordGlyph';
import streamnookLogo from '../../assets/streamnook-logo.png';
import { Logger } from '../../utils/logger';

/** Generic path validation response. */
interface ChatRuntimePathInfo {
  resolved_path: string;
  file_name: string;
}

type ChatRuntimeKind =
  | 'bundled_bluzyrino'
  | 'custom_bluzyrino'
  | 'custom_moltorino'
  | 'custom'
  | 'legacy_bundled_moltorino';

/** The first four fields retain the legacy runtime-status contract. */
interface ChatRuntimeStatus {
  available: boolean;
  source: 'custom' | 'bundled' | null;
  executable_path: string | null;
  error: string | null;
  runtime_kind: ChatRuntimeKind | null;
  display_name: string | null;
  installed_version: string | null;
  manifest_valid: boolean;
  managed_by_streamnook: boolean;
  updater_eligible: boolean;
}

// Module scope, not inside the render body: a nested component definition gets a
// fresh type every render, so React unmounts and remounts it (losing the CSS
// transition) and the static-components lint rule flags it. Same shape as the
// Toggle in ChatSettings.tsx.
const Toggle = ({ enabled, onChange }: { enabled: boolean; onChange: () => void }) => (
  <button
    onClick={onChange}
    className={`relative inline-flex h-6 w-11 flex-shrink-0 items-center rounded-full transition-colors ${
      enabled ? 'bg-accent' : 'bg-gray-600'
    }`}
  >
    <span
      className={`inline-block h-4 w-4 transform rounded-full bg-white transition-transform ${
        enabled ? 'translate-x-6' : 'translate-x-1'
      }`}
    />
  </button>
);

const IntegrationsSettings = () => {
  const { settings, updateSettings } = useAppStore();

  // === Moltorino (optional external chat client) ===
  const moltorino = settings.moltorino ?? {};
  const storedPath = moltorino.executable_path ?? '';
  // Controlled input, kept separate from the persisted value so typing never
  // writes settings.json on every keystroke — we persist (and validate) on blur,
  // on Browse, and on the explicit Verify press.
  const [pathInput, setPathInput] = useState(storedPath);
  const [checking, setChecking] = useState(false);
  const [check, setCheck] = useState<{ ok: boolean; message: string } | null>(null);
  // Which runtime the backend would actually use (bundled vs custom), or the
  // not-found reason. This is the source of truth for the status line — the empty
  // path field no longer implies "unavailable", because a bundled copy may exist.
  const [runtime, setRuntime] = useState<ChatRuntimeStatus | null>(null);

  const setMoltorino = (patch: Partial<typeof moltorino>) =>
    updateSettings({ ...settings, moltorino: { ...moltorino, ...patch } });

  // Ask the backend which runtime it would launch. Read-only (never spawns
  // Moltorino); safe to call on mount and after the saved path changes.
  const refreshRuntime = useCallback(() => {
    let alive = true;
    invoke<ChatRuntimeStatus>('chat_runtime_status')
      .then((status) => {
        if (alive) setRuntime(status);
      })
      .catch((e) => {
        if (alive)
          setRuntime({
            available: false,
            source: null,
            executable_path: null,
            error: String(e),
            runtime_kind: null,
            display_name: null,
            installed_version: null,
            manifest_valid: false,
            managed_by_streamnook: false,
            updater_eligible: false,
          });
      });
    return () => {
      alive = false;
    };
  }, []);

  // Validate whatever is already saved when the tab opens, so a returning user
  // sees the confirmed exe name instead of an unlabeled path. Also re-seeds the
  // input if the stored value changes underneath us (e.g. settings finish loading).
  useEffect(() => {
    setPathInput(storedPath);
    if (!storedPath) {
      setCheck(null);
      return;
    }
    let alive = true;
    invoke<ChatRuntimePathInfo>('validate_chat_runtime_path', { path: storedPath })
      .then((info) => {
        if (alive) setCheck({ ok: true, message: `Found ${info.file_name}` });
      })
      .catch((e) => {
        if (alive) setCheck({ ok: false, message: String(e) });
      });
    return () => {
      alive = false;
    };
  }, [storedPath]);

  // Refresh the runtime status on mount and whenever the saved path changes, so
  // the status line reflects the value actually persisted (and thus what the
  // resolver would pick) — clearing the field flips it to the bundled state, and
  // setting a valid custom path flips it to custom, without a manual reload.
  useEffect(() => refreshRuntime(), [storedPath, refreshRuntime]);

  // Save + validate in one step so the feedback always describes the value that
  // is actually persisted. An invalid path is still saved (so it isn't lost and
  // can be corrected) but the error stays on screen.
  const commitPath = async (raw: string) => {
    const next = raw.trim();
    if (next !== storedPath) setMoltorino({ executable_path: next });
    if (!next) {
      setCheck(null);
      return;
    }
    setChecking(true);
    try {
      const info = await invoke<ChatRuntimePathInfo>('validate_chat_runtime_path', { path: next });
      setCheck({ ok: true, message: `Found ${info.file_name}` });
    } catch (e) {
      setCheck({ ok: false, message: String(e) });
    } finally {
      setChecking(false);
    }
  };

  const browseForChatRuntime = async () => {
    try {
      const { open } = await import('@tauri-apps/plugin-dialog');
      const picked = await open({
        title: 'Select a compatible chat application',
        multiple: false,
        directory: false,
        filters: [{ name: 'Application', extensions: ['exe'] }],
      });
      if (typeof picked !== 'string' || !picked) return; // cancelled
      setPathInput(picked);
      await commitPath(picked);
    } catch (e) {
      Logger.error('[Integrations] Chat runtime browse failed:', e);
    }
  };
  // Plugins contribute their own integration panels here, the same way a drops
  // plugin contributes into the Drops center's settings slot. The tab renders
  // whatever is contributed and names none of it; with no such plugin installed
  // the slot is empty and only the built-in integrations show.
  const pluginPanels = usePluginUiRegistry(selectSlot('integrations.settings'));

  return (
    <div className="flex min-h-full flex-col items-center py-6">
      {/* my-auto centers the group vertically when there's room and degrades to
          top-aligned + scrollable when the content outgrows the pane — unlike
          justify-center, which clips the top out of reach. */}
      <div className="my-auto flex w-full max-w-[640px] flex-col items-center">
        {/* Intro — anchors the tab so a single integration reads as a deliberate,
            centered screen rather than one stray row across a wide empty page. */}
        <div className="mb-5 flex max-w-[340px] flex-col items-center text-center">
        <div className="mb-3 flex h-14 w-14 items-center justify-center rounded-2xl bg-accent/10 text-accent">
          <Plug className="h-6 w-6" />
        </div>
        <h2 className="text-[17px] font-semibold text-textPrimary">Integrations</h2>
        <p className="mt-1.5 text-[13px] leading-relaxed text-textSecondary">
          Connect StreamNook with the apps you already use.
        </p>
      </div>

        {/* Integration cards (room to stack as more land here) */}
        <div className="w-full space-y-3">
        <div className="glass-panel rounded-lg p-4">
          <div className="flex items-center gap-3.5">
            {/* Just the two marks with a + between them, signalling StreamNook and
                Discord working together — no tiles, aligned on one line. */}
            <div className="flex flex-shrink-0 items-center gap-1.5">
              <img
                src={streamnookLogo}
                alt="StreamNook"
                className="h-7 w-7 object-contain"
              />
              <span className="text-[13px] font-medium text-textMuted">+</span>
              <DiscordGlyph size={26} className="text-[#5865F2]" />
            </div>
            <div className="min-w-0 flex-1">
              <div className="text-[14px] font-semibold text-textPrimary">Discord Rich Presence</div>
              <p className="mt-0.5 text-[12px] leading-relaxed text-textSecondary">
                Show what you're watching on your Discord profile.
              </p>
            </div>
            <Toggle
              enabled={settings.discord_rpc_enabled}
              onChange={() =>
                updateSettings({ ...settings, discord_rpc_enabled: !settings.discord_rpc_enabled })
              }
            />
          </div>
        </div>

        {/* Moltorino — an external Twitch chat client the user installs and
            maintains themselves. StreamNook doesn't bundle, embed, or link it;
            it just launches the exe the user points at here. Native chat is
            unaffected and stays the default everywhere. */}
        <div id="settings-section-moltorino" className="glass-panel rounded-lg p-4">
          <div className="flex items-center gap-3.5">
            <div className="flex flex-shrink-0 items-center gap-1.5">
              <img src={streamnookLogo} alt="StreamNook" className="h-7 w-7 object-contain" />
              <span className="text-[13px] font-medium text-textMuted">+</span>
              <MessagesSquare className="h-[26px] w-[26px] text-accent" />
            </div>
            <div className="min-w-0 flex-1">
              <div className="text-[14px] font-semibold text-textPrimary">Chat Runtime</div>
              <p className="mt-0.5 text-[12px] leading-relaxed text-textSecondary">
                Bundled runtime: Bluzyrino. You can instead choose another compatible chat executable.
                Native chat keeps running as normal.
              </p>
            </div>
          </div>

          <div className="mt-3.5 space-y-2.5 border-t border-white/[0.06] pt-3.5">
            {/* Runtime status — the backend resolver is the source of truth for
                which executable actually runs. An empty custom path means "use the
                bundled copy", so we never infer availability from the field alone. */}
            {runtime && (
              <div
                className={`flex items-start gap-1.5 text-[12px] leading-relaxed ${
                  runtime.available ? 'text-emerald-400' : 'text-red-400'
                }`}
              >
                {runtime.available ? (
                  <Check className="mt-[1px] h-3 w-3 flex-shrink-0" />
                ) : (
                  <AlertCircle className="mt-[1px] h-3 w-3 flex-shrink-0" />
                )}
                <span className="min-w-0 break-words">
                  {runtime.available
                    ? runtime.runtime_kind === 'bundled_bluzyrino'
                      ? 'Using bundled Bluzyrino'
                      : runtime.runtime_kind === 'custom_bluzyrino'
                        ? 'Using custom Bluzyrino'
                        : runtime.runtime_kind === 'custom_moltorino'
                          ? 'Using custom Moltorino'
                          : runtime.runtime_kind === 'legacy_bundled_moltorino'
                            ? 'Using legacy bundled Moltorino'
                            : 'Using custom chat runtime'
                    : 'Chat runtime not found'}
                  {runtime.executable_path && (
                    <span className="text-textMuted"> — {runtime.executable_path}</span>
                  )}
                </span>
              </div>
            )}

            {runtime?.available && (
              <div className="rounded-md border border-white/[0.06] bg-white/[0.025] px-3 py-2 text-[12px] leading-relaxed">
                {runtime.runtime_kind === 'bundled_bluzyrino' ? (
                  <>
                    <div className="flex flex-wrap items-center gap-x-3 gap-y-1">
                      <span className="text-textSecondary">
                        Installed version:{' '}
                        <span className="font-medium text-textPrimary">
                          {runtime.installed_version ?? 'Unavailable'}
                        </span>
                      </span>
                      <span className={runtime.manifest_valid ? 'text-emerald-400' : 'text-red-400'}>
                        Manifest {runtime.manifest_valid ? 'valid' : 'invalid'}
                      </span>
                    </div>
                    <div
                      className={`mt-1 ${
                        runtime.managed_by_streamnook && runtime.updater_eligible
                          ? 'text-emerald-400'
                          : 'text-textMuted'
                      }`}
                    >
                      {runtime.managed_by_streamnook && runtime.updater_eligible
                        ? 'Managed by StreamNook'
                        : 'Bundled runtime is not eligible for managed updates'}
                    </div>
                  </>
                ) : runtime.runtime_kind === 'legacy_bundled_moltorino' ? (
                  <span className="text-textMuted">Legacy bundled Moltorino is not managed by the Chat Runtime updater.</span>
                ) : (
                  <span className="text-textMuted">Custom runtime not managed.</span>
                )}
              </div>
            )}

            <div>
              <div className="mb-1.5 text-[12px] font-medium text-textPrimary">
                Custom chat executable
              </div>
              <div className="flex items-center gap-2">
                <input
                  type="text"
                  value={pathInput}
                  onChange={(e) => setPathInput(e.target.value)}
                  onBlur={() => commitPath(pathInput)}
                  onKeyDown={(e) => {
                    if (e.key === 'Enter') commitPath(pathInput);
                  }}
                  spellCheck={false}
                  placeholder="Leave empty to use bundled Bluzyrino"
                  aria-label="Custom chat executable path"
                  className="glass-input min-w-0 flex-1 rounded-md px-3 py-1.5 text-[13px] text-textPrimary"
                />
                <button
                  type="button"
                  onClick={browseForChatRuntime}
                  className="glass-button-secondary flex-shrink-0 px-3 py-1.5 text-[13px] text-textSecondary hover:text-textPrimary"
                >
                  Browse
                </button>
              </div>
              {/* Result of the last validation: the resolved exe name, or exactly
                  which check the path failed. */}
              {checking && (
                <div className="mt-1.5 flex items-center gap-1.5 text-[12px] text-textMuted">
                  <Loader2 className="h-3 w-3 animate-spin" />
                  Checking that path...
                </div>
              )}
              {!checking && check && (
                <div
                  className={`mt-1.5 flex items-start gap-1.5 text-[12px] leading-relaxed ${
                    check.ok ? 'text-emerald-400' : 'text-red-400'
                  }`}
                >
                  {check.ok ? (
                    <Check className="mt-[1px] h-3 w-3 flex-shrink-0" />
                  ) : (
                    <AlertCircle className="mt-[1px] h-3 w-3 flex-shrink-0" />
                  )}
                  <span className="min-w-0 break-words">{check.message}</span>
                </div>
              )}
              {!checking && !check && (
                <p className="mt-1.5 text-[12px] leading-relaxed text-textMuted">
                  Leave this empty to use the bundled Bluzyrino runtime. A compatible custom
                  executable overrides the bundled runtime. This setting stays out of portable
                  settings backups because the path is specific to this machine.
                </p>
              )}
            </div>

            <div className="flex items-start justify-between gap-4 pt-0.5">
              <div className="min-w-0 flex-1">
                <div className="text-[12px] font-medium text-textPrimary">Open chat externally</div>
                <p className="mt-0.5 text-[12px] leading-relaxed text-textSecondary">
                  Add an external chat-runtime button next to Pop out chat in the Twitch chat header.
                  Needs a working bundled or custom runtime.
                </p>
              </div>
              <Toggle
                enabled={moltorino.show_chat_button ?? false}
                onChange={() => setMoltorino({ show_chat_button: !(moltorino.show_chat_button ?? false) })}
              />
            </div>

            {/* Phase 2: embed Moltorino inside the main chat area instead of
                launching it in a separate window. Native chat stays the default
                (and the universal fallback for VODs, non-Twitch, offline chat,
                and any embedding failure). Windows-only; needs a working runtime. */}
            <div className="flex items-start justify-between gap-4 pt-0.5">
              <div className="min-w-0 flex-1">
                <div className="text-[12px] font-medium text-textPrimary">Embedded chat</div>
                <p className="mt-0.5 text-[12px] leading-relaxed text-textSecondary">
                  Show the compatible runtime inside the main chat panel, following the channel you're
                  watching. Windows only. Native chat takes over for unsupported contexts or failures.
                </p>
              </div>
              <Toggle
                enabled={moltorino.embedded_chat ?? false}
                onChange={() => setMoltorino({ embedded_chat: !(moltorino.embedded_chat ?? false) })}
              />
            </div>
          </div>
        </div>

        {pluginPanels.map((c) => {
          const Icon = c.Icon;
          return (
            <div
              key={`${c.pluginId}:${c.id}`}
              className="space-y-2 border-t border-white/[0.06] pt-3"
            >
              <div className="flex items-center gap-2 px-1">
                <Icon size={14} className="text-accent" />
                <span className="text-[11px] uppercase tracking-[0.12em] text-textMuted">
                  {c.label}
                </span>
              </div>
              <c.Component />
            </div>
          );
        })}
        </div>
      </div>
    </div>
  );
};

export default IntegrationsSettings;
