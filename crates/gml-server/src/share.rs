//! Phase-5 share-UX helpers (`docs/MODS_PACKAGES_TZ.md` §"Фаза 5"):
//! zip a package directory for export, and safely extract an imported zip into
//! the library — plus the manifest inspection that decides whether an archive is
//! a WORLD or a STORY.
//!
//! HARD RULES (no fallbacks):
//! - A package directory that does not exist is never zipped into an empty
//!   archive — callers check existence first and 404.
//! - An archive whose top-level manifest is missing or whose `format` field is
//!   not a known tag is REJECTED; nothing is written to disk.
//! - Extraction guards against zip-slip: any entry whose normalized path escapes
//!   the destination (absolute path, `..`, drive/UNC prefix) aborts the whole
//!   import — no partial package is left behind.

use std::collections::BTreeMap;
use std::io::{Cursor, Read, Write};
use std::path::{Component, Path, PathBuf};

use serde_json::Value;
use zip::write::SimpleFileOptions;
use zip::{ZipArchive, ZipWriter};

/// `format` tag of a world manifest (`world.json`).
pub const WORLD_FORMAT: &str = "gmlab.world/1";
/// `format` tag of a story manifest (`story.json`).
pub const STORY_FORMAT: &str = "gmlab.story/1";
/// `format` tag of a character manifest (`character.json`).
pub const CHARACTER_FORMAT: &str = "gmlab.character/1";

/// An error in the share (export/import) flow. All variants are hard errors that
/// leave the filesystem unchanged.
#[derive(Debug)]
pub enum ShareError {
    /// An I/O error while reading a package dir or writing the extraction target.
    Io(String),
    /// The zip container itself was malformed / unreadable.
    Zip(String),
    /// The archive did not contain a recognized top-level manifest, or its
    /// `format` field was missing/unknown.
    Unrecognized(String),
    /// A zip entry tried to escape the destination directory (zip-slip).
    Traversal(String),
}

impl std::fmt::Display for ShareError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ShareError::Io(m) => write!(f, "io error: {m}"),
            ShareError::Zip(m) => write!(f, "bad zip archive: {m}"),
            ShareError::Unrecognized(m) => write!(f, "unrecognized package: {m}"),
            ShareError::Traversal(m) => write!(f, "unsafe archive path: {m}"),
        }
    }
}

impl std::error::Error for ShareError {}

/// What kind of package an imported archive carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageKind {
    World,
    Story,
    Character,
}

impl PackageKind {
    pub fn as_str(self) -> &'static str {
        match self {
            PackageKind::World => "world",
            PackageKind::Story => "story",
            PackageKind::Character => "character",
        }
    }
}

/// An in-memory archive: every file entry's path (forward-slash, dest-relative,
/// already validated as safe) -> its bytes. Directories are implicit.
pub struct Archive {
    entries: BTreeMap<String, Vec<u8>>,
}

/// Zip-bomb defenses for import. A crafted archive can declare a tiny
/// compressed size yet inflate to gigabytes; these caps bound the total work an
/// import may do. Exceeding any cap is a hard [`ShareError`] — nothing is
/// written to disk.
///
/// Per-entry uncompressed cap: a single file inside a package archive (a
/// manifest, a small image, a save) has no legitimate reason to exceed this.
pub const MAX_ENTRY_UNCOMPRESSED_BYTES: u64 = 32 * 1024 * 1024; // 32 MiB
/// Total uncompressed budget across all entries.
pub const MAX_TOTAL_UNCOMPRESSED_BYTES: u64 = 128 * 1024 * 1024; // 128 MiB
/// Maximum number of file entries an imported archive may contain.
pub const MAX_ENTRY_COUNT: usize = 4096;

impl Archive {
    /// Parse zip bytes into an [`Archive`], validating every entry path against
    /// zip-slip and enforcing zip-bomb caps (per-entry + total uncompressed
    /// budget, entry count). A malformed container, an unsafe entry path, or a
    /// cap breach is a hard error and nothing is retained.
    pub fn from_zip_bytes(bytes: &[u8]) -> Result<Self, ShareError> {
        let reader = Cursor::new(bytes);
        let mut zip =
            ZipArchive::new(reader).map_err(|e| ShareError::Zip(e.to_string()))?;
        if zip.len() > MAX_ENTRY_COUNT {
            return Err(ShareError::Zip(format!(
                "archive has too many entries ({} > {MAX_ENTRY_COUNT})",
                zip.len()
            )));
        }
        let mut entries: BTreeMap<String, Vec<u8>> = BTreeMap::new();
        let mut total: u64 = 0;
        for i in 0..zip.len() {
            let mut file = zip
                .by_index(i)
                .map_err(|e| ShareError::Zip(e.to_string()))?;
            // `enclosed_name` rejects absolute paths and `..` traversal; we
            // additionally normalize to forward slashes and re-validate.
            let raw_name = file.name().to_string();
            if file.is_dir() {
                continue;
            }
            let safe = safe_rel_path(&raw_name)?;
            // Read with a hard per-entry ceiling. We do NOT pre-size the Vec
            // from the attacker-declared `file.size()` (a zip bomb lies about
            // it); read through a capped reader and reject on overflow.
            let mut buf = Vec::new();
            let cap = MAX_ENTRY_UNCOMPRESSED_BYTES;
            let read = file
                .by_ref()
                .take(cap + 1)
                .read_to_end(&mut buf)
                .map_err(|e| ShareError::Io(e.to_string()))?;
            if read as u64 > cap {
                return Err(ShareError::Zip(format!(
                    "entry {safe} exceeds per-entry cap ({MAX_ENTRY_UNCOMPRESSED_BYTES} bytes)"
                )));
            }
            total = total.saturating_add(read as u64);
            if total > MAX_TOTAL_UNCOMPRESSED_BYTES {
                return Err(ShareError::Zip(format!(
                    "archive exceeds total uncompressed budget ({MAX_TOTAL_UNCOMPRESSED_BYTES} bytes)"
                )));
            }
            entries.insert(safe, buf);
        }
        Ok(Archive { entries })
    }

    /// Read a top-level entry's bytes, if present.
    fn top_level(&self, name: &str) -> Option<&[u8]> {
        self.entries.get(name).map(|v| v.as_slice())
    }

    /// Inspect the manifest to decide the package kind. Validates the `format`
    /// tag (`gmlab.world/1` / `gmlab.story/1`); a missing/unknown manifest is a
    /// hard [`ShareError::Unrecognized`].
    pub fn detect_kind(&self) -> Result<PackageKind, ShareError> {
        if let Some(bytes) = self.top_level("world.json") {
            check_format(bytes, WORLD_FORMAT)?;
            return Ok(PackageKind::World);
        }
        if let Some(bytes) = self.top_level("story.json") {
            check_format(bytes, STORY_FORMAT)?;
            return Ok(PackageKind::Story);
        }
        if let Some(bytes) = self.top_level("character.json") {
            check_format(bytes, CHARACTER_FORMAT)?;
            return Ok(PackageKind::Character);
        }
        Err(ShareError::Unrecognized(
            "archive has no top-level world.json, story.json, or character.json".to_string(),
        ))
    }

    /// The `id` declared in the top-level manifest, if any (used as a fallback
    /// hint; the destination folder name is the authoritative id).
    pub fn manifest_id(&self, kind: PackageKind) -> Option<String> {
        let name = match kind {
            PackageKind::World => "world.json",
            PackageKind::Story => "story.json",
            PackageKind::Character => "character.json",
        };
        let bytes = self.top_level(name)?;
        let value: Value = serde_json::from_slice(bytes).ok()?;
        value
            .get("id")
            .and_then(Value::as_str)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    /// Parse the top-level `character.json` manifest into a [`Value`] (K1
    /// import structural validation). Returns `None` when absent or unparsable.
    pub fn character_manifest(&self) -> Option<Value> {
        let bytes = self.top_level("character.json")?;
        serde_json::from_slice(bytes).ok()
    }

    /// All top-level entries (no `/` after the first segment is irrelevant — this
    /// returns the *first* path segment set). Used to detect a baked `world/`.
    pub fn has_baked_world(&self) -> bool {
        self.entries
            .keys()
            .any(|k| k.starts_with("world/") && k.ends_with("world.json") && k == "world/world.json")
    }

    /// Extract the subset of entries whose path begins with `prefix` (a directory
    /// path ending in `/`), stripping the prefix, into a new [`Archive`]. Returns
    /// an empty archive when nothing matches.
    pub fn subtree(&self, prefix: &str) -> Archive {
        let mut entries = BTreeMap::new();
        for (k, v) in &self.entries {
            if let Some(rest) = k.strip_prefix(prefix) {
                if !rest.is_empty() {
                    entries.insert(rest.to_string(), v.clone());
                }
            }
        }
        Archive { entries }
    }

    /// Write every entry (excluding those under `world/`, i.e. only the
    /// dest-relative top of the package) into `dest_dir`. Used to materialize the
    /// package itself; the baked `world/` subtree is imported separately via
    /// [`Self::subtree`]. Creates parent directories as needed.
    pub fn extract_excluding(&self, dest_dir: &Path, exclude_prefix: &str) -> Result<(), ShareError> {
        for (rel, bytes) in &self.entries {
            if rel.starts_with(exclude_prefix) {
                continue;
            }
            write_entry(dest_dir, rel, bytes)?;
        }
        Ok(())
    }

    /// Write every entry into `dest_dir` (full extraction). Creates parent
    /// directories as needed.
    pub fn extract_all(&self, dest_dir: &Path) -> Result<(), ShareError> {
        for (rel, bytes) in &self.entries {
            write_entry(dest_dir, rel, bytes)?;
        }
        Ok(())
    }

    /// Whether the archive carries any entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Write one dest-relative entry into `dest_dir`, re-validating the path.
fn write_entry(dest_dir: &Path, rel: &str, bytes: &[u8]) -> Result<(), ShareError> {
    let safe = safe_rel_path(rel)?;
    let target = dest_dir.join(&safe);
    // Defense in depth: the joined path must stay inside dest_dir.
    if !target.starts_with(dest_dir) {
        return Err(ShareError::Traversal(format!("entry escapes dest: {rel}")));
    }
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).map_err(|e| ShareError::Io(e.to_string()))?;
    }
    let mut f = std::fs::File::create(&target).map_err(|e| ShareError::Io(e.to_string()))?;
    f.write_all(bytes).map_err(|e| ShareError::Io(e.to_string()))?;
    Ok(())
}

/// Parse + check the `format` field of a manifest's bytes.
fn check_format(bytes: &[u8], expected: &str) -> Result<(), ShareError> {
    let value: Value = serde_json::from_slice(bytes)
        .map_err(|e| ShareError::Unrecognized(format!("manifest is not valid JSON: {e}")))?;
    let format = value.get("format").and_then(Value::as_str).unwrap_or("");
    if format != expected {
        return Err(ShareError::Unrecognized(format!(
            "manifest format {format:?} is not {expected:?}"
        )));
    }
    Ok(())
}

/// Normalize an archive entry path to a safe, forward-slash, destination-relative
/// path. Rejects absolute paths, drive/UNC prefixes, and any `..` component
/// (zip-slip). Returns the cleaned relative path.
fn safe_rel_path(name: &str) -> Result<String, ShareError> {
    let normalized = name.replace('\\', "/");
    if normalized.is_empty() {
        return Err(ShareError::Traversal("empty entry name".to_string()));
    }
    if normalized.starts_with('/') {
        return Err(ShareError::Traversal(format!("absolute path: {name}")));
    }
    let path = Path::new(&normalized);
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::Normal(c) => out.push(c),
            Component::CurDir => {}
            Component::ParentDir => {
                return Err(ShareError::Traversal(format!("'..' in path: {name}")))
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(ShareError::Traversal(format!(
                    "absolute/prefixed path: {name}"
                )))
            }
        }
    }
    let cleaned = out.to_string_lossy().replace('\\', "/");
    if cleaned.is_empty() {
        return Err(ShareError::Traversal(format!("path normalizes to empty: {name}")));
    }
    Ok(cleaned)
}

/// Recursively zip a package directory into in-memory bytes. Every file under
/// `dir` is stored at its path relative to `dir` (forward slashes), under the
/// optional `prefix` (e.g. `"world/"` to nest a baked world). Directories with no
/// files produce no entries (an empty package dir yields an empty zip, which the
/// caller has already ruled out via an existence check).
pub fn zip_dir(dir: &Path, prefix: &str) -> Result<Vec<u8>, ShareError> {
    let mut buf = Vec::new();
    {
        let cursor = Cursor::new(&mut buf);
        let mut zip = ZipWriter::new(cursor);
        let options = SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        add_dir_recursive(&mut zip, dir, dir, prefix, &options)?;
        zip.finish().map_err(|e| ShareError::Zip(e.to_string()))?;
    }
    Ok(buf)
}

/// Zip a story package together with a baked copy of its world under `world/`,
/// applying `mutate_story` to the story manifest bytes before storing them (the
/// caller flips `world_embedded=true`).
pub fn zip_story_with_world(
    story_dir: &Path,
    world_dir: &Path,
    story_manifest: &[u8],
) -> Result<Vec<u8>, ShareError> {
    let mut buf = Vec::new();
    {
        let cursor = Cursor::new(&mut buf);
        let mut zip = ZipWriter::new(cursor);
        let options = SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        // Story package: everything except story.json verbatim, then our mutated
        // story.json copy.
        add_dir_recursive_filtered(
            &mut zip,
            story_dir,
            story_dir,
            "",
            &options,
            &["story.json"],
        )?;
        zip.start_file("story.json", options)
            .map_err(|e| ShareError::Zip(e.to_string()))?;
        zip.write_all(story_manifest)
            .map_err(|e| ShareError::Io(e.to_string()))?;
        // Baked world under world/.
        add_dir_recursive(&mut zip, world_dir, world_dir, "world/", &options)?;
        zip.finish().map_err(|e| ShareError::Zip(e.to_string()))?;
    }
    Ok(buf)
}

fn add_dir_recursive<W: Write + std::io::Seek>(
    zip: &mut ZipWriter<W>,
    base: &Path,
    dir: &Path,
    prefix: &str,
    options: &SimpleFileOptions,
) -> Result<(), ShareError> {
    add_dir_recursive_filtered(zip, base, dir, prefix, options, &[])
}

/// Whether a bare file `name` (last path segment) must be excluded from an
/// export archive: atomic-write temp files (`.<...>.tmp`) and sqlite sidecars
/// (`*-wal`, `*-shm`, `*-journal`). The `rag.sqlite3` main DB is deliberately
/// NOT matched — it is the Phase-B warm-start layer and must ship.
///
/// The `-wal`/`-shm`/`-journal` suffix match is INTENTIONALLY broad (any such
/// name, not only `rag.sqlite3-*`): privacy trumps precision — we must never
/// ship sqlite sidecar texts, and since package contents are controlled the
/// odds of a legitimate asset ending in one of those suffixes are negligible.
fn is_export_excluded_name(name: &str) -> bool {
    (name.starts_with('.') && name.ends_with(".tmp"))
        || name.ends_with("-wal")
        || name.ends_with("-shm")
        || name.ends_with("-journal")
}

/// Recursively add files; any path (relative to `base`, forward-slash) listed in
/// `skip` at the TOP level is omitted.
fn add_dir_recursive_filtered<W: Write + std::io::Seek>(
    zip: &mut ZipWriter<W>,
    base: &Path,
    dir: &Path,
    prefix: &str,
    options: &SimpleFileOptions,
    skip: &[&str],
) -> Result<(), ShareError> {
    let entries = std::fs::read_dir(dir).map_err(|e| ShareError::Io(e.to_string()))?;
    for entry in entries {
        let entry = entry.map_err(|e| ShareError::Io(e.to_string()))?;
        let path = entry.path();
        let rel = path
            .strip_prefix(base)
            .map_err(|e| ShareError::Io(e.to_string()))?
            .to_string_lossy()
            .replace('\\', "/");
        // Skip atomic-write temp files (`.world.json.<tok>.tmp`) so a concurrent
        // save never leaks a partial file, AND sqlite sidecars (`*-wal`,
        // `*-shm`, `*-journal`) anywhere in the tree so a live/crashed RAG
        // cache never ships transient DB state (RAG_PER_WORLD_TZ §2.5). The
        // `rag.sqlite3` main file itself is NOT skipped — it is the future
        // Phase-B package warm-start layer and must ship.
        if rel
            .rsplit('/')
            .next()
            .map(is_export_excluded_name)
            .unwrap_or(false)
        {
            continue;
        }
        if path.is_dir() {
            add_dir_recursive_filtered(zip, base, &path, prefix, options, skip)?;
        } else {
            if skip.contains(&rel.as_str()) {
                continue;
            }
            let name = format!("{prefix}{rel}");
            zip.start_file(name, *options)
                .map_err(|e| ShareError::Zip(e.to_string()))?;
            let bytes = std::fs::read(&path).map_err(|e| ShareError::Io(e.to_string()))?;
            zip.write_all(&bytes).map_err(|e| ShareError::Io(e.to_string()))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn export_excludes_sqlite_sidecars_but_keeps_rag_db() {
        // Unit: the name predicate.
        assert!(is_export_excluded_name(".world.json.abc123.tmp"));
        assert!(is_export_excluded_name("rag.sqlite3-wal"));
        assert!(is_export_excluded_name("rag.sqlite3-shm"));
        assert!(is_export_excluded_name("rag.sqlite3-journal"));
        assert!(!is_export_excluded_name("rag.sqlite3")); // main DB must ship
        assert!(!is_export_excluded_name("world.json"));
        assert!(!is_export_excluded_name("cover-final.png")); // "-" mid-name is fine

        // Integration: sidecars anywhere in the tree are dropped by the walk,
        // while `rag.sqlite3` and normal files survive.
        let src = tempfile::tempdir().unwrap();
        std::fs::write(
            src.path().join("world.json"),
            br#"{"format":"gmlab.world/1","id":"w"}"#,
        )
        .unwrap();
        std::fs::write(src.path().join("rag.sqlite3"), b"MAINDB").unwrap();
        std::fs::write(src.path().join("rag.sqlite3-wal"), b"WAL").unwrap();
        std::fs::write(src.path().join("rag.sqlite3-shm"), b"SHM").unwrap();
        std::fs::write(src.path().join("rag.sqlite3-journal"), b"JRN").unwrap();
        std::fs::write(src.path().join(".world.json.tok.tmp"), b"PARTIAL").unwrap();
        // A sidecar in a subdirectory must also be skipped.
        std::fs::create_dir_all(src.path().join("nested")).unwrap();
        std::fs::write(src.path().join("nested").join("cache.sqlite3-wal"), b"WAL2").unwrap();
        std::fs::write(src.path().join("nested").join("keep.json"), b"{}").unwrap();

        let bytes = zip_dir(src.path(), "").unwrap();
        let arch = Archive::from_zip_bytes(&bytes).unwrap();
        let names: Vec<&str> = arch.entries.keys().map(|s| s.as_str()).collect();

        assert!(names.contains(&"world.json"), "{names:?}");
        assert!(names.contains(&"rag.sqlite3"), "rag.sqlite3 must ship: {names:?}");
        assert!(names.contains(&"nested/keep.json"), "{names:?}");
        for excluded in [
            "rag.sqlite3-wal",
            "rag.sqlite3-shm",
            "rag.sqlite3-journal",
            ".world.json.tok.tmp",
            "nested/cache.sqlite3-wal",
        ] {
            assert!(
                !names.contains(&excluded),
                "excluded name leaked into archive: {excluded} in {names:?}"
            );
        }
    }

    #[test]
    fn rejects_traversal_paths() {
        assert!(safe_rel_path("../etc/passwd").is_err());
        assert!(safe_rel_path("/etc/passwd").is_err());
        assert!(safe_rel_path("a/../../b").is_err());
        assert_eq!(safe_rel_path("a/b/c.json").unwrap(), "a/b/c.json");
        assert_eq!(safe_rel_path("a\\b\\c.json").unwrap(), "a/b/c.json");
        assert_eq!(safe_rel_path("./a.json").unwrap(), "a.json");
    }

    #[test]
    fn detect_kind_requires_known_format() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("world.json"), br#"{"format":"gmlab.world/1","id":"w"}"#)
            .unwrap();
        let bytes = zip_dir(dir.path(), "").unwrap();
        let arch = Archive::from_zip_bytes(&bytes).unwrap();
        assert_eq!(arch.detect_kind().unwrap(), PackageKind::World);

        let dir2 = tempfile::tempdir().unwrap();
        std::fs::write(dir2.path().join("world.json"), br#"{"format":"bogus/9"}"#).unwrap();
        let bytes2 = zip_dir(dir2.path(), "").unwrap();
        let arch2 = Archive::from_zip_bytes(&bytes2).unwrap();
        assert!(arch2.detect_kind().is_err());
    }

    #[test]
    fn detect_kind_recognizes_character() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("character.json"),
            r#"{"format":"gmlab.character/1","id":"c","payload":{"player_character":{"name":"hero"}}}"#.as_bytes(),
        )
        .unwrap();
        let bytes = zip_dir(dir.path(), "").unwrap();
        let arch = Archive::from_zip_bytes(&bytes).unwrap();
        assert_eq!(arch.detect_kind().unwrap(), PackageKind::Character);
        assert_eq!(arch.manifest_id(PackageKind::Character).as_deref(), Some("c"));

        // Wrong format tag is rejected.
        let dir2 = tempfile::tempdir().unwrap();
        std::fs::write(dir2.path().join("character.json"), br#"{"format":"bogus/9"}"#).unwrap();
        let bytes2 = zip_dir(dir2.path(), "").unwrap();
        let arch2 = Archive::from_zip_bytes(&bytes2).unwrap();
        assert!(arch2.detect_kind().is_err());
    }

    #[test]
    fn unknown_archive_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("readme.txt"), b"hi").unwrap();
        let bytes = zip_dir(dir.path(), "").unwrap();
        let arch = Archive::from_zip_bytes(&bytes).unwrap();
        assert!(arch.detect_kind().is_err());
    }

    #[test]
    fn empty_bytes_is_bad_zip() {
        assert!(Archive::from_zip_bytes(&[]).is_err());
    }

    #[test]
    fn roundtrip_dir_zip_unzip() {
        let src = tempfile::tempdir().unwrap();
        std::fs::write(src.path().join("world.json"), br#"{"format":"gmlab.world/1","id":"w"}"#)
            .unwrap();
        std::fs::create_dir_all(src.path().join("assets")).unwrap();
        std::fs::write(src.path().join("assets").join("cover.png"), b"PNGDATA").unwrap();
        let bytes = zip_dir(src.path(), "").unwrap();
        let arch = Archive::from_zip_bytes(&bytes).unwrap();

        let dest = tempfile::tempdir().unwrap();
        arch.extract_all(dest.path()).unwrap();
        assert!(dest.path().join("world.json").is_file());
        assert_eq!(
            std::fs::read(dest.path().join("assets").join("cover.png")).unwrap(),
            b"PNGDATA"
        );
    }

    #[test]
    fn over_budget_entry_is_rejected() {
        // A single entry whose UNCOMPRESSED size exceeds the per-entry cap is
        // rejected — even though zeros compress to almost nothing (a zip bomb).
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("world.json"), br#"{"format":"gmlab.world/1","id":"w"}"#)
            .unwrap();
        let big = vec![0u8; (MAX_ENTRY_UNCOMPRESSED_BYTES + 1) as usize];
        std::fs::write(dir.path().join("payload.bin"), &big).unwrap();
        let bytes = zip_dir(dir.path(), "").unwrap();
        // The compressed archive is tiny but the entry inflates past the cap.
        assert!(
            bytes.len() < 1_000_000,
            "zeros should compress small, got {} bytes",
            bytes.len()
        );
        match Archive::from_zip_bytes(&bytes) {
            Err(ShareError::Zip(_)) => {}
            other => panic!("expected ShareError::Zip, got {:?}", other.err()),
        }
    }

    #[test]
    fn too_many_entries_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..(MAX_ENTRY_COUNT + 1) {
            std::fs::write(dir.path().join(format!("f{i}.txt")), b"x").unwrap();
        }
        let bytes = zip_dir(dir.path(), "").unwrap();
        match Archive::from_zip_bytes(&bytes) {
            Err(ShareError::Zip(_)) => {}
            other => panic!("expected ShareError::Zip, got {:?}", other.err()),
        }
    }

    /// Build a zip whose single entry is stored under the EXACT (possibly
    /// malicious) name, bypassing `zip_dir`'s safe path derivation. Uses
    /// `FileOptions` with the large_file flag off; the `zip` crate stores the
    /// name verbatim, which is exactly what an attacker-crafted archive does.
    fn zip_with_raw_entry(entry_name: &str, bytes: &[u8]) -> Vec<u8> {
        let mut buf = Vec::new();
        {
            let cursor = Cursor::new(&mut buf);
            let mut zip = ZipWriter::new(cursor);
            let options = SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            // `start_file` keeps the supplied name verbatim in the archive.
            zip.start_file(entry_name, options).expect("start malicious file");
            zip.write_all(bytes).expect("write malicious bytes");
            zip.finish().expect("finish malicious zip");
        }
        buf
    }

    #[test]
    fn zip_slip_entry_names_are_rejected_by_from_zip_bytes() {
        // Every crafted entry name must be rejected as a Traversal before any
        // bytes are retained — covering `..`, backslash `..`, an absolute path,
        // and a Windows drive / UNC prefix.
        let malicious = [
            "../evil.json",
            "..\\evil.json",
            "/abs/evil.json",
            "C:\\Windows\\evil.json",
            "\\\\server\\share\\evil.json",
            "sub/../../evil.json",
        ];
        for name in malicious {
            let bytes = zip_with_raw_entry(name, b"pwn");
            match Archive::from_zip_bytes(&bytes) {
                Err(ShareError::Traversal(_)) => {}
                other => panic!(
                    "entry {name:?} must be rejected as Traversal, got {:?}",
                    other.err()
                ),
            }
        }
    }

    #[test]
    fn zip_slip_writes_nothing_outside_dest() {
        // A would-be escaping entry name fed straight to write_entry/extract must
        // error and leave NOTHING outside the destination directory.
        let outer = tempfile::tempdir().unwrap();
        let dest = outer.path().join("dest");
        std::fs::create_dir_all(&dest).unwrap();
        // A sentinel sibling we will assert was never overwritten/created.
        let escaped_target = outer.path().join("evil.json");
        assert!(!escaped_target.exists());

        // write_entry re-validates the relative path and refuses traversal.
        for rel in ["../evil.json", "..\\evil.json", "/abs/evil.json"] {
            match write_entry(&dest, rel, b"pwn") {
                Err(ShareError::Traversal(_)) => {}
                other => panic!("rel {rel:?} must be Traversal, got {other:?}"),
            }
        }
        // Nothing escaped the destination.
        assert!(
            !escaped_target.exists(),
            "a zip-slip entry must not write outside dest"
        );
        // The destination itself stays empty (no partial files).
        assert_eq!(
            std::fs::read_dir(&dest).unwrap().count(),
            0,
            "no entries should be written into dest either"
        );
    }

    #[test]
    fn baked_world_subtree() {
        let story = tempfile::tempdir().unwrap();
        std::fs::write(
            story.path().join("story.json"),
            br#"{"format":"gmlab.story/1","id":"s"}"#,
        )
        .unwrap();
        let world = tempfile::tempdir().unwrap();
        std::fs::write(
            world.path().join("world.json"),
            br#"{"format":"gmlab.world/1","id":"w"}"#,
        )
        .unwrap();
        let manifest = br#"{"format":"gmlab.story/1","id":"s","world_embedded":true}"#;
        let bytes = zip_story_with_world(story.path(), world.path(), manifest).unwrap();
        let arch = Archive::from_zip_bytes(&bytes).unwrap();
        assert_eq!(arch.detect_kind().unwrap(), PackageKind::Story);
        assert!(arch.has_baked_world());
        let sub = arch.subtree("world/");
        assert_eq!(sub.detect_kind().unwrap(), PackageKind::World);
    }
}
