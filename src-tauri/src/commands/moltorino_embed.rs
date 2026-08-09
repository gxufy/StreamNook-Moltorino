//! Optional *embedded* Moltorino chat surface (Phase 2).
//!
//! Phase 1 (see [`super::moltorino`]) launches the user's Moltorino as a separate
//! window, fire-and-forget. Phase 2 reuses the same user-supplied executable but
//! reparents Moltorino's chat split *inside* StreamNook's main window, so it can
//! stand in for the native chat panel while following whatever Twitch channel
//! StreamNook currently has focused.
//!
//! Nothing here bundles, links, or modifies Moltorino. We only:
//!   1. create a StreamNook-owned Win32 host window (a child of the main window),
//!   2. launch the user's Moltorino with `--x-attach-split-to-window <hwnd>`,
//!   3. let Moltorino `SetParent` its own split window into our host and report
//!      the created child HWND back over `WM_COPYDATA`,
//!   4. drive channel changes into that child with `set-channel` `WM_COPYDATA`
//!      messages sent via `SendMessageTimeoutW` (a frozen Moltorino can never
//!      freeze StreamNook), and
//!   5. track and tear down ONLY the process and HWNDs we created.
//!
//! The exact wire protocol is defined by Moltorino's `FramelessEmbedWindow`
//! (vendor/moltorino/src/widgets/FramelessEmbedWindow.cpp) and must match byte
//! for byte — see [`parse_created_window`] and [`build_set_channel_json`].

// ---------------------------------------------------------------------------
// Pure, platform-independent protocol helpers (unit-tested on every platform).
// ---------------------------------------------------------------------------

/// Parse a `created-window` `WM_COPYDATA` payload from Moltorino.
///
/// Moltorino builds this with `QJsonDocument::toJson()` (pretty-printed, so it
/// carries newlines and two-space indentation), encodes the HWND as a JSON
/// *string* of its decimal `unsigned long long` value, and appends a trailing
/// `'\0'` which is included in `cbData`. We therefore have to tolerate:
///   * leading/trailing whitespace and indentation,
///   * one or more trailing NUL bytes,
///   * `window-id` arriving as a JSON string (its real shape) OR a JSON number
///     (defensive, in case a future build stops stringifying it).
///
/// Returns the child window handle as an `isize`, or `None` if the payload is
/// not a well-formed `created-window` message with a non-zero id.
pub fn parse_created_window(bytes: &[u8]) -> Option<isize> {
    // Drop every trailing NUL (Qt appends exactly one, but be liberal) and any
    // surrounding ASCII whitespace before handing the slice to serde_json.
    let trimmed = {
        let end = bytes
            .iter()
            .rposition(|&b| b != 0)
            .map(|i| i + 1)
            .unwrap_or(0);
        &bytes[..end]
    };

    let value: serde_json::Value = serde_json::from_slice(trimmed).ok()?;
    let obj = value.as_object()?;

    if obj.get("type").and_then(|t| t.as_str()) != Some("created-window") {
        return None;
    }

    let id = match obj.get("window-id")? {
        // The real shape: a decimal string like "1904326".
        serde_json::Value::String(s) => s.trim().parse::<u64>().ok()?,
        // Defensive: accept a bare JSON number too.
        serde_json::Value::Number(n) => n.as_u64()?,
        _ => return None,
    };

    if id == 0 {
        return None;
    }
    Some(id as isize)
}

/// Build the `set-channel` `WM_COPYDATA` payload Moltorino expects.
///
/// Moltorino reads it with `QString::fromUtf8(lpData, cbData)` then
/// `QJsonDocument::fromJson`, checking `type == "set-channel"`,
/// `provider == "twitch"`, and reading `channel-name`. We send compact JSON with
/// **no** trailing NUL: `cbData` is exactly the byte length, so the parser sees
/// clean JSON with no trailing garbage.
///
/// The channel is assumed already normalized/validated by the caller
/// ([`super::moltorino::is_valid_twitch_login`]); `serde_json` still escapes it,
/// so a stray value can never break out of the JSON string.
pub fn build_set_channel_json(channel: &str) -> Vec<u8> {
    let value = serde_json::json!({
        "type": "set-channel",
        "provider": "twitch",
        "channel-name": channel,
    });
    // to_vec never fails for a plain object of owned values.
    serde_json::to_vec(&value).unwrap_or_default()
}

/// Translate a point given in the *webview* (source) window's client coordinates
/// into the *root* (visible top-level) window's client coordinates.
///
/// The frontend measures the placeholder with `getBoundingClientRect()`, whose
/// origin is the webview client-area top-left — but our host window is a
/// `WS_CHILD` of the visible **root** window, so its position must be expressed
/// in the root's client coordinates. `offset` is the source client-origin's
/// position within the root's client area, i.e. what mapping `(0, 0)` through
/// `MapWindowPoints(source, root, ..)` yields; adding it relocates any
/// webview-relative point into root-client space.
///
/// This is the whole numeric core of the coordinate fix, split out so it can be
/// unit-tested without a live HWND. Width/height are physical pixel *sizes*, not
/// screen positions, so they are never passed through here.
///
/// `saturating_add` guards the arithmetic against the extreme negative origins
/// (~ -32000) an offscreen/internal source window can carry, so a translation can
/// never wrap around `i32`.
pub fn translate_client_point(x: i32, y: i32, offset: (i32, i32)) -> (i32, i32) {
    (x.saturating_add(offset.0), y.saturating_add(offset.1))
}

/// Clip a proposed cutout hole to the WebView's client bounds, in
/// WebView-relative coordinates.
///
/// The embedded chat panel occupies a rectangle *inside* the WebView; to make
/// the WebView not paint (and not eat input) over that rectangle we punch a hole
/// in the WebView's window region there. This helper is the pure numeric core of
/// that calculation, split out so the geometry is unit-tested without a live
/// HWND.
///
/// Inputs:
///   * `hole` is `(left, top, width, height)` — the panel origin already mapped
///     into the WebView's client space (may be partially or fully offscreen, or
///     carry non-positive sizes if the frontend hasn't laid out yet).
///   * `webview` is `(width, height)` — the WebView client area size; the hole
///     is clamped to `[0, width] x [0, height]`.
///
/// Returns `Some((left, top, right, bottom))` — an inclusive-left/exclusive-right
/// rectangle ready for `CreateRectRgn` — or `None` when there is nothing to punch:
///   * the hole has non-positive width or height,
///   * the WebView has non-positive dimensions,
///   * the hole lies fully outside the WebView, or
///   * clipping collapses it to an empty rectangle.
///
/// Returning `None` is the signal to *restore* (no hole) rather than apply a
/// degenerate region, so a bad layout can never blank the WebView.
pub fn clip_hole_to_webview(
    hole: (i32, i32, i32, i32),
    webview: (i32, i32),
) -> Option<(i32, i32, i32, i32)> {
    let (hx, hy, hw, hh) = hole;
    let (vw, vh) = webview;

    // A zero/negative hole or a zero/negative WebView means "no hole".
    if hw <= 0 || hh <= 0 || vw <= 0 || vh <= 0 {
        return None;
    }

    // The hole's own edges (exclusive right/bottom), guarded against overflow.
    let hole_right = hx.saturating_add(hw);
    let hole_bottom = hy.saturating_add(hh);

    // Intersect the hole with the WebView rect [0,vw) x [0,vh).
    let left = hx.max(0);
    let top = hy.max(0);
    let right = hole_right.min(vw);
    let bottom = hole_bottom.min(vh);

    // Fully outside, or clipped to nothing.
    if right <= left || bottom <= top {
        return None;
    }

    Some((left, top, right, bottom))
}

/// Compute the `(width, height)` a reparented child must take to fill a host's
/// client area, from that host's client rectangle `(left, top, right, bottom)`.
///
/// `GetClientRect` always reports `left`/`top` as `0`, but we take the full
/// rectangle and derive the extent by subtraction so an inverted or garbage
/// rectangle can never yield a negative `SetWindowPos` dimension: `saturating_sub`
/// guards the arithmetic and `max(0)` floors each side at zero. A zero-sized host
/// therefore produces `(0, 0)` (the child collapses rather than the call failing),
/// and an inverted rect (`right < left`) also clamps to zero rather than going
/// negative.
///
/// Split out as a pure function so the clamping is unit-tested without a live
/// HWND; the live `GetClientRect`/`SetWindowPos` wiring lives in
/// `imp::fit_child_to_host`, which is the single place the embedded child is
/// sized — always from the host's live client rect, never a cached `state.bounds`.
pub fn host_client_size(rect: (i32, i32, i32, i32)) -> (i32, i32) {
    let (left, top, right, bottom) = rect;
    let w = right.saturating_sub(left).max(0);
    let h = bottom.saturating_sub(top).max(0);
    (w, h)
}

/// Decide whether the embedded child needs a corrective refit, given the host's
/// client size and the child's current client size.
///
/// This is the pure core of the `WM_APP_REFIT_CHILD` handler and — critically —
/// the loop terminator for the WinEvent-driven refit. Qt reparents its split
/// window and then, a beat later, snaps it back to its own default size (the
/// live-captured 300x150), firing `EVENT_OBJECT_LOCATIONCHANGE`. We answer that
/// by refitting the child to the host. But our own `SetWindowPos` *also* moves
/// the child, firing another location-change; if we refit unconditionally we'd
/// loop forever. So the rule is: **only refit when the sizes actually differ.**
/// Once the child matches the host, the echo event finds them equal and stops.
///
/// A zero-sized host (`(0, 0)`, e.g. hidden/not-yet-laid-out) never asks for a
/// refit: fitting a child to nothing is pointless and would just churn. Every
/// input is already clamped non-negative by [`host_client_size`], so there is no
/// sign/overflow hazard here.
pub fn should_refit_child(host: (i32, i32), child: (i32, i32)) -> bool {
    let (hw, hh) = host;
    if hw <= 0 || hh <= 0 {
        return false;
    }
    child != host
}

/// Build the flags used whenever the attached child is fit to its host.
///
/// `SWP_NOSENDCHANGING` is critical for Qt clients whose minimum-size handling
/// in `WM_WINDOWPOSCHANGING` would otherwise override the host's exact width.
#[cfg(windows)]
fn child_fit_flags(
    extra: windows::Win32::UI::WindowsAndMessaging::SET_WINDOW_POS_FLAGS,
) -> windows::Win32::UI::WindowsAndMessaging::SET_WINDOW_POS_FLAGS {
    use windows::Win32::UI::WindowsAndMessaging::{
        SWP_NOACTIVATE, SWP_NOSENDCHANGING, SWP_NOZORDER,
    };
    SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOSENDCHANGING | extra
}

/// Whether a WinEvent's window handle refers to the child we're currently
/// embedding, used to filter the `EVENT_OBJECT_LOCATIONCHANGE` callback.
///
/// The out-of-context WinEvent hook is scoped to Moltorino's process/thread, but
/// that thread can own more than one window, and — more importantly — a stale
/// event queued before a Settings-close teardown must never act on the *new*
/// child attached after the following fresh start. The callback therefore
/// compares the event's HWND against the currently-attached child (`None` when
/// nothing is attached yet) and ignores everything else. Split out as a pure
/// `isize` comparison so the stale-child filtering is unit-tested without a live
/// HWND or a live hook.
pub fn is_current_child(event_hwnd: isize, current_child: Option<isize>) -> bool {
    current_child == Some(event_hwnd)
}

// ---------------------------------------------------------------------------
// Windows implementation.
// ---------------------------------------------------------------------------

#[cfg(windows)]
mod imp {
    use super::{
        build_set_channel_json, child_fit_flags, host_client_size, is_current_child,
        parse_created_window, should_refit_child,
    };
    use crate::commands::moltorino::{
        display_path, is_valid_twitch_login, resolve_moltorino_runtime,
    };
    use std::ffi::c_void;
    use std::path::Path;
    use std::sync::atomic::{AtomicIsize, Ordering};
    use std::sync::mpsc::{Receiver, Sender};
    use std::sync::{Arc, Mutex, OnceLock};
    use tauri::{AppHandle, Emitter};

    use windows::core::{w, PCWSTR};
    use windows::Win32::Foundation::{
        CloseHandle, GetLastError, ERROR_CLASS_ALREADY_EXISTS, HANDLE, HWND, LPARAM, LRESULT,
        POINT, RECT, WPARAM,
    };
    use windows::Win32::Graphics::Gdi::{
        CombineRgn, CreateRectRgn, DeleteObject, MapWindowPoints, SetWindowRgn, HGDIOBJ, HRGN,
        RGN_DIFF, RGN_ERROR,
    };
    use windows::Win32::System::DataExchange::COPYDATASTRUCT;
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::System::Threading::{
        GetCurrentProcessId, OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE,
    };
    use windows::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK};
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, EnumChildWindows,
        GetAncestor, GetClassNameW, GetClientRect, GetMessageW, GetParent, GetWindowLongPtrW,
        GetWindowThreadProcessId, IsWindow, IsWindowVisible, PostMessageW, PostQuitMessage,
        RegisterClassW, SendMessageTimeoutW, SetParent, SetWindowLongPtrW, SetWindowPos,
        ShowWindow, TranslateMessage, CREATESTRUCTW, EVENT_OBJECT_LOCATIONCHANGE, GA_ROOT,
        GWLP_USERDATA, GWL_STYLE, HWND_TOP, MSG, OBJID_WINDOW, SEND_MESSAGE_TIMEOUT_FLAGS,
        SET_WINDOW_POS_FLAGS, SMTO_ABORTIFHUNG, SWP_FRAMECHANGED, SWP_HIDEWINDOW, SWP_NOACTIVATE,
        SWP_SHOWWINDOW, SW_HIDE, SW_SHOW, WINDOW_EX_STYLE, WINEVENT_OUTOFCONTEXT, WM_APP,
        WM_COPYDATA, WM_CREATE, WM_DESTROY, WNDCLASSW, WS_CAPTION, WS_CHILD, WS_CLIPCHILDREN,
        WS_CLIPSIBLINGS, WS_MAXIMIZEBOX, WS_MINIMIZEBOX, WS_POPUP, WS_SYSMENU, WS_THICKFRAME,
    };

    /// Wake the message loop to drain the command queue.
    const WM_APP_WAKE: u32 = WM_APP + 1;
    /// The Moltorino process exited on its own.
    const WM_APP_EXITED: u32 = WM_APP + 2;
    /// The embedded child fired a location-change (Qt snapped it back to its own
    /// default size); coalesced request to re-fit it to the host on our thread.
    const WM_APP_REFIT_CHILD: u32 = WM_APP + 3;

    /// Event emitted to the frontend when the embedded surface can't be used and
    /// the UI must fall back to native chat.
    const FALLBACK_EVENT: &str = "moltorino-embed-fallback";

    /// One command from the async command layer to the owning message-loop
    /// thread. Everything the thread touches lives on that thread; commands carry
    /// only plain data.
    enum Cmd {
        /// Reposition the host window (physical pixels, relative to the main
        /// window client area) and set visibility in one shot.
        Bounds {
            x: i32,
            y: i32,
            w: i32,
            h: i32,
            visible: bool,
        },
        /// Follow a new Twitch channel (already lowercased + validated).
        Channel(String),
        /// Tear everything down: kill our process, destroy our window, quit loop.
        Shutdown,
    }

    /// Lives on the message-loop thread, reached from the WndProc via
    /// `GWLP_USERDATA`. Owns every resource we must clean up.
    struct HostState {
        host: HWND,
        /// The webview/native window `window.hwnd()` handed us. The frontend's
        /// bounds are physical pixels relative to *this* window's client area, so
        /// it is the source frame for the client-to-client coordinate conversion.
        source: HWND,
        /// The visible top-level (`GA_ROOT`) window our host is a `WS_CHILD` of.
        /// Bounds are translated into this window's client coordinates on every
        /// update (see [`client_offset`]).
        root: HWND,
        /// Moltorino's reparented split window, once it reports itself. `None`
        /// until the `created-window` message arrives.
        child: Option<HWND>,
        /// Channel requested before `child` was known; applied on arrival.
        pending_channel: Option<String>,
        /// Last bounds we were told (raw webview-relative), so a late
        /// `created-window` can size the child correctly.
        bounds: (i32, i32, i32, i32),
        /// The last source→root client offset we emitted a debug log for, so the
        /// 250ms bounds ticks don't spam the log: we only log when the offset
        /// actually shifts (a monitor/DPI/dock/restore move), not on every tick.
        logged_offset: Option<(i32, i32)>,
        visible: bool,
        rx: Receiver<Cmd>,
        /// The Moltorino process, kept so we can kill exactly it on shutdown.
        child_proc: Option<std::process::Child>,
        app: AppHandle,
        /// Set while we are deliberately tearing down, so the process-exit
        /// watcher's `WM_APP_EXITED` doesn't fire a spurious fallback.
        shutting_down: bool,
        /// The resolved `WRY_WEBVIEW` descendant of `root` (Tauri's WebView2
        /// host window), the window we punch the cutout region into. Resolved
        /// lazily and re-resolved if the stored handle ever goes invalid; `None`
        /// until first resolved (or if resolution fails).
        webview: Option<HWND>,
        /// The hole rectangle (WebView-relative, left/top/right/bottom) currently
        /// installed on `webview`, or `None` when the full (un-holed) region is in
        /// effect. Doubles as the "is a cutout active?" flag so `restore_webview`
        /// is idempotent, and lets `apply_cutout` skip a redundant `SetWindowRgn`
        /// (and its redraw) when the 250ms bounds ticks don't actually move the
        /// hole. Reset to `None` whenever `webview` is (re-)resolved to a fresh
        /// handle, since that new window starts with no region.
        applied_hole: Option<(i32, i32, i32, i32)>,
        /// The out-of-context WinEvent hook watching Moltorino's UI thread for
        /// `EVENT_OBJECT_LOCATIONCHANGE`, so we notice when the reparented child
        /// self-resizes back to Qt's default (300x150) after we fit it. `None`
        /// until a child attaches and the hook is installed; unhooked on every
        /// teardown path. Installed and removed on this (the message-loop) thread,
        /// which is also where its callback is delivered (WINEVENT_OUTOFCONTEXT).
        win_event_hook: Option<HWINEVENTHOOK>,
        /// Coalescing flag: set when a `WM_APP_REFIT_CHILD` is already queued so a
        /// burst of location-change events collapses into a single refit pass
        /// instead of flooding the message queue. Cleared when that message is
        /// handled.
        refit_pending: bool,
        /// Completion signal for the synchronous app-exit path. `WM_DESTROY` sends
        /// on this once teardown has fully run (child killed+reaped, window
        /// destroyed) just before the state is dropped; dropping the state also
        /// disconnects the channel, so [`stop_blocking`] observes completion even
        /// on a teardown path that somehow bypasses the explicit send. Held only so
        /// its send/Drop reaches the paired `done_rx`; never read here.
        done_tx: Sender<()>,
    }

    /// What the command layer keeps so it can reach a running host.
    struct EmbedHandle {
        host: isize,
        tx: Sender<Cmd>,
        /// Fired by the message-loop thread from `WM_DESTROY` once teardown is
        /// fully done (child killed+reaped, window destroyed). The synchronous
        /// app-exit path ([`stop_blocking`]) waits on this so StreamNook never
        /// exits out from under a not-yet-killed Moltorino — the race that left an
        /// orphaned embedded process behind.
        done_rx: Receiver<()>,
    }

    fn embed() -> &'static Mutex<Option<EmbedHandle>> {
        static EMBED: OnceLock<Mutex<Option<EmbedHandle>>> = OnceLock::new();
        EMBED.get_or_init(|| Mutex::new(None))
    }

    /// Data handed to the freshly-spawned message-loop thread.
    struct Init {
        /// The HWND from `window.hwnd()` — the *source* frame for the frontend's
        /// bounds, NOT necessarily the visible parent (see [`resolve_root`]).
        source_hwnd: isize,
        exe: std::path::PathBuf,
        channel: String,
        bounds: (i32, i32, i32, i32),
        visible: bool,
        rx: Receiver<Cmd>,
        app: AppHandle,
        /// The thread publishes its host HWND here the instant the window is
        /// created — BEFORE spawning Moltorino. If `start`'s readiness wait times
        /// out, this lets it still reach the (real, already-created) host to tear
        /// it down, so a window/process we created can never be orphaned.
        host_slot: Arc<AtomicIsize>,
        /// Moved into `HostState` and fired from `WM_DESTROY` so the synchronous
        /// app-exit path can block until this host's teardown has fully completed.
        done_tx: Sender<()>,
    }

    const CLASS_NAME: PCWSTR = w!("StreamNookMoltorinoEmbedHost");

    /// Register our window class exactly once for the process lifetime.
    fn ensure_class_registered() -> Result<(), String> {
        static REGISTERED: OnceLock<Result<(), String>> = OnceLock::new();
        REGISTERED
            .get_or_init(|| unsafe {
                let hinstance =
                    GetModuleHandleW(None).map_err(|e| format!("GetModuleHandleW failed: {e}"))?;
                let wc = WNDCLASSW {
                    lpfnWndProc: Some(wndproc),
                    hInstance: hinstance.into(),
                    lpszClassName: CLASS_NAME,
                    ..Default::default()
                };
                let atom = RegisterClassW(&wc);
                if atom == 0 {
                    let err = GetLastError();
                    // Another thread may have won the race; that's fine.
                    if err != ERROR_CLASS_ALREADY_EXISTS {
                        return Err(format!("RegisterClassW failed: {err:?}"));
                    }
                }
                Ok(())
            })
            .clone()
    }

    /// Spawn Moltorino attached to our host window. Mirrors
    /// [`super::super::moltorino`]'s spawn flags (no console window) but uses the
    /// hidden `--x-attach-split-to-window <decimal hwnd>` embed entry point
    /// instead of the standalone `-c t:<channel>` layout.
    fn spawn_embedded(exe: &Path, host_hwnd: isize) -> Result<std::process::Child, String> {
        use std::os::windows::process::CommandExt;
        std::process::Command::new(exe)
            .arg("--x-attach-split-to-window")
            .arg(host_hwnd.to_string())
            .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
            .spawn()
            .map_err(|e| format!("Couldn't start Moltorino at {}: {}", display_path(exe), e))
    }

    unsafe fn state_ptr(hwnd: HWND) -> *mut HostState {
        GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut HostState
    }

    /// Send a `set-channel` message to Moltorino's child window without ever
    /// blocking StreamNook: `SMTO_ABORTIFHUNG` returns promptly if Moltorino's
    /// message pump is wedged, and the 1s cap bounds even a healthy round-trip.
    unsafe fn send_channel(child: HWND, channel: &str) {
        let payload = build_set_channel_json(channel);
        let cds = COPYDATASTRUCT {
            dwData: 0,
            cbData: payload.len() as u32,
            lpData: payload.as_ptr() as *mut c_void,
        };
        let mut result: usize = 0;
        let _ = SendMessageTimeoutW(
            child,
            WM_COPYDATA,
            WPARAM(0),
            LPARAM(&cds as *const _ as isize),
            SEND_MESSAGE_TIMEOUT_FLAGS(SMTO_ABORTIFHUNG.0),
            1000,
            Some(&mut result as *mut usize),
        );
    }

    /// Size `child` to exactly fill `host`'s client area, applying `extra_flags`
    /// on top of the always-present no-z-order/no-activate/no-size-negotiation flags.
    ///
    /// This is the single place the embedded child is ever sized. It reads the
    /// host's *live* client rectangle with `GetClientRect` rather than trusting a
    /// cached `state.bounds`, so child sizing is ordering-independent: it no longer
    /// matters whether the last bounds tick had landed when a late `created-window`
    /// arrives — the child is always fit to the host's real current size. (The bug
    /// this fixes: after a Settings-close remount the child could stick at Qt's
    /// 300x150 default inside a correctly-sized host, leaving a stale WebView-cutout
    /// gap beside it.) The host itself is positioned in root coordinates by
    /// `apply_bounds`; the child lives in the host's client space, origin `(0,0)`.
    ///
    /// After sizing, it reads the child's resulting client size back and emits one
    /// concise debug line. A mismatch between the requested host size and the
    /// child's actual size — or any failed Win32 call — is escalated to a warning,
    /// since that is exactly the undersize symptom we're guarding against.
    unsafe fn fit_child_to_host(child: HWND, host: HWND, extra_flags: SET_WINDOW_POS_FLAGS) {
        let mut host_rect = RECT::default();
        if GetClientRect(host, &mut host_rect).is_err() {
            log::warn!(
                "[Moltorino] embed fit_child: GetClientRect(host={:#x}) failed: {:?}",
                host.0 as isize,
                GetLastError()
            );
            return;
        }
        let (w, h) = super::host_client_size((
            host_rect.left,
            host_rect.top,
            host_rect.right,
            host_rect.bottom,
        ));

        if let Err(e) = SetWindowPos(
            child,
            None,
            0,
            0,
            w,
            h,
            child_fit_flags(extra_flags),
        ) {
            log::warn!(
                "[Moltorino] embed fit_child: SetWindowPos(child={:#x}) to ({w}x{h}) failed: {e:?}",
                child.0 as isize,
            );
            return;
        }

        // Read the child's resulting client size back to verify the fit actually
        // took. Equal dimensions => success (one debug line); any difference =>
        // the undersize symptom, escalated to a warning.
        let mut child_rect = RECT::default();
        if GetClientRect(child, &mut child_rect).is_err() {
            log::warn!(
                "[Moltorino] embed fit_child: GetClientRect(child={:#x}) failed after resize: {:?}",
                child.0 as isize,
                GetLastError()
            );
            return;
        }
        let (cw, ch) = super::host_client_size((
            child_rect.left,
            child_rect.top,
            child_rect.right,
            child_rect.bottom,
        ));

        if cw != w || ch != h {
            log::warn!(
                "[Moltorino] embed fit_child: child={:#x} size ({cw}x{ch}) != host client ({w}x{h})",
                child.0 as isize,
            );
        } else {
            log::debug!(
                "[Moltorino] embed fit_child: child={:#x} filled host client ({w}x{h})",
                child.0 as isize,
            );
        }
    }

    /// Resize Moltorino's child to fill our host's client area. Once reparented
    /// its origin *is* the host's client origin, so it sits at 0,0 + host size.
    /// We keep it pinned to the top of the host's z-order (`HWND_TOP`) without
    /// activating it, so it never steals keyboard focus from StreamNook.
    unsafe fn fit_child(state: &HostState) {
        if let Some(child) = state.child {
            fit_child_to_host(child, state.host, SET_WINDOW_POS_FLAGS(0));
        }
    }

    thread_local! {
        /// The host HWND for *this* message-loop thread, published so the
        /// out-of-context WinEvent callback (which carries no user pointer) can
        /// reach the thread's `HostState` via `state_ptr`. Each `start` spawns its
        /// own thread with its own host, so this is naturally one-per-host and
        /// never shared across embeds. `0` means "no host on this thread yet".
        static HOST_HWND: std::cell::Cell<isize> = const { std::cell::Cell::new(0) };
    }

    /// Out-of-context WinEvent callback watching Moltorino's UI thread for
    /// `EVENT_OBJECT_LOCATIONCHANGE`. Because the hook was installed with
    /// `WINEVENT_OUTOFCONTEXT` *on the message-loop thread*, this fires on that
    /// same thread, serialized through its message pump — never concurrently with
    /// the WndProc handlers — so reaching `HostState` here is single-threaded and
    /// safe.
    ///
    /// It stays deliberately minimal and does NOT size anything itself: it only
    /// decides whether this event concerns the currently-attached child and, if
    /// so, coalesces a single `WM_APP_REFIT_CHILD` onto the queue. The actual
    /// `SetWindowPos` happens in the WndProc handler, off the callback stack, so a
    /// resize we trigger can't re-enter the hook mid-callback.
    unsafe extern "system" fn win_event_proc(
        _hook: HWINEVENTHOOK,
        event: u32,
        hwnd: HWND,
        id_object: i32,
        _id_child: i32,
        _id_event_thread: u32,
        _dwms_event_time: u32,
    ) {
        // We only registered the LOCATIONCHANGE range, but filter strictly: react
        // only to the window object's own move/resize, not child sub-objects.
        if event != EVENT_OBJECT_LOCATIONCHANGE || id_object != OBJID_WINDOW.0 {
            return;
        }
        let host = HOST_HWND.with(|h| h.get());
        if host == 0 {
            return;
        }
        let host_hwnd = HWND(host as *mut c_void);
        let ptr = state_ptr(host_hwnd);
        if ptr.is_null() {
            return;
        }
        let state = &mut *ptr;
        // Ignore events for anything but the child we're embedding right now. A
        // stale event queued before a Settings-close teardown must never act on a
        // child attached by the following fresh start.
        let current = state.child.map(|c| c.0 as isize);
        if !super::is_current_child(hwnd.0 as isize, current) {
            return;
        }
        // Coalesce a burst of location-change events into one queued refit.
        if state.refit_pending {
            return;
        }
        state.refit_pending = true;
        let _ = PostMessageW(Some(host_hwnd), WM_APP_REFIT_CHILD, WPARAM(0), LPARAM(0));
    }

    /// Install the location-change hook on Moltorino's UI thread so we notice when
    /// the reparented child self-resizes back to Qt's default after we fit it.
    ///
    /// Must be called on the message-loop thread, after `state.child` is set. The
    /// hook is scoped to Moltorino's own process + thread (via
    /// `GetWindowThreadProcessId` on the child), so it observes only that UI
    /// thread's events, not the whole desktop. Idempotent: a no-op if a hook is
    /// already installed or the child handle can't be resolved to a thread.
    unsafe fn install_win_event_hook(state: &mut HostState) {
        if state.win_event_hook.is_some() {
            return;
        }
        let Some(child) = state.child else {
            return;
        };
        let mut pid: u32 = 0;
        let tid = GetWindowThreadProcessId(child, Some(&mut pid as *mut u32));
        if tid == 0 || pid == 0 {
            log::warn!(
                "[Moltorino] embed win-event: GetWindowThreadProcessId(child={:#x}) \
                 gave pid={pid} tid={tid}; not hooking",
                child.0 as isize
            );
            return;
        }
        // Publish the host so the callback (same thread) can find HostState.
        HOST_HWND.with(|h| h.set(state.host.0 as isize));
        let hook = SetWinEventHook(
            EVENT_OBJECT_LOCATIONCHANGE,
            EVENT_OBJECT_LOCATIONCHANGE,
            None,
            Some(win_event_proc),
            pid,
            tid,
            WINEVENT_OUTOFCONTEXT,
        );
        if hook.0.is_null() {
            log::warn!(
                "[Moltorino] embed win-event: SetWinEventHook failed for child={:#x} \
                 (pid={pid} tid={tid}): {:?}",
                child.0 as isize,
                GetLastError()
            );
            return;
        }
        state.win_event_hook = Some(hook);
        log::debug!(
            "[Moltorino] embed win-event hook installed: child={:#x} pid={pid} tid={tid}",
            child.0 as isize
        );
    }

    /// Remove the location-change hook if installed, clearing the coalescing flag.
    /// Idempotent and safe on every teardown path; must run on the same
    /// (message-loop) thread that installed it, before the child/host it watches
    /// is cleared or destroyed, so a stale callback can never act on freed state.
    unsafe fn remove_win_event_hook(state: &mut HostState, reason: &str) {
        if let Some(hook) = state.win_event_hook.take() {
            let _ = UnhookWinEvent(hook);
            state.refit_pending = false;
            log::debug!("[Moltorino] embed win-event hook removed (reason: {reason})");
        }
    }

    /// Reparent Moltorino's returned split window into our host and convert it
    /// from a free-floating top-level popup into a framed-less `WS_CHILD`.
    ///
    /// Moltorino hands back a top-level `WS_POPUP` Qt window (verified: its
    /// `GetParent` is 0 before this runs). Simply moving it isn't enough — it
    /// stays a separate top-level window with its own chrome and z-order. We
    /// must both flip its window style to `WS_CHILD` (dropping the caption,
    /// resize frame, and min/max/system-menu chrome) *and* `SetParent` it onto
    /// the host. Order matters: clearing `WS_POPUP`/adding `WS_CHILD` before the
    /// `SetParent` keeps Windows from briefly treating it as a top-level during
    /// the transition, and the trailing `SWP_FRAMECHANGED` makes the style edit
    /// take visual effect.
    ///
    /// On any failure the child is left untouched (we never half-reparent it)
    /// and the caller falls back to native chat.
    unsafe fn attach_child(state: &mut HostState, child: HWND) -> Result<(), String> {
        if !IsWindow(Some(child)).as_bool() {
            return Err("Moltorino reported a window handle that is no longer valid.".to_string());
        }

        let old_parent = GetParent(child).unwrap_or_default();
        let old_style = GetWindowLongPtrW(child, GWL_STYLE);

        // Drop everything that only makes sense for a top-level window and add
        // the child bit; preserve every unrelated style flag.
        let strip = (WS_POPUP.0
            | WS_CAPTION.0
            | WS_THICKFRAME.0
            | WS_SYSMENU.0
            | WS_MINIMIZEBOX.0
            | WS_MAXIMIZEBOX.0) as isize;
        let new_style = (old_style & !strip) | WS_CHILD.0 as isize;

        // Restyle first so the SetParent lands on an already-child window.
        SetWindowLongPtrW(child, GWL_STYLE, new_style);

        // SetParent returns the previous parent (0/None for a top-level). A
        // genuine failure returns an error we surface for the fallback reason.
        let set_parent = SetParent(child, Some(state.host));
        if set_parent.is_err() {
            let err = GetLastError();
            // Undo the style edit so we leave the child exactly as we found it.
            SetWindowLongPtrW(child, GWL_STYLE, old_style);
            return Err(format!("SetParent failed: {err:?}"));
        }

        // Confirm the reparent actually took — a returned Ok with the wrong
        // parent would leave us embedding nothing.
        let final_parent = GetParent(child).unwrap_or_default();
        if final_parent != state.host {
            SetWindowLongPtrW(child, GWL_STYLE, old_style);
            return Err(format!(
                "Child did not reparent: expected host={:#x}, got parent={:#x}.",
                state.host.0 as isize, final_parent.0 as isize
            ));
        }

        // Apply the new frame and place the child at the host's client origin,
        // filling it, on top of the host's z-order, without activating. Size is
        // taken from the host's live client rect (not state.bounds) so a late
        // created-window always fills the correctly-sized host regardless of
        // whether the last bounds tick had landed. SWP_FRAMECHANGED makes the
        // WS_CHILD style edit take visual effect; SWP_SHOWWINDOW mirrors the
        // subsequent ShowWindow when the surface is meant to be visible.
        let mut extra_flags = SWP_FRAMECHANGED;
        if state.visible {
            extra_flags |= SWP_SHOWWINDOW;
        }
        fit_child_to_host(child, state.host, extra_flags);
        let _ = ShowWindow(child, if state.visible { SW_SHOW } else { SW_HIDE });

        log::debug!(
            "[Moltorino] embed child reparented: child={:#x} old-parent={:#x} \
             set-parent-prev={:#x} final-parent={:#x} old-style={:#x} new-style={:#x} \
             visible={}",
            child.0 as isize,
            old_parent.0 as isize,
            set_parent.map(|p| p.0 as isize).unwrap_or(0),
            final_parent.0 as isize,
            old_style,
            new_style,
            state.visible,
        );

        Ok(())
    }

    /// The offset of the `source` window's client origin within the `root`
    /// window's client area, i.e. `MapWindowPoints(source, root, {0,0})`.
    ///
    /// The frontend's bounds are physical pixels relative to the source
    /// (webview) client area, but our host is a `WS_CHILD` of the visible root,
    /// so every position must be expressed in root-client coordinates. Adding
    /// this offset (via [`super::translate_client_point`]) relocates a
    /// webview-relative point into that frame. When the source *is* the root
    /// (the common single-window case) this is `(0, 0)` and the translation is a
    /// no-op, so nothing regresses for that path.
    ///
    /// `MapWindowPoints` doesn't set `GetLastError` and can't fail here (both
    /// handles are validated in `start`), so a returning `0` genuinely means "no
    /// offset" rather than an error we'd need to distinguish.
    unsafe fn client_offset(source: HWND, root: HWND) -> (i32, i32) {
        let mut pt = [POINT { x: 0, y: 0 }];
        let _ = MapWindowPoints(Some(source), Some(root), &mut pt);
        (pt[0].x, pt[0].y)
    }

    /// The exact class name of Tauri/wry's WebView2 host window.
    const WRY_WEBVIEW_CLASS: &str = "WRY_WEBVIEW";

    /// State threaded through the [`EnumChildWindows`] callback while hunting for
    /// the `WRY_WEBVIEW` descendant of our root.
    struct FindWebview {
        /// Only accept a WebView owned by the same process as the root, so we can
        /// never latch onto Moltorino's (or any other app's) window.
        want_pid: u32,
        /// First matching visible WebView (preferred).
        visible: Option<HWND>,
        /// First matching WebView of any visibility (fallback).
        any: Option<HWND>,
    }

    /// Read a window's class name into a small stack buffer and compare it,
    /// without allocating. `GetClassNameW` returns the length written (excluding
    /// the NUL); 0 means failure, which simply won't match.
    unsafe fn class_name_is(hwnd: HWND, expected: &str) -> bool {
        let mut buf = [0u16; 64];
        let len = GetClassNameW(hwnd, &mut buf);
        if len <= 0 {
            return false;
        }
        let name = String::from_utf16_lossy(&buf[..len as usize]);
        name == expected
    }

    /// `EnumChildWindows` callback: record the first same-process `WRY_WEBVIEW`
    /// child, preferring a visible one. Always returns `TRUE` to keep enumerating
    /// (so "visible" can win even if an invisible one is seen first), except it
    /// stops early once a visible match is in hand.
    unsafe extern "system" fn find_webview_proc(hwnd: HWND, lparam: LPARAM) -> windows::core::BOOL {
        let ctx = &mut *(lparam.0 as *mut FindWebview);

        if !class_name_is(hwnd, WRY_WEBVIEW_CLASS) {
            return true.into();
        }

        // Reject anything not owned by our process (defence against grabbing
        // Moltorino / an IME / an unrelated top-level WebView).
        let mut pid: u32 = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut pid as *mut u32));
        if pid != ctx.want_pid {
            return true.into();
        }

        if ctx.any.is_none() {
            ctx.any = Some(hwnd);
        }
        if IsWindowVisible(hwnd).as_bool() {
            ctx.visible = Some(hwnd);
            // A visible, same-process WRY_WEBVIEW is exactly what we want; stop.
            return false.into();
        }
        true.into()
    }

    /// Find the `WRY_WEBVIEW` window belonging to `root` (Tauri's WebView2 host).
    ///
    /// Enumerates every descendant of the root, matches the exact class name,
    /// requires the same process as the root, prefers a visible instance, and
    /// validates the pick with `IsWindow`. Never returns the root itself,
    /// Moltorino's child, an IME window, or any unrelated window. Returns `None`
    /// if no qualifying WebView exists (the caller then logs + falls back).
    unsafe fn resolve_webview(root: HWND) -> Option<HWND> {
        if !IsWindow(Some(root)).as_bool() {
            return None;
        }
        // The WebView lives in the same process as the root window; match on the
        // root's PID rather than our own, since a future multi-process split
        // could put the UI thread elsewhere.
        let mut root_pid: u32 = 0;
        GetWindowThreadProcessId(root, Some(&mut root_pid as *mut u32));

        let mut ctx = FindWebview {
            want_pid: root_pid,
            visible: None,
            any: None,
        };
        let _ = EnumChildWindows(
            Some(root),
            Some(find_webview_proc),
            LPARAM(&mut ctx as *mut FindWebview as isize),
        );

        let picked = ctx.visible.or(ctx.any)?;
        if IsWindow(Some(picked)).as_bool() {
            Some(picked)
        } else {
            None
        }
    }

    /// Return the cached WebView HWND, resolving (or re-resolving) it if we don't
    /// have one or the cached handle has gone invalid. Resets `applied_hole` when
    /// a *fresh* handle is adopted, since a new window carries no region.
    unsafe fn ensure_webview(state: &mut HostState) -> Option<HWND> {
        if let Some(wv) = state.webview {
            if IsWindow(Some(wv)).as_bool() {
                return Some(wv);
            }
            // Stored handle died (e.g. webview recreated); drop our stale region
            // bookkeeping and re-resolve below.
            log::debug!(
                "[Moltorino] embed WRY_WEBVIEW handle {:#x} went invalid; re-resolving",
                wv.0 as isize
            );
            state.webview = None;
            state.applied_hole = None;
        }

        let resolved = resolve_webview(state.root);
        if let Some(wv) = resolved {
            log::debug!(
                "[Moltorino] embed resolved WRY_WEBVIEW: root={:#x} webview={:#x}",
                state.root.0 as isize,
                wv.0 as isize
            );
            state.webview = Some(wv);
            // A newly-adopted window starts with the default (full) region.
            state.applied_hole = None;
        }
        resolved
    }

    /// Restore the WebView's full window region (remove any hole). Idempotent and
    /// always safe: a no-op when no cutout is active or the WebView is gone.
    ///
    /// This MUST run on every path where the embedded chat stops occupying the
    /// panel (hidden, disabled, fallback, attach/launch failure, process exit,
    /// teardown) — otherwise the user is left with a transparent hole that also
    /// swallows mouse/keyboard input over that area of the WebView.
    ///
    /// `SetWindowRgn(hwnd, None, TRUE)` hands the window back its default region;
    /// per the Win32 contract we do NOT own/free anything after this call.
    unsafe fn restore_webview(state: &mut HostState, reason: &str) {
        if state.applied_hole.is_none() {
            return; // Nothing installed; stay quiet and cheap.
        }
        let Some(wv) = state.webview else {
            // We think a hole is active but lost the handle; clear our flag so we
            // don't loop, and move on — there's no window left to restore.
            state.applied_hole = None;
            return;
        };
        if !IsWindow(Some(wv)).as_bool() {
            state.webview = None;
            state.applied_hole = None;
            return;
        }
        // bredraw = true so the reclaimed area repaints immediately.
        let _ = SetWindowRgn(wv, None, true);
        state.applied_hole = None;
        log::debug!(
            "[Moltorino] embed WebView region restored (reason: {reason}) webview={:#x}",
            wv.0 as isize
        );
    }

    /// Compute the current hole rectangle in WebView-relative client coordinates
    /// and punch it into the WebView's window region (full region minus hole via
    /// `RGN_DIFF`), so the still-playing stream shows through everywhere except
    /// the embedded chat panel.
    ///
    /// Called after the WebView is resolved, after the host is created, after
    /// Moltorino attaches, and after every bounds/visibility update — never on a
    /// timer. When the embedded chat should not be showing (not visible, no child
    /// attached yet, degenerate/off-screen geometry) it restores the full region
    /// instead, so we never leave a blank hole.
    ///
    /// HRGN ownership (strict, no leaks):
    ///   * `full` and `hole` are temporaries we always delete.
    ///   * `final_rgn` is the combined result; on a successful `SetWindowRgn`
    ///     Windows takes ownership and we must NOT delete it, and must not reuse
    ///     it. On any failure we delete every region we still own.
    unsafe fn apply_cutout(state: &mut HostState, reason: &str) {
        // Only punch a hole when the panel is actually meant to be visible with
        // Moltorino attached inside it; otherwise ensure the WebView is whole.
        if !state.visible || state.child.is_none() {
            restore_webview(state, reason);
            return;
        }

        let Some(wv) = ensure_webview(state) else {
            // No WebView to cut — make sure nothing stale is installed and let the
            // caller's fallback logic (if any) take over. Without a resolved
            // WebView there is nothing we could have punched, so there is no blank
            // hole to leave behind.
            log::warn!(
                "[Moltorino] embed could not resolve WRY_WEBVIEW under root={:#x}; \
                 skipping cutout ({reason})",
                state.root.0 as isize
            );
            return;
        };

        // WebView client size (the region is in client coordinates).
        let mut wv_rect = RECT::default();
        if GetClientRect(wv, &mut wv_rect).is_err() {
            log::warn!(
                "[Moltorino] embed GetClientRect(webview={:#x}) failed: {:?}",
                wv.0 as isize,
                GetLastError()
            );
            return;
        }
        let vw = wv_rect.right - wv_rect.left;
        let vh = wv_rect.bottom - wv_rect.top;

        // Map the host's client origin (0,0) into the WebView's client space.
        // The host is sized to the panel, so its origin + (w,h) is the hole.
        let mut origin = [POINT { x: 0, y: 0 }];
        let _ = MapWindowPoints(Some(state.host), Some(wv), &mut origin);
        let (_, _, w, h) = state.bounds;

        let clipped = super::clip_hole_to_webview((origin[0].x, origin[0].y, w, h), (vw, vh));

        let Some((left, top, right, bottom)) = clipped else {
            // Degenerate/off-screen hole: don't punch a bad region — restore the
            // full WebView so the panel simply isn't cut out this tick.
            restore_webview(state, "degenerate-hole");
            return;
        };

        // Skip a redundant region swap (and its repaint flicker) when the hole
        // hasn't moved since we last applied it.
        if state.applied_hole == Some((left, top, right, bottom)) {
            return;
        }

        // full = the entire WebView client rect; hole = the panel rect.
        let full = CreateRectRgn(0, 0, vw, vh);
        let hole = CreateRectRgn(left, top, right, bottom);
        if full.is_invalid() || hole.is_invalid() {
            log::warn!(
                "[Moltorino] embed CreateRectRgn failed: {:?}",
                GetLastError()
            );
            if !full.is_invalid() {
                let _ = DeleteObject(HGDIOBJ(full.0));
            }
            if !hole.is_invalid() {
                let _ = DeleteObject(HGDIOBJ(hole.0));
            }
            return;
        }

        // final = full - hole. CombineRgn writes into its first (dest) argument,
        // so `full` becomes the result region; `hole` stays a separate temporary.
        let combined = CombineRgn(Some(full), Some(full), Some(hole), RGN_DIFF);
        // `hole` has served its purpose either way.
        let _ = DeleteObject(HGDIOBJ(hole.0));
        if combined == RGN_ERROR {
            log::warn!("[Moltorino] embed CombineRgn(RGN_DIFF) failed");
            let _ = DeleteObject(HGDIOBJ(full.0));
            return;
        }

        // Hand the region to the WebView. On success Windows OWNS `full` — we must
        // neither delete nor reuse it. On failure we still own it and must free it.
        let ok = SetWindowRgn(wv, Some(full), true) != 0;
        if !ok {
            log::warn!(
                "[Moltorino] embed SetWindowRgn(webview={:#x}) failed: {:?}",
                wv.0 as isize,
                GetLastError()
            );
            let _ = DeleteObject(HGDIOBJ(full.0));
            return;
        }

        state.applied_hole = Some((left, top, right, bottom));
        log::debug!(
            "[Moltorino] embed WebView cutout applied ({reason}): webview={:#x} \
             webview-size=({vw}x{vh}) hole=(l={left},t={top},r={right},b={bottom}) \
             hole-size=({}x{})",
            wv.0 as isize,
            right - left,
            bottom - top,
        );
    }

    unsafe fn apply_bounds(state: &mut HostState, x: i32, y: i32, w: i32, h: i32, visible: bool) {
        state.bounds = (x, y, w, h);
        state.visible = visible;

        // Translate the webview-relative origin into root-client coordinates.
        // Width/height are physical pixel sizes and pass through untouched.
        let offset = client_offset(state.source, state.root);
        let (rx, ry) = super::translate_client_point(x, y, offset);

        // Log only when the offset actually shifts (monitor/DPI/dock/restore
        // move), so the 250ms safety-poll bounds ticks never spam the log.
        if state.logged_offset != Some(offset) {
            state.logged_offset = Some(offset);
            log::debug!(
                "[Moltorino] embed bounds: webview=({x},{y}) offset={offset:?} \
                 root-client=({rx},{ry}) size=({w}x{h}) visible={visible}"
            );
        }

        // Raise the host above its WebView sibling and reposition/resize it in
        // one shot. HWND_TOP keeps it painted over the chat area; SWP_NOACTIVATE
        // means we never steal keyboard focus, and SWP_SHOWWINDOW/SW_HIDE drives
        // visibility without a separate MoveWindow.
        let mut flags = SWP_NOACTIVATE;
        flags |= if visible {
            SWP_SHOWWINDOW
        } else {
            SWP_HIDEWINDOW
        };
        let _ = SetWindowPos(
            state.host,
            Some(HWND_TOP),
            rx,
            ry,
            w.max(0),
            h.max(0),
            flags,
        );
        fit_child(state);

        // Re-punch (or restore) the WebView cutout for the host's new
        // position/size/visibility. This is the single place bounds ticks flow
        // through — moves, resizes, maximize/restore, and hidden<->visible
        // transitions all land here — so no polling timer is needed.
        apply_cutout(state, "bounds");
    }

    unsafe fn apply_channel(state: &mut HostState, channel: String) {
        match state.child {
            Some(child) => send_channel(child, &channel),
            None => state.pending_channel = Some(channel),
        }
    }

    unsafe fn drain_commands(hwnd: HWND) {
        let ptr = state_ptr(hwnd);
        if ptr.is_null() {
            return;
        }
        let state = &mut *ptr;
        // Collect first so we don't hold the borrow across teardown.
        let cmds: Vec<Cmd> = state.rx.try_iter().collect();
        for cmd in cmds {
            match cmd {
                Cmd::Bounds {
                    x,
                    y,
                    w,
                    h,
                    visible,
                } => apply_bounds(state, x, y, w, h, visible),
                Cmd::Channel(c) => apply_channel(state, c),
                Cmd::Shutdown => {
                    state.shutting_down = true;
                    // Stop watching Moltorino's UI thread before we tear anything
                    // down, so a queued location-change can't act on freed state.
                    remove_win_event_hook(state, "shutdown");
                    // Restore the WebView before we destroy anything, so the panel
                    // area is never left as a blank, input-swallowing hole.
                    restore_webview(state, "shutdown");
                    if let Some(mut proc) = state.child_proc.take() {
                        let _ = proc.kill();
                        let _ = proc.wait();
                    }
                    let _ = DestroyWindow(state.host);
                    return;
                }
            }
        }
    }

    unsafe extern "system" fn wndproc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match msg {
            WM_CREATE => {
                let cs = lparam.0 as *const CREATESTRUCTW;
                if !cs.is_null() {
                    let state = (*cs).lpCreateParams as *mut HostState;
                    SetWindowLongPtrW(hwnd, GWLP_USERDATA, state as isize);
                }
                LRESULT(0)
            }
            WM_COPYDATA => {
                let cds = lparam.0 as *const COPYDATASTRUCT;
                if !cds.is_null() {
                    let bytes = std::slice::from_raw_parts(
                        (*cds).lpData as *const u8,
                        (*cds).cbData as usize,
                    );
                    if let Some(child_raw) = parse_created_window(bytes) {
                        let ptr = state_ptr(hwnd);
                        if !ptr.is_null() {
                            let state = &mut *ptr;
                            let child = HWND(child_raw as *mut c_void);
                            match attach_child(state, child) {
                                Ok(()) => {
                                    state.child = Some(child);
                                    if let Some(chan) = state.pending_channel.take() {
                                        send_channel(child, &chan);
                                    }
                                    // Moltorino is now inside the host; punch the
                                    // WebView hole so the panel shows through.
                                    apply_cutout(state, "child-attached");
                                    // Watch Moltorino's UI thread for the late
                                    // self-resize back to Qt's 300x150 default: the
                                    // location-change hook posts WM_APP_REFIT_CHILD so
                                    // we re-fit the child with no timer/poll.
                                    install_win_event_hook(state);
                                }
                                Err(reason) => {
                                    // We couldn't reparent Moltorino's window. Leave
                                    // its child untracked, tell the UI to fall back
                                    // to native chat, kill only our own Moltorino
                                    // process, and tear the host down cleanly. Mirror
                                    // the WM_APP_EXITED teardown so the global clears
                                    // and a later start recreates fresh.
                                    log::warn!("[Moltorino] embed attach failed: {reason}");
                                    state.shutting_down = true;
                                    // No hook was installed on this path (install
                                    // happens only on attach success), but remove is
                                    // idempotent and keeps every teardown uniform.
                                    remove_win_event_hook(state, "attach-failed");
                                    // Restore the WebView first so no blank hole is
                                    // ever left behind (it almost certainly isn't
                                    // active this early, but restore is idempotent).
                                    restore_webview(state, "attach-failed");
                                    if let Some(mut proc) = state.child_proc.take() {
                                        let _ = proc.kill();
                                        let _ = proc.wait();
                                    }
                                    let _ = state.app.emit(FALLBACK_EVENT, reason.as_str());
                                    clear_global_if(state.host.0 as isize);
                                    let _ = DestroyWindow(state.host);
                                }
                            }
                        }
                    }
                }
                LRESULT(1)
            }
            WM_APP_WAKE => {
                drain_commands(hwnd);
                LRESULT(0)
            }
            WM_APP_REFIT_CHILD => {
                // A location-change on Moltorino's child was coalesced into this
                // single message by the WinEvent callback. Clear the coalescing
                // flag first so any events arriving from here on queue a fresh
                // refit, then correct the child's size — but ONLY if it actually
                // deviates from the host client size. That conditional is the loop
                // terminator: our own SetWindowPos below fires another
                // location-change, which re-enters here, finds the sizes equal, and
                // does nothing.
                let ptr = state_ptr(hwnd);
                if !ptr.is_null() {
                    let state = &mut *ptr;
                    state.refit_pending = false;
                    if let Some(child) = state.child {
                        if IsWindow(Some(child)).as_bool() && IsWindow(Some(state.host)).as_bool() {
                            // Host client size.
                            let mut hr = RECT::default();
                            // Child client size.
                            let mut cr = RECT::default();
                            if GetClientRect(state.host, &mut hr).is_ok()
                                && GetClientRect(child, &mut cr).is_ok()
                            {
                                let host_size =
                                    super::host_client_size((hr.left, hr.top, hr.right, hr.bottom));
                                let child_size =
                                    super::host_client_size((cr.left, cr.top, cr.right, cr.bottom));
                                if super::should_refit_child(host_size, child_size) {
                                    log::debug!(
                                        "[Moltorino] embed win-event refit: child={:#x} \
                                         ({}x{}) -> host client ({}x{})",
                                        child.0 as isize,
                                        child_size.0,
                                        child_size.1,
                                        host_size.0,
                                        host_size.1,
                                    );
                                    fit_child_to_host(child, state.host, SET_WINDOW_POS_FLAGS(0));
                                }
                            }
                        }
                    }
                }
                LRESULT(0)
            }
            WM_APP_EXITED => {
                let ptr = state_ptr(hwnd);
                if !ptr.is_null() {
                    let state = &mut *ptr;
                    if !state.shutting_down {
                        // Moltorino died unexpectedly: drop our handle to it, tell
                        // the UI to fall back to native chat, and tear the host
                        // down. Clear the global so a later start recreates cleanly.
                        // Remove the hook first: the child it watched is gone, so a
                        // late callback must never look for it.
                        remove_win_event_hook(state, "process-exited");
                        // Restore the WebView first: the child is gone, so leaving
                        // the hole would strand an empty, input-swallowing gap.
                        restore_webview(state, "process-exited");
                        state.child = None;
                        state.child_proc = None;
                        let _ = state.app.emit(FALLBACK_EVENT, "process-exited");
                        clear_global_if(state.host.0 as isize);
                        let _ = DestroyWindow(state.host);
                    }
                }
                LRESULT(0)
            }
            WM_DESTROY => {
                let ptr = state_ptr(hwnd);
                if !ptr.is_null() {
                    // Safety net: every intentional teardown path already removes
                    // the hook before reaching here, but if a WM_DESTROY ever
                    // arrives with one still installed, unhook it before we free the
                    // state it reaches through, so a queued callback can't touch
                    // freed memory. Also clears the thread-local host pointer.
                    let state = &mut *ptr;
                    remove_win_event_hook(state, "wm-destroy");
                    HOST_HWND.with(|h| h.set(0));
                    // Signal the synchronous app-exit path that teardown is fully
                    // done: by the time any path reaches WM_DESTROY the child has
                    // already been killed+reaped (Cmd::Shutdown / attach-failed /
                    // WM_APP_EXITED all do that before calling DestroyWindow), so a
                    // waiter unblocked here knows no Moltorino is left behind. The
                    // send is best-effort — if `stop_blocking` already timed out and
                    // dropped its receiver this is a harmless no-op. (The subsequent
                    // drop of `done_tx` with the state would disconnect the channel
                    // anyway, so completion is signalled either way.)
                    let _ = state.done_tx.send(());
                    // Reclaim and drop the boxed state (closes the channel, drops
                    // any surviving child handle).
                    SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                    drop(Box::from_raw(ptr));
                }
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }

    /// Clear the global handle iff it still points at `host` (avoids a natural
    /// exit stomping a handle a concurrent restart just installed).
    fn clear_global_if(host: isize) {
        if let Ok(mut guard) = embed().lock() {
            if guard.as_ref().map(|h| h.host) == Some(host) {
                *guard = None;
            }
        }
    }

    fn thread_main(init: Init, ready: Sender<Result<isize, String>>) {
        unsafe {
            if let Err(e) = ensure_class_registered() {
                let _ = ready.send(Err(e));
                return;
            }

            // `window.hwnd()` is the webview/native window the frontend's bounds
            // are relative to — NOT necessarily the visible top-level. Resolve the
            // real root and validate both before we parent anything to it: an
            // internal/offscreen source window (its client origin can sit near
            // -32000) would otherwise drag the host — and Moltorino — offscreen.
            let source = HWND(init.source_hwnd as *mut c_void);
            if !IsWindow(Some(source)).as_bool() {
                // Nothing allocated yet, so there's nothing to reclaim.
                let _ = ready.send(Err(
                    "The StreamNook window handle is no longer valid.".to_string()
                ));
                return;
            }
            // GA_ROOT climbs to the visible top-level owner. If it somehow yields
            // an invalid handle, fall back to the source so we never parent to
            // null — the translation then collapses to a no-op offset of (0,0).
            let root = {
                let r = GetAncestor(source, GA_ROOT);
                if !r.0.is_null() && IsWindow(Some(r)).as_bool() {
                    r
                } else {
                    source
                }
            };

            log::debug!(
                "[Moltorino] embed HWNDs: source={:#x} root={:#x}",
                source.0 as isize,
                root.0 as isize
            );

            let (x, y, w, h) = init.bounds;
            // The host is a WS_CHILD of `root`, so its initial position must be in
            // root-client coordinates too — same translation the later bounds
            // updates apply.
            let offset = client_offset(source, root);
            let (rx, ry) = super::translate_client_point(x, y, offset);

            let boxed = Box::new(HostState {
                host: HWND(std::ptr::null_mut()),
                source,
                root,
                child: None,
                pending_channel: Some(init.channel),
                bounds: init.bounds,
                logged_offset: Some(offset),
                visible: init.visible,
                rx: init.rx,
                child_proc: None,
                app: init.app.clone(),
                shutting_down: false,
                webview: None,
                applied_hole: None,
                win_event_hook: None,
                refit_pending: false,
                done_tx: init.done_tx,
            });
            let state_raw = Box::into_raw(boxed);

            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                CLASS_NAME,
                w!(""),
                WS_CHILD | WS_CLIPCHILDREN | WS_CLIPSIBLINGS,
                rx,
                ry,
                w.max(0),
                h.max(0),
                Some(root),
                None,
                None,
                Some(state_raw as *const c_void),
            );

            let hwnd = match hwnd {
                Ok(h) if !h.0.is_null() => h,
                _ => {
                    let err = GetLastError();
                    // WM_CREATE never stored the pointer on failure; reclaim it.
                    drop(Box::from_raw(state_raw));
                    let _ = ready.send(Err(format!("CreateWindowExW failed: {err:?}")));
                    return;
                }
            };

            // WM_CREATE ran synchronously and stored state_raw; fill in the host
            // handle and initial placement now that we have it.
            (*state_raw).host = hwnd;
            log::debug!(
                "[Moltorino] embed host created: host={:#x} webview=({x},{y}) \
                 offset={offset:?} root-client=({rx},{ry}) size=({w}x{h})",
                hwnd.0 as isize
            );
            // Publish the host handle so a timed-out `start` can still reach and
            // tear down this real window/process instead of orphaning it.
            init.host_slot.store(hwnd.0 as isize, Ordering::SeqCst);
            let _ = ShowWindow(hwnd, if init.visible { SW_SHOW } else { SW_HIDE });

            // Resolve (and log) the WRY_WEBVIEW up front now that the host and
            // root are settled. The cutout itself is only punched once Moltorino
            // attaches (see the WM_COPYDATA handler / `apply_cutout`), but caching
            // the handle here surfaces a resolution failure early in the logs.
            ensure_webview(&mut *state_raw);

            match spawn_embedded(&init.exe, hwnd.0 as isize) {
                Err(e) => {
                    let _ = DestroyWindow(hwnd); // frees state via WM_DESTROY
                    let _ = ready.send(Err(e));
                    return;
                }
                Ok(child) => {
                    let pid = child.id();
                    // Assign to the shared crash-safe Job Object before we store the
                    // handle, so an abnormal StreamNook exit (crash/kill) still takes
                    // this embedded Moltorino down with it. Best-effort by exact
                    // process handle; the normal route stays the explicit kill+wait
                    // on `child_proc` in the Shutdown/attach-failed/exit paths.
                    {
                        use std::os::windows::io::AsRawHandle;
                        crate::commands::moltorino::jobobject::assign(
                            child.as_raw_handle() as isize
                        );
                    }
                    (*state_raw).child_proc = Some(child);
                    spawn_exit_watcher(pid, hwnd.0 as isize);
                }
            }

            let _ = ready.send(Ok(hwnd.0 as isize));

            // Pump until WM_DESTROY -> PostQuitMessage. GetMessageW returns 0 on
            // WM_QUIT and -1 on error; both end the loop.
            let mut msg = MSG::default();
            loop {
                let got = GetMessageW(&mut msg, None, 0, 0).0;
                if got <= 0 {
                    break;
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }

    /// Wait on exactly our process (by PID, `SYNCHRONIZE` only) and post
    /// `WM_APP_EXITED` when it ends. No polling; blocks in the kernel.
    fn spawn_exit_watcher(pid: u32, host: isize) {
        std::thread::spawn(move || unsafe {
            let handle: HANDLE = match OpenProcess(PROCESS_SYNCHRONIZE, false, pid) {
                Ok(h) => h,
                Err(_) => {
                    // Can't observe it; assume the worst so the UI still recovers.
                    let _ = PostMessageW(
                        Some(HWND(host as *mut c_void)),
                        WM_APP_EXITED,
                        WPARAM(0),
                        LPARAM(0),
                    );
                    return;
                }
            };
            let _ = WaitForSingleObject(handle, u32::MAX);
            let _ = CloseHandle(handle);
            let _ = PostMessageW(
                Some(HWND(host as *mut c_void)),
                WM_APP_EXITED,
                WPARAM(0),
                LPARAM(0),
            );
        });
    }

    fn wake(host: isize) {
        unsafe {
            let _ = PostMessageW(
                Some(HWND(host as *mut c_void)),
                WM_APP_WAKE,
                WPARAM(0),
                LPARAM(0),
            );
        }
    }

    /// Ensure an embedded host exists and is following `channel`, creating it on
    /// first use and reusing the one process/window afterwards.
    pub fn start(
        channel: String,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        visible: bool,
        source_hwnd: isize,
        exe_path: &str,
        app: AppHandle,
    ) -> Result<(), String> {
        let channel = channel.trim().to_ascii_lowercase();
        if !is_valid_twitch_login(&channel) {
            return Err(format!("\"{channel}\" isn't a valid Twitch channel name."));
        }
        // Shared runtime picker: a valid configured override wins, else the copy
        // bundled beside StreamNook, else a clear not-found error (surfaced to the
        // UI as a native-chat fallback). Resolving before we touch the embed lock
        // keeps a bad path from ever creating a host window.
        let runtime = resolve_moltorino_runtime(exe_path)?;
        log::debug!(
            "[Moltorino] embed runtime resolved: source={} path={}",
            runtime.source.as_status(),
            display_path(&runtime.path)
        );
        let exe = runtime.path;

        let mut guard = embed()
            .lock()
            .map_err(|_| "embed lock poisoned".to_string())?;

        if let Some(handle) = guard.as_ref() {
            // Reuse: push the latest bounds + channel into the running host.
            let _ = handle.tx.send(Cmd::Bounds {
                x,
                y,
                w: width,
                h: height,
                visible,
            });
            let _ = handle.tx.send(Cmd::Channel(channel));
            wake(handle.host);
            return Ok(());
        }

        let (cmd_tx, cmd_rx) = std::sync::mpsc::channel::<Cmd>();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<isize, String>>();
        // Fired by the message-loop thread from WM_DESTROY once teardown is fully
        // complete. `stop_blocking` (app exit) waits on it so we never race the OS
        // tearing StreamNook down before the child is killed. Kept in the handle.
        let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
        // Shared slot the thread fills the instant its window exists. We keep our
        // own clone so that if the readiness wait times out we can still find and
        // tear down the (real, already-created) host instead of orphaning it.
        let host_slot = Arc::new(AtomicIsize::new(0));
        let init = Init {
            source_hwnd,
            exe,
            channel,
            bounds: (x, y, width, height),
            visible,
            rx: cmd_rx,
            app,
            host_slot: host_slot.clone(),
            done_tx,
        };
        std::thread::spawn(move || thread_main(init, ready_tx));

        match ready_rx.recv_timeout(std::time::Duration::from_secs(5)) {
            Ok(Ok(host)) => {
                *guard = Some(EmbedHandle {
                    host,
                    tx: cmd_tx,
                    done_rx,
                });
                Ok(())
            }
            Ok(Err(e)) => Err(e),
            Err(_) => {
                // The thread never reported success in time. If it had already
                // created the host window (published its HWND), it may also have
                // spawned Moltorino — an orphan that nothing else can reach, since
                // we never stored an EmbedHandle. Tell that thread to shut down via
                // the command channel + a direct wake to the published HWND, so the
                // window and process we created are always cleaned up.
                let published = host_slot.load(Ordering::Acquire);
                if published != 0 {
                    let _ = cmd_tx.send(Cmd::Shutdown);
                    wake(published);
                }
                Err("Timed out starting the embedded Moltorino host.".to_string())
            }
        }
    }

    /// Update bounds/visibility of a running host. No-op (Ok) if none exists.
    pub fn set_bounds(
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        visible: bool,
    ) -> Result<(), String> {
        let guard = embed()
            .lock()
            .map_err(|_| "embed lock poisoned".to_string())?;
        if let Some(handle) = guard.as_ref() {
            let _ = handle.tx.send(Cmd::Bounds {
                x,
                y,
                w: width,
                h: height,
                visible,
            });
            wake(handle.host);
        }
        Ok(())
    }

    /// Change the followed channel on a running host. No-op (Ok) if none exists.
    pub fn set_channel(channel: String) -> Result<(), String> {
        let channel = channel.trim().to_ascii_lowercase();
        if !is_valid_twitch_login(&channel) {
            return Err(format!("\"{channel}\" isn't a valid Twitch channel name."));
        }
        let guard = embed()
            .lock()
            .map_err(|_| "embed lock poisoned".to_string())?;
        if let Some(handle) = guard.as_ref() {
            let _ = handle.tx.send(Cmd::Channel(channel));
            wake(handle.host);
        }
        Ok(())
    }

    /// Tear down the host (kill our process, destroy our window). Idempotent.
    ///
    /// Non-blocking: it posts `Cmd::Shutdown` and returns immediately, letting the
    /// message-loop thread do the kill+wait+destroy. This is the right shape for
    /// the UI-driven path (`moltorino_embed_stop`), which runs on the async
    /// runtime and must not block it. The app-exit path uses [`stop_blocking`]
    /// instead, which waits for the teardown to actually finish.
    pub fn stop() -> Result<(), String> {
        let mut guard = embed()
            .lock()
            .map_err(|_| "embed lock poisoned".to_string())?;
        if let Some(handle) = guard.take() {
            let _ = handle.tx.send(Cmd::Shutdown);
            wake(handle.host);
        }
        Ok(())
    }

    /// Synchronous teardown for application exit. Posts `Cmd::Shutdown`, wakes the
    /// message-loop thread, then *waits* for that thread to confirm it has killed
    /// and reaped Moltorino and destroyed the host window (signalled via
    /// `done_rx`). This closes the shutdown race that left an orphaned embedded
    /// Moltorino behind: previously `stop()` only posted the command and returned,
    /// so StreamNook could exit — and the OS tear the process down — before the
    /// thread had run the kill.
    ///
    /// Bounded wait: if the thread doesn't confirm within the timeout (a wedged
    /// Moltorino message pump, say), we log and return rather than hang shutdown.
    /// The Job Object backstop still guarantees the child dies moments later when
    /// our process — and thus our sole job handle — goes away. Never panics.
    pub fn stop_blocking() {
        // Take the handle out under the lock, then release the lock before we block
        // on `done_rx`, so nothing else can deadlock behind a shutdown that's
        // waiting on the message-loop thread.
        let handle = match embed().lock() {
            Ok(mut guard) => guard.take(),
            Err(_) => {
                log::warn!("[Moltorino] embed lock poisoned during exit; skipping embed teardown");
                return;
            }
        };
        let Some(handle) = handle else {
            return; // Never started, or already torn down.
        };
        // If the send fails the thread is already gone (window destroyed), so
        // teardown has effectively completed; don't wait on a dead channel.
        if handle.tx.send(Cmd::Shutdown).is_err() {
            return;
        }
        wake(handle.host);
        match handle
            .done_rx
            .recv_timeout(std::time::Duration::from_secs(3))
        {
            Ok(()) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                // Thread dropped its sender without signalling — it has exited, so
                // the window/process are gone. Nothing left to wait for.
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                log::warn!(
                    "[Moltorino] embedded host didn't confirm teardown within 3s; \
                     relying on the Job Object to reap it at process exit"
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tauri commands (thin wrappers; real work is in `imp` on Windows).
// ---------------------------------------------------------------------------

/// Start (or reuse) the embedded Moltorino host and follow `channel`.
#[cfg(windows)]
#[tauri::command]
pub async fn moltorino_embed_start(
    channel: String,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    visible: bool,
    window: tauri::WebviewWindow,
    state: tauri::State<'_, crate::models::settings::AppState>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    let exe_path = {
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
    // This is the webview/native window handle, which is not necessarily the
    // visible top-level window — `imp::start` resolves the real root (GA_ROOT)
    // and treats this as the source frame for coordinate translation.
    let hwnd = window
        .hwnd()
        .map_err(|e| format!("Couldn't get the StreamNook window handle: {e}"))?;
    imp::start(
        channel,
        x,
        y,
        width,
        height,
        visible,
        hwnd.0 as isize,
        &exe_path,
        app,
    )
}

/// Update the embedded host's position/size/visibility.
#[cfg(windows)]
#[tauri::command]
pub async fn moltorino_embed_set_bounds(
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    visible: bool,
) -> Result<(), String> {
    imp::set_bounds(x, y, width, height, visible)
}

/// Point the embedded host at a different Twitch channel.
#[cfg(windows)]
#[tauri::command]
pub async fn moltorino_embed_set_channel(channel: String) -> Result<(), String> {
    imp::set_channel(channel)
}

/// Tear the embedded host down and return to native chat.
#[cfg(windows)]
#[tauri::command]
pub async fn moltorino_embed_stop() -> Result<(), String> {
    imp::stop()
}

/// Synchronous teardown for the app-exit path (called from `RunEvent::Exit`,
/// which is not an async context). Idempotent; a no-op if nothing was started.
///
/// Unlike the UI-driven `moltorino_embed_stop`, this *waits* for the message-loop
/// thread to confirm it has killed and reaped Moltorino before returning, so
/// StreamNook never exits out from under a not-yet-killed embedded process.
#[cfg(windows)]
pub fn moltorino_embed_stop_sync() {
    imp::stop_blocking();
}

/// Non-Windows: nothing to tear down.
#[cfg(not(windows))]
pub fn moltorino_embed_stop_sync() {}

// Non-Windows stubs: the integration is Win32-only, so these keep the command
// surface identical while making the feature inert off-Windows.
#[cfg(not(windows))]
#[tauri::command]
pub async fn moltorino_embed_start(
    _channel: String,
    _x: i32,
    _y: i32,
    _width: i32,
    _height: i32,
    _visible: bool,
) -> Result<(), String> {
    Err("The embedded Moltorino integration is only available on Windows.".to_string())
}

#[cfg(not(windows))]
#[tauri::command]
pub async fn moltorino_embed_set_bounds(
    _x: i32,
    _y: i32,
    _width: i32,
    _height: i32,
    _visible: bool,
) -> Result<(), String> {
    Ok(())
}

#[cfg(not(windows))]
#[tauri::command]
pub async fn moltorino_embed_set_channel(_channel: String) -> Result<(), String> {
    Ok(())
}

#[cfg(not(windows))]
#[tauri::command]
pub async fn moltorino_embed_stop() -> Result<(), String> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        build_set_channel_json, clip_hole_to_webview, host_client_size, is_current_child,
        parse_created_window, should_refit_child, translate_client_point,
    };

    /// The real shape Moltorino emits: pretty-printed, string window-id, one
    /// trailing NUL byte.
    #[test]
    fn parses_real_pretty_payload_with_trailing_nul() {
        let mut payload =
            b"{\n    \"type\": \"created-window\",\n    \"window-id\": \"1904326\"\n}".to_vec();
        payload.push(0);
        assert_eq!(parse_created_window(&payload), Some(1904326));
    }

    #[test]
    fn parses_compact_payload_without_nul() {
        let payload = br#"{"type":"created-window","window-id":"42"}"#;
        assert_eq!(parse_created_window(payload), Some(42));
    }

    #[test]
    fn tolerates_multiple_trailing_nuls_and_whitespace() {
        let payload = b"{\"type\":\"created-window\",\"window-id\":\"7\"}  \0\0\0";
        assert_eq!(parse_created_window(payload), Some(7));
    }

    #[test]
    fn accepts_numeric_window_id_defensively() {
        let payload = br#"{"type":"created-window","window-id":99}"#;
        assert_eq!(parse_created_window(payload), Some(99));
    }

    #[test]
    fn rejects_wrong_type() {
        let payload = br#"{"type":"set-channel","window-id":"5"}"#;
        assert_eq!(parse_created_window(payload), None);
    }

    #[test]
    fn rejects_zero_and_missing_and_garbage() {
        assert_eq!(
            parse_created_window(br#"{"type":"created-window","window-id":"0"}"#),
            None
        );
        assert_eq!(parse_created_window(br#"{"type":"created-window"}"#), None);
        assert_eq!(parse_created_window(b"not json at all"), None);
        assert_eq!(parse_created_window(b""), None);
        assert_eq!(parse_created_window(b"\0\0\0"), None);
    }

    #[test]
    fn rejects_non_numeric_window_id_string() {
        let payload = br#"{"type":"created-window","window-id":"abc"}"#;
        assert_eq!(parse_created_window(payload), None);
    }

    #[test]
    fn set_channel_json_has_exact_shape_and_no_trailing_nul() {
        let bytes = build_set_channel_json("forsen");
        assert_eq!(bytes.last(), Some(&b'}'));
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["type"], "set-channel");
        assert_eq!(v["provider"], "twitch");
        assert_eq!(v["channel-name"], "forsen");
    }

    #[test]
    fn set_channel_json_escapes_the_channel() {
        // The caller validates logins, but the encoder must still never let a
        // value break out of the JSON string.
        let bytes = build_set_channel_json("a\"b\\c");
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["channel-name"], "a\"b\\c");
    }

    // --- Coordinate translation (the offscreen-embedding fix) ----------------

    #[test]
    fn translate_is_identity_when_source_is_root() {
        // The common single-window case: source == root, so MapWindowPoints
        // yields a (0,0) offset and every point passes through unchanged. This
        // pins that the fix never regresses the normal path.
        assert_eq!(translate_client_point(0, 0, (0, 0)), (0, 0));
        assert_eq!(translate_client_point(120, 480, (0, 0)), (120, 480));
    }

    #[test]
    fn translate_adds_a_positive_source_offset() {
        // Source client origin sits 8px right, 31px down inside the root client
        // area (a titlebar/border inset), so a webview-relative (100, 200) lands
        // at (108, 231) in root-client space.
        assert_eq!(translate_client_point(100, 200, (8, 31)), (108, 231));
    }

    #[test]
    fn translate_undoes_an_offscreen_source_origin() {
        // The field-report symptom: the source window's client origin reported
        // near -32000. MapWindowPoints(source -> root) then returns a large
        // POSITIVE offset (the source is that far *left* of the root), so adding
        // it pulls a webview-relative point back onto the visible root, rather
        // than the host landing at ~-32000 as it did before the fix.
        assert_eq!(
            translate_client_point(50, 60, (30474, 31959)),
            (30524, 32019)
        );
    }

    #[test]
    fn translate_saturates_instead_of_wrapping_on_extreme_input() {
        // Defense in depth: even a pathological offset can never wrap i32 and
        // silently produce a valid-looking-but-wrong coordinate.
        assert_eq!(
            translate_client_point(i32::MAX, i32::MAX, (1, 1)),
            (i32::MAX, i32::MAX)
        );
        assert_eq!(
            translate_client_point(i32::MIN, i32::MIN, (-1, -1)),
            (i32::MIN, i32::MIN)
        );
    }

    // --- WebView cutout geometry (clip_hole_to_webview) ----------------------

    #[test]
    fn clip_normal_right_side_hole_is_unchanged() {
        // The live-proven case: a 1920x1080 WebView with a 402x1040 panel docked
        // at the right edge (left=1518, top=40). Fully inside, so the returned
        // rect is exactly the hole as inclusive-left/exclusive-right bounds.
        let hole = (1518, 40, 402, 1040);
        assert_eq!(
            clip_hole_to_webview(hole, (1920, 1080)),
            Some((1518, 40, 1920, 1080))
        );
    }

    #[test]
    fn clip_partially_offscreen_hole_is_clamped() {
        // A panel that spills past the right and bottom edges is clamped to the
        // WebView bounds; the left/top stay put, the right/bottom snap to the
        // WebView size so we never punch outside the window.
        let hole = (1700, 900, 400, 400);
        assert_eq!(
            clip_hole_to_webview(hole, (1920, 1080)),
            Some((1700, 900, 1920, 1080))
        );
        // A hole starting left/above the origin is clamped up to (0,0).
        assert_eq!(
            clip_hole_to_webview((-50, -20, 300, 200), (1920, 1080)),
            Some((0, 0, 250, 180))
        );
    }

    #[test]
    fn clip_fully_outside_hole_is_none() {
        // Entirely right of the WebView.
        assert_eq!(
            clip_hole_to_webview((2000, 10, 100, 100), (1920, 1080)),
            None
        );
        // Entirely below the WebView.
        assert_eq!(
            clip_hole_to_webview((10, 2000, 100, 100), (1920, 1080)),
            None
        );
        // Flush against the right edge (left == width): nothing remains.
        assert_eq!(clip_hole_to_webview((1920, 0, 50, 50), (1920, 1080)), None);
    }

    #[test]
    fn clip_zero_sized_inputs_are_none() {
        // Zero/negative hole dimensions -> nothing to punch.
        assert_eq!(clip_hole_to_webview((10, 10, 0, 100), (1920, 1080)), None);
        assert_eq!(clip_hole_to_webview((10, 10, 100, 0), (1920, 1080)), None);
        assert_eq!(clip_hole_to_webview((10, 10, -5, 100), (1920, 1080)), None);
        // Zero/negative WebView dimensions -> no valid region.
        assert_eq!(clip_hole_to_webview((10, 10, 100, 100), (0, 1080)), None);
        assert_eq!(clip_hole_to_webview((10, 10, 100, 100), (1920, 0)), None);
    }

    #[test]
    fn clip_second_monitor_coords_after_conversion_are_webview_relative() {
        // On a second monitor the host's *screen* origin might be something like
        // (3018, 40), but the cutout is always computed in WebView-*relative*
        // client coordinates (MapWindowPoints host -> webview), so by the time a
        // hole reaches this helper it is already relative to the WebView's own
        // client area. A right-docked 402x1040 panel inside a maximized 1920x1080
        // WebView therefore clips identically no matter which monitor it's on.
        let webview_relative_hole = (1518, 40, 402, 1040);
        assert_eq!(
            clip_hole_to_webview(webview_relative_hole, (1920, 1080)),
            Some((1518, 40, 1920, 1080))
        );
        // And a hole whose relative origin lands past the WebView (e.g. a stale
        // pre-move tick that hasn't been re-measured yet) is rejected, not punched
        // at a wrong spot.
        assert_eq!(
            clip_hole_to_webview((5000, 40, 402, 1040), (1920, 1080)),
            None
        );
    }

    // --- Host client -> child size (the undersize-child fix) -----------------

    #[test]
    fn host_client_size_matches_the_live_bug_rect() {
        // The exact host client rect from the captured glitch: a 402x1040 host.
        // GetClientRect reports left/top as 0, so the extent is right/bottom.
        // The child must be sized to 402x1040 (it was stuck at Qt's 300x150).
        assert_eq!(host_client_size((0, 0, 402, 1040)), (402, 1040));
    }

    #[test]
    fn host_client_size_zero_host_is_safe() {
        // A zero-sized host (not yet laid out) collapses the child to (0,0)
        // rather than producing a failed/garbage SetWindowPos call.
        assert_eq!(host_client_size((0, 0, 0, 0)), (0, 0));
    }

    #[test]
    fn host_client_size_inverted_rect_clamps_to_zero() {
        // A malformed/inverted rectangle (right < left, bottom < top) must clamp
        // to zero, never yield a negative width/height that SetWindowPos would
        // interpret as garbage.
        assert_eq!(host_client_size((100, 200, 40, 60)), (0, 0));
        // Mixed: width valid, height inverted -> only the bad axis clamps.
        assert_eq!(host_client_size((0, 500, 402, 100)), (402, 0));
    }

    #[test]
    fn host_client_size_saturates_on_extreme_span() {
        // A pathological rect spanning the full i32 range can't wrap: saturating
        // subtraction floors it at i32::MAX rather than overflowing.
        assert_eq!(
            host_client_size((i32::MIN, i32::MIN, i32::MAX, i32::MAX)),
            (i32::MAX, i32::MAX)
        );
    }

    // --- Attached-child SetWindowPos flags -----------------------------------

    #[cfg(windows)]
    #[test]
    fn child_fit_flags_bypass_qt_minimum_size_negotiation() {
        use super::child_fit_flags;
        use windows::Win32::UI::WindowsAndMessaging::{
            SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOSENDCHANGING, SWP_NOZORDER,
        };

        let flags = child_fit_flags(SWP_FRAMECHANGED);
        assert_eq!(flags & SWP_NOZORDER, SWP_NOZORDER);
        assert_eq!(flags & SWP_NOACTIVATE, SWP_NOACTIVATE);
        assert_eq!(flags & SWP_NOSENDCHANGING, SWP_NOSENDCHANGING);
        assert_eq!(flags & SWP_FRAMECHANGED, SWP_FRAMECHANGED);
    }

    // --- WinEvent-driven refit decision (should_refit_child) -----------------

    #[test]
    fn should_refit_when_child_shrank_to_qt_default() {
        // The exact live-captured bug: host is the correct 402x1040, child snapped
        // back to Qt's 300x150 default. The sizes differ, so a refit is required.
        assert!(should_refit_child((402, 1040), (300, 150)));
    }

    #[test]
    fn should_not_refit_when_child_already_matches_host() {
        // The loop terminator: after we refit, our own SetWindowPos fires another
        // location-change; by then child == host, so this must return false and
        // stop the cycle.
        assert!(!should_refit_child((402, 1040), (402, 1040)));
    }

    #[test]
    fn should_not_refit_when_host_is_zero_sized() {
        // A hidden / not-yet-laid-out host reports (0,0). Fitting a child to
        // nothing is pointless churn, so never ask for a refit — even if the child
        // currently has some other size.
        assert!(!should_refit_child((0, 0), (300, 150)));
        assert!(!should_refit_child((0, 0), (0, 0)));
        // A single zero axis is still degenerate.
        assert!(!should_refit_child((402, 0), (300, 150)));
        assert!(!should_refit_child((0, 1040), (300, 150)));
    }

    #[test]
    fn should_refit_when_only_one_axis_differs() {
        // A partial mismatch (right width, wrong height, or vice-versa) still needs
        // correcting — the child must match the host on both axes.
        assert!(should_refit_child((402, 1040), (402, 150)));
        assert!(should_refit_child((402, 1040), (300, 1040)));
    }

    // --- WinEvent stale-child filtering (is_current_child) -------------------

    #[test]
    fn is_current_child_matches_the_attached_child() {
        // The common case: the event's HWND is exactly the child we're embedding.
        assert!(is_current_child(0x44f0c3c, Some(0x44f0c3c)));
    }

    #[test]
    fn is_current_child_rejects_a_stale_or_other_window() {
        // A different window on Moltorino's thread, or a stale event for a child
        // that was torn down before a Settings-close remount, must be ignored so we
        // never resize the wrong (or a freed) window.
        assert!(!is_current_child(0xdead, Some(0x44f0c3c)));
    }

    #[test]
    fn is_current_child_rejects_when_nothing_attached() {
        // Before any child attaches (or after teardown clears it), every event is
        // foreign — there is nothing to refit.
        assert!(!is_current_child(0x44f0c3c, None));
    }
}
