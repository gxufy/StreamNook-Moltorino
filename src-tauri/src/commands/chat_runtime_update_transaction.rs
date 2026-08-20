//! Shared transaction primitives for bundled chat-runtime updates.
//!
//! The install transaction itself belongs to Phase 4B1-B. Phase 4B1-A keeps the
//! Windows path-key primitive here because validation and the future transaction
//! must use exactly the same filesystem comparison semantics.

use super::{
    sha256_file, validate_installed_runtime, RuntimeUpdateManifest, MAX_ARCHIVE_SIZE_BYTES,
    MAX_EXPANDED_SIZE_BYTES, MAX_RUNTIME_FILE_COUNT, RUNTIME_ARCHIVE_ROOT, RUNTIME_MANIFEST_NAME,
};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PreparedRuntimeCandidate {
    pub(super) transaction_id: String,
    pub(super) workspace: PathBuf,
    pub(super) staged_runtime: PathBuf,
    pub(super) candidate_runtime: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EntryKind {
    File,
    Directory,
}

#[derive(Debug, Clone)]
struct EntryPlan {
    index: usize,
    archive_path: String,
    relative_path: PathBuf,
    path_key: String,
    kind: EntryKind,
    declared_size: u64,
}

#[derive(Debug)]
struct CentralEntry {
    name: Vec<u8>,
    system: u8,
    external_attributes: u32,
}

/// Produce a stable key with Windows' invariant Unicode lowercase mapping.
///
/// Runtime archives use `/` separators before reaching this helper. Mapping UTF-16
/// through Windows avoids Rust's platform-independent Unicode casing rules when we
/// are detecting names that collide on the target filesystem.
#[cfg(windows)]
pub(super) fn windows_path_key(path: &str) -> Result<String, String> {
    use windows::Win32::Foundation::LPARAM;
    use windows::Win32::Globalization::{LCMapStringEx, LCMAP_LOWERCASE, LOCALE_NAME_INVARIANT};

    let source: Vec<u16> = path.encode_utf16().collect();
    let required = unsafe {
        LCMapStringEx(
            LOCALE_NAME_INVARIANT,
            LCMAP_LOWERCASE,
            &source,
            None,
            None,
            None,
            LPARAM(0),
        )
    };
    if required == 0 {
        return Err(format!(
            "Couldn't case-fold runtime path for Windows comparison: {}",
            std::io::Error::last_os_error()
        ));
    }

    let mut mapped = vec![0u16; required as usize];
    let written = unsafe {
        LCMapStringEx(
            LOCALE_NAME_INVARIANT,
            LCMAP_LOWERCASE,
            &source,
            Some(&mut mapped),
            None,
            None,
            LPARAM(0),
        )
    };
    if written == 0 {
        return Err(format!(
            "Couldn't case-fold runtime path for Windows comparison: {}",
            std::io::Error::last_os_error()
        ));
    }
    mapped.truncate(written as usize);
    String::from_utf16(&mapped)
        .map_err(|error| format!("Windows returned an invalid runtime path key: {error}"))
}

#[cfg(not(windows))]
pub(super) fn windows_path_key(path: &str) -> Result<String, String> {
    Ok(path.to_lowercase())
}

#[cfg(test)]
mod tests {
    use super::windows_path_key;

    #[test]
    fn windows_path_keys_are_case_insensitive_and_separator_preserving() {
        assert_eq!(
            windows_path_key("Platforms/QWindows.DLL").unwrap(),
            windows_path_key("platforms/qwindows.dll").unwrap()
        );
        assert_ne!(
            windows_path_key("platforms/qwindows.dll").unwrap(),
            windows_path_key("platforms\\qwindows.dll").unwrap()
        );
    }
}
