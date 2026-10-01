
    use super::*;
    use serde_json::json;

    #[test]
    fn safety_classroom_denies_desktop_and_side_effects_even_when_trusted() {
        assert!(!Policy::default().classroom_safe);
        let policy = Policy { classroom_safe: true, auto_approve: true, ..Policy::default() };
        for tool in crate::tools::REGISTRY {
            let decision = policy.decide(tool, &json!({}));
            if tool.risk != Risk::Read || matches!(tool.name, "screenshot" | "screen_elements" | "screen_element_search") {
                assert!(matches!(decision, Decision::Deny(_)), "{}", tool.name);
            } else {
                assert_eq!(decision, Decision::Allow, "{}", tool.name);
            }
        }
        let web = crate::find("web_search").unwrap();
        assert!(matches!(policy.decide(web, &json!({"query":"test", "open_browser":true})), Decision::Deny(_)));
        assert_eq!(policy.decide(web, &json!({"query":"test", "open_browser":false})), Decision::Allow);
    }

    #[test]
    fn read_tools_never_ask() {
        let p = Policy::default();
        let tool = crate::find("read_file").unwrap();
        assert_eq!(p.decide(tool, &json!({ "path": "a.txt" })), Decision::Allow);
    }

    #[test]
    fn write_tools_ask_unless_trusted() {
        let tool = crate::find("write_file").unwrap();
        let args = json!({ "path": "a.txt", "content": "hi" });
        assert!(matches!(
            Policy::default().decide(tool, &args),
            Decision::Confirm(_)
        ));
        let trusted = Policy {
            auto_approve: true,
            ..Policy::default()
        };
        assert_eq!(trusted.decide(tool, &args), Decision::Allow);
    }

    #[test]
    fn read_only_denies_mutations() {
        let p = Policy::read_only();
        let exec = crate::find("powershell").unwrap();
        let args = json!({ "command": "Get-ChildItem" });
        assert!(matches!(p.decide(exec, &args), Decision::Deny(_)));
        let read = crate::find("read_file").unwrap();
        assert_eq!(p.decide(read, &json!({ "path": "a.txt" })), Decision::Allow);
    }

    #[test]
    fn unknown_argument_is_denied_before_asking() {
        let p = Policy::default();
        let tool = crate::find("write_file").unwrap();
        let d = p.decide(
            tool,
            &json!({ "path": "a.txt", "content": "x", "rm_rf": true }),
        );
        assert!(matches!(d, Decision::Deny(_)));
    }

    #[test]
    fn web_search_open_browser_escalates_to_open_risk() {
        let tool = crate::find("web_search").unwrap();
        let grab_only = json!({ "query": "楞次定律" });
        let with_open = json!({ "query": "楞次定律", "open_browser": true });

        // 纯抓取永远只读放行
        assert_eq!(Policy::default().decide(tool, &grab_only), Decision::Allow);
        // 拉起浏览器 → Open 档：allow_open=false 时必须拒
        let no_open = Policy {
            allow_open: false,
            ..Policy::default()
        };
        assert!(matches!(no_open.decide(tool, &with_open), Decision::Deny(_)));
        // 同一开关下不带 open_browser 的抓取不受影响
        assert_eq!(no_open.decide(tool, &grab_only), Decision::Allow);
        assert_eq!(Policy::default().decide(tool, &with_open), Decision::Allow);
    }
