
    use super::*;

    fn scope() -> Scope {
        Scope::new(std::env::temp_dir().join("neo-scope-test"))
    }

    #[test]
    fn cancellation_is_optional_and_shared_by_clones() {
        let original = scope();
        assert!(!original.is_cancelled());
        let token = Arc::new(AtomicBool::new(false));
        let bound = original.clone().with_cancel(token.clone());
        let cloned = bound.clone();
        assert!(!cloned.is_cancelled());
        token.store(true, Ordering::Release);
        assert!(bound.is_cancelled());
        assert!(cloned.is_cancelled());
        assert!(!original.is_cancelled());
    }

    #[test]
    fn relative_paths_land_inside_root() {
        let s = scope();
        let p = s.resolve("src/main.rs").unwrap();
        assert!(p.starts_with(s.root()));
        assert_eq!(s.display(&p), "src/main.rs");
    }

    #[test]
    fn traversal_escape_is_rejected() {
        let s = scope();
        for bad in [
            "../../etc/passwd",
            "a/../../../../etc/passwd",
            "/etc/passwd",
        ] {
            let err = s.resolve(bad).unwrap_err();
            assert_eq!(err.kind, crate::ErrorKind::NotAllowed, "{bad} 应被拒绝");
        }
    }

    #[test]
    fn inner_dotdot_is_fine() {
        let s = scope();
        // a/../b 仍在根内，属于正常写法，不该被误杀。
        let p = s.resolve("a/../b/c.txt").unwrap();
        assert_eq!(s.display(&p), "b/c.txt");
    }

    #[cfg(windows)]
    #[test]
    fn windows_plain_and_verbatim_paths_share_one_fence() {
        let s = Scope::new(r"C:\neo-scope-absolute-test");
        for path in [
            r"C:\neo-scope-absolute-test\a.txt",
            r"\\?\C:\neo-scope-absolute-test\a.txt",
            r"c:\neo-scope-absolute-test\a.txt",
        ] {
            assert_eq!(s.display(&s.resolve(path).unwrap()), "a.txt");
        }
        assert_eq!(
            s.resolve(r"C:\neo-scope-absolute-test\..\outside.txt")
                .unwrap_err()
                .kind,
            crate::ErrorKind::NotAllowed
        );
        let unc = Scope {
            root: normalize(Path::new(r"\\server\share\workspace")),
            cancel: None,
        };
        assert_eq!(
            unc.display(&unc.resolve(r"\\server\share\workspace\a.txt").unwrap()),
            "a.txt"
        );
        assert_eq!(
            unc.resolve(r"\\server\other\workspace\a.txt")
                .unwrap_err()
                .kind,
            crate::ErrorKind::NotAllowed
        );
    }

    #[test]
    fn empty_and_nul_rejected() {
        let s = scope();
        assert_eq!(
            s.resolve("  ").unwrap_err().kind,
            crate::ErrorKind::BadArguments
        );
        assert_eq!(
            s.resolve("a\0b").unwrap_err().kind,
            crate::ErrorKind::BadArguments
        );
    }
