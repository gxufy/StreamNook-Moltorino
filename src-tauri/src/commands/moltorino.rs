//! Compatible external and embedded chat-runtime integration.
//!
//! Bluzyrino is the bundled Twitch chat client. StreamNook can also run a
//! compatible user-supplied executable; this module validates its path and
//! launches it with a channel
//! argument. StreamNook's native chat is untouched and remains the default for
//! every surface.
//!
//! Process ownership: every chat runtime this module launches is tracked by its live
//! [`std::process::Child`] handle and terminated when StreamNook exits (see
//! [`shutdown_all_standalone`]), and assigned to a shared Windows Job Object with
//! `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` as a crash-safe backstop (see the
//! [`jobobject`] submodule). Cleanup is by exact handle only — nothing here ever
//! enumerates, opens, or kills a process by PID or image name, so an unrelated
//! Moltorino the user started themselves can never be reached. Launches also
//! deduplicate by channel: a standalone we already have running on a channel is
//! left alone rather than duplicated.

use serde::Serialize;
use std::path::{Path, PathBuf};

/// Process-wide Windows Job Object used as a crash-safe backstop for every
/// Moltorino child StreamNook spawns (standalone here, and the embedded host in
/// [`super::moltorino_embed`]). It is created lazily on first assignment with
/// `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, so if StreamNook dies for any reason —
/// crash, kill, power loss — the OS closes our one job handle and terminates
/// *only the processes we explicitly assigned to it*. Unrelated Moltorino
/// instances the user started themselves are never assigned and cannot be
/// touched. The explicit `Child::kill()` + `wait()` paths remain the normal
/// route; this only catches the abnormal-exit case those paths can't reach.
#[cfg(windows)]
pub(crate) mod jobobject {
    use std::sync::{Mutex, OnceLock};
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };

    /// A `HANDLE` we can move across threads. The job handle is only ever created
    /// once and closed once (at shutdown); `windows`' `HANDLE` is a raw pointer so
    /// it isn't `Send` by default, but a bare Win32 job handle is safe to use from
    /// any thread.
    struct JobHandle(HANDLE);
    unsafe impl Send for JobHandle {}

    fn slot() -> &'static Mutex<Option<JobHandle>> {
        static JOB: OnceLock<Mutex<Option<JobHandle>>> = OnceLock::new();
        JOB.get_or_init(|| Mutex::new(None))
    }

    /// Lazily create the shared job (once) and return its handle. Returns `None`
    /// if creation or configuration fails — callers treat the job as a best-effort
    /// backstop and fall back to the explicit kill paths, so a failure here is
    /// logged and non-fatal.
    fn ensure_job(guard: &mut Option<JobHandle>) -> Option<HANDLE> {
        if let Some(j) = guard.as_ref() {
            return Some(j.0);
        }
        unsafe {
            let job = match CreateJobObjectW(None, None) {
                Ok(h) if !h.is_invalid() => h,
                Ok(_) => {
                    log::warn!("[ChatRuntime] CreateJobObjectW returned an invalid handle");
                    return None;
                }
                Err(e) => {
                    log::warn!("[ChatRuntime] CreateJobObjectW failed: {e}");
                    return None;
                }
            };
            let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            if let Err(e) = SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const core::ffi::c_void,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            ) {
                log::warn!("[ChatRuntime] SetInformationJobObject failed: {e}");
                let _ = windows::Win32::Foundation::CloseHandle(job);
                return None;
            }
            *guard = Some(JobHandle(job));
            Some(job)
        }
    }

    /// Assign one freshly-spawned child (by its OS process handle) to the shared
    /// job. Best-effort: any failure is logged and ignored, since the explicit
    /// kill-and-wait path remains responsible for normal teardown.
    ///
    /// SAFETY: `raw_handle` must be a valid process `HANDLE` for a child we just
    /// spawned. We take it as `isize` (from `Child::as_raw_handle`) so this stays
    /// a pure Win32 concern and never inspects a PID.
    pub(crate) fn assign(raw_handle: isize) {
        let mut guard = match slot().lock() {
            Ok(g) => g,
            Err(_) => {
                log::warn!("[ChatRuntime] job-object lock poisoned; skipping assign");
                return;
            }
        };
        let Some(job) = ensure_job(&mut guard) else {
            return;
        };
        unsafe {
            if let Err(e) =
                AssignProcessToJobObject(job, HANDLE(raw_handle as *mut core::ffi::c_void))
            {
                // A common benign cause: the child already exited between spawn and
                // assign. Log and move on; explicit kill+wait still covers it.
                log::warn!("[ChatRuntime] AssignProcessToJobObject failed: {e}");
            }
        }
    }

    /// Close the shared job handle during *normal* shutdown, after the explicit
    /// per-child kill+wait has already run. With `KILL_ON_JOB_CLOSE`, closing the
    /// last handle terminates any child still assigned — a final net that can only
    /// ever hit processes we ourselves put in the job. Idempotent.
    pub(crate) fn close() {
        let mut guard = match slot().lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        if let Some(job) = guard.take() {
            unsafe {
                let _ = windows::Win32::Foundation::CloseHandle(job.0);
            }
        }
    }
}

/// Exact-PID window inspection. Everything here is scoped to a single process id
/// we already own (the retained [`std::process::Child`] keeps that pid from being
/// reused while we hold it), so it can only ever look at windows belonging to a
/// child *we* launched — it never enumerates or acts by executable name.
#[cfg(windows)]
mod winprobe {
    use windows::core::BOOL;
    use windows::Win32::Foundation::{HWND, LPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindow, GetWindowThreadProcessId, IsWindow, IsWindowVisible,
        SetForegroundWindow, ShowWindow, GW_OWNER, SW_RESTORE,
    };

    /// Continue enumeration (matches the codebase's `EnumWindows` callbacks).
    const CONTINUE: BOOL = BOOL(1);

    /// Collected across the [`EnumWindows`] callback: the pid we want and the first
    /// usable top-level window we find for it.
    struct Ctx {
        pid: u32,
        found: HWND,
    }

    fn is_null(h: HWND) -> bool {
        h.0.is_null()
    }

    /// EnumWindows predicate: record the first *visible, unowned, top-level* window
    /// owned by the target pid. Owned windows (`GW_OWNER` non-null) are tool/dialog
    /// popups, not the main chat window, so they're skipped. Never panics.
    unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let ctx = &mut *(lparam.0 as *mut Ctx);
        if !is_null(ctx.found) {
            return CONTINUE; // already have one; let enumeration finish cheaply
        }
        let mut wnd_pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut wnd_pid));
        if wnd_pid != ctx.pid {
            return CONTINUE;
        }
        if !IsWindowVisible(hwnd).as_bool() {
            return CONTINUE; // hidden/headless: not user-facing
        }
        // A top-level window has no owner; GW_OWNER returns null (Err or HWND(0)).
        let owner = GetWindow(hwnd, GW_OWNER).unwrap_or_default();
        if !is_null(owner) {
            return CONTINUE; // owned popup, not the main window
        }
        ctx.found = hwnd;
        CONTINUE
    }

    /// Find a usable top-level window for `pid` and, if present, restore + focus it.
    /// Returns `true` when such a window existed. All calls are best-effort: a
    /// window can vanish between enumeration and the restore, and Windows may refuse
    /// `SetForegroundWindow` — neither is fatal, and neither is treated as a failure
    /// to *find* a window (the window is still there for the user).
    pub(super) fn restore_and_focus(pid: u32) -> bool {
        let mut ctx = Ctx {
            pid,
            found: HWND::default(),
        };
        unsafe {
            // We always return CONTINUE from the callback, so EnumWindows walks the
            // full list; ignore its result and use whatever we recorded.
            let _ = EnumWindows(Some(enum_proc), LPARAM(&mut ctx as *mut Ctx as isize));
            let hwnd = ctx.found;
            if is_null(hwnd) || !IsWindow(Some(hwnd)).as_bool() {
                return false;
            }
            let _ = ShowWindow(hwnd, SW_RESTORE);
            let _ = SetForegroundWindow(hwnd);
            true
        }
    }
}

/// A process handle the standalone registry can reap on its own and force-
/// terminate on shutdown. Abstracted over a trait purely so the registry's
/// reap/dedupe/drain *logic* can be unit-tested with a fake child — a real
/// [`std::process::Child`] can't be constructed in a test without spawning an
/// actual process. Production always uses the `std::process::Child` impl below.
#[cfg(windows)]
pub(crate) trait TrackedChild {
    /// Reap this child iff it has already exited on its own. Returns `true` when
    /// the process is gone (so the registry can drop the dead entry). Must not
    /// block on a still-running process.
    fn reap_if_exited(&mut self) -> bool;
    /// Force-terminate and reap. An already-exited child is reaped without a kill.
    /// Returns an error whenever termination/reaping is uncertain, so callers keep
    /// the ownership record and never launch a replacement beside a possibly-live
    /// stale child.
    fn terminate_and_reap(&mut self) -> Result<(), String>;
    /// Look for a usable, user-facing top-level window belonging to *this exact
    /// process* and, if one exists, restore + foreground it. Returns `true` when
    /// such a window was found (the caller should surface it instead of spawning a
    /// duplicate), `false` when the process currently owns no usable window (it may
    /// be still starting, or a stale headless leftover). Read-only with respect to
    /// process lifetime and scoped to the tracked PID only — it never enumerates by
    /// image name and can never touch an unrelated Moltorino.
    fn focus_existing_window(&self) -> bool;
}

#[cfg(windows)]
impl TrackedChild for std::process::Child {
    fn reap_if_exited(&mut self) -> bool {
        matches!(self.try_wait(), Ok(Some(_)))
    }

    fn terminate_and_reap(&mut self) -> Result<(), String> {
        // Already exited on its own: reap without a kill.
        match self.try_wait() {
            Ok(Some(_)) => return Ok(()),
            Ok(None) => {}
            Err(e) => {
                return Err(format!(
                    "Couldn't determine whether the stale chat runtime exited: {e}"
                ));
            }
        }
        if let Err(kill_error) = self.kill() {
            // The process may have exited naturally between try_wait and kill. Confirm
            // that before treating the kill failure as fatal.
            return match self.try_wait() {
                Ok(Some(_)) => Ok(()),
                Ok(None) => Err(format!(
                    "Couldn't terminate the stale chat runtime: {kill_error}"
                )),
                Err(wait_error) => Err(format!(
                    "Couldn't terminate the stale chat runtime ({kill_error}) or confirm it exited ({wait_error})"
                )),
            };
        }
        self.wait()
            .map(|_| ())
            .map_err(|e| format!("Couldn't reap the terminated stale chat runtime: {e}"))
    }

    fn focus_existing_window(&self) -> bool {
        // `self.id()` is the pid of the child we spawned and still hold open, so
        // the probe is bound to exactly this process — never an unrelated one.
        winprobe::restore_and_focus(self.id())
    }
}

/// One standalone chat runtime that *this* StreamNook instance launched, tracked so
/// it can be terminated on exit by its exact retained handle — never by PID or
/// image name. Generic over [`TrackedChild`] only so the registry logic is
/// testable; production is always `Owned<std::process::Child>`.
#[cfg(windows)]
struct Owned<C: TrackedChild> {
    /// The Twitch channel it was launched for (normalized via [`normalize_channel`]),
    /// used only to deduplicate repeat launches for the same channel.
    channel: String,
    child: C,
    /// When we spawned it. Used purely to grant a short startup grace period before
    /// a windowless process is classified as stale/headless (a freshly-launched Qt
    /// app hasn't drawn its first window yet).
    launched_at: std::time::Instant,
}

/// Normalize a channel name for dedupe comparison: trim + ASCII-lowercase. Twitch
/// logins are case-insensitive, so `Reginald` and `reginald` are the same chat.
/// Kept as one helper so the launch path and the registry compare identically.
fn normalize_channel(channel: &str) -> String {
    channel.trim().to_ascii_lowercase()
}

/// How long after launch a standalone with no visible window is still treated as
/// "starting" rather than stale. Covers Qt/Moltorino's cold-start window creation
/// without an unbounded wait; past it, a windowless process is replaced.
#[cfg(windows)]
const STARTUP_GRACE: std::time::Duration = std::time::Duration::from_secs(3);

/// What a same-channel launch request should do about an existing tracked entry.
#[cfg(windows)]
#[derive(Debug, PartialEq, Eq)]
enum LaunchPlan {
    /// No live entry owns the channel — spawn a fresh standalone. `replaced_stale`
    /// records whether we first terminated a stale headless entry (for reporting).
    Spawn { replaced_stale: bool },
    /// A live entry has a usable window that we restored/focused — do not spawn.
    Focused,
    /// A live entry exists with no window yet but is inside the startup grace
    /// period — leave it be and let it finish drawing; do not spawn.
    Starting,
}

/// Serializable result of a launch request, so the caller can tell newly-launched,
/// already-open, still-starting, and stale-replaced apart. The frontend currently
/// ignores the value (it only cares about Ok vs Err), so returning this richer type
/// is backward-compatible.
#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LaunchOutcome {
    /// A new standalone process was started.
    Launched,
    /// An existing standalone's window was restored and brought to the foreground.
    Focused,
    /// A just-launched standalone is still creating its window; nothing spawned.
    Starting,
    /// A stale (alive but windowless) standalone was terminated and replaced.
    Replaced,
}

/// Decide what a same-channel launch request should do, mutating the registry to
/// match: reap entries that exited on their own, then resolve the (at most one)
/// live entry for `channel`.
///
/// `within_grace` reports whether a given entry is still inside its startup grace
/// window; it's injected so the time-dependent branch is unit-testable without
/// real sleeps. Decision order for the matching live entry:
///   1. usable window  → restore/focus it, keep the process        → `Focused`
///   2. no window, still within grace → leave it starting          → `Starting`
///   3. no window, past grace → terminate+reap *that exact child*,
///      remove it, and signal a replacement spawn                  → `Spawn{replaced_stale:true}`
/// No matching live entry at all → `Spawn{replaced_stale:false}`.
///
/// Only ever inspects/acts on entries already in the registry, so it can never
/// select or terminate an unrelated Moltorino the user started themselves.
#[cfg(windows)]
fn plan_same_channel<C: TrackedChild>(
    entries: &mut Vec<Owned<C>>,
    channel: &str,
    within_grace: impl Fn(&Owned<C>) -> bool,
) -> Result<LaunchPlan, String> {
    // Drop any entries whose process already exited on its own first, so a dead
    // handle is never probed or selected below.
    entries.retain_mut(|o| !o.child.reap_if_exited());

    let Some(idx) = entries.iter().position(|o| o.channel == channel) else {
        return Ok(LaunchPlan::Spawn {
            replaced_stale: false,
        });
    };

    // A usable, user-facing window means the chat is already open: surface it.
    if entries[idx].child.focus_existing_window() {
        return Ok(LaunchPlan::Focused);
    }
    // No window yet, but freshly launched — let it finish drawing.
    if within_grace(&entries[idx]) {
        return Ok(LaunchPlan::Starting);
    }
    // Alive but windowless past the grace period: a stale headless leftover (e.g.
    // the user closed its window). Remove its ownership record only after the exact
    // tracked child is certainly gone; on any uncertainty retain it for shutdown.
    entries[idx].child.terminate_and_reap()?;
    entries.remove(idx);
    Ok(LaunchPlan::Spawn {
        replaced_stale: true,
    })
}

/// Terminate and reap every tracked child, draining the registry. Idempotent: a
/// second call on the now-empty registry is a harmless no-op. Never panics.
#[cfg(windows)]
fn drain_and_terminate<C: TrackedChild>(entries: &mut Vec<Owned<C>>) {
    entries.retain_mut(|owned| {
        if let Err(e) = owned.child.terminate_and_reap() {
            log::warn!("[ChatRuntime] shutdown cleanup failed: {e}");
            true
        } else {
            false
        }
    });
}

/// Registry of standalone chat-runtime processes this instance owns. Guarded by a
/// plain `Mutex`; every access reaps already-exited entries so the list never
/// grows without bound and a dead handle is never selected for a kill.
#[cfg(windows)]
fn owned_standalone() -> &'static std::sync::Mutex<Vec<Owned<std::process::Child>>> {
    use std::sync::{Mutex, OnceLock};
    static OWNED: OnceLock<Mutex<Vec<Owned<std::process::Child>>>> = OnceLock::new();
    OWNED.get_or_init(|| Mutex::new(Vec::new()))
}

/// Terminate and reap every standalone chat runtime this instance launched, by its
/// exact retained handle. Idempotent and safe to call during shutdown: it never
/// panics, logs actionable errors, and continues to the next child on failure.
/// Called from `RunEvent::Exit`. No-op on non-Windows.
#[cfg(windows)]
pub fn shutdown_all_standalone() {
    let mut guard = match owned_standalone().lock() {
        Ok(g) => g,
        Err(poisoned) => {
            // Even if a prior holder panicked, we still want to drain the children.
            log::warn!(
                "[ChatRuntime] standalone registry lock poisoned during shutdown; draining anyway"
            );
            poisoned.into_inner()
        }
    };
    drain_and_terminate(&mut guard);
}

#[cfg(not(windows))]
pub fn shutdown_all_standalone() {}

/// Result of checking a user-supplied chat-runtime executable path, surfaced in
/// the Integrations settings card.
#[derive(Serialize, Clone)]
pub struct ChatRuntimePathInfo {
    /// Absolute, symlink-resolved path we would actually spawn.
    pub resolved_path: String,
    /// Trailing file name, for a compact confirmation line in the UI.
    pub file_name: String,
}

/// Twitch login rule: 1-25 characters of ASCII letters, digits, or underscore.
///
/// Hand-rolled rather than a `Regex` so the check is allocation-free and cannot
/// drift from the intended character class. This runs *before* the value reaches
/// the argument vector, which (together with `Command`'s argv-based spawn) is why
/// no shell metacharacter can ever be interpreted.
pub(crate) fn is_valid_twitch_login(channel: &str) -> bool {
    !channel.is_empty()
        && channel.len() <= 25
        && channel
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// Strip Windows' `\\?\` verbatim prefix for display. `canonicalize` always
/// returns that form on Windows, and showing it in the settings card or an error
/// message reads as a glitch to users who only recognize `C:\...`. Spawning still
/// uses the untouched canonical path — this is display-only.
pub(crate) fn display_path(path: &Path) -> String {
    let s = path.to_string_lossy();
    // UNC shares canonicalize to `\\?\UNC\server\share`; restore the `\\server\`
    // form rather than leaving a half-stripped path.
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        return format!(r"\\{rest}");
    }
    s.strip_prefix(r"\\?\").unwrap_or(&s).to_string()
}

/// Shared path validation for both commands. Returns the canonicalized path or a
/// user-facing error explaining exactly which check failed.
pub(crate) fn resolve_executable(raw: &str) -> Result<PathBuf, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(
            "No custom chat executable is set. Choose one in Settings → Integrations."
                .to_string(),
        );
    }

    // Canonicalize first: it resolves relative segments and symlinks, and its
    // failure is also our not-found signal, so one syscall covers both.
    let path = Path::new(trimmed);
    let canonical = std::fs::canonicalize(path)
        .map_err(|e| format!("Can't find that file: {} ({})", trimmed, e))?;

    if canonical.is_dir() {
        return Err(format!(
            "That path is a folder, not an application: {}",
            display_path(&canonical)
        ));
    }
    if !canonical.is_file() {
        return Err(format!(
            "That path isn't a file: {}",
            display_path(&canonical)
        ));
    }

    // Windows-only integration (Moltorino's embed/attach surface is Win32), so
    // require the real extension rather than accepting any executable bit.
    let is_exe = canonical
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("exe"))
        .unwrap_or(false);
    if !is_exe {
        return Err(format!(
            "Pick a Windows .exe file — that one is: {}",
            canonical
                .file_name()
                .map(|f| f.to_string_lossy().to_string())
                .unwrap_or_else(|| display_path(&canonical))
        ));
    }

    Ok(canonical)
}

/// Which compatible chat runtime was resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChatRuntimeKind {
    BundledBluzyrino,
    Custom,
    LegacyBundledMoltorino,
}

/// Presentation identity for a resolved runtime. This never affects launch
/// arguments or process handling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChatRuntimeIdentity {
    Bluzyrino,
    Moltorino,
    Generic,
}

impl ChatRuntimeIdentity {
    pub(crate) fn display_name(self) -> &'static str {
        match self {
            Self::Bluzyrino => "Bluzyrino",
            Self::Moltorino => "Moltorino",
            Self::Generic => "Chat runtime",
        }
    }
}

/// A resolved compatible executable and its source.
#[derive(Debug)]
pub(crate) struct ResolvedChatRuntime {
    pub(crate) kind: ChatRuntimeKind,
    pub(crate) executable_path: PathBuf,
}

impl ResolvedChatRuntime {
    /// Backward-compatible status value. Do not enrich this field: old callers
    /// understand only `custom` and `bundled`.
    pub(crate) fn source_status(&self) -> &'static str {
        match self.kind {
            ChatRuntimeKind::Custom => "custom",
            ChatRuntimeKind::BundledBluzyrino | ChatRuntimeKind::LegacyBundledMoltorino => {
                "bundled"
            }
        }
    }

    pub(crate) fn identity(&self) -> ChatRuntimeIdentity {
        match self.kind {
            ChatRuntimeKind::BundledBluzyrino => ChatRuntimeIdentity::Bluzyrino,
            ChatRuntimeKind::LegacyBundledMoltorino => ChatRuntimeIdentity::Moltorino,
            ChatRuntimeKind::Custom => identity_from_path(&self.executable_path),
        }
    }

    pub(crate) fn runtime_kind_status(&self) -> &'static str {
        match (self.kind, self.identity()) {
            (ChatRuntimeKind::BundledBluzyrino, _) => "bundled_bluzyrino",
            (ChatRuntimeKind::LegacyBundledMoltorino, _) => "legacy_bundled_moltorino",
            (ChatRuntimeKind::Custom, ChatRuntimeIdentity::Bluzyrino) => "custom_bluzyrino",
            (ChatRuntimeKind::Custom, ChatRuntimeIdentity::Moltorino) => "custom_moltorino",
            (ChatRuntimeKind::Custom, ChatRuntimeIdentity::Generic) => "custom",
        }
    }
}

fn identity_from_path(path: &Path) -> ChatRuntimeIdentity {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    if name.eq_ignore_ascii_case("Bluzyrino.exe") {
        ChatRuntimeIdentity::Bluzyrino
    } else if name.eq_ignore_ascii_case("Moltorino7.exe")
        || name.eq_ignore_ascii_case("Moltorino.exe")
    {
        ChatRuntimeIdentity::Moltorino
    } else {
        ChatRuntimeIdentity::Generic
    }
}

pub(crate) fn bundled_bluzyrino_path(exe_dir: &Path) -> PathBuf {
    exe_dir.join("chat-runtime").join("Bluzyrino.exe")
}

pub(crate) fn legacy_bundled_moltorino_path(exe_dir: &Path) -> PathBuf {
    exe_dir.join("moltorino").join("Moltorino7.exe")
}

/// Validate a user-configured override. A direct path may name any `.exe`; a
/// directory is probed for the two known compatible executable names.
fn resolve_custom_override(raw: &str) -> Result<PathBuf, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(
            "No custom chat executable is set. Choose one in Settings → Integrations."
                .to_string(),
        );
    }
    let path = Path::new(trimmed);
    if path.is_dir() {
        for file_name in ["Bluzyrino.exe", "Moltorino7.exe"] {
            let candidate = path.join(file_name);
            if candidate.is_file() {
                return resolve_executable(&candidate.to_string_lossy());
            }
        }
        return Err(format!(
            "That folder doesn't contain Bluzyrino.exe or Moltorino7.exe: {}",
            display_path(path)
        ));
    }
    resolve_executable(trimmed)
}

/// Resolve the configured custom executable, then the new bundled Bluzyrino
/// layout, then the legacy bundled Moltorino layout.
pub(crate) fn resolve_runtime_in(
    configured: &str,
    exe_dir: &Path,
) -> Result<ResolvedChatRuntime, String> {
    if !configured.trim().is_empty() {
        match resolve_custom_override(configured) {
            Ok(executable_path) => {
                return Ok(ResolvedChatRuntime {
                    kind: ChatRuntimeKind::Custom,
                    executable_path,
                });
            }
            Err(e) => {
                log::warn!(
                    "[ChatRuntime] configured path is unusable, trying bundled runtimes: {e}"
                );
            }
        }
    }

    let bluzyrino = bundled_bluzyrino_path(exe_dir);
    if bluzyrino.is_file() {
        return Ok(ResolvedChatRuntime {
            kind: ChatRuntimeKind::BundledBluzyrino,
            executable_path: bluzyrino,
        });
    }

    let moltorino = legacy_bundled_moltorino_path(exe_dir);
    if moltorino.is_file() {
        return Ok(ResolvedChatRuntime {
            kind: ChatRuntimeKind::LegacyBundledMoltorino,
            executable_path: moltorino,
        });
    }

    Err(format!(
        "Chat runtime not found. StreamNook looked for bundled copies at {} and {} and no usable \
         custom path is set in Settings → Integrations.",
        display_path(&bluzyrino),
        display_path(&moltorino)
    ))
}

pub(crate) fn resolve_chat_runtime(configured: &str) -> Result<ResolvedChatRuntime, String> {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .ok_or_else(|| "Couldn't locate the StreamNook program folder.".to_string())?;
    resolve_runtime_in(configured, &exe_dir)
}

/// Internal compatibility alias used by the unchanged embed implementation.
pub(crate) fn resolve_moltorino_runtime(
    configured: &str,
) -> Result<ResolvedChatRuntime, String> {
    resolve_chat_runtime(configured)
}

/// Read-only view of runtime resolution. The first four fields are the legacy
/// contract; all newer fields are additive presentation and updater-trust metadata.
#[derive(Serialize, Clone)]
pub struct ChatRuntimeStatus {
    pub available: bool,
    /// Exactly `custom`, `bundled`, or null for backward compatibility.
    pub source: Option<String>,
    pub executable_path: Option<String>,
    pub error: Option<String>,
    pub runtime_kind: Option<String>,
    pub display_name: Option<String>,
    /// Present only for a fully validated bundled Bluzyrino manifest.
    pub installed_version: Option<String>,
    pub manifest_valid: bool,
    pub managed_by_streamnook: bool,
    pub updater_eligible: bool,
}

/// Compatibility type name for Rust callers; serialization is unchanged.
pub type MoltorinoRuntimeStatus = ChatRuntimeStatus;

fn runtime_status_for(configured: &str, exe_dir: &Path) -> ChatRuntimeStatus {
    match resolve_runtime_in(configured, exe_dir) {
        Ok(runtime) => {
            let installed = if runtime.kind == ChatRuntimeKind::BundledBluzyrino {
                let runtime_root = runtime
                    .executable_path
                    .parent()
                    .unwrap_or_else(|| Path::new(""));
                match super::chat_runtime_update::validate_installed_runtime(runtime_root) {
                    Ok(validated) => Some(validated),
                    Err(error) => {
                        // Resolution and launch compatibility remain existence-based. A
                        // bad manifest removes updater trust, not runtime availability.
                        log::warn!(
                            "[ChatRuntime] bundled Bluzyrino manifest is invalid; managed updates disabled: {error}"
                        );
                        None
                    }
                }
            } else {
                None
            };
            let manifest_valid = installed.is_some();
            ChatRuntimeStatus {
                available: true,
                source: Some(runtime.source_status().to_string()),
                executable_path: Some(display_path(&runtime.executable_path)),
                error: None,
                runtime_kind: Some(runtime.runtime_kind_status().to_string()),
                display_name: Some(runtime.identity().display_name().to_string()),
                installed_version: installed.map(|runtime| runtime.version),
                manifest_valid,
                managed_by_streamnook: manifest_valid,
                updater_eligible: manifest_valid,
            }
        }
        Err(e) => ChatRuntimeStatus {
            available: false,
            source: None,
            executable_path: None,
            error: Some(e),
            runtime_kind: None,
            display_name: None,
            installed_version: None,
            manifest_valid: false,
            managed_by_streamnook: false,
            updater_eligible: false,
        },
    }
}

fn configured_runtime_path(
    state: &tauri::State<'_, crate::models::settings::AppState>,
) -> Result<String, String> {
    let settings = state
        .settings
        .lock()
        .map_err(|_| "Couldn't read settings.".to_string())?;
    Ok(settings
        .moltorino
        .executable_path
        .clone()
        .unwrap_or_default())
}

fn current_exe_dir() -> Result<PathBuf, String> {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .ok_or_else(|| "Couldn't locate the StreamNook program folder.".to_string())
}

#[tauri::command]
pub async fn chat_runtime_status(
    state: tauri::State<'_, crate::models::settings::AppState>,
) -> Result<ChatRuntimeStatus, String> {
    let configured = configured_runtime_path(&state)?;
    let exe_dir = current_exe_dir()?;
    // A complete bundle validation hashes every payload file. Keep that blocking
    // filesystem work off Tauri's async executor while preserving the same
    // read-only resolver/status implementation used by tests.
    tokio::task::spawn_blocking(move || runtime_status_for(&configured, &exe_dir))
        .await
        .map_err(|e| format!("Couldn't validate the chat runtime: {e}"))
}

#[tauri::command]
pub async fn moltorino_runtime_status(
    state: tauri::State<'_, crate::models::settings::AppState>,
) -> Result<MoltorinoRuntimeStatus, String> {
    chat_runtime_status(state).await
}

fn validate_runtime_path(path: &str) -> Result<ChatRuntimePathInfo, String> {
    let resolved = resolve_custom_override(path)?;
    Ok(ChatRuntimePathInfo {
        file_name: resolved
            .file_name()
            .map(|f| f.to_string_lossy().to_string())
            .unwrap_or_default(),
        resolved_path: display_path(&resolved),
    })
}

#[tauri::command]
pub async fn validate_chat_runtime_path(path: String) -> Result<ChatRuntimePathInfo, String> {
    validate_runtime_path(&path)
}

#[tauri::command]
pub async fn validate_moltorino_path(path: String) -> Result<ChatRuntimePathInfo, String> {
    validate_chat_runtime_path(path).await
}

fn launch_runtime_with_configured(
    path: Option<String>,
    channel: String,
    configured: String,
) -> Result<LaunchOutcome, String> {
    let raw = path.filter(|p| !p.trim().is_empty()).unwrap_or(configured);
    let channel = normalize_channel(&channel);
    if !is_valid_twitch_login(&channel) {
        return Err(format!("\"{}\" isn't a valid Twitch channel name.", channel));
    }
    let runtime = resolve_chat_runtime(&raw)?;
    spawn_moltorino(&runtime.executable_path, &channel)
}

/// Launch a compatible runtime with `-c t:<channel>` using the shared owned-child
/// registry and lifecycle implementation.
#[tauri::command]
pub async fn launch_chat_runtime(
    path: Option<String>,
    channel: String,
    state: tauri::State<'_, crate::models::settings::AppState>,
) -> Result<LaunchOutcome, String> {
    let configured = configured_runtime_path(&state)?;
    launch_runtime_with_configured(path, channel, configured)
}

#[tauri::command]
pub async fn launch_moltorino(
    path: Option<String>,
    channel: String,
    state: tauri::State<'_, crate::models::settings::AppState>,
) -> Result<LaunchOutcome, String> {
    launch_chat_runtime(path, channel, state).await
}

/// Launch the full chat-runtime UI without a channel argument.
/// This lets bundled Bluzyrino users sign in and edit Bluzyrino settings
/// without manually browsing into the chat-runtime directory.
#[tauri::command]
pub async fn launch_chat_runtime_setup(
    path: Option<String>,
    state: tauri::State<'_, crate::models::settings::AppState>,
) -> Result<LaunchOutcome, String> {
    // Bluzyrino/Chatterino is effectively single-instance. A running embedded
    // process or a StreamNook-owned "-c" external-chat process can otherwise
    // capture this no-argument launch, leaving the user in command-line mode
    // where account/settings editing is forbidden.
    super::moltorino_embed::stop_blocking_for_setup();
    shutdown_all_standalone();

    let configured = configured_runtime_path(&state)?;
    let raw = path
        .filter(|p| !p.trim().is_empty())
        .unwrap_or(configured);
    let runtime = resolve_chat_runtime(&raw)?;
    spawn_chat_runtime_setup(&runtime.executable_path)
}

/// Close only the full Bluzyrino account/settings instance StreamNook launched.
/// This is used by "Use Bluzyrino Here" before the embedded instance is restarted,
/// so the new process rereads the saved account/preferences.
#[tauri::command]
pub async fn close_chat_runtime_setup() -> Result<(), String> {
    #[cfg(windows)]
    {
        const SETUP_REGISTRY_KEY: &str = "__streamnook_chat_runtime_setup__";

        let mut guard = owned_standalone()
            .lock()
            .map_err(|_| "Couldn't access the chat-runtime process registry.".to_string())?;

        // Reap anything that already exited naturally first.
        guard.retain_mut(|owned| !owned.child.reap_if_exited());

        let Some(index) = guard
            .iter()
            .position(|owned| owned.channel == SETUP_REGISTRY_KEY)
        else {
            return Ok(());
        };

        let mut owned = guard.remove(index);
        if let Err(e) = owned.child.terminate_and_reap() {
            // Keep ownership if teardown is uncertain. We must never lose the
            // retained handle and accidentally leave a StreamNook-owned process
            // outside our lifecycle tracking.
            guard.insert(index, owned);
            return Err(format!("Couldn't close Bluzyrino account/settings: {e}"));
        }

        Ok(())
    }

    #[cfg(not(windows))]
    {
        Ok(())
    }
}

#[cfg(windows)]
fn spawn_chat_runtime_setup(exe: &Path) -> Result<LaunchOutcome, String> {
    use std::os::windows::io::AsRawHandle;
    use std::os::windows::process::CommandExt;

    const SETUP_REGISTRY_KEY: &str = "__streamnook_chat_runtime_setup__";

    let mut guard = owned_standalone()
        .lock()
        .map_err(|_| "Couldn't access the chat-runtime process registry.".to_string())?;

    let replaced_stale = match plan_same_channel(
        &mut guard,
        SETUP_REGISTRY_KEY,
        |o| o.launched_at.elapsed() < STARTUP_GRACE,
    )? {
        LaunchPlan::Focused => return Ok(LaunchOutcome::Focused),
        LaunchPlan::Starting => return Ok(LaunchOutcome::Starting),
        LaunchPlan::Spawn { replaced_stale } => replaced_stale,
    };

    let child = std::process::Command::new(exe)
        .creation_flags(0x0800_0000)
        .spawn()
        .map_err(|e| {
            format!(
                "Couldn't start the chat runtime at {}: {}",
                display_path(exe),
                e
            )
        })?;

    jobobject::assign(child.as_raw_handle() as isize);

    guard.push(Owned {
        channel: SETUP_REGISTRY_KEY.to_string(),
        child,
        launched_at: std::time::Instant::now(),
    });

    Ok(if replaced_stale {
        LaunchOutcome::Replaced
    } else {
        LaunchOutcome::Launched
    })
}

#[cfg(not(windows))]
fn spawn_chat_runtime_setup(
    _exe: &Path,
) -> Result<LaunchOutcome, String> {
    Err("The chat-runtime integration is only available on Windows.".to_string())
}

#[cfg(windows)]
fn spawn_moltorino(exe: &Path, channel: &str) -> Result<LaunchOutcome, String> {
    use std::os::windows::io::AsRawHandle;
    use std::os::windows::process::CommandExt;

    // Take the registry lock up front and hold it across the whole decide→spawn
    // sequence, so two rapid same-channel clicks can't both get past the check:
    // the first replaces/focuses/records under the lock, the second then sees that
    // result. Exactly one replacement can be spawned per channel per contended run.
    let mut guard = owned_standalone()
        .lock()
        .map_err(|_| "Couldn't access the chat-runtime process registry.".to_string())?;

    // Resolve what to do about any existing standalone on this channel. This reaps
    // dead entries, focuses a live window if there is one, respects a startup grace
    // window, and otherwise terminates the *exact* stale headless child so we can
    // replace it. It only ever touches children we launched — never an unrelated
    // Moltorino the user started themselves.
    let replaced_stale = match plan_same_channel(&mut guard, channel, |o| {
        o.launched_at.elapsed() < STARTUP_GRACE
    })? {
        LaunchPlan::Focused => {
            log::debug!("[ChatRuntime] standalone for '{channel}' already open; focused it");
            return Ok(LaunchOutcome::Focused);
        }
        LaunchPlan::Starting => {
            log::debug!(
                "[ChatRuntime] standalone for '{channel}' is still starting; not launching another"
            );
            return Ok(LaunchOutcome::Starting);
        }
        LaunchPlan::Spawn { replaced_stale } => {
            if replaced_stale {
                log::info!("[ChatRuntime] replacing stale windowless standalone for '{channel}'");
            }
            replaced_stale
        }
    };

    // Argument vector, never a shell string — nothing here is parsed by cmd.exe.
    let child = std::process::Command::new(exe)
        .arg("-c")
        .arg(format!("t:{channel}"))
        // CREATE_NO_WINDOW: no stray console flashes behind the GUI. Matches the
        // plugin host's spawn flags.
        .creation_flags(0x0800_0000)
        .spawn()
        .map_err(|e| format!("Couldn't start the chat runtime at {}: {}", display_path(exe), e))?;

    // Assign to the crash-safe job before recording: if StreamNook dies before a
    // clean shutdown, the OS still kills this child. Best-effort — the explicit
    // kill+wait in `shutdown_all_standalone` is the normal teardown path.
    jobobject::assign(child.as_raw_handle() as isize);

    guard.push(Owned {
        channel: channel.to_string(),
        child,
        launched_at: std::time::Instant::now(),
    });
    Ok(if replaced_stale {
        LaunchOutcome::Replaced
    } else {
        LaunchOutcome::Launched
    })
}

#[cfg(not(windows))]
fn spawn_moltorino(_exe: &Path, _channel: &str) -> Result<LaunchOutcome, String> {
    Err("The chat-runtime integration is only available on Windows.".to_string())
}

#[cfg(test)]
mod tests {
    use super::{
        bundled_bluzyrino_path, display_path, identity_from_path, is_valid_twitch_login,
        legacy_bundled_moltorino_path, resolve_executable, resolve_runtime_in,
        runtime_status_for, ChatRuntimeIdentity, ChatRuntimeKind,
    };
    use sha2::{Digest, Sha256};
    use std::path::Path;

    /// A unique temp directory for one test, plus a helper to drop a real
    /// `Moltorino7.exe` (or an arbitrary file) into it. Cleaned up on `Drop` so no
    /// probe files leak between runs. Uses the process id + a per-call counter so
    /// parallel tests never collide on the same directory.
    struct TempTree {
        root: std::path::PathBuf,
    }

    impl TempTree {
        fn new(tag: &str) -> Self {
            use std::sync::atomic::{AtomicU32, Ordering};
            static SEQ: AtomicU32 = AtomicU32::new(0);
            let n = SEQ.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir().join(format!(
                "streamnook_moltorino_rt_{}_{}_{}",
                std::process::id(),
                tag,
                n
            ));
            std::fs::create_dir_all(&root).expect("create temp tree root");
            TempTree { root }
        }

        /// Create a directory under the tree and return its path.
        fn dir(&self, rel: &str) -> std::path::PathBuf {
            let p = self.root.join(rel);
            std::fs::create_dir_all(&p).expect("create temp subdir");
            p
        }

        /// Write a file (any bytes) at a relative path, creating parents.
        fn file(&self, rel: &str) -> std::path::PathBuf {
            self.file_with(rel, b"probe")
        }

        fn file_with(&self, rel: &str, bytes: &[u8]) -> std::path::PathBuf {
            let p = self.root.join(rel);
            if let Some(parent) = p.parent() {
                std::fs::create_dir_all(parent).expect("create temp file parent");
            }
            std::fs::write(&p, bytes).expect("write temp file");
            p
        }

        fn valid_bluzyrino_bundle(&self) {
            let exe = b"bundled-bluzyrino";
            let support = b"qt-platform";
            self.file_with("app/chat-runtime/Bluzyrino.exe", exe);
            self.file_with("app/chat-runtime/platforms/qwindows.dll", support);
            let manifest = serde_json::json!({
                "runtime_id": "bluzyrino",
                "version": "2.0.3",
                "entrypoint": "Bluzyrino.exe",
                "architecture": "x86_64",
                "generated_utc": "2026-08-11T03:20:03Z",
                "archive_root": "chat-runtime",
                "file_count": 2,
                "total_size_bytes": exe.len() + support.len(),
                "files": [
                    {
                        "path": "Bluzyrino.exe",
                        "size": exe.len(),
                        "sha256": format!("{:x}", Sha256::digest(exe)),
                    },
                    {
                        "path": "platforms/qwindows.dll",
                        "size": support.len(),
                        "sha256": format!("{:x}", Sha256::digest(support)),
                    }
                ]
            });
            self.file_with(
                "app/chat-runtime/runtime-manifest.json",
                &serde_json::to_vec_pretty(&manifest).expect("serialize runtime manifest"),
            );
        }
    }

    impl Drop for TempTree {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn accepts_real_logins() {
        assert!(is_valid_twitch_login("forsen"));
        assert!(is_valid_twitch_login("some_user_123"));
        assert!(is_valid_twitch_login("a"));
        assert!(is_valid_twitch_login(&"a".repeat(25)));
    }

    #[test]
    fn rejects_empty_overlong_and_injection_shapes() {
        assert!(!is_valid_twitch_login(""));
        assert!(!is_valid_twitch_login(&"a".repeat(26)));
        assert!(!is_valid_twitch_login("has space"));
        assert!(!is_valid_twitch_login("semi;colon"));
        assert!(!is_valid_twitch_login("--flag"));
        assert!(!is_valid_twitch_login("t:prefixed"));
        assert!(!is_valid_twitch_login("quote\"mark"));
        assert!(!is_valid_twitch_login("amp&persand"));
    }

    #[test]
    fn strips_verbatim_prefix_for_display() {
        assert_eq!(
            display_path(Path::new(r"\\?\C:\Apps\Moltorino\moltorino.exe")),
            r"C:\Apps\Moltorino\moltorino.exe"
        );
        // UNC round-trips back to the double-backslash form users recognize.
        assert_eq!(
            display_path(Path::new(r"\\?\UNC\nas\share\moltorino.exe")),
            r"\\nas\share\moltorino.exe"
        );
        // Anything already plain is passed through untouched.
        assert_eq!(
            display_path(Path::new(r"C:\Apps\moltorino.exe")),
            r"C:\Apps\moltorino.exe"
        );
    }

    #[test]
    fn rejects_empty_and_missing_paths() {
        assert!(resolve_executable("").is_err());
        assert!(resolve_executable("   ").is_err());
        assert!(resolve_executable(r"C:\definitely\not\here\moltorino.exe").is_err());
    }

    /// A directory must be rejected even though it exists — and on Windows the
    /// temp dir has no `.exe` extension either, so this also pins that the error
    /// is reported rather than the path being accepted.
    #[test]
    fn rejects_a_directory() {
        let dir = std::env::temp_dir();
        assert!(resolve_executable(&dir.to_string_lossy()).is_err());
    }

    /// An existing file with the wrong extension is rejected by the .exe check.
    #[test]
    fn rejects_non_exe_file() {
        let path = std::env::temp_dir().join("streamnook_moltorino_test_probe.txt");
        std::fs::write(&path, b"probe").expect("write temp probe");
        let result = resolve_executable(&path.to_string_lossy());
        let _ = std::fs::remove_file(&path);
        let err = result.expect_err("a .txt file must not validate as Moltorino");
        assert!(err.contains(".exe"), "unexpected error text: {err}");
    }

    // --- Chat runtime resolution ---------------------------------------------

    #[test]
    fn bundled_paths_are_beside_the_streamnook_executable() {
        let exe_dir = Path::new(r"C:\Program Files\StreamNook");
        assert!(bundled_bluzyrino_path(exe_dir)
            .ends_with(Path::new("chat-runtime").join("Bluzyrino.exe")));
        assert!(legacy_bundled_moltorino_path(exe_dir)
            .ends_with(Path::new("moltorino").join("Moltorino7.exe")));
    }

    #[test]
    fn direct_custom_executables_accept_known_and_arbitrary_names() {
        let tree = TempTree::new("direct_customs");
        let exe_dir = tree.dir("app");
        for (name, identity) in [
            ("Bluzyrino.exe", ChatRuntimeIdentity::Bluzyrino),
            ("Moltorino7.exe", ChatRuntimeIdentity::Moltorino),
            ("CompatibleChat.exe", ChatRuntimeIdentity::Generic),
        ] {
            let custom = tree.file(&format!("custom/{name}"));
            let runtime = resolve_runtime_in(&custom.to_string_lossy(), &exe_dir)
                .expect("direct custom executable must resolve");
            assert_eq!(runtime.kind, ChatRuntimeKind::Custom);
            assert_eq!(runtime.identity(), identity);
            assert_eq!(runtime.executable_path.file_name().unwrap(), name);
        }
    }

    #[test]
    fn custom_directory_prefers_bluzyrino_then_falls_back_to_moltorino() {
        let tree = TempTree::new("custom_dirs");
        let both = tree.dir("both");
        tree.file("both/Bluzyrino.exe");
        tree.file("both/Moltorino7.exe");
        let exe_dir = tree.dir("app");
        let runtime = resolve_runtime_in(&both.to_string_lossy(), &exe_dir).unwrap();
        assert_eq!(runtime.executable_path.file_name().unwrap(), "Bluzyrino.exe");

        let legacy = tree.dir("legacy");
        tree.file("legacy/Moltorino7.exe");
        let runtime = resolve_runtime_in(&legacy.to_string_lossy(), &exe_dir).unwrap();
        assert_eq!(runtime.executable_path.file_name().unwrap(), "Moltorino7.exe");
    }

    #[test]
    fn valid_custom_wins_over_both_bundles() {
        let tree = TempTree::new("custom_wins");
        let custom = tree.file("custom/CompatibleChat.exe");
        let exe_dir = tree.dir("app");
        tree.file("app/chat-runtime/Bluzyrino.exe");
        tree.file("app/moltorino/Moltorino7.exe");
        let runtime = resolve_runtime_in(&custom.to_string_lossy(), &exe_dir).unwrap();
        assert_eq!(runtime.kind, ChatRuntimeKind::Custom);
        assert!(runtime.executable_path.to_string_lossy().contains("custom"));
    }

    #[test]
    fn bundled_bluzyrino_precedes_legacy_moltorino() {
        let tree = TempTree::new("bundle_order");
        let exe_dir = tree.dir("app");
        tree.file("app/chat-runtime/Bluzyrino.exe");
        tree.file("app/moltorino/Moltorino7.exe");
        let runtime = resolve_runtime_in("", &exe_dir).unwrap();
        assert_eq!(runtime.kind, ChatRuntimeKind::BundledBluzyrino);
    }

    #[test]
    fn legacy_bundled_moltorino_remains_a_fallback() {
        let tree = TempTree::new("legacy_bundle");
        let exe_dir = tree.dir("app");
        tree.file("app/moltorino/Moltorino7.exe");
        let runtime = resolve_runtime_in("", &exe_dir).unwrap();
        assert_eq!(runtime.kind, ChatRuntimeKind::LegacyBundledMoltorino);
        assert_eq!(runtime.source_status(), "bundled");
    }

    #[test]
    fn invalid_custom_falls_back_to_available_bundle() {
        let tree = TempTree::new("invalid_falls_back");
        let exe_dir = tree.dir("app");
        tree.file("app/chat-runtime/Bluzyrino.exe");
        let runtime = resolve_runtime_in(r"C:\nope\not\here\chat.exe", &exe_dir).unwrap();
        assert_eq!(runtime.kind, ChatRuntimeKind::BundledBluzyrino);
    }

    #[test]
    fn unavailable_state_preserves_legacy_status_fields() {
        let tree = TempTree::new("unavailable");
        let exe_dir = tree.dir("app");
        let status = runtime_status_for("", &exe_dir);
        assert!(!status.available);
        assert_eq!(status.source, None);
        assert_eq!(status.executable_path, None);
        assert!(status.error.is_some());
        assert_eq!(status.runtime_kind, None);
    }

    #[test]
    fn status_source_semantics_remain_backward_compatible() {
        let tree = TempTree::new("status_source");
        let exe_dir = tree.dir("app");
        let custom = tree.file("custom/Bluzyrino.exe");
        let custom_status = runtime_status_for(&custom.to_string_lossy(), &exe_dir);
        assert_eq!(custom_status.source.as_deref(), Some("custom"));
        assert_eq!(custom_status.runtime_kind.as_deref(), Some("custom_bluzyrino"));

        tree.file("app/chat-runtime/Bluzyrino.exe");
        let bundled_status = runtime_status_for("", &exe_dir);
        assert_eq!(bundled_status.source.as_deref(), Some("bundled"));
        assert_eq!(bundled_status.runtime_kind.as_deref(), Some("bundled_bluzyrino"));
    }

    #[test]
    fn valid_bundled_status_is_versioned_managed_and_eligible() {
        let tree = TempTree::new("managed_bundle");
        let exe_dir = tree.dir("app");
        tree.valid_bluzyrino_bundle();

        let status = runtime_status_for("", &exe_dir);
        assert!(status.available);
        assert_eq!(status.installed_version.as_deref(), Some("2.0.3"));
        assert!(status.manifest_valid);
        assert!(status.managed_by_streamnook);
        assert!(status.updater_eligible);
    }

    #[test]
    fn missing_or_invalid_bundled_manifest_stays_available_but_ineligible() {
        let missing = TempTree::new("missing_bundle_manifest");
        let missing_exe_dir = missing.dir("app");
        missing.file("app/chat-runtime/Bluzyrino.exe");
        let missing_status = runtime_status_for("", &missing_exe_dir);
        assert!(missing_status.available);
        assert_eq!(
            missing_status.runtime_kind.as_deref(),
            Some("bundled_bluzyrino")
        );
        assert_eq!(missing_status.installed_version, None);
        assert!(!missing_status.manifest_valid);
        assert!(!missing_status.managed_by_streamnook);
        assert!(!missing_status.updater_eligible);

        let invalid = TempTree::new("invalid_bundle_manifest");
        let invalid_exe_dir = invalid.dir("app");
        invalid.valid_bluzyrino_bundle();
        invalid.file_with("app/chat-runtime/Bluzyrino.exe", b"tampered");
        let invalid_status = runtime_status_for("", &invalid_exe_dir);
        assert!(invalid_status.available);
        assert_eq!(invalid_status.installed_version, None);
        assert!(!invalid_status.manifest_valid);
        assert!(!invalid_status.updater_eligible);
    }

    #[test]
    fn custom_runtime_is_unmanaged_and_ineligible_even_with_valid_bundle() {
        let tree = TempTree::new("custom_unmanaged");
        let exe_dir = tree.dir("app");
        tree.valid_bluzyrino_bundle();
        let custom = tree.file("custom/Bluzyrino.exe");

        let status = runtime_status_for(&custom.to_string_lossy(), &exe_dir);
        assert!(status.available);
        assert_eq!(status.runtime_kind.as_deref(), Some("custom_bluzyrino"));
        assert_eq!(status.installed_version, None);
        assert!(!status.manifest_valid);
        assert!(!status.managed_by_streamnook);
        assert!(!status.updater_eligible);
    }

    #[test]
    fn legacy_bundled_moltorino_is_unmanaged_and_ineligible() {
        let tree = TempTree::new("legacy_unmanaged");
        let exe_dir = tree.dir("app");
        tree.file("app/moltorino/Moltorino7.exe");

        let status = runtime_status_for("", &exe_dir);
        assert!(status.available);
        assert_eq!(
            status.runtime_kind.as_deref(),
            Some("legacy_bundled_moltorino")
        );
        assert_eq!(status.installed_version, None);
        assert!(!status.manifest_valid);
        assert!(!status.managed_by_streamnook);
        assert!(!status.updater_eligible);
    }

    #[test]
    fn display_identity_is_case_insensitive_and_presentation_only() {
        assert_eq!(identity_from_path(Path::new("BLUZYRINO.EXE")), ChatRuntimeIdentity::Bluzyrino);
        assert_eq!(identity_from_path(Path::new("moltorino.exe")), ChatRuntimeIdentity::Moltorino);
        assert_eq!(identity_from_path(Path::new("MOLTORINO7.EXE")), ChatRuntimeIdentity::Moltorino);
        assert_eq!(identity_from_path(Path::new("Other.exe")), ChatRuntimeIdentity::Generic);
        assert_eq!(ChatRuntimeIdentity::Generic.display_name(), "Chat runtime");
    }

    // --- Standalone process-ownership registry -------------------------------
    //
    // These exercise the reap / same-channel decision / drain logic against a fake
    // child, since a real `std::process::Child` can't be constructed without
    // spawning. They pin the invariants the lifecycle + relaunch fix depend on:
    // an already-exited entry is dropped and allows a fresh spawn; a live entry
    // *with a usable window* is focused (never duplicated); a windowless entry is
    // "starting" within the grace window and "replaced" (exact child terminated +
    // removed) past it; channel matching is normalized; different channels stay
    // independent; shutdown drains and terminates every entry exactly once; a
    // duplicate drain is a harmless no-op; and the registry only ever touches
    // children it was handed (so an unrelated Moltorino can never be selected).

    #[cfg(windows)]
    mod registry {
        use super::super::{
            drain_and_terminate, plan_same_channel, LaunchPlan, Owned, TrackedChild,
        };
        use std::cell::Cell;
        use std::rc::Rc;

        /// A fake tracked child. `alive` models whether the OS process is still
        /// running; `has_window` models whether it currently owns a usable
        /// user-facing window; `terminated`/`focused` count force-terminations and
        /// focus attempts so tests can assert kill-/focus-exactly-once and
        /// drain-idempotence.
        struct FakeChild {
            alive: bool,
            has_window: bool,
            cleanup_error: Option<&'static str>,
            exits_during_cleanup: bool,
            terminated: Rc<Cell<u32>>,
            focused: Rc<Cell<u32>>,
        }

        /// Handles a test holds onto to observe a `FakeChild`: (terminate count,
        /// focus count).
        type Counters = (Rc<Cell<u32>>, Rc<Cell<u32>>);

        impl FakeChild {
            fn make(alive: bool, has_window: bool) -> (Self, Counters) {
                let terminated = Rc::new(Cell::new(0));
                let focused = Rc::new(Cell::new(0));
                (
                    FakeChild {
                        alive,
                        has_window,
                        cleanup_error: None,
                        exits_during_cleanup: false,
                        terminated: terminated.clone(),
                        focused: focused.clone(),
                    },
                    (terminated, focused),
                )
            }
            /// Alive with a usable window (an open, on-screen chat).
            fn live_with_window() -> (Self, Counters) {
                Self::make(true, true)
            }
            /// Alive but windowless (still starting, or a stale headless leftover).
            fn live_headless() -> (Self, Counters) {
                Self::make(true, false)
            }
            /// Already exited on its own.
            fn dead() -> (Self, Counters) {
                Self::make(false, false)
            }
        }

        impl TrackedChild for FakeChild {
            fn reap_if_exited(&mut self) -> bool {
                // Reaps (reports gone) exactly when the process already exited.
                !self.alive
            }
            fn terminate_and_reap(&mut self) -> Result<(), String> {
                if self.exits_during_cleanup {
                    self.alive = false;
                    return Ok(());
                }
                if let Some(error) = self.cleanup_error {
                    return Err(error.to_string());
                }
                // Idempotent: a dead child is reaped without a "kill", a live one
                // is killed once and marked dead.
                if self.alive {
                    self.alive = false;
                    self.terminated.set(self.terminated.get() + 1);
                }
                Ok(())
            }
            fn focus_existing_window(&self) -> bool {
                self.focused.set(self.focused.get() + 1);
                // A dead process never has a usable window, regardless of the flag.
                self.alive && self.has_window
            }
        }

        /// Build an entry. `grace` decides whether the injected grace closure will
        /// report it as still-starting (only consulted for the windowless case).
        fn owned(channel: &str, child: FakeChild) -> Owned<FakeChild> {
            Owned {
                channel: channel.to_string(),
                child,
                // Real time is irrelevant: every test injects its own grace closure.
                launched_at: std::time::Instant::now(),
            }
        }

        /// Grace closures the tests inject in place of the real elapsed-time check.
        fn within_grace(_o: &Owned<FakeChild>) -> bool {
            true
        }
        fn past_grace(_o: &Owned<FakeChild>) -> bool {
            false
        }

        fn test_plan(
            entries: &mut Vec<Owned<FakeChild>>,
            channel: &str,
            grace: impl Fn(&Owned<FakeChild>) -> bool,
        ) -> LaunchPlan {
            plan_same_channel(entries, channel, grace).expect("decision should succeed")
        }

        /// A live standalone *with a usable window* is focused, not duplicated.
        #[test]
        fn live_window_is_focused_not_duplicated() {
            let (c, (kills, focuses)) = FakeChild::live_with_window();
            let mut entries = vec![owned("reginald", c)];
            assert_eq!(
                test_plan(&mut entries, "reginald", within_grace),
                LaunchPlan::Focused,
                "an open window must be focused rather than spawning a duplicate"
            );
            assert_eq!(entries.len(), 1, "the live entry must be retained");
            assert_eq!(focuses.get(), 1, "we must have tried to focus it once");
            assert_eq!(kills.get(), 0, "focusing must never terminate the child");
        }

        /// A live windowless standalone *inside* the startup grace period is left
        /// alone as "starting" — never killed, never duplicated.
        #[test]
        fn windowless_within_grace_is_starting() {
            let (c, (kills, _)) = FakeChild::live_headless();
            let mut entries = vec![owned("reginald", c)];
            assert_eq!(
                test_plan(&mut entries, "reginald", within_grace),
                LaunchPlan::Starting,
                "a just-launched windowless process must be treated as starting"
            );
            assert_eq!(entries.len(), 1, "the starting entry must be retained");
            assert_eq!(kills.get(), 0, "a starting process must not be terminated");
        }

        /// A live windowless standalone *past* the grace period is stale: the exact
        /// child is terminated + reaped, removed, and a replacement is allowed.
        #[test]
        fn windowless_past_grace_is_replaced() {
            let (c, (kills, _)) = FakeChild::live_headless();
            let mut entries = vec![owned("reginald", c)];
            assert_eq!(
                test_plan(&mut entries, "reginald", past_grace),
                LaunchPlan::Spawn {
                    replaced_stale: true
                },
                "a stale headless standalone must be replaced"
            );
            assert!(entries.is_empty(), "the stale entry must have been removed");
            assert_eq!(kills.get(), 1, "the exact stale child must be killed once");
        }

        /// A different channel is never deduped against an unrelated live entry,
        /// and that unrelated entry is never focused or killed.
        #[test]
        fn different_channel_always_launches() {
            let (c, (kills, focuses)) = FakeChild::live_with_window();
            let mut entries = vec![owned("reginald", c)];
            assert_eq!(
                test_plan(&mut entries, "someone_else", within_grace),
                LaunchPlan::Spawn {
                    replaced_stale: false
                },
                "a launch for a different channel must proceed as a fresh spawn"
            );
            assert_eq!(entries.len(), 1, "the unrelated entry stays put");
            assert_eq!(focuses.get(), 0, "an unrelated entry must not be focused");
            assert_eq!(kills.get(), 0, "an unrelated entry must not be killed");
        }

        /// An entry that exited on its own is reaped, and a launch for that same
        /// channel then proceeds as a fresh (non-replacement) spawn.
        #[test]
        fn dead_entry_is_reaped_and_relaunch_allowed() {
            let (c, (kills, _)) = FakeChild::dead();
            let mut entries = vec![owned("reginald", c)];
            assert_eq!(
                test_plan(&mut entries, "reginald", within_grace),
                LaunchPlan::Spawn {
                    replaced_stale: false
                },
                "a dead standalone must not block a relaunch on its channel"
            );
            assert!(entries.is_empty(), "the dead entry must have been reaped");
            assert_eq!(kills.get(), 0, "an exited child is reaped, not killed");
        }

        /// The registry only ever inspects/kills children it was handed: an entry
        /// we never registered is untouched no matter what channel we launch.
        #[test]
        fn only_registered_children_are_touched() {
            let (mine, (kmine, fmine)) = FakeChild::live_with_window();
            let mut entries = vec![owned("mine", mine)];
            // Launching an unrelated channel touches nothing already present.
            assert_eq!(
                test_plan(&mut entries, "unrelated", within_grace),
                LaunchPlan::Spawn {
                    replaced_stale: false
                }
            );
            assert_eq!(kmine.get(), 0, "an unrelated entry must never be killed");
            assert_eq!(fmine.get(), 0, "an unrelated entry must never be focused");
            assert_eq!(entries.len(), 1, "the registered entry stays put");
        }

        /// Channel matching is normalized: an entry stored under a normalized login
        /// still matches. (Registration always stores the normalized channel; here
        /// we confirm the same normalized value resolves to the existing entry
        /// rather than spawning a second one.)
        #[test]
        fn channel_match_is_normalized() {
            use super::super::normalize_channel;
            // The launch path normalizes before both storing and comparing.
            let stored = normalize_channel("  Reginald  ");
            assert_eq!(stored, "reginald");
            let (c, (_, focuses)) = FakeChild::live_with_window();
            let mut entries = vec![owned(&stored, c)];
            assert_eq!(
                test_plan(&mut entries, &normalize_channel("REGINALD"), within_grace),
                LaunchPlan::Focused,
                "a case/space-different spelling of the same login must dedupe"
            );
            assert_eq!(focuses.get(), 1);
        }

        /// The observed defect sequence end-to-end: the same windowless entry is
        /// "starting" within grace, then "replaced" once the grace elapses — the
        /// exact child is killed exactly once and removed so a fresh spawn proceeds.
        #[test]
        fn starting_then_stale_replaces_exact_child() {
            let (c, (kills, _)) = FakeChild::live_headless();
            let mut entries = vec![owned("reginald", c)];
            // First click, still starting: retained, not killed.
            assert_eq!(
                test_plan(&mut entries, "reginald", within_grace),
                LaunchPlan::Starting
            );
            assert_eq!(kills.get(), 0);
            assert_eq!(entries.len(), 1);
            // Later click, grace elapsed: the exact child is terminated + removed.
            assert_eq!(
                test_plan(&mut entries, "reginald", past_grace),
                LaunchPlan::Spawn {
                    replaced_stale: true
                }
            );
            assert_eq!(kills.get(), 1, "the stale child killed exactly once");
            assert!(entries.is_empty());
        }

        #[test]
        fn failed_stale_cleanup_retains_entry_and_blocks_replacement() {
            let (mut c, (kills, _)) = FakeChild::live_headless();
            c.cleanup_error = Some("kill failed");
            let mut entries = vec![owned("reginald", c)];

            let err = plan_same_channel(&mut entries, "reginald", past_grace)
                .expect_err("uncertain cleanup must block replacement");

            assert_eq!(err, "kill failed");
            assert_eq!(entries.len(), 1, "ownership must be retained");
            assert_eq!(kills.get(), 0);
        }

        #[test]
        fn failed_stale_reap_retains_entry_and_blocks_replacement() {
            let (mut c, (kills, _)) = FakeChild::live_headless();
            c.cleanup_error = Some("wait failed");
            let mut entries = vec![owned("reginald", c)];

            let err = plan_same_channel(&mut entries, "reginald", past_grace)
                .expect_err("uncertain reap must block replacement");

            assert_eq!(err, "wait failed");
            assert_eq!(entries.len(), 1, "ownership must be retained");
            assert_eq!(kills.get(), 0);
        }

        #[test]
        fn natural_exit_during_stale_cleanup_allows_replacement() {
            let (mut c, (kills, _)) = FakeChild::live_headless();
            c.exits_during_cleanup = true;
            let mut entries = vec![owned("reginald", c)];

            assert_eq!(
                test_plan(&mut entries, "reginald", past_grace),
                LaunchPlan::Spawn {
                    replaced_stale: true
                }
            );
            assert!(entries.is_empty());
            assert_eq!(kills.get(), 0, "natural exit must not be killed");
        }

        /// Shutdown still drains every entry and terminates each live one exactly
        /// once, regardless of window state; a second drain is a harmless no-op.
        #[test]
        fn shutdown_drains_and_terminates_each_once() {
            let (a, (ka, _)) = FakeChild::live_with_window();
            let (b, (kb, _)) = FakeChild::live_headless();
            let (d, (kd, _)) = FakeChild::dead();
            let mut entries = vec![owned("a", a), owned("b", b), owned("d", d)];

            drain_and_terminate(&mut entries);
            assert!(entries.is_empty(), "drain must empty the registry");
            assert_eq!(ka.get(), 1, "live windowed child killed exactly once");
            assert_eq!(kb.get(), 1, "live headless child killed exactly once");
            assert_eq!(kd.get(), 0, "an already-dead child is never killed");

            // Idempotent: draining again does nothing and never panics.
            drain_and_terminate(&mut entries);
            assert_eq!(ka.get(), 1, "no double-kill on a second drain");
        }
    }
}
