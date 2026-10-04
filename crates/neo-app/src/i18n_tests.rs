use super::*;

#[test]
fn language_codes_default_and_scoped_override() {
    assert_eq!(Language::default(), Language::ZhCn);
    for code in ["", "zh-CN", "unknown", "en", "en-GB"] {
        assert_eq!(Language::from_code(code), Language::ZhCn);
    }
    for code in ["en-US", "EN-us", " en_US "] {
        assert_eq!(Language::from_code(code), Language::EnUs);
    }
    for lang in [Language::ZhCn, Language::EnUs] {
        assert_eq!(Language::from_code(lang.code()), lang);
    }
    with_language(Language::ZhCn, || {
        with_language(Language::EnUs, || {
            assert_eq!(language(), Language::EnUs);
            assert_eq!(std::thread::spawn(language).join().unwrap(), Language::ZhCn);
        });
        assert_eq!(language(), Language::ZhCn);
        let _ = std::panic::catch_unwind(|| with_language(Language::EnUs, || panic!("guard")));
        assert_eq!(language(), Language::ZhCn);
    });
}

#[test]
fn named_format_is_single_pass_and_preserves_unknowns() {
    let args = [
        ("name", "{count}".to_owned()),
        ("count", "2".to_owned()),
        ("name", "ignored".to_owned()),
    ];
    assert_eq!(
        format_named("{name}: {count}/{name} {unknown}", &args),
        "{count}: 2/{count} {unknown}"
    );
    assert_eq!(
        format_named("中文 {} {bad-name} {name", &args),
        "中文 {} {bad-name} {name"
    );
    assert_eq!(format_named("{{name}}", &args), "{{count}}");
    with_language(Language::ZhCn, || {
        assert_eq!(
            tf("未收录模板 {name} {other}", &args),
            "未收录模板 {count} {other}"
        );
    });
}

#[test]
fn catalog_format_size_values_and_placeholders_are_validated() {
    for invalid in [
        "[]",
        "null",
        "{",
        r#"{"a":1}"#,
        r#"{"a":" "}"#,
        r#"{"":"a"}"#,
        r#"{"{name}":"{other}"}"#,
        r#"{"{name} {name}":"{name}"}"#,
        r#"{"a":"\u0000"}"#,
    ] {
        assert!(parse_catalog(invalid.as_bytes()).is_none(), "{invalid}");
    }
    assert!(parse_catalog(&[0xff]).is_none());
    assert!(parse_catalog(&vec![b' '; CATALOG_LIMIT + 1]).is_none());
    assert!(parse_catalog(br#"{"{a} {b}":"{b}: {a}"}"#).is_some());
    assert!(parse_catalog(b"{}").is_some());
}

#[test]
fn external_precedence_partial_override_and_bad_file_fallback() {
    let dir = std::env::temp_dir().join(format!(
        "neo-i18n-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    struct Cleanup(PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = Cleanup(dir.clone());
    let source = dir.join("source.lang");
    let exe = dir.join("exe.lang");
    let embedded = r#"{"a":"embedded","b":"fallback"}"#;
    assert_eq!(load_catalog(embedded, &source, Some(&exe))["a"], "embedded");
    std::fs::write(&source, r#"{"a":"source"}"#).unwrap();
    std::fs::write(&exe, r#"{"a":"exe"}"#).unwrap();
    let catalog = load_catalog(embedded, &source, Some(&exe));
    assert_eq!(catalog["a"], "exe");
    assert_eq!(catalog["b"], "fallback");
    for bad in [
        b"broken".to_vec(),
        br#"{"a":""}"#.to_vec(),
        vec![0xff],
        vec![b' '; CATALOG_LIMIT + 1],
    ] {
        std::fs::write(&exe, bad).unwrap();
        assert_eq!(load_catalog(embedded, &source, Some(&exe))["a"], "source");
    }
    std::fs::write(&source, "broken").unwrap();
    assert_eq!(load_catalog(embedded, &source, Some(&exe))["a"], "embedded");
}

#[test]
fn embedded_languages_have_matching_keys_and_valid_values() {
    let zh = parse_catalog(ZH_CN.as_bytes()).expect("valid embedded Chinese catalog");
    let en = parse_catalog(EN_US.as_bytes()).expect("valid embedded English catalog");
    assert!(
        !zh.is_empty(),
        "merge language resources before running catalog tests"
    );
    assert_eq!(zh.keys().collect::<Vec<_>>(), en.keys().collect::<Vec<_>>());
    for (key, value) in &zh {
        assert_eq!(key, value, "Chinese catalog preserves source text");
    }
    assert_eq!(en["Neo — 教室大屏 AI 助手"], "Neo — Classroom AI Assistant");
}

#[test]
fn lookup_defaults_to_source_and_reuses_static_catalog() {
    for lang in [Language::ZhCn, Language::EnUs] {
        with_language(lang, || {
            assert_eq!(tr("未收录的源文案"), "未收录的源文案");
            let first = tr("Neo — 教室大屏 AI 助手");
            for _ in 0..100 {
                assert!(std::ptr::eq(first, tr("Neo — 教室大屏 AI 助手")));
            }
            assert!(std::ptr::eq(catalogs(), catalogs()));
            assert_eq!(
                first,
                if lang == Language::ZhCn {
                    "Neo — 教室大屏 AI 助手"
                } else {
                    "Neo — Classroom AI Assistant"
                }
            );
        });
    }
}
