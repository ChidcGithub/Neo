
    use super::*;
    use serde_json::json;

    #[test]
    fn timeout_default_is_500s_and_range_allows_it() {
        let p = PARAMS
            .iter()
            .find(|p| p.name == "timeout_ms")
            .expect("timeout_ms 参数存在");
        assert_eq!(p.default, Some(crate::spec::Default::Int(500_000)));
        let (lo, hi) = p.range.expect("有区间");
        assert!(lo < 500_000 && hi >= 500_000, "区间必须容得下默认值");
        assert_eq!(hi, 1_800_000);
    }

    #[test]
    fn background_flag_defaults_off() {
        let p = PARAMS
            .iter()
            .find(|p| p.name == "background")
            .expect("background 参数存在");
        assert!(!p.required);
        assert_eq!(p.default, Some(crate::spec::Default::Bool(false)));
    }

    #[test]
    fn hints_require_explicit_background_for_gui_lifetime() {
        let command = PARAMS.iter().find(|p| p.name == "command").unwrap();
        assert!(command.desc.contains("GUI"));
        assert!(command.desc.contains("background: true"));
        assert!(command.desc.contains("Start-Process"));
        assert!(command.desc.contains("清理整棵进程树"));
        let background = PARAMS.iter().find(|p| p.name == "background").unwrap();
        assert!(background.desc.contains("超时或取消"));
        assert!(background.desc.contains("自行关闭"));
        assert!(background.desc.contains("shell PID"));
        assert!(background.desc.contains("成功只表示 shell 已启动"));
    }

    #[test]
    fn preview_says_background_when_asked() {
        let tool = crate::find("powershell").unwrap();

        let v = json!({ "command": "npm run dev", "background": true });
        assert!(preview(&Args::new(tool, &v)).contains("后台执行"));

        let v = json!({ "command": "cargo test" });
        assert!(!preview(&Args::new(tool, &v)).contains("后台"));
    }

    /// 参数说明必须点明"不是 bash，要 bash 请换工具" —— 模型不看文档也要能选对。
    #[test]
    fn command_hint_warns_about_shell_dialect() {
        let p = PARAMS.iter().find(|p| p.name == "command").unwrap();
        assert!(p.desc.contains("不是 bash"), "说明里要标明方言");
        assert!(p.desc.contains("`bash` 工具"), "要指向姊妹工具");
    }
