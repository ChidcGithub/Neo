//! 工具的实现与注册表。
//!
//! 每个工具一个文件，导出 `PARAMS`（参数声明）、`preview`（给人看的摘要）
//! 与 `run`（执行体），由 [`REGISTRY`] 汇总。
//! 加工具 = 加一个文件 + 在 [`REGISTRY`] 里加一行 + 补 `docs-pri/tools.md`。
//!
//! 两个 shell 工具只差方言和宿主，执行骨架共用 [`shell`]。

pub(crate) mod base64_lite;
pub mod app_control;
pub mod ask_user;
pub mod bash;
pub mod click;
pub mod daily;
pub mod desktop_snapshot;
pub mod drag;
pub mod edit_file;
pub mod memory;
pub mod move_mouse;
pub mod open_app;
pub mod open_file;
pub mod powershell;
pub mod present_result;
pub mod read_document;
pub mod read_file;
pub mod screen;
pub mod screen_element_search;
pub mod screen_elements;
pub mod screen_uia;
pub mod screenshot;
pub mod screenshot_space;
pub mod scroll;
pub mod shell;
pub mod shortcut;
pub mod target;
pub mod type_text;
pub mod view_image;
pub mod wait;
pub mod web_search;
pub mod write_file;

use crate::policy::Risk;
use crate::spec::Tool;

/// 注册表。顺序即推荐顺序，也是系统提示词里的顺序：
/// **先读、再写、最后执行** —— 让模型形成"先看再做"的默认路径。
pub static REGISTRY: &[Tool] = &[
    Tool {
        name: "read_file",
        title: "查看文件",
        purpose: "Read a text file; use offset/limit to continue paging.",
        risk: Risk::Read,
        params: read_file::PARAMS,
        preview: read_file::preview,
        run: read_file::run,
    },
    Tool {
        name: "read_document",
        title: "读取文档",
        purpose: "Extract text from DOC/DOCX/PPT/PPTX files; continue with next_offset.",
        risk: Risk::Read,
        params: read_document::PARAMS,
        preview: read_document::preview,
        run: read_document::run,
    },
    Tool {
        name: "view_image",
        title: "查看图片",
        purpose: "Inspect an image; optionally return its encoded data.",
        risk: Risk::Read,
        params: view_image::PARAMS,
        preview: view_image::preview,
        run: view_image::run,
    },
    // 联网读取：只出网、不进工作区，所以排在本地「读」之后、「打开」之前。
    Tool {
        name: "web_search",
        title: "联网搜索",
        purpose: "Search the web for current or more detailed information.",
        risk: Risk::Read,
        params: web_search::PARAMS,
        preview: web_search::preview,
        run: web_search::run,
    },
    Tool {
        name: "open_file",
        title: "打开文件",
        purpose: "Open a file with its default app or reveal it in Explorer.",
        risk: Risk::Open,
        params: open_file::PARAMS,
        preview: open_file::preview,
        run: open_file::run,
    },
    Tool {
        name: "open_app",
        title: "打开主界面",
        purpose: "Bring the Neo main window back to the screen.",
        risk: Risk::Open,
        params: &[],
        preview: open_app::preview,
        run: open_app::run,
    },
    // 交互工具：不碰机器，只向用户要一个决定 —— 与「打开」同类相邻。
    Tool {
        name: "ask_user",
        title: "向用户提问",
        purpose: "Ask the user a question with 2–4 tappable options.",
        risk: Risk::Read,
        params: ask_user::PARAMS,
        preview: ask_user::preview,
        run: ask_user::run,
    },
    Tool {
        name: "present_result",
        title: "展示结果",
        purpose: "Show a Markdown result card until the user closes it.",
        risk: Risk::Read,
        params: present_result::PARAMS,
        preview: present_result::preview,
        run: present_result::run,
    },
    Tool {
        name: "write_file",
        title: "写入文件",
        purpose: "Create or overwrite a file with complete content.",
        risk: Risk::Write,
        params: write_file::PARAMS,
        preview: write_file::preview,
        run: write_file::run,
    },
    Tool {
        name: "edit_file",
        title: "修改文件",
        purpose: "Replace one exact text block in an existing file.",
        risk: Risk::Write,
        params: edit_file::PARAMS,
        preview: edit_file::preview,
        run: edit_file::run,
    },
    Tool {
        name: "powershell",
        title: "执行命令",
        purpose: "Run PowerShell commands inside the workspace.",
        risk: Risk::Exec,
        params: powershell::PARAMS,
        preview: powershell::preview,
        run: powershell::run,
    },
    Tool {
        name: "bash",
        title: "执行命令",
        purpose: "Run Bash commands in the bundled Unix environment.",
        risk: Risk::Exec,
        params: bash::PARAMS,
        preview: bash::preview,
        run: bash::run,
    },
    // 长期记忆：改的是 AI 自己的记忆文件，不动用户数据 —— 归 open 档放行。
    Tool {
        name: "remember",
        title: "记住",
        purpose: "Store a durable user preference or fact.",
        risk: Risk::Open,
        params: memory::REMEMBER_PARAMS,
        preview: memory::remember_preview,
        run: memory::remember_run,
    },
    Tool {
        name: "forget",
        title: "忘掉",
        purpose: "Delete one long-term memory by id or keyword.",
        risk: Risk::Open,
        params: memory::FORGET_PARAMS,
        preview: memory::forget_preview,
        run: memory::forget_run,
    },
    // 每日记忆：只对当天/某天有意义的速记（用户明确要求的记录），与长期记忆相邻。
    Tool {
        name: "recall_day",
        title: "回想某天",
        purpose: "Read daily notes; omit date to list the index.",
        risk: Risk::Read,
        params: daily::RECALL_PARAMS,
        preview: daily::recall_preview,
        run: daily::recall_run,
    },
    Tool {
        name: "note_day",
        title: "记当天",
        purpose: "Append a timestamped note to today's daily log.",
        risk: Risk::Open,
        params: daily::NOTE_PARAMS,
        preview: daily::note_preview,
        run: daily::note_run,
    },
    // 屏幕交互：比"执行命令"更贴近直接操作这台机器，排在最后。
    Tool {
        name: "screenshot",
        title: "截取屏幕",
        purpose: "Capture the screen or a region for visual inspection.",
        risk: Risk::Read,
        params: screenshot::PARAMS,
        preview: screenshot::preview,
        run: screenshot::run,
    },
    Tool {
        name: "screen_elements",
        title: "屏幕元素",
        purpose: "List interactive UI elements with stable element IDs.",
        risk: Risk::Read,
        params: screen_elements::PARAMS,
        preview: screen_elements::preview,
        run: screen_elements::run,
    },
    Tool {
        name: "screen_element_search",
        title: "搜索屏幕元素",
        purpose: "Search the latest UI element list by name, type, or position.",
        risk: Risk::Read,
        params: screen_element_search::PARAMS,
        preview: screen_element_search::preview,
        run: screen_element_search::run,
    },
    Tool {
        name: "desktop_snapshot",
        title: "桌面快照",
        purpose: "Get windows, focus, UI elements, and an optional screenshot.",
        risk: Risk::Read,
        params: desktop_snapshot::PARAMS,
        preview: desktop_snapshot::preview,
        run: desktop_snapshot::run,
    },
    Tool {
        name: "click",
        title: "点击鼠标",
        purpose: "Click a UI element or a screen coordinate.",
        risk: Risk::Exec,
        params: click::PARAMS,
        preview: click::preview,
        run: click::run,
    },
    Tool {
        name: "drag",
        title: "拖动鼠标",
        purpose: "Drag the mouse from one point to another.",
        risk: Risk::Exec,
        params: drag::PARAMS,
        preview: drag::preview,
        run: drag::run,
    },
    Tool {
        name: "type_text",
        title: "输入文字",
        purpose: "Type text into the focused input; optionally submit.",
        risk: Risk::Exec,
        params: type_text::PARAMS,
        preview: type_text::preview,
        run: type_text::run,
    },
    Tool {
        name: "scroll",
        title: "滚动屏幕",
        purpose: "Scroll the mouse wheel at an element or coordinate.",
        risk: Risk::Exec,
        params: scroll::PARAMS,
        preview: scroll::preview,
        run: scroll::run,
    },
    Tool {
        name: "shortcut",
        title: "按键",
        purpose: "Send a keyboard shortcut to the focused window.",
        risk: Risk::Exec,
        params: shortcut::PARAMS,
        preview: shortcut::preview,
        run: shortcut::run,
    },
    Tool {
        name: "move_mouse",
        title: "移动鼠标",
        purpose: "Move the pointer to an element or coordinate.",
        risk: Risk::Exec,
        params: move_mouse::PARAMS,
        preview: move_mouse::preview,
        run: move_mouse::run,
    },
    Tool {
        name: "app_control",
        title: "应用控制",
        purpose: "Launch an app or switch/resize a window.",
        risk: Risk::Exec,
        params: app_control::PARAMS,
        preview: app_control::preview,
        run: app_control::run,
    },
    Tool {
        name: "wait",
        title: "等待",
        purpose: "Wait up to 120 seconds; cancellable by the user.",
        risk: Risk::Read,
        params: wait::PARAMS,
        preview: wait::preview,
        run: wait::run,
    },
];

/// 各工具共用的体积上限，集中一处方便审计。
pub mod limits {
    /// 读文件上限（1 MiB）。
    pub const READ_BYTES: u64 = 1024 * 1024;
    /// 写文件上限（1 MiB）。
    pub const WRITE_BYTES: u64 = 1024 * 1024;
    /// 图片上限（8 MiB）。
    pub const IMAGE_BYTES: u64 = 8 * 1024 * 1024;
    /// **内联**发给模型的一张图的上限（32 MiB，官方规定）。
    /// 超了就别把图塞进请求，只给文件路径并说明下一步。
    pub const INLINE_IMAGE_BYTES: usize = 32 * 1024 * 1024;
    /// 单条命令输出上限（64 KiB，超出截断）。
    pub const OUTPUT_BYTES: usize = 64 * 1024;
    /// 回灌给模型的内容上限（字符）。
    pub const MODEL_JSON_CHARS: usize = 24_000;
}
