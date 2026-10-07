//! Explicit, read-only discovery; no execution, PATH lookup, downloads or grants.
//!
//! The caller must supply an absolute, trusted root and keep it and its ancestors
//! stable during discovery. Metadata checks reject links/reparse points, but are
//! not a sandbox against concurrent path replacement by an untrusted writer.
//! Only immediate children are packages. Directory names need not match manifest
//! IDs and confer no official/trusted identity. Any failure aborts the whole scan.
use super::{Catalog, Check, Error, ValidatedManifest, MAX_CATALOG, MAX_MANIFEST_BYTES};
use std::{
    fs::{self, Metadata, OpenOptions},
    io::{self, Read},
    path::{Component, Path, PathBuf},
};

/// Counts all root entries, including files and invalid packages.
pub const MAX_DISCOVERY_ENTRIES: usize = MAX_CATALOG;

pub struct DiscoveredMod {
    manifest: ValidatedManifest,
    directory: PathBuf,
    executable: PathBuf,
}

impl DiscoveredMod {
    pub fn manifest(&self) -> &ValidatedManifest {
        &self.manifest
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }

    /// A metadata-checked regular file, not a loaded or approved executable.
    pub fn executable(&self) -> &Path {
        &self.executable
    }
}

pub struct Discovery {
    entries: Vec<DiscoveredMod>,
    catalog: Catalog,
}

impl Discovery {
    /// In directory-path order; available only after the entire scan succeeds.
    pub fn entries(&self) -> &[DiscoveredMod] {
        &self.entries
    }

    /// The existing inert catalog is the sole source of enabled-state semantics.
    pub fn catalog(&self) -> &Catalog {
        &self.catalog
    }
}

/// Reads at most 32,769 bytes per manifest and examines at most 128 packages.
/// Does not create a missing root or inspect package data/executable contents.
/// Errors are static and never include manifest text, paths or OS diagnostics.
pub fn discover(root: &Path) -> Result<Discovery, Error> {
    if !root.is_absolute() {
        return Err(Error("MOD root must be absolute"));
    }
    check_directories(root, "MOD root missing")?;

    // Bound enumeration before validation so invalid entries also consume budget.
    let children = fs::read_dir(root).map_err(|_| Error("cannot read MOD root"))?;
    let mut directories = Vec::new();
    for child in children.take(MAX_DISCOVERY_ENTRIES + 1) {
        if directories.len() == MAX_DISCOVERY_ENTRIES {
            return Err(Error("MOD discovery entry limit"));
        }
        directories.push(
            child
                .map_err(|_| Error("cannot read MOD root entry"))?
                .path(),
        );
    }
    directories.sort();

    let mut discovery = Discovery {
        entries: Vec::new(),
        catalog: Catalog::default(),
    };
    for directory in directories {
        if !inspect(&directory, "MOD directory missing")?.is_dir() {
            return Err(Error("MOD entry must be a directory"));
        }
        let manifest = read_manifest(&directory.join("manifest.json"))?;
        let mut executable = directory.clone();
        // The strict parser already rejects absolute paths, backslashes, dot
        // segments, Windows device names and other unsafe executable spellings.
        let mut parts = manifest.manifest().executable.split('/').peekable();
        while let Some(part) = parts.next() {
            executable.push(part);
            let metadata = inspect(&executable, "MOD executable missing")?;
            if parts.peek().is_some() {
                if !metadata.is_dir() {
                    return Err(Error("MOD executable ancestor must be a directory"));
                }
            } else if !metadata.is_file() {
                return Err(Error("MOD executable must be a regular file"));
            }
        }
        // Reuse exact ID/tool collision rules, without exposing partial results.
        discovery.catalog.register(manifest.clone())?;
        discovery.entries.push(DiscoveredMod {
            manifest,
            directory,
            executable,
        });
    }
    Ok(discovery)
}

fn check_directories(path: &Path, missing: &'static str) -> Check {
    let mut current = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => {
                // Device namespaces and UNC/network roots are not local roots.
                #[cfg(windows)]
                if !matches!(
                    prefix.kind(),
                    std::path::Prefix::Disk(_) | std::path::Prefix::VerbatimDisk(_)
                ) {
                    return Err(Error("unsupported MOD root prefix"));
                }
                current.push(prefix.as_os_str());
                continue;
            }
            Component::RootDir | Component::Normal(_) => current.push(component.as_os_str()),
            _ => return Err(Error("invalid MOD root component")),
        }
        if !inspect(&current, missing)?.is_dir() {
            return Err(Error("MOD path must be a directory"));
        }
    }
    Ok(())
}

fn inspect(path: &Path, missing: &'static str) -> Check<Metadata> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            Error(missing)
        } else {
            Error("cannot inspect MOD path")
        }
    })?;
    reject_link(&metadata)?;
    Ok(metadata)
}

fn reject_link(metadata: &Metadata) -> Check {
    if metadata.file_type().is_symlink() {
        return Err(Error("MOD links and reparse points are forbidden"));
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        reject_reparse_attributes(metadata.file_attributes())?;
    }
    Ok(())
}

#[cfg(windows)]
fn reject_reparse_attributes(attributes: u32) -> Check {
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(Error("MOD links and reparse points are forbidden"));
    }
    Ok(())
}

fn read_manifest(path: &Path) -> Check<ValidatedManifest> {
    if !inspect(path, "MOD manifest missing")?.is_file() {
        return Err(Error("MOD manifest must be a regular file"));
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = options
        .open(path)
        .map_err(|_| Error("cannot read MOD manifest"))?;
    let metadata = file
        .metadata()
        .map_err(|_| Error("cannot inspect MOD manifest"))?;
    reject_link(&metadata)?;
    if !metadata.is_file() {
        return Err(Error("MOD manifest must be a regular file"));
    }
    parse_reader(file)
}

fn parse_reader(reader: impl Read) -> Check<ValidatedManifest> {
    let mut bytes = Vec::new();
    reader
        .take(MAX_MANIFEST_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| Error("cannot read MOD manifest"))?;
    if bytes.len() > MAX_MANIFEST_BYTES {
        return Err(Error("MOD manifest byte limit"));
    }
    ValidatedManifest::parse(&bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    use std::sync::atomic::{AtomicU64, Ordering};

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let base = fs::canonicalize(std::env::temp_dir()).unwrap();
            let nonce = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let root = base.join(format!(
                "neo-discovery-{}-{nonce}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&root).unwrap();
            Self(root)
        }

        fn package(&self, directory: &str, id: &str) -> PathBuf {
            let path = self.0.join(directory);
            fs::create_dir_all(path.join("bin")).unwrap();
            // Deliberately inert, not a program. Discovery must not load/run it.
            fs::write(path.join("bin/tool.exe"), b"not an executable").unwrap();
            self.manifest(&path, &manifest(id));
            path
        }

        fn manifest(&self, path: &Path, value: &Value) {
            fs::write(
                path.join("manifest.json"),
                serde_json::to_vec(value).unwrap(),
            )
            .unwrap();
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn manifest(id: &str) -> Value {
        let schema = json!({
            "type": "object", "properties": {}, "required": [],
            "additionalProperties": false
        });
        json!({
            "api_version": 1, "id": id, "revision": 1,
            "executable": "bin/tool.exe",
            "capabilities": ["workspace_read", "network", "process_spawn"],
            "limits": {
                "timeout_ms": 5000, "memory_mib": 64, "max_in_flight": 1,
                "max_message_bytes": 65536, "max_result_bytes": 32768
            },
            "tools": [{
                "name": "echo", "description": "Inert fixture",
                "input_schema": schema, "output_schema": schema
            }]
        })
    }

    fn failure(root: &Path) -> Error {
        discover(root).err().expect("must fail closed")
    }

    #[test]
    fn explicit_root_missing_empty_and_invalid_roots() {
        let fixture = Fixture::new();
        let missing = fixture.0.join("missing");
        assert_eq!(failure(&missing), Error("MOD root missing"));
        assert!(!missing.exists());
        assert_eq!(
            failure(Path::new("relative")),
            Error("MOD root must be absolute")
        );
        assert!(discover(&fixture.0).unwrap().entries().is_empty());
        fs::write(&missing, b"").unwrap();
        assert_eq!(failure(&missing), Error("MOD path must be a directory"));
        assert!(discover(&fixture.0.join("../missing")).is_err());
    }

    #[test]
    fn packages_remain_disabled_and_data_is_not_traversed() {
        let fixture = Fixture::new();
        let package = fixture.package("not-the-manifest-id", "org.example");
        fs::create_dir_all(package.join("data/nested")).unwrap();
        fs::write(
            package.join("data/nested/manifest.json"),
            b"secret invalid JSON",
        )
        .unwrap();
        let discovery = discover(&fixture.0).unwrap();
        assert_eq!(discovery.entries().len(), 1);
        let entry = &discovery.entries()[0];
        assert_eq!(entry.directory(), package);
        assert_eq!(entry.executable(), package.join("bin/tool.exe"));
        assert_eq!(entry.manifest().manifest().capabilities.len(), 3);
        let catalog_entry = discovery.catalog().get("org.example").unwrap();
        assert!(!catalog_entry.enabled());
        assert_eq!(catalog_entry.manifest().manifest().id, "org.example");
    }

    #[test]
    fn all_root_entries_count_toward_the_limit() {
        let fixture = Fixture::new();
        for i in 0..MAX_DISCOVERY_ENTRIES {
            fixture.package(&format!("p{i:03}"), &format!("org.p{i}"));
        }
        assert_eq!(
            discover(&fixture.0).unwrap().entries().len(),
            MAX_DISCOVERY_ENTRIES
        );
        fs::write(fixture.0.join("invalid"), b"").unwrap();
        assert_eq!(failure(&fixture.0), Error("MOD discovery entry limit"));

        let invalid = Fixture::new();
        for i in 0..=MAX_DISCOVERY_ENTRIES {
            fs::write(invalid.0.join(format!("invalid{i}")), b"").unwrap();
        }
        assert_eq!(failure(&invalid.0), Error("MOD discovery entry limit"));
    }

    #[test]
    fn invalid_directory_manifest_and_executable_fail_closed() {
        let fixture = Fixture::new();
        let path = fixture.0.join("package");
        fs::write(&path, b"").unwrap();
        assert_eq!(failure(&fixture.0), Error("MOD entry must be a directory"));
        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        assert_eq!(failure(&fixture.0), Error("MOD manifest missing"));
        fs::create_dir(path.join("manifest.json")).unwrap();
        assert_eq!(
            failure(&fixture.0),
            Error("MOD manifest must be a regular file")
        );
        fs::remove_dir(path.join("manifest.json")).unwrap();
        fixture.manifest(&path, &manifest("org.example"));
        assert_eq!(failure(&fixture.0), Error("MOD executable missing"));
        fs::write(path.join("bin"), b"").unwrap();
        assert_eq!(
            failure(&fixture.0),
            Error("MOD executable ancestor must be a directory")
        );
        fs::remove_file(path.join("bin")).unwrap();
        fs::create_dir_all(path.join("bin/tool.exe")).unwrap();
        assert_eq!(
            failure(&fixture.0),
            Error("MOD executable must be a regular file")
        );
    }

    #[test]
    fn strict_parser_errors_never_echo_secrets() {
        let fixture = Fixture::new();
        let package = fixture.package("package", "org.example");
        for bytes in [
            b"manifest-secret".as_slice(),
            b"{\"id\":\"manifest-secret\",\"id\":\"org.example\"}",
        ] {
            fs::write(package.join("manifest.json"), bytes).unwrap();
            assert_eq!(failure(&fixture.0), Error("invalid JSON"));
        }
        let mut value = manifest("org.example");
        value["manifest-secret"] = json!("manifest-secret");
        fixture.manifest(&package, &value);
        assert_eq!(failure(&fixture.0), Error("invalid contract fields"));
        for executable in [
            "../secret",
            "/secret",
            "bin\\secret",
            "C:/secret",
            "bin/NUL",
            "bin//secret",
        ] {
            let mut value = manifest("org.example");
            value["executable"] = json!(executable);
            fixture.manifest(&package, &value);
            assert_eq!(failure(&fixture.0), Error("invalid identity or executable"));
        }
    }

    #[test]
    fn manifest_byte_limit_is_inclusive_and_read_is_bounded() {
        let fixture = Fixture::new();
        let package = fixture.package("package", "org.example");
        let mut bytes = serde_json::to_vec(&manifest("org.example")).unwrap();
        bytes.resize(MAX_MANIFEST_BYTES, b' ');
        fs::write(package.join("manifest.json"), &bytes).unwrap();
        assert!(discover(&fixture.0).is_ok());
        bytes.push(b' ');
        fs::write(package.join("manifest.json"), &bytes).unwrap();
        assert_eq!(failure(&fixture.0), Error("MOD manifest byte limit"));

        struct Endless(usize);
        impl Read for Endless {
            fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
                buffer.fill(b' ');
                self.0 += buffer.len();
                assert!(self.0 <= MAX_MANIFEST_BYTES + 1);
                Ok(buffer.len())
            }
        }
        let mut reader = Endless(0);
        assert_eq!(
            parse_reader(&mut reader).unwrap_err(),
            Error("MOD manifest byte limit")
        );
        assert_eq!(reader.0, MAX_MANIFEST_BYTES + 1);
    }

    #[test]
    fn permission_errors_are_static() {
        struct Denied;
        impl Read for Denied {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "manifest-secret",
                ))
            }
        }
        assert_eq!(
            parse_reader(Denied).unwrap_err(),
            Error("cannot read MOD manifest")
        );
    }

    #[test]
    fn duplicate_ids_and_tool_names_keep_catalog_semantics() {
        let fixture = Fixture::new();
        fixture.package("a", "org.example");
        let second = fixture.package("b", "org.example");
        assert_eq!(failure(&fixture.0), Error("duplicate MOD id"));
        fixture.manifest(&second, &manifest("org.example2"));
        assert_eq!(discover(&fixture.0).unwrap().catalog().len(), 2);
        let mut duplicate = manifest("org.example2");
        let tool = duplicate["tools"][0].clone();
        duplicate["tools"].as_array_mut().unwrap().push(tool);
        fixture.manifest(&second, &duplicate);
        assert_eq!(failure(&fixture.0), Error("duplicate tool name"));

        let variants = Fixture::new();
        variants.package("a", "a.bc");
        variants.package("b", "ab.c");
        let discovery = discover(&variants.0).unwrap();
        assert_eq!(discovery.catalog().len(), 2);
        assert!(!discovery.catalog().get("a.bc").unwrap().enabled());
        assert!(!discovery.catalog().get("ab.c").unwrap().enabled());
    }

    #[cfg(unix)]
    #[test]
    fn links_are_rejected_at_every_inspected_level() {
        use std::os::unix::fs::symlink;
        for relative in [
            "package",
            "package/manifest.json",
            "package/bin",
            "package/bin/tool.exe",
        ] {
            let fixture = Fixture::new();
            let outside = Fixture::new();
            fixture.package("package", "org.example");
            let original = fixture.0.join(relative);
            let target = outside.0.join("target");
            fs::rename(&original, &target).unwrap();
            symlink(&target, &original).unwrap();
            assert_eq!(
                failure(&fixture.0),
                Error("MOD links and reparse points are forbidden")
            );
        }
        let fixture = Fixture::new();
        let outside = Fixture::new();
        fs::create_dir(outside.0.join("root")).unwrap();
        symlink(&outside.0, fixture.0.join("link")).unwrap();
        for root in [fixture.0.join("link"), fixture.0.join("link/root")] {
            assert_eq!(
                failure(&root),
                Error("MOD links and reparse points are forbidden")
            );
        }
        fs::remove_file(fixture.0.join("link")).unwrap();
        symlink(outside.0.join("missing"), fixture.0.join("dangling")).unwrap();
        assert_eq!(
            failure(&fixture.0),
            Error("MOD links and reparse points are forbidden")
        );
    }

    #[cfg(windows)]
    #[test]
    fn all_reparse_attributes_are_rejected_not_only_symlink_tags() {
        for attributes in [0x400, 0x410, 0x420] {
            assert_eq!(
                reject_reparse_attributes(attributes),
                Err(Error("MOD links and reparse points are forbidden"))
            );
        }
        assert!(reject_reparse_attributes(0x10).is_ok());
        assert!(reject_reparse_attributes(0x20).is_ok());
    }
}
