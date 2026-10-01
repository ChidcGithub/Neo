
    use super::*;
    use serde_json::json;

    #[test]
    fn timeout_and_background_match_the_powershell_tool() {
        let ps = crate::find("powershell").unwrap();
        let sh = crate::find("bash").unwrap();
        // 两个工具是同一骨架的两个宿主，参数形状必须完全一致 ——
        // 分开写两份最容易漂移的就是这里。
        assert_eq!(ps.params.len(), sh.params.len());
        for (a, b) in ps.params.iter().zip(sh.params.iter()) {
            assert_eq!(a.name, b.name, "参数名漂移");
            assert_eq!(a.default, b.default, "`{}` 的默认值漂移", a.name);
            assert_eq!(a.range, b.range, "`{}` 的区间漂移", a.name);
            assert_eq!(a.required, b.required, "`{}` 的必填性漂移", a.name);
        }
    }

    #[test]
    fn preview_names_the_unix_shell() {
        let tool = crate::find("bash").unwrap();
        let v = json!({ "command": "git status" });
        assert!(preview(&Args::new(tool, &v)).contains("Git Bash"));
        let v = json!({ "command": "npm run dev", "background": true });
        assert!(preview(&Args::new(tool, &v)).contains("后台执行"));
    }

    /// 参数说明要点明方言，并指向姊妹工具。
    #[test]
    fn command_hint_warns_about_shell_dialect() {
        let p = PARAMS.iter().find(|p| p.name == "command").unwrap();
        assert!(p.desc.contains("不是 PowerShell"), "说明里要标明方言");
        assert!(p.desc.contains("`powershell` 工具"), "要指向姊妹工具");
    }
