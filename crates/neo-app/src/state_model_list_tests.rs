
    use super::*;

    #[test]
    fn diagnostics_model_list_records_count_status_and_discard_without_identity() {
        let mut state = AppState::default();
        let view = diagnostics::capture_for_test(|| {
            for result in [Ok(vec!["PRIVATE_MODEL".to_owned()]), Err("模型列表接口返回 HTTP 503，PRIVATE_BODY".to_owned())] {
                let (tx, rx) = std::sync::mpsc::channel();
                state.model_fetch_diagnostic = Some(Span::new("model_list", None));
                state.model_fetch = Some(rx);
                tx.send(result).unwrap();
                assert!(state.poll_model_fetch());
            }
            let (tx, rx) = std::sync::mpsc::channel();
            state.model_fetch = Some(rx);
            state.model_fetch_diagnostic = Some(Span::new("model_list", None));
            drop(tx);
            assert!(!state.poll_model_fetch());
            state.model_fetch_diagnostic = Some(Span::new("model_list", None));
            state.model_fetch_config = Some(("PRIVATE_URL".into(), "PRIVATE_KEY".into()));
            assert!(!state.poll_model_fetch());
        });
        let text = format!("{view:?}");
        for expected in ["event=completed", "count=1", "kind=http_5xx", "kind=channel_disconnected", "event=result_discarded", "kind=configuration"] {
            assert!(text.contains(expected), "missing {expected}");
        }
        assert!(!text.contains("PRIVATE"));
    }

    /// **默认为空** —— 模型列表只由模型商提供，代码里不再写死候选。
    #[test]
    fn starts_with_no_models() {
        let st = AppState::default();
        assert!(st.models.is_empty(), "默认不该预置任何模型");
        assert!(!st.has_models());
        assert!(st.model_def().is_none());
        // 空列表下这几个取值必须"能画"，而不是崩或者显示一个假模型
        assert_eq!(st.model_display(), "未选择模型");
        assert_eq!(st.model_id(), "");
        assert_eq!(st.model_ids_joined(), "");
    }

    /// 没有模型时**不能**发真实请求：请求体里的 model 会是空串，服务端只会回 400。
    #[test]
    fn no_models_blocks_real_calls() {
        let st = AppState {
            api_base: "https://api.deepseek.com".to_owned(),
            api_key: "sk-test".to_owned(),
            ..AppState::default()
        };
        assert!(st.llm_config().is_configured(), "密钥与地址都齐了");
        assert!(!st.can_call_real(), "但没有模型 → 不该发");
        assert!(st.needs_model_list(), "这正是该去拉一次的状态");
    }

    /// 没配密钥走的是"离线演示"那条路，与"缺模型"是两回事。
    #[test]
    fn needs_model_list_is_false_without_credentials() {
        let st = AppState::default();
        assert!(!st.needs_model_list());
        assert!(!st.can_call_real());
    }

    /// 从库里读回来的列表也要保住选中项。
    #[test]
    fn provider_list_keeps_the_current_selection() {
        let mut st = AppState::default();
        st.set_models_from_provider(vec![
            "deepseek-chat".to_owned(),
            "deepseek-reasoner".to_owned(),
        ]);
        st.model = 1; // 用户选了 deepseek-reasoner
        st.set_models_from_provider(vec![
            "deepseek-chat".to_owned(),
            "deepseek-reasoner".to_owned(),
            "deepseek-coder".to_owned(),
        ]);
        assert_eq!(st.model_id(), "deepseek-reasoner", "选中项被换掉了");
        assert_eq!(st.models.len(), 3);
    }

    #[test]
    fn selection_falls_back_when_the_model_disappears() {
        let mut st = AppState::default();
        st.set_models_from_provider(vec![
            "deepseek-chat".to_owned(),
            "deepseek-reasoner".to_owned(),
        ]);
        st.model = 1;
        st.set_models_from_provider(vec!["deepseek-chat".to_owned()]);
        assert_eq!(st.model_id(), "deepseek-chat", "应退回第一条");
        assert_eq!(st.models.len(), 1);
    }

    /// 服务商给的顺序就是显示顺序；重复项并掉。
    #[test]
    fn provider_order_is_preserved_and_deduped() {
        let mut st = AppState::default();
        st.set_models_from_provider(vec![
            "deepseek-coder".to_owned(),
            "deepseek-chat".to_owned(),
            "deepseek-coder".to_owned(),
        ]);
        let ids: Vec<&str> = st.models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, vec!["deepseek-coder", "deepseek-chat"]);
    }

    #[test]
    fn known_names_win_over_bare_ids() {
        let mut st = AppState::default();
        st.set_models_from_provider(vec!["deepseek-chat".to_owned()]);
        // 已知模型有展示名，就别显示裸 id
        assert_eq!(st.model_display(), "DeepSeek-V3.2");
    }

    #[test]
    fn empty_provider_list_is_ignored() {
        let mut st = AppState::default();
        st.set_models_from_provider(vec!["deepseek-chat".to_owned()]);
        let before = st.models.len();
        st.set_models_from_provider(Vec::new());
        assert_eq!(st.models.len(), before, "空列表不该把已经存下的列表毁掉");
    }

    /// 存库 → 读回必须逐项一致，包括带 `/` 和 `,` 的 id
    /// （这正是用换行当分隔符、而不是逗号的原因）。
    #[test]
    fn model_ids_round_trip() {
        let mut st = AppState::default();
        st.set_models_from_provider(vec![
            "Qwen/QwQ-32B".to_owned(),
            "deepseek-chat".to_owned(),
            "vendor,inc/model-x".to_owned(),
        ]);
        let saved = st.model_ids_joined();
        let mut back = AppState::default();
        back.set_models_from_provider(saved.lines().map(str::to_owned).collect());
        let ids: Vec<&str> = back.models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["Qwen/QwQ-32B", "deepseek-chat", "vendor,inc/model-x"]
        );
        // 展示名也该一起回来（读回和拉取走的是同一个入口）
        assert_eq!(back.models[1].display, "DeepSeek-V3.2");
    }

    /// 读回存下来的列表：内容、顺序、展示名、选中项都要对。
    #[test]
    fn saved_list_is_restored() {
        let mut st = AppState::default();
        st.restore_models("deepseek-chat\ndeepseek-coder\n", Some("deepseek-coder"));
        assert_eq!(st.models.len(), 2);
        assert_eq!(st.model_id(), "deepseek-coder", "选中项按 id 恢复");
        assert_eq!(st.model_display(), "deepseek-coder", "表外的 id 原样显示");
        // 首尾空行要被吃掉（存的时候没有，但手改过库也得认）
        assert_eq!(
            st.models[0].display, "DeepSeek-V3.2",
            "表内的 id 换成展示名"
        );
    }

    /// 老库只有数字索引：仍要能认出选中项，别让升级把选择清掉。
    #[test]
    fn legacy_index_selection_still_works() {
        let mut st = AppState::default();
        st.restore_models("deepseek-chat\ndeepseek-coder", Some("1"));
        assert_eq!(st.model_id(), "deepseek-coder");
    }

    #[test]
    fn restore_models_rejects_out_of_range_legacy_indices() {
        for selected in [usize::MAX.to_string(), "999".into(), "2".into(), "missing".into()] {
            let mut st = AppState::default();
            st.restore_models("first\nsecond", Some(&selected));
            assert_eq!(st.model, 0, "{selected}");
            assert_eq!(st.model_id(), "first");
            assert_eq!((st.model + 1) % st.models.len(), 1);
        }
    }

    #[test]
    fn restore_models_uses_filtered_list_bounds_and_numeric_id_priority() {
        let mut st = AppState::default();
        st.restore_models("first\nbad\tmodel\nsecond", Some("2"));
        assert_eq!(st.models.len(), 2);
        assert_eq!(st.model, 0);
        st.restore_models("first\nbad\tmodel\nsecond", Some("1"));
        assert_eq!(st.model, 1);
        assert_eq!(st.model_id(), "second");
        st.restore_models("first\n999\n1", Some("999"));
        assert_eq!(st.model, 1);
        assert_eq!(st.model_id(), "999");
        st.restore_models("first\n999\n1", Some("1"));
        assert_eq!(st.model, 2, "numeric ID takes priority over legacy index");
    }

    #[test]
    fn restore_models_empty_list_keeps_zero_index() {
        for selected in [None, Some("999"), Some("1")] {
            let mut st = AppState {
                model: usize::MAX,
                ..Default::default()
            };
            st.restore_models("", selected);
            assert!(st.models.is_empty());
            assert_eq!(st.model, 0);
            assert!(st.model_def().is_none());
        }
    }

    /// 空串（比如库是新的）不动列表 —— 不能把运行中的列表清掉。
    #[test]
    fn empty_saved_list_keeps_the_current_one() {
        let mut st = AppState::default();
        st.set_models_from_provider(vec!["deepseek-chat".to_owned()]);
        st.restore_models("", None);
        assert_eq!(st.models.len(), 1);
    }

    #[test]
    fn reasoning_is_inferred_conservatively() {
        assert!(ModelDef::from_id("deepseek-reasoner").reasoning);
        assert!(ModelDef::from_id("deepseek-r1").reasoning);
        assert!(ModelDef::from_id("QwQ-32B-Reasoning").reasoning);
        // 不明确的当普通模型：宁可少带工具，也别给不支持的模型带
        assert!(!ModelDef::from_id("deepseek-chat").reasoning);
        assert!(!ModelDef::from_id("gpt-4o").reasoning);
        assert!(!ModelDef::from_id("qwen2.5-72b").reasoning);
    }
