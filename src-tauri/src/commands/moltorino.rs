//! Optional external Moltorino chat integration.
//!
//! Moltorino is a separate Twitch chat client (a Chatterino fork) that the user
//! installs themselves. StreamNook does not bundle it, link against it, or embed
//! it — this module only validates a user-supplied executable path and spawns it
//! with a channel argument. StreamNook's native chat is untouched and remains
//! the default for every surface.
//!
//! Deliberately absent: any process tracking, PID storage, reuse, or kill logic.
//! Every launch is fire-and-forget, so there is no code path that could reach an
//! unrelated Moltorino instance the user started themselves.

use serde::Serialize;
use std::path::{Path, PathBuf};

/// Result of checking a user-supplied Moltorino executable path, surfaced in the
/// Integrations settings card.
#[derive(Serialize, Clone)]
pub struct MoltorinoPathInfo {
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
            "No Moltorino executable is set. Choose Moltorino.exe in Settings → Integrations."
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

/// Where a resolved Moltorino runtime came from, so callers (and the settings
/// card) can explain which executable is actually in use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MoltorinoSource {
    /// The user's explicitly-configured path (the advanced override).
    CustomOverride,
    /// The copy StreamNook ships next to its own executable.
    BundledRuntime,
}

impl MoltorinoSource {
    /// Stable lowercase tag for the settings-card status payload and debug logs.
    pub(crate) fn as_status(self) -> &'static str {
        match self {
            MoltorinoSource::CustomOverride => "custom",
            MoltorinoSource::BundledRuntime => "bundled",
        }
    }
}

/// A resolved, on-disk Moltorino executable plus where it came from.
#[derive(Debug)]
pub(crate) struct MoltorinoRuntime {
    pub(crate) path: PathBuf,
    pub(crate) source: MoltorinoSource,
}

/// The bundled runtime's expected location: `moltorino\Moltorino7.exe` beside the
/// StreamNook executable. Split out (and taking `exe_dir` explicitly) so it is
/// pure and unit-testable without depending on the real install location.
pub(crate) fn bundled_runtime_path(exe_dir: &Path) -> PathBuf {
    exe_dir.join("moltorino").join("Moltorino7.exe")
}

/// Validate a user-configured override, which may point either directly at an
/// executable OR at the folder that contains `Moltorino7.exe`. A folder resolves
/// to that file; anything else falls through to the shared [`resolve_executable`]
/// file checks, so the error text stays identical to the standalone path field.
fn resolve_custom_override(raw: &str) -> Result<PathBuf, String> {
    let trimmed = raw.trim();
    // Empty is not an override — the caller decides what to do with that.
    if trimmed.is_empty() {
        return Err(
            "No Moltorino executable is set. Choose Moltorino.exe in Settings → Integrations."
                .to_string(),
        );
    }
    // A configured folder is allowed: look for Moltorino7.exe inside it. Check the
    // raw path first so the folder branch wins before the file-only checks below.
    let path = Path::new(trimmed);
    if path.is_dir() {
        let candidate = path.join("Moltorino7.exe");
        return resolve_executable(&candidate.to_string_lossy());
    }
    resolve_executable(trimmed)
}

/// Resolve which Moltorino executable to run, given the user's configured path
/// and the directory StreamNook's own executable lives in. Pure (takes `exe_dir`)
/// so the whole precedence can be unit-tested against temporary directories.
///
/// Order:
///   1. A non-empty configured override that resolves to a valid executable.
///   2. The bundled `moltorino\Moltorino7.exe` beside StreamNook.
///   3. Otherwise a clear "not found" error.
///
/// A configured-but-invalid override never hard-fails resolution: it is logged
/// and we fall through to the bundled copy, so a stale path can't strand a user
/// who has the bundle available.
pub(crate) fn resolve_runtime_in(
    configured: &str,
    exe_dir: &Path,
) -> Result<MoltorinoRuntime, String> {
    if !configured.trim().is_empty() {
        match resolve_custom_override(configured) {
            Ok(path) => {
                return Ok(MoltorinoRuntime {
                    path,
                    source: MoltorinoSource::CustomOverride,
                });
            }
            Err(e) => {
                log::warn!(
                    "[Moltorino] configured path is unusable, trying the bundled runtime: {e}"
                );
            }
        }
    }

    let bundled = bundled_runtime_path(exe_dir);
    if bundled.is_file() {
        return Ok(MoltorinoRuntime {
            path: bundled,
            source: MoltorinoSource::BundledRuntime,
        });
    }

    Err(format!(
        "Moltorino isn't installed. StreamNook looked for a bundled copy at {} and no custom \
         path is set in Settings → Integrations.",
        display_path(&bundled)
    ))
}

/// Convenience wrapper that discovers StreamNook's executable directory via
/// [`std::env::current_exe`] and delegates to [`resolve_runtime_in`].
pub(crate) fn resolve_moltorino_runtime(configured: &str) -> Result<MoltorinoRuntime, String> {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .ok_or_else(|| "Couldn't locate the StreamNook program folder.".to_string())?;
    resolve_runtime_in(configured, &exe_dir)
}

/// Read-only view of what runtime resolution would pick right now, surfaced in
/// the Integrations settings card. Never launches anything.
#[derive(Serialize, Clone)]
pub struct MoltorinoRuntimeStatus {
    /// Whether a runnable Moltorino executable was found.
    pub available: bool,
    /// `"custom"` or `"bundled"` when available; `null` otherwise.
    pub source: Option<String>,
    /// Display path of the resolved executable when available; `null` otherwise.
    pub executable_path: Option<String>,
    /// User-facing explanation when nothing was found; `null` when available.
    pub error: Option<String>,
}

/// Report which Moltorino runtime resolution picks, using the persisted custom
/// path. Read-only: it resolves and validates but never spawns Moltorino.
#[tauri::command]
pub async fn moltorino_runtime_status(
    state: tauri::State<'_, crate::models::settings::AppState>,
) -> Result<MoltorinoRuntimeStatus, String> {
    let configured = {
        let settings = state
            .settings
            .lock()
            .map_err(|_| "Couldn't read settings.".to_string())?;
        settings
            .moltorino
            .executable_path
            .clone()
            .unwrap_or_default()
    };
    Ok(match resolve_moltorino_runtime(&configured) {
        Ok(runtime) => MoltorinoRuntimeStatus {
            available: true,
            source: Some(runtime.source.as_status().to_string()),
            executable_path: Some(display_path(&runtime.path)),
            error: None,
        },
        Err(e) => MoltorinoRuntimeStatus {
            available: false,
            source: None,
            executable_path: None,
            error: Some(e),
        },
    })
}

/// Check a candidate Moltorino executable path without launching anything.
/// Called by the settings card's Verify button and on blur.
#[tauri::command]
pub async fn validate_moltorino_path(path: String) -> Result<MoltorinoPathInfo, String> {
    let resolved = resolve_executable(&path)?;
    Ok(MoltorinoPathInfo {
        file_name: resolved
            .file_name()
            .map(|f| f.to_string_lossy().to_string())
            .unwrap_or_default(),
        resolved_path: display_path(&resolved),
    })
}

/// Launch the user's Moltorino with a single Twitch channel.
///
/// `-c t:<channel>` (rather than `-a`) is deliberate: `-c` builds a clean
/// single-channel layout and sets Moltorino's own `dontSaveSettings` flag, so an
/// on-demand launch from StreamNook never rewrites the user's saved Moltorino tab
/// layout.
#[tauri::command]
pub async fn launch_moltorino(
    path: Option<String>,
    channel: String,
    state: tauri::State<'_, crate::models::settings::AppState>,
) -> Result<(), String> {
    // Explicit argument wins; otherwise fall back to the persisted setting. The
    // lock is scoped so it is released before we touch the filesystem or spawn.
    let configured = {
        let settings = state
            .settings
            .lock()
            .map_err(|_| "Couldn't read settings.".to_string())?;
        settings
            .moltorino
            .executable_path
            .clone()
            .unwrap_or_default()
    };
    // An explicitly-passed blank is treated as "not supplied" rather than an
    // error, so a caller that always sends the field still gets the saved path.
    let raw = path.filter(|p| !p.trim().is_empty()).unwrap_or(configured);

    let channel = channel.trim().to_ascii_lowercase();
    if !is_valid_twitch_login(&channel) {
        return Err(format!(
            "\"{}\" isn't a valid Twitch channel name.",
            channel
        ));
    }

    // Resolve through the shared runtime picker so an unset/blank path falls back
    // to the bundled copy instead of erroring — same precedence the embed uses.
    let runtime = resolve_moltorino_runtime(&raw)?;
    spawn_moltorino(&runtime.path, &channel)
}

#[cfg(windows)]
fn spawn_moltorino(exe: &Path, channel: &str) -> Result<(), String> {
    use std::os::windows::process::CommandExt;

    // Argument vector, never a shell string — nothing here is parsed by cmd.exe.
    std::process::Command::new(exe)
        .arg("-c")
        .arg(format!("t:{channel}"))
        // CREATE_NO_WINDOW: no stray console flashes behind the GUI. Matches the
        // plugin host's spawn flags.
        .creation_flags(0x0800_0000)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("Couldn't start Moltorino at {}: {}", display_path(exe), e))
}

#[cfg(not(windows))]
fn spawn_moltorino(_exe: &Path, _channel: &str) -> Result<(), String> {
    Err("The Moltorino integration is only available on Windows.".to_string())
}

#[cfg(test)]
mod tests {
    use super::{
        bundled_runtime_path, display_path, is_valid_twitch_login, resolve_executable,
        resolve_runtime_in, MoltorinoSource,
    };
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
            let p = self.root.join(rel);
            if let Some(parent) = p.parent() {
                std::fs::create_dir_all(parent).expect("create temp file parent");
            }
            std::fs::write(&p, b"probe").expect("write temp file");
            p
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

    // --- Bundled runtime resolution ------------------------------------------

    /// The bundled location is exactly `<exe dir>\moltorino\Moltorino7.exe`.
    #[test]
    fn bundled_path_is_moltorino7_beside_exe() {
        let exe_dir = Path::new(r"C:\Program Files\StreamNook");
        let bundled = bundled_runtime_path(exe_dir);
        assert!(bundled.ends_with(Path::new("moltorino").join("Moltorino7.exe")));
        assert_eq!(bundled.parent().unwrap().file_name().unwrap(), "moltorino");
        assert_eq!(bundled.parent().unwrap().parent().unwrap(), exe_dir);
    }

    /// A valid configured override outranks a valid bundled copy.
    #[test]
    fn valid_custom_wins_over_valid_bundled() {
        let tree = TempTree::new("custom_wins");
        let custom = tree.file("custom/Moltorino7.exe");
        // A real bundled copy also exists beside the (fake) exe dir.
        let exe_dir = tree.dir("app");
        tree.file("app/moltorino/Moltorino7.exe");

        let runtime = resolve_runtime_in(&custom.to_string_lossy(), &exe_dir)
            .expect("a valid custom path must resolve");
        assert_eq!(runtime.source, MoltorinoSource::CustomOverride);
        assert!(runtime.path.ends_with("Moltorino7.exe"));
        // Resolved to the custom copy, not the bundled one.
        assert!(runtime.path.to_string_lossy().contains("custom"));
    }

    /// An unusable configured path is logged and we fall back to the bundle.
    #[test]
    fn invalid_custom_falls_back_to_bundled() {
        let tree = TempTree::new("invalid_falls_back");
        let exe_dir = tree.dir("app");
        tree.file("app/moltorino/Moltorino7.exe");

        let runtime = resolve_runtime_in(r"C:\nope\not\here\moltorino.exe", &exe_dir)
            .expect("an invalid custom path must still resolve to the bundle");
        assert_eq!(runtime.source, MoltorinoSource::BundledRuntime);
    }

    /// An empty configured path means "use the bundle".
    #[test]
    fn empty_custom_uses_bundled() {
        let tree = TempTree::new("empty_uses_bundled");
        let exe_dir = tree.dir("app");
        tree.file("app/moltorino/Moltorino7.exe");

        let runtime =
            resolve_runtime_in("   ", &exe_dir).expect("empty path must resolve to the bundle");
        assert_eq!(runtime.source, MoltorinoSource::BundledRuntime);
    }

    /// A configured *folder* resolves to the `Moltorino7.exe` inside it.
    #[test]
    fn custom_directory_resolves_moltorino7() {
        let tree = TempTree::new("custom_dir");
        let custom_dir = tree.dir("MyMoltorino");
        tree.file("MyMoltorino/Moltorino7.exe");
        let exe_dir = tree.dir("app"); // no bundle here on purpose

        let runtime = resolve_runtime_in(&custom_dir.to_string_lossy(), &exe_dir)
            .expect("a folder containing Moltorino7.exe must resolve");
        assert_eq!(runtime.source, MoltorinoSource::CustomOverride);
        assert_eq!(runtime.path.file_name().unwrap(), "Moltorino7.exe");
    }

    /// Nothing configured and no bundle present is a clear missing-runtime error.
    #[test]
    fn neither_present_is_missing_runtime_error() {
        let tree = TempTree::new("neither");
        let exe_dir = tree.dir("app"); // empty: no moltorino\Moltorino7.exe

        let err = resolve_runtime_in("", &exe_dir)
            .expect_err("no custom path and no bundle must error");
        assert!(
            err.contains("Moltorino7.exe") || err.to_lowercase().contains("moltorino"),
            "unexpected error text: {err}"
        );
    }
}
