//! Read-only validation for StreamNook-managed chat runtimes.
//!
//! This module deliberately contains no network, extraction, installation, or
//! process-lifecycle code. It validates the installed Bluzyrino tree and the
//! contract for a possible future runtime feed; discovery remains dormant until
//! redistribution and feed details are resolved.

use semver::Version;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use url::Url;

const RUNTIME_MANIFEST_NAME: &str = "runtime-manifest.json";
const RUNTIME_ID: &str = "bluzyrino";
const RUNTIME_ENTRYPOINT: &str = "Bluzyrino.exe";
const RUNTIME_ARCHITECTURE: &str = "x86_64";
const RUNTIME_ARCHIVE_ROOT: &str = "chat-runtime";

const UPDATE_SCHEMA_VERSION: u64 = 1;
const UPDATE_CHANNEL: &str = "beta";
const UPDATE_PLATFORM: &str = "windows-x64";
const UPDATE_ARCHIVE_FORMAT: &str = "zip";
const UPDATE_HOST: &str = "github.com";
const UPDATE_OWNER: &str = "gxufy";
const UPDATE_REPOSITORY: &str = "StreamNook-Moltorino";
const UPDATE_TAG_PREFIX: &str = "runtime-v";

/// Local ceilings cannot be raised by a remote manifest.
const MAX_ARCHIVE_SIZE_BYTES: u64 = 512 * 1024 * 1024;
const MAX_EXPANDED_SIZE_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_RUNTIME_FILE_COUNT: u64 = 10_000;
const MAX_RELEASE_NOTES_CHARS: usize = 20_000;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct InstalledRuntimeManifest {
    runtime_id: String,
    version: String,
    entrypoint: String,
    architecture: String,
    #[serde(default)]
    generated_utc: Option<String>,
    archive_root: String,
    file_count: u64,
    total_size_bytes: u64,
    files: Vec<InstalledRuntimeFile>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct InstalledRuntimeFile {
    path: String,
    size: u64,
    sha256: String,
}

/// Metadata is trusted only after every manifest and filesystem check succeeds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ValidatedInstalledRuntime {
    pub(crate) version: String,
}

/// Validate `<runtime_root>/runtime-manifest.json` and the complete payload tree.
pub(crate) fn validate_installed_runtime(
    runtime_root: &Path,
) -> Result<ValidatedInstalledRuntime, String> {
    let root_metadata = fs::symlink_metadata(runtime_root)
        .map_err(|e| format!("Chat runtime folder is unavailable: {e}"))?;
    if root_metadata.file_type().is_symlink() || !root_metadata.is_dir() {
        return Err("Chat runtime root must be a regular directory".to_string());
    }

    let manifest_path = runtime_root.join(RUNTIME_MANIFEST_NAME);
    let manifest_metadata = fs::symlink_metadata(&manifest_path)
        .map_err(|e| format!("Runtime manifest is missing: {e}"))?;
    if manifest_metadata.file_type().is_symlink() || !manifest_metadata.is_file() {
        return Err("Runtime manifest must be a regular file".to_string());
    }
    let json = fs::read_to_string(&manifest_path)
        .map_err(|e| format!("Couldn't read runtime manifest: {e}"))?;
    let manifest: InstalledRuntimeManifest =
        serde_json::from_str(&json).map_err(|e| format!("Invalid runtime manifest: {e}"))?;
    validate_installed_manifest_identity(&manifest)?;

    let declared_count = u64::try_from(manifest.files.len())
        .map_err(|_| "Runtime manifest contains too many file entries".to_string())?;
    if manifest.file_count != declared_count {
        return Err(format!(
            "Runtime manifest file_count is {} but lists {} files",
            manifest.file_count, declared_count
        ));
    }
    if declared_count > MAX_RUNTIME_FILE_COUNT {
        return Err(format!(
            "Runtime manifest cannot list more than {MAX_RUNTIME_FILE_COUNT} files"
        ));
    }
    if manifest.total_size_bytes > MAX_EXPANDED_SIZE_BYTES {
        return Err(format!(
            "Runtime manifest total_size_bytes cannot exceed {MAX_EXPANDED_SIZE_BYTES}"
        ));
    }

    let mut listed_paths = HashSet::with_capacity(manifest.files.len());
    let mut listed_casefolded = HashMap::with_capacity(manifest.files.len());
    let mut declared_total = 0u64;
    for file in &manifest.files {
        validate_relative_manifest_path(&file.path)?;
        validate_sha256(&file.sha256, &format!("SHA-256 for {}", file.path))?;
        if !listed_paths.insert(file.path.clone()) {
            return Err(format!("Duplicate runtime manifest path: {}", file.path));
        }
        let folded = file.path.to_lowercase();
        if let Some(previous) = listed_casefolded.insert(folded, file.path.clone()) {
            return Err(format!(
                "Case-insensitive runtime manifest path collision: {previous} and {}",
                file.path
            ));
        }
        declared_total = declared_total
            .checked_add(file.size)
            .ok_or_else(|| "Runtime manifest file sizes overflow total_size_bytes".to_string())?;
    }
    if !listed_paths.contains(RUNTIME_ENTRYPOINT) {
        return Err(format!(
            "Runtime manifest must list its entrypoint: {RUNTIME_ENTRYPOINT}"
        ));
    }
    if manifest.total_size_bytes != declared_total {
        return Err(format!(
            "Runtime manifest total_size_bytes is {} but listed files total {}",
            manifest.total_size_bytes, declared_total
        ));
    }

    let disk_files = inventory_runtime_tree(runtime_root)?;
    if u64::try_from(disk_files.len()).unwrap_or(u64::MAX) != manifest.file_count {
        return Err(format!(
            "Runtime disk file count is {} but manifest declares {}",
            disk_files.len(),
            manifest.file_count
        ));
    }
    for disk_path in disk_files.keys() {
        if !listed_casefolded.contains_key(disk_path) {
            return Err(format!(
                "Unlisted runtime file found: {}",
                disk_files[disk_path]
            ));
        }
    }

    let mut actual_total = 0u64;
    for file in &manifest.files {
        let full_path = join_manifest_path(runtime_root, &file.path);
        let metadata = fs::symlink_metadata(&full_path)
            .map_err(|e| format!("Runtime file is missing ({}): {e}", file.path))?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(format!("Runtime file is not a regular file: {}", file.path));
        }
        if metadata.len() != file.size {
            return Err(format!(
                "Runtime file size mismatch for {} (expected {}, got {})",
                file.path,
                file.size,
                metadata.len()
            ));
        }
        let actual_sha = sha256_file(&full_path)
            .map_err(|e| format!("Couldn't hash runtime file {}: {e}", file.path))?;
        if !actual_sha.eq_ignore_ascii_case(&file.sha256) {
            return Err(format!("Runtime file SHA-256 mismatch for {}", file.path));
        }
        actual_total = actual_total
            .checked_add(metadata.len())
            .ok_or_else(|| "Runtime file sizes overflow total_size_bytes".to_string())?;
    }
    if actual_total != manifest.total_size_bytes {
        return Err(format!(
            "Runtime files total {actual_total} bytes but manifest declares {}",
            manifest.total_size_bytes
        ));
    }

    Ok(ValidatedInstalledRuntime {
        version: manifest.version,
    })
}

fn validate_installed_manifest_identity(manifest: &InstalledRuntimeManifest) -> Result<(), String> {
    if manifest.runtime_id != RUNTIME_ID {
        return Err(format!("runtime_id must be '{RUNTIME_ID}'"));
    }
    Version::parse(&manifest.version)
        .map_err(|e| format!("Runtime version is not valid SemVer: {e}"))?;
    if manifest.entrypoint != RUNTIME_ENTRYPOINT {
        return Err(format!("entrypoint must be '{RUNTIME_ENTRYPOINT}'"));
    }
    if manifest.architecture != RUNTIME_ARCHITECTURE {
        return Err(format!("architecture must be '{RUNTIME_ARCHITECTURE}'"));
    }
    if manifest.archive_root != RUNTIME_ARCHIVE_ROOT {
        return Err(format!("archive_root must be '{RUNTIME_ARCHIVE_ROOT}'"));
    }
    if manifest
        .generated_utc
        .as_deref()
        .is_some_and(|value| value.trim().is_empty())
    {
        return Err("generated_utc cannot be empty when present".to_string());
    }
    Ok(())
}

fn validate_relative_manifest_path(relative: &str) -> Result<(), String> {
    if relative.trim().is_empty() {
        return Err("Runtime manifest contains an empty path".to_string());
    }
    if relative.contains('\\') {
        return Err(format!(
            "Runtime manifest path must use forward slashes: {relative}"
        ));
    }
    if relative.contains(':') || relative.starts_with('/') || Path::new(relative).is_absolute() {
        return Err(format!(
            "Runtime manifest path is rooted or drive-qualified: {relative}"
        ));
    }
    if relative
        .split('/')
        .any(|segment| segment.is_empty() || segment == "." || segment == "..")
    {
        return Err(format!(
            "Runtime manifest path contains an unsafe segment: {relative}"
        ));
    }
    if relative == RUNTIME_MANIFEST_NAME {
        return Err("Runtime payload cannot list runtime-manifest.json".to_string());
    }
    Ok(())
}

fn join_manifest_path(root: &Path, relative: &str) -> PathBuf {
    relative
        .split('/')
        .fold(root.to_path_buf(), |path, segment| path.join(segment))
}

/// Return case-folded payload paths to display paths. The manifest itself is metadata.
fn inventory_runtime_tree(runtime_root: &Path) -> Result<HashMap<String, String>, String> {
    fn visit(
        root: &Path,
        directory: &Path,
        files: &mut HashMap<String, String>,
    ) -> Result<(), String> {
        let entries = fs::read_dir(directory).map_err(|e| {
            format!(
                "Couldn't inspect runtime folder {}: {e}",
                directory.display()
            )
        })?;
        for entry in entries {
            let entry =
                entry.map_err(|e| format!("Couldn't inspect runtime directory entry: {e}"))?;
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)
                .map_err(|e| format!("Couldn't inspect runtime path {}: {e}", path.display()))?;
            if metadata.file_type().is_symlink() {
                return Err(format!(
                    "Runtime tree contains a symbolic link: {}",
                    path.display()
                ));
            }
            if metadata.is_dir() {
                visit(root, &path, files)?;
            } else if metadata.is_file() {
                let relative = path
                    .strip_prefix(root)
                    .map_err(|_| "Runtime inventory path escaped its root".to_string())?
                    .to_string_lossy()
                    .replace('\\', "/");
                if relative == RUNTIME_MANIFEST_NAME {
                    continue;
                }
                let folded = relative.to_lowercase();
                if let Some(previous) = files.insert(folded, relative.clone()) {
                    return Err(format!(
                        "Case-insensitive runtime disk path collision: {previous} and {relative}"
                    ));
                }
            } else {
                return Err(format!(
                    "Runtime tree contains a special filesystem entry: {}",
                    path.display()
                ));
            }
        }
        Ok(())
    }

    let mut files = HashMap::new();
    visit(runtime_root, runtime_root, &mut files)?;
    Ok(files)
}

fn sha256_file(path: &Path) -> std::io::Result<String> {
    let mut reader = BufReader::new(File::open(path)?);
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn validate_sha256(value: &str, label: &str) -> Result<(), String> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(format!(
            "{label} must be a 64-character hexadecimal SHA-256"
        ));
    }
    Ok(())
}

/// Dormant contract for a possible future gxufy-hosted runtime feed.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RuntimeUpdateManifest {
    schema_version: u64,
    runtime_id: String,
    channel: String,
    version: String,
    platform: String,
    architecture: String,
    archive_url: String,
    archive_name: String,
    archive_sha256: String,
    archive_size_bytes: u64,
    archive_format: String,
    archive_root: String,
    runtime_manifest_sha256: String,
    entrypoint: String,
    minimum_streamnook_version: String,
    maximum_expanded_size_bytes: u64,
    maximum_file_count: u64,
    release_notes: String,
}

/// Parse and validate the dormant update contract against the running build.
#[allow(dead_code)]
pub(crate) fn parse_runtime_update_manifest(
    json: &str,
    installed_runtime_version: &str,
) -> Result<(), String> {
    parse_runtime_update_manifest_for_versions(
        json,
        installed_runtime_version,
        crate::build_identity::app_version(),
    )
}

fn parse_runtime_update_manifest_for_versions(
    json: &str,
    installed_runtime_version: &str,
    current_streamnook_version: &str,
) -> Result<(), String> {
    let manifest: RuntimeUpdateManifest =
        serde_json::from_str(json).map_err(|e| format!("Invalid runtime update manifest: {e}"))?;
    validate_runtime_update_manifest(
        &manifest,
        installed_runtime_version,
        current_streamnook_version,
    )
}

fn validate_runtime_update_manifest(
    manifest: &RuntimeUpdateManifest,
    installed_runtime_version: &str,
    current_streamnook_version: &str,
) -> Result<(), String> {
    if manifest.schema_version != UPDATE_SCHEMA_VERSION {
        return Err(format!(
            "Runtime update schema_version must be {UPDATE_SCHEMA_VERSION}"
        ));
    }
    for (actual, expected, field) in [
        (&manifest.runtime_id, RUNTIME_ID, "runtime_id"),
        (&manifest.channel, UPDATE_CHANNEL, "channel"),
        (&manifest.platform, UPDATE_PLATFORM, "platform"),
        (&manifest.architecture, RUNTIME_ARCHITECTURE, "architecture"),
        (
            &manifest.archive_format,
            UPDATE_ARCHIVE_FORMAT,
            "archive_format",
        ),
        (&manifest.archive_root, RUNTIME_ARCHIVE_ROOT, "archive_root"),
        (&manifest.entrypoint, RUNTIME_ENTRYPOINT, "entrypoint"),
    ] {
        if actual != expected {
            return Err(format!("Runtime update {field} must be '{expected}'"));
        }
    }

    let remote_version = Version::parse(&manifest.version)
        .map_err(|e| format!("Runtime update version is not valid SemVer: {e}"))?;
    let installed_version = Version::parse(installed_runtime_version)
        .map_err(|e| format!("Installed runtime version is not valid SemVer: {e}"))?;
    if remote_version <= installed_version {
        return Err(format!(
            "Runtime update version {remote_version} is not newer than installed {installed_version}"
        ));
    }

    let minimum_streamnook = Version::parse(&manifest.minimum_streamnook_version)
        .map_err(|e| format!("minimum_streamnook_version is not valid SemVer: {e}"))?;
    let current_streamnook = Version::parse(current_streamnook_version)
        .map_err(|e| format!("Current StreamNook version is not valid SemVer: {e}"))?;
    if current_streamnook < minimum_streamnook {
        return Err(format!(
            "Runtime update requires StreamNook {minimum_streamnook} or newer"
        ));
    }

    validate_sha256(&manifest.archive_sha256, "archive_sha256")?;
    validate_sha256(&manifest.runtime_manifest_sha256, "runtime_manifest_sha256")?;
    validate_update_resource_limits(manifest)?;
    validate_archive_name(&manifest.archive_name)?;
    validate_runtime_archive_url(
        &manifest.archive_url,
        &manifest.archive_name,
        &manifest.version,
    )?;

    let notes_len = manifest.release_notes.chars().count();
    if manifest.release_notes.trim().is_empty() || notes_len > MAX_RELEASE_NOTES_CHARS {
        return Err(format!(
            "release_notes must contain 1 to {MAX_RELEASE_NOTES_CHARS} characters"
        ));
    }
    Ok(())
}

fn validate_update_resource_limits(manifest: &RuntimeUpdateManifest) -> Result<(), String> {
    if manifest.archive_size_bytes == 0 || manifest.archive_size_bytes > MAX_ARCHIVE_SIZE_BYTES {
        return Err(format!(
            "archive_size_bytes must be between 1 and {MAX_ARCHIVE_SIZE_BYTES}"
        ));
    }
    if manifest.maximum_expanded_size_bytes == 0
        || manifest.maximum_expanded_size_bytes > MAX_EXPANDED_SIZE_BYTES
    {
        return Err(format!(
            "maximum_expanded_size_bytes must be between 1 and {MAX_EXPANDED_SIZE_BYTES}"
        ));
    }
    if manifest.maximum_file_count == 0 || manifest.maximum_file_count > MAX_RUNTIME_FILE_COUNT {
        return Err(format!(
            "maximum_file_count must be between 1 and {MAX_RUNTIME_FILE_COUNT}"
        ));
    }
    if manifest.archive_size_bytes > manifest.maximum_expanded_size_bytes {
        return Err("archive_size_bytes cannot exceed maximum_expanded_size_bytes".to_string());
    }
    Ok(())
}

fn validate_archive_name(name: &str) -> Result<(), String> {
    if name.trim().is_empty()
        || name != name.trim()
        || name.contains('/')
        || name.contains('\\')
        || name.contains(':')
        || name == "."
        || name == ".."
        || !name.to_ascii_lowercase().ends_with(".zip")
    {
        return Err("archive_name must be a plain ZIP file name".to_string());
    }
    Ok(())
}

fn validate_runtime_archive_url(
    archive_url: &str,
    archive_name: &str,
    version: &str,
) -> Result<(), String> {
    let url = Url::parse(archive_url).map_err(|e| format!("Invalid runtime archive_url: {e}"))?;
    if url.scheme() != "https"
        || url.host_str() != Some(UPDATE_HOST)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("Runtime archive_url must use the allowed gxufy HTTPS namespace".to_string());
    }
    let segments = url
        .path_segments()
        .map(|segments| segments.collect::<Vec<_>>())
        .unwrap_or_default();
    let expected_tag = format!("{UPDATE_TAG_PREFIX}{version}");
    let allowed = segments.len() == 6
        && segments[0].eq_ignore_ascii_case(UPDATE_OWNER)
        && segments[1].eq_ignore_ascii_case(UPDATE_REPOSITORY)
        && segments[2] == "releases"
        && segments[3] == "download"
        && segments[4] == expected_tag
        && segments[5] == archive_name;
    if !allowed {
        return Err(
            "Runtime archive_url is outside the dormant gxufy runtime release contract".to_string(),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    use std::sync::atomic::{AtomicU32, Ordering};

    struct TempRuntime {
        root: PathBuf,
    }

    impl TempRuntime {
        fn new(tag: &str) -> Self {
            static SEQUENCE: AtomicU32 = AtomicU32::new(0);
            let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir().join(format!(
                "streamnook_runtime_manifest_{}_{}_{}",
                std::process::id(),
                tag,
                sequence
            ));
            fs::create_dir_all(&root).expect("create temporary runtime");
            Self { root }
        }

        fn write(&self, relative: &str, bytes: &[u8]) {
            let path = join_manifest_path(&self.root, relative);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).expect("create runtime parent");
            }
            fs::write(path, bytes).expect("write runtime file");
        }

        fn create_valid_manifest(&self) -> Value {
            let payloads = [
                (RUNTIME_ENTRYPOINT, b"bluzyrino".as_slice()),
                ("platforms/qwindows.dll", b"qt-platform".as_slice()),
            ];
            let files = payloads
                .iter()
                .map(|(path, bytes)| {
                    self.write(path, bytes);
                    json!({
                        "path": path,
                        "size": bytes.len(),
                        "sha256": format!("{:x}", Sha256::digest(bytes)),
                    })
                })
                .collect::<Vec<_>>();
            let total_size = payloads
                .iter()
                .map(|(_, bytes)| bytes.len() as u64)
                .sum::<u64>();
            let manifest = json!({
                "runtime_id": RUNTIME_ID,
                "version": "2.0.3",
                "entrypoint": RUNTIME_ENTRYPOINT,
                "architecture": RUNTIME_ARCHITECTURE,
                "generated_utc": "2026-08-11T03:20:03Z",
                "archive_root": RUNTIME_ARCHIVE_ROOT,
                "file_count": files.len(),
                "total_size_bytes": total_size,
                "files": files,
            });
            self.write_manifest(&manifest);
            manifest
        }

        fn write_manifest(&self, manifest: &Value) {
            fs::write(
                self.root.join(RUNTIME_MANIFEST_NAME),
                serde_json::to_vec_pretty(manifest).expect("serialize runtime manifest"),
            )
            .expect("write runtime manifest");
        }
    }

    impl Drop for TempRuntime {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn valid_update_manifest() -> Value {
        json!({
            "schema_version": UPDATE_SCHEMA_VERSION,
            "runtime_id": RUNTIME_ID,
            "channel": UPDATE_CHANNEL,
            "version": "2.1.0",
            "platform": UPDATE_PLATFORM,
            "architecture": RUNTIME_ARCHITECTURE,
            "archive_url": "https://github.com/gxufy/StreamNook-Bluzyrino/releases/download/runtime-v2.1.0/Bluzyrino-2.1.0-windows-x64.zip",
            "archive_name": "Bluzyrino-2.1.0-windows-x64.zip",
            "archive_sha256": "a".repeat(64),
            "archive_size_bytes": 64 * 1024 * 1024,
            "archive_format": UPDATE_ARCHIVE_FORMAT,
            "archive_root": RUNTIME_ARCHIVE_ROOT,
            "runtime_manifest_sha256": "b".repeat(64),
            "entrypoint": RUNTIME_ENTRYPOINT,
            "minimum_streamnook_version": "8.3.0",
            "maximum_expanded_size_bytes": 256 * 1024 * 1024,
            "maximum_file_count": 500,
            "release_notes": "Runtime update validation fixture",
        })
    }

    fn parse_update(value: &Value, installed: &str, streamnook: &str) -> Result<(), String> {
        parse_runtime_update_manifest_for_versions(
            &serde_json::to_string(value).expect("serialize update manifest"),
            installed,
            streamnook,
        )
    }

    #[test]
    fn valid_bundled_manifest_validates_complete_tree() {
        let runtime = TempRuntime::new("valid");
        runtime.create_valid_manifest();
        let validated = validate_installed_runtime(&runtime.root).unwrap();
        assert_eq!(validated.version, "2.0.3");
    }

    #[test]
    fn missing_manifest_is_rejected() {
        let runtime = TempRuntime::new("missing_manifest");
        runtime.write(RUNTIME_ENTRYPOINT, b"bluzyrino");
        assert!(validate_installed_runtime(&runtime.root).is_err());
    }

    #[test]
    fn corrupt_hash_is_rejected() {
        let runtime = TempRuntime::new("corrupt_hash");
        let mut manifest = runtime.create_valid_manifest();
        manifest["files"][0]["sha256"] = Value::String("0".repeat(64));
        runtime.write_manifest(&manifest);
        assert!(validate_installed_runtime(&runtime.root).is_err());
    }

    #[test]
    fn missing_listed_file_is_rejected() {
        let runtime = TempRuntime::new("missing_file");
        runtime.create_valid_manifest();
        fs::remove_file(runtime.root.join("platforms/qwindows.dll")).unwrap();
        assert!(validate_installed_runtime(&runtime.root).is_err());
    }

    #[test]
    fn extra_unlisted_file_is_rejected() {
        let runtime = TempRuntime::new("extra_file");
        runtime.create_valid_manifest();
        runtime.write("unexpected.dll", b"not listed");
        assert!(validate_installed_runtime(&runtime.root).is_err());
    }

    #[test]
    fn duplicate_and_case_colliding_manifest_paths_are_rejected() {
        for (tag, second_path) in [
            ("duplicate", RUNTIME_ENTRYPOINT),
            ("case_collision", "bluzyrino.EXE"),
        ] {
            let runtime = TempRuntime::new(tag);
            let mut manifest = runtime.create_valid_manifest();
            let duplicate = manifest["files"][0].clone();
            let size = duplicate["size"].as_u64().unwrap();
            let mut second = duplicate;
            second["path"] = Value::String(second_path.to_string());
            manifest["files"].as_array_mut().unwrap().push(second);
            manifest["file_count"] = json!(3);
            manifest["total_size_bytes"] =
                json!(manifest["total_size_bytes"].as_u64().unwrap() + size);
            runtime.write_manifest(&manifest);
            assert!(
                validate_installed_runtime(&runtime.root).is_err(),
                "accepted {tag}"
            );
        }
    }

    #[test]
    fn unsafe_manifest_paths_are_rejected() {
        for (tag, unsafe_path) in [
            ("parent", "../Bluzyrino.exe"),
            ("dot", "./Bluzyrino.exe"),
            ("rooted", "/Bluzyrino.exe"),
            ("drive", "C:/Bluzyrino.exe"),
            ("backslash", "platforms\\qwindows.dll"),
            ("empty_segment", "platforms//qwindows.dll"),
            ("metadata", RUNTIME_MANIFEST_NAME),
        ] {
            let runtime = TempRuntime::new(tag);
            let mut manifest = runtime.create_valid_manifest();
            manifest["files"][0]["path"] = Value::String(unsafe_path.to_string());
            runtime.write_manifest(&manifest);
            assert!(
                validate_installed_runtime(&runtime.root).is_err(),
                "accepted unsafe path {unsafe_path}"
            );
        }
    }

    #[test]
    fn installed_manifest_metadata_fails_closed() {
        let cases = [
            ("count", "/file_count", json!(99)),
            (
                "count_ceiling",
                "/file_count",
                json!(MAX_RUNTIME_FILE_COUNT + 1),
            ),
            ("total", "/total_size_bytes", json!(99)),
            (
                "total_ceiling",
                "/total_size_bytes",
                json!(MAX_EXPANDED_SIZE_BYTES + 1),
            ),
            ("identity", "/runtime_id", json!("other")),
            ("version", "/version", json!("not-semver")),
            ("entrypoint", "/entrypoint", json!("Other.exe")),
            ("architecture", "/architecture", json!("arm64")),
            ("root", "/archive_root", json!("other-root")),
            ("sha", "/files/0/sha256", json!("not-a-hash")),
        ];
        for (tag, pointer, replacement) in cases {
            let runtime = TempRuntime::new(tag);
            let mut manifest = runtime.create_valid_manifest();
            *manifest.pointer_mut(pointer).unwrap() = replacement;
            runtime.write_manifest(&manifest);
            assert!(
                validate_installed_runtime(&runtime.root).is_err(),
                "accepted {tag}"
            );
        }
    }

    #[test]
    fn installed_manifest_must_list_bluzyrino_entrypoint() {
        let runtime = TempRuntime::new("unlisted_entrypoint");
        let mut manifest = runtime.create_valid_manifest();
        manifest["files"][0]["path"] = json!("Bluzyrino-copy.exe");
        runtime.write("Bluzyrino-copy.exe", b"bluzyrino");
        fs::remove_file(runtime.root.join(RUNTIME_ENTRYPOINT)).unwrap();
        runtime.write_manifest(&manifest);
        assert!(validate_installed_runtime(&runtime.root).is_err());
    }

    #[test]
    fn strict_future_update_manifest_accepts_valid_contract() {
        assert!(parse_update(&valid_update_manifest(), "2.0.3", "8.3.12").is_ok());
    }

    #[test]
    fn future_update_manifest_rejects_missing_unknown_and_malformed_fields() {
        let mut missing = valid_update_manifest();
        missing.as_object_mut().unwrap().remove("archive_sha256");
        let mut unknown = valid_update_manifest();
        unknown["unexpected"] = json!(true);
        assert!(parse_update(&missing, "2.0.3", "8.3.12").is_err());
        assert!(parse_update(&unknown, "2.0.3", "8.3.12").is_err());
        assert!(parse_runtime_update_manifest_for_versions("not json", "2.0.3", "8.3.12").is_err());
    }

    #[test]
    fn future_update_manifest_rejects_wrong_fixed_contract_values() {
        let cases = [
            ("/schema_version", json!(2)),
            ("/runtime_id", json!("other")),
            ("/channel", json!("stable")),
            ("/platform", json!("linux-x64")),
            ("/architecture", json!("arm64")),
            ("/archive_format", json!("7z")),
            ("/archive_root", json!("other")),
            ("/entrypoint", json!("Other.exe")),
        ];
        for (pointer, replacement) in cases {
            let mut manifest = valid_update_manifest();
            *manifest.pointer_mut(pointer).unwrap() = replacement;
            assert!(parse_update(&manifest, "2.0.3", "8.3.12").is_err());
        }
    }

    #[test]
    fn future_update_manifest_rejects_invalid_hashes_and_versions() {
        for (pointer, replacement) in [
            ("/archive_sha256", json!("bad")),
            ("/runtime_manifest_sha256", json!("g".repeat(64))),
            ("/version", json!("not-semver")),
            ("/minimum_streamnook_version", json!("not-semver")),
        ] {
            let mut manifest = valid_update_manifest();
            *manifest.pointer_mut(pointer).unwrap() = replacement;
            assert!(parse_update(&manifest, "2.0.3", "8.3.12").is_err());
        }
        assert!(parse_update(&valid_update_manifest(), "not-semver", "8.3.12").is_err());
        assert!(parse_update(&valid_update_manifest(), "2.0.3", "not-semver").is_err());
    }

    #[test]
    fn future_update_manifest_rejects_equal_version_and_downgrade() {
        assert!(parse_update(&valid_update_manifest(), "2.1.0", "8.3.12").is_err());
        assert!(parse_update(&valid_update_manifest(), "2.2.0", "8.3.12").is_err());
    }

    #[test]
    fn minimum_streamnook_version_is_enforced_at_semver_boundary() {
        let manifest = valid_update_manifest();
        assert!(parse_update(&manifest, "2.0.3", "8.3.0").is_ok());
        assert!(parse_update(&manifest, "2.0.3", "8.2.99").is_err());
    }

    #[test]
    fn dormant_parser_compares_the_actual_running_streamnook_version() {
        let mut manifest = valid_update_manifest();
        manifest["minimum_streamnook_version"] = json!(crate::build_identity::app_version());
        let json = serde_json::to_string(&manifest).unwrap();
        assert!(parse_runtime_update_manifest(&json, "2.0.3").is_ok());

        manifest["minimum_streamnook_version"] = json!("9999.0.0");
        let json = serde_json::to_string(&manifest).unwrap();
        assert!(parse_runtime_update_manifest(&json, "2.0.3").is_err());
    }

    #[test]
    fn future_update_resource_limits_are_nonzero_consistent_and_locally_capped() {
        for (pointer, replacement) in [
            ("/archive_size_bytes", json!(0)),
            ("/archive_size_bytes", json!(MAX_ARCHIVE_SIZE_BYTES + 1)),
            ("/maximum_expanded_size_bytes", json!(0)),
            (
                "/maximum_expanded_size_bytes",
                json!(MAX_EXPANDED_SIZE_BYTES + 1),
            ),
            ("/maximum_file_count", json!(0)),
            ("/maximum_file_count", json!(MAX_RUNTIME_FILE_COUNT + 1)),
        ] {
            let mut manifest = valid_update_manifest();
            *manifest.pointer_mut(pointer).unwrap() = replacement;
            assert!(parse_update(&manifest, "2.0.3", "8.3.12").is_err());
        }
        let mut inconsistent = valid_update_manifest();
        inconsistent["archive_size_bytes"] = json!(1024);
        inconsistent["maximum_expanded_size_bytes"] = json!(512);
        assert!(parse_update(&inconsistent, "2.0.3", "8.3.12").is_err());
    }

    #[test]
    fn future_update_url_is_exact_https_gxufy_contract() {
        for rejected in [
            "http://github.com/gxufy/StreamNook-Bluzyrino/releases/download/runtime-v2.1.0/Bluzyrino-2.1.0-windows-x64.zip",
            "https://github.com/other/StreamNook-Moltorino/releases/download/runtime-v2.1.0/Bluzyrino-2.1.0-windows-x64.zip",
            "https://github.com/gxufy/Other/releases/download/runtime-v2.1.0/Bluzyrino-2.1.0-windows-x64.zip",
            "https://github.com/gxufy/StreamNook-Bluzyrino/releases/latest/download/Bluzyrino-2.1.0-windows-x64.zip",
            "https://github.com/gxufy/StreamNook-Bluzyrino/releases/download/runtime-v2.2.0/Bluzyrino-2.1.0-windows-x64.zip",
            "https://github.com/gxufy/StreamNook-Bluzyrino/releases/download/runtime-v2.1.0/Other.zip",
            "https://github.com/gxufy/StreamNook-Bluzyrino/releases/download/runtime-v2.1.0/Bluzyrino-2.1.0-windows-x64.zip?download=1",
            "https://user@github.com/gxufy/StreamNook-Bluzyrino/releases/download/runtime-v2.1.0/Bluzyrino-2.1.0-windows-x64.zip",
        ] {
            let mut manifest = valid_update_manifest();
            manifest["archive_url"] = Value::String(rejected.to_string());
            assert!(parse_update(&manifest, "2.0.3", "8.3.12").is_err(), "accepted {rejected}");
        }
    }

    #[test]
    fn future_update_archive_name_and_release_notes_are_bounded() {
        for name in ["", "../runtime.zip", "folder/runtime.zip", "runtime.7z"] {
            let mut manifest = valid_update_manifest();
            manifest["archive_name"] = Value::String(name.to_string());
            assert!(parse_update(&manifest, "2.0.3", "8.3.12").is_err());
        }
        let mut empty_notes = valid_update_manifest();
        empty_notes["release_notes"] = json!("  ");
        assert!(parse_update(&empty_notes, "2.0.3", "8.3.12").is_err());
    }
}
