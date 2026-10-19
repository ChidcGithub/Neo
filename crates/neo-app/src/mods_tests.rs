
    use super::*;
    use neo_tools::mods::API_VERSION;
    use serde_json::json;
    use std::fs;

    struct EnvGuard(Option<std::ffi::OsString>);

    impl EnvGuard {
        fn set(value: &str) -> Self {
            let old = std::env::var_os("NEO_MODS_DIR");
            std::env::set_var("NEO_MODS_DIR", value);
            Self(old)
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match self.0.take() {
                Some(old) => std::env::set_var("NEO_MODS_DIR", old),
                None => std::env::remove_var("NEO_MODS_DIR"),
            }
        }
    }

    #[test]
    fn root_defaults_beside_the_executable_and_env_wins() {
        let _guard = EnvGuard::set(r"C:\elsewhere\mods");
        assert_eq!(root(), PathBuf::from(r"C:\elsewhere\mods"));
        drop(_guard);
        if std::env::var_os("NEO_MODS_DIR").is_some() {
            return; // 环境变量优先，跳过默认值断言
        }
        assert_eq!(root(), crate::state::app_dir().join("mods"));
    }

    #[test]
    fn grant_roundtrip_is_deterministic() {
        let manifest = manifest();
        let mut grants = BTreeMap::new();
        grants.insert("org.example".to_owned(), Grant::from_manifest(&manifest));
        let encoded = encode_grants(&grants);
        assert_eq!(encode_grants(&decode_grants(&encoded)), encoded);
        let decoded = decode_grants(&encoded);
        assert!(decoded["org.example"].matches(&manifest));
    }

    #[test]
    fn decode_grants_fails_toward_disabled() {
        assert!(decode_grants("").is_empty(), "空串不是启用");
        assert!(decode_grants("garbage").is_empty(), "坏 JSON 不是启用");
        assert!(decode_grants("[1,2]").is_empty(), "非对象不是启用");
        let mixed = r#"{
            "org.good": {"revision": 1, "capabilities": ["workspace_read"]},
            "org.bad": {"revision": "x"},
            "org.also.bad": {"revision": 1, "capabilities": ["not_a_capability"]}
        }"#;
        let decoded = decode_grants(mixed);
        assert_eq!(decoded.len(), 1, "坏条目跳过、好条目保留");
        assert_eq!(decoded["org.good"].revision, 1, "好条目原样保留");
    }

    fn manifest() -> Manifest {
        serde_json::from_value::<Manifest>(manifest_json("org.example")).expect("fixture manifest")
    }

    fn manifest_json(id: &str) -> serde_json::Value {
        let schema = json!({
            "type": "object", "properties": {}, "required": [],
            "additionalProperties": false
        });
        json!({
            "api_version": API_VERSION, "id": id, "revision": 1,
            "executable": "bin/tool.exe",
            "capabilities": ["process_spawn", "workspace_read", "network"],
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

    #[test]
    fn grant_binds_revision_and_capability_set() {
        let manifest = manifest();
        let grant = Grant::from_manifest(&manifest);
        assert_eq!(
            grant.capabilities,
            vec![
                neo_tools::mods::Capability::WorkspaceRead,
                neo_tools::mods::Capability::Network,
                neo_tools::mods::Capability::ProcessSpawn,
            ],
            "能力排序后写入，编码才确定"
        );
        assert!(grant.matches(&manifest));

        let mut newer = manifest.clone();
        newer.revision = 2;
        assert!(!grant.matches(&newer), "revision 变了要重新确认");

        let mut fewer = manifest.clone();
        fewer.capabilities.pop();
        assert!(!grant.matches(&fewer), "能力集合变了要重新确认");

        let mut reordered = manifest.clone();
        reordered.capabilities.reverse();
        assert!(grant.matches(&reordered), "顺序不算差异");
    }

    #[test]
    fn reconcile_reports_off_on_and_reconfirm() {
        let manifest = manifest();
        let mut row = ModRow {
            manifest: manifest.clone(),
            directory: PathBuf::from("pkg"),
            executable: PathBuf::from("pkg/bin/tool.exe"),
            status: ModStatus::Off,
            expanded: false,
        };
        let mut grants = BTreeMap::new();

        reconcile(std::slice::from_mut(&mut row), &grants);
        assert_eq!(row.status, ModStatus::Off, "无意图是关");

        grants.insert("org.example".to_owned(), Grant::from_manifest(&manifest));
        reconcile(std::slice::from_mut(&mut row), &grants);
        assert_eq!(row.status, ModStatus::On, "意图一致是开");

        grants
            .get_mut("org.example")
            .unwrap()
            .revision = 99;
        reconcile(std::slice::from_mut(&mut row), &grants);
        assert_eq!(row.status, ModStatus::NeedsReconfirm, "旧意图要重新确认");

        grants.clear();
        reconcile(std::slice::from_mut(&mut row), &grants);
        assert_eq!(row.status, ModStatus::Off, "撤掉意图回到关");
    }

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            use std::sync::atomic::{AtomicU64, Ordering};
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let base = fs::canonicalize(std::env::temp_dir()).unwrap();
            let nonce = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let root = base.join(format!(
                "neo-app-mods-{}-{nonce}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&root).unwrap();
            Self(root)
        }

        fn package(&self, id: &str) -> PathBuf {
            let path = self.0.join("package");
            fs::create_dir_all(path.join("bin")).unwrap();
            fs::write(path.join("bin/tool.exe"), b"inert").unwrap();
            fs::write(
                path.join("manifest.json"),
                serde_json::to_vec(&manifest_json(id)).unwrap(),
            )
            .unwrap();
            path
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn scan_reads_rows_and_fails_closed_on_missing_root() {
        let fixture = Fixture::new();
        let package = fixture.package("org.example");

        let rows = scan(&fixture.0).expect("有效目录要能扫");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].manifest.id, "org.example");
        assert_eq!(rows[0].directory, package);
        assert_eq!(rows[0].executable, package.join("bin/tool.exe"));
        assert_eq!(rows[0].status, ModStatus::Off, "扫描不产生启用状态");

        let missing = fixture.0.join("missing");
        let error = scan(&missing).expect_err("缺根目录必须整批失败");
        assert!(!error.is_empty(), "错误是契约层的静态文案");
    }
