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

    let exe = resolve_executable(&raw)?;
    spawn_moltorino(&exe, &channel)
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
    use super::{display_path, is_valid_twitch_login, resolve_executable};
    use std::path::Path;

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
}
