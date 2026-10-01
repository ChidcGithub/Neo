
    use super::*;

    fn streaming_state() -> AppState {
        let mut st = AppState::default();
        st.messages.push(ChatMessage::new(Role::User, "问"));
        let mut placeholder = ChatMessage::new(Role::Assistant, "");
        placeholder.streaming = true;
        st.messages.push(placeholder);
        st
    }

    /// **回归**：推理分片必须原样拼接，不许被插入换行。
    ///
    /// 真实流是一小片一小片的（"第一"、"步："、"判断"…），
    /// 早先每片之间补 '\n'，于是整块思考过程变成一列碎字。
    #[test]
    fn reasoning_deltas_are_concatenated_verbatim() {
        let mut st = streaming_state();
        for piece in ["第一", "步：", "判断磁场", "方向，", "然后", "用右手定则。"]
        {
            st.append_delta("", piece);
        }
        assert_eq!(
            st.messages[1].reasoning, "第一步：判断磁场方向，然后用右手定则。",
            "推理分片之间被插了换行"
        );
        assert!(!st.messages[1].reasoning.contains('\n'), "不该有换行");
    }

    /// 模型自己发的换行要保留（分段由模型决定）。
    #[test]
    fn model_newlines_are_preserved() {
        let mut st = streaming_state();
        st.append_delta("", "第一条\n");
        st.append_delta("", "第二条");
        assert_eq!(st.messages[1].reasoning, "第一条\n第二条");
    }

    /// 正文与推理互不干扰。
    #[test]
    fn content_and_reasoning_are_independent() {
        let mut st = streaming_state();
        st.append_delta("答", "想");
        st.append_delta("案", "");
        assert_eq!(st.messages[1].content, "答案");
        assert_eq!(st.messages[1].reasoning, "想");
    }
