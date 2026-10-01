
    use super::*;

    /// 隔离：指向临时 NEO_HOME，跑完恢复（与 memory.rs 同款，测试串行跑）。
    struct EnvGuard(Option<String>);
    impl EnvGuard {
        fn set(dir: &std::path::Path) -> Self {
            let old = std::env::var("NEO_HOME").ok();
            std::env::set_var("NEO_HOME", dir);
            Self(old)
        }
    }
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.0 {
                Some(v) => std::env::set_var("NEO_HOME", v),
                None => std::env::remove_var("NEO_HOME"),
            }
        }
    }

    #[test]
    fn date_boundaries_are_validated_before_path_use() {
        for date in ["2024-02-29", "2000-02-29", "2026-12-31", "2027-01-01"] {
            assert!(valid_date(date));
        }
        for date in ["1900-02-29", "2026-02-29", "2026-04-31", "2026-00-01", "2026-01-00", "../memories", "C:\\outside", "２０２６-01-01"] {
            assert!(!valid_date(date));
            assert!(load_day(date).is_empty());
            assert_eq!(append_class_on(date, fixture("invalid date")).unwrap_err().kind,
                crate::result::ErrorKind::BadArguments);
        }
    }

    fn fixture(content: &str) -> NewClassNote {
        NewClassNote {
            started_ms: 1_790_000_000_000,
            subject: "数学".into(),
            summary: content.into(),
            screen_notes: vec!["板书：三角函数定义".into()],
            transcript: vec!["同学们看这里".into()],
        }
    }

    /// 两个场景共享 NEO_HOME 环境变量，必须合在一个测试里串行跑
    /// （memory.rs 已经踩过并行串台的坑）。跨模块再靠 `NEO_HOME_TEST_LOCK`。
    #[test]
    fn append_roundtrip_and_corruption_handling() {
        let _lock = crate::NEO_HOME_TEST_LOCK.lock().unwrap();
        let dir = std::env::temp_dir().join(format!("neo-classlog-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let _g = EnvGuard::set(&dir);

        // 模拟跨年两日：即使第二日已有记录，第一日的回执丢失重试仍命中原条目。
        let first_date = "2026-12-31";
        let next_date = "2027-01-01";
        let first = append_class_on(first_date, fixture("跨日总结")).unwrap();
        let next = append_class_on(next_date, fixture("次日总结")).unwrap();
        let retry = append_class_on(first_date, fixture("跨日总结")).unwrap();
        assert_eq!((retry.id, retry.ended_ms), (first.id, first.ended_ms));
        assert_eq!(load_day(first_date).len(), 1);
        let next_day = load_day(next_date);
        assert_eq!(next_day.len(), 1);
        assert_eq!(next_day[0].summary, next.summary);
        assert!(class_path(first_date).is_file());
        assert!(class_path(next_date).is_file());
        std::fs::remove_dir_all(&dir).unwrap();

        // 便利 API 仍保存到当天；固定日期测试不依赖实际系统日期。
        let a = append_class(fixture("诱导公式")).unwrap();
        let retry = append_class(fixture("诱导公式")).unwrap();
        assert_eq!((retry.id, retry.ended_ms), (a.id, a.ended_ms));
        let b = append_class(fixture("图象变换")).unwrap();
        assert_eq!(b.id, a.id + 1);

        let day = load_day(&today_key());
        assert_eq!(day.len(), 2);
        assert_eq!(day[0].subject, "数学");
        assert_eq!(day[0].summary, "诱导公式");
        assert_eq!(day[0].transcript, vec!["同学们看这里".to_string()]);
        assert!(!day[0].over_limit);
        assert!(day[0].ended_ms >= day[0].started_ms);

        // 超限标记：1501 个字符 → over_limit。
        let long = "字".repeat(SUMMARY_LIMIT + 1);
        let c = append_class(fixture(&long)).unwrap();
        assert!(c.over_limit);

        // 损坏文件：改名 .bad 留档，读出来是空而不是崩。
        std::fs::write(class_path(&today_key()), b"{ not json").unwrap();
        assert!(load_day(&today_key()).is_empty());
        assert!(class_path(&today_key()).with_extension("json.bad").exists());

        let _ = std::fs::remove_dir_all(&dir);
    }
