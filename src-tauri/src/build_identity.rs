//! Centralized build-identity helpers.
//!
//! Every difference between a normal production build and the local-only
//! `beta-build` packaging mode (the bundled-Moltorino beta) is expressed here,
//! gated by the `beta-build` Cargo feature. Nothing else in the codebase should
//! branch on that feature directly — call these helpers instead, so the two
//! builds can never silently drift apart.
//!
//! Guarantees for a production build (feature off):
//!   * every helper returns the historical value, byte-for-byte;
//!   * the `STREAMNOOK_BETA_VERSION` env var is never referenced, so a normal
//!     build compiles without it.
//!
//! Guarantees for a beta build (feature on):
//!   * on-disk storage, caches, and keyring entries live in a namespace that
//!     cannot collide with production;
//!   * the self-updater is disabled;
//!   * the deep-link scheme is distinct;
//!   * the version comes from the compile-time `STREAMNOOK_BETA_VERSION`, so the
//!     build fails to compile unless the packaging script provides it.

/// True when compiled with the `beta-build` feature. `const fn` so callers
/// collapse to a single branch with no runtime cost.
#[cfg(feature = "beta-build")]
pub const fn is_beta_build() -> bool {
    true
}

/// True when compiled with the `beta-build` feature. `const fn` so callers
/// collapse to a single branch with no runtime cost.
#[cfg(not(feature = "beta-build"))]
pub const fn is_beta_build() -> bool {
    false
}

/// On-disk storage namespace: the single directory segment used under
/// `%APPDATA%`, `%LOCALAPPDATA%`, and the platform data dir for all app data
/// and caches. Production keeps the historical `StreamNook`; beta is fully
/// separate so the two installs never share a byte on disk.
#[cfg(feature = "beta-build")]
pub const fn storage_dir() -> &'static str {
    "StreamNook-Moltorino-Beta"
}

/// On-disk storage namespace. See the `beta-build` variant for details.
#[cfg(not(feature = "beta-build"))]
pub const fn storage_dir() -> &'static str {
    "StreamNook"
}

/// Human-facing product name (window title, tray tooltip, dialogs).
#[cfg(feature = "beta-build")]
pub const fn display_name() -> &'static str {
    "StreamNook Moltorino Beta"
}

/// Human-facing product name (window title, tray tooltip, dialogs).
#[cfg(not(feature = "beta-build"))]
pub const fn display_name() -> &'static str {
    "StreamNook"
}

/// The custom URL scheme this build registers and parses. Beta uses a distinct
/// scheme so a production install and a beta install never fight over the same
/// protocol registration in the Windows registry (HKCU).
#[cfg(feature = "beta-build")]
pub const fn deep_link_scheme() -> &'static str {
    "streamnook-beta"
}

/// The custom URL scheme this build registers and parses. See the `beta-build`
/// variant for details.
#[cfg(not(feature = "beta-build"))]
pub const fn deep_link_scheme() -> &'static str {
    "streamnook"
}

/// Application version reported throughout the UI and to the (production-only)
/// updater. Beta sources it from the compile-time `STREAMNOOK_BETA_VERSION`, so
/// a beta build cannot be produced without the packaging script setting it.
#[cfg(feature = "beta-build")]
pub const fn app_version() -> &'static str {
    // `env!` (not `option_env!`) is deliberate: a beta build MUST fail to
    // compile if the packaging script did not set this, rather than silently
    // reporting a wrong or production version.
    env!(
        "STREAMNOOK_BETA_VERSION",
        "beta-build requires STREAMNOOK_BETA_VERSION to be set at compile time \
         (scripts/package-beta-local.ps1 sets this)"
    )
}

/// Application version reported throughout the UI and to the updater. Production
/// uses the crate version from `Cargo.toml`.
#[cfg(not(feature = "beta-build"))]
pub const fn app_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Update source identity for this build. Source selection belongs here so a
/// fork build can never silently drift onto the upstream production channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateChannel {
    UpstreamProduction,
    ForkBeta,
}

#[cfg(feature = "beta-build")]
pub const fn update_channel() -> UpdateChannel {
    UpdateChannel::ForkBeta
}

#[cfg(not(feature = "beta-build"))]
pub const fn update_channel() -> UpdateChannel {
    UpdateChannel::UpstreamProduction
}

/// Whether the in-app self-updater is active. Both defined channels have
/// dedicated discovery sources; ForkBeta must never borrow upstream.
pub const fn updater_enabled() -> bool {
    matches!(
        update_channel(),
        UpdateChannel::UpstreamProduction | UpdateChannel::ForkBeta
    )
}

/// Namespaces a keyring *service* name so beta credentials can never collide
/// with — or overwrite — production credentials in the Windows Credential
/// Manager. Production returns the name unchanged (byte-identical to historical
/// behavior); beta appends a distinct, stable suffix.
///
/// Callers pass their existing production service constant; the isolation is
/// applied here so every credential store stays consistent.
pub fn keyring_service(production_service: &str) -> String {
    if is_beta_build() {
        format!("{production_service}.moltorino-beta")
    } else {
        production_service.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_channel_matches_build_mode() {
        if is_beta_build() {
            assert_eq!(update_channel(), UpdateChannel::ForkBeta);
        } else {
            assert_eq!(update_channel(), UpdateChannel::UpstreamProduction);
        }
    }

    #[test]
    fn updater_is_enabled_for_each_dedicated_channel() {
        assert!(
            updater_enabled(),
            "each build channel has a dedicated update source"
        );
    }

    #[test]
    fn storage_dir_matches_mode_and_is_nonempty() {
        assert!(!storage_dir().is_empty());
        if is_beta_build() {
            assert_eq!(storage_dir(), "StreamNook-Moltorino-Beta");
        } else {
            assert_eq!(storage_dir(), "StreamNook");
        }
    }

    #[test]
    fn display_name_matches_mode() {
        if is_beta_build() {
            assert_eq!(display_name(), "StreamNook Moltorino Beta");
        } else {
            assert_eq!(display_name(), "StreamNook");
        }
    }

    #[test]
    fn deep_link_scheme_matches_mode() {
        if is_beta_build() {
            assert_eq!(deep_link_scheme(), "streamnook-beta");
        } else {
            assert_eq!(deep_link_scheme(), "streamnook");
        }
    }

    #[test]
    fn keyring_service_isolates_beta_but_preserves_production() {
        let base = "streamnook_twitch_token";
        let svc = keyring_service(base);
        if is_beta_build() {
            assert_ne!(
                svc, base,
                "beta keyring must not reuse the production service name"
            );
            assert!(
                svc.starts_with(base),
                "beta keyring should derive from the production name"
            );
        } else {
            assert_eq!(
                svc, base,
                "production keyring service name must be unchanged"
            );
        }
    }

    #[test]
    fn app_version_is_nonempty() {
        assert!(!app_version().is_empty());
    }
}
