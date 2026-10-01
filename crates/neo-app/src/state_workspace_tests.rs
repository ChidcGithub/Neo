
    use super::*;

    /// 默认（未选目录）时，工作区是 **APP 目录**，不是进程当前目录。
    #[test]
    fn default_workspace_is_the_app_directory() {
        let st = AppState::default();
        assert!(st.workspace_dir.is_none(), "默认不该预设工作目录");
        if std::env::var("NEO_WORKSPACE").is_ok() {
            return; // 环境变量优先，跳过
        }
        let root = st.workspace_root();
        assert!(root.is_dir(), "工作区必须是个真实目录：{root:?}");
        let exe_dir = app_dir();
        assert_eq!(root, exe_dir, "默认工作区应当是 APP 目录");
    }

    /// 选了目录就用目录。
    #[test]
    fn selected_directory_wins() {
        let mut st = AppState::default();
        let dir = std::env::temp_dir();
        st.workspace_dir = Some(dir.clone());
        assert_eq!(st.workspace_root(), dir);
    }
