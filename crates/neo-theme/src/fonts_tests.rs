use super::*;

#[test]
fn bold_chain_only_references_registered_fonts() {
    for regular_cjk in [false, true] {
        for bold_cjk in [false, true] {
            let mut defs = FontDefinitions::default();
            let fixture = Arc::new(FontData::from_static(PHOSPHOR_TTF));
            if regular_cjk {
                defs.font_data.insert("neo-ui-cjk".into(), fixture.clone());
            }
            if bold_cjk {
                defs.font_data.insert("neo-bold-cjk".into(), fixture.clone());
            }
            let chain = bold_family(&defs);
            assert!(!chain.is_empty());
            assert!(chain.iter().all(|key| defs.font_data.contains_key(key)));
            assert_eq!(chain.contains(&"neo-ui-cjk".to_owned()), regular_cjk);
            assert_eq!(chain.contains(&"neo-bold-cjk".to_owned()), bold_cjk);
            if regular_cjk && bold_cjk {
                assert!(chain.iter().position(|k| k == "neo-bold-cjk")
                    < chain.iter().position(|k| k == "neo-ui-cjk"));
            }
        }
    }
}
