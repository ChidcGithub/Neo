//! 工具的实现与注册表。
//!
//! 每个工具一个文件，导出 `PARAMS`（参数声明）、`preview`（给人看的摘要）
//! 与 `run`（执行体），由 [`REGISTRY`] 汇总。
//! 加工具 = 加一个文件 + 在 [`REGISTRY`] 里加一行 + 补 `docs/tools.md`。
//!
//! 两个 shell 工具只差方言和宿主，执行骨架共用 [`shell`]。

pub(crate) mod base64_lite;
pub mod ask_user;
pub mod bash;
pub mod click;
pub mod daily;
pub mod drag;
pub mod edit_file;
pub mod memory;
pub mod open_app;
pub mod open_file;
pub mod powershell;
pub mod read_document;
pub mod read_file;
pub mod screen;
pub mod screen_element_search;
pub mod screen_elements;
pub(crate) mod screen_uia;
pub mod screenshot;
pub mod shell;
pub mod view_image;
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
        purpose: "读取工作区内文本文件的内容（可分段）。改文件之前先用它确认原文。",
        risk: Risk::Read,
        params: read_file::PARAMS,
        preview: read_file::preview,
        run: read_file::run,
    },
    Tool {
        name: "read_document",
        title: "读取文档",
        purpose: "解析工作区内 DOC/DOCX Word 文档、PPT/PPTX PowerPoint 演示文稿和文本，按字符分页返回正文。默认 4000、最多 8000 字符；用 next_offset 续读。文件限 32 MiB，最多提取 80,000 字符，截断或格式局限见 warning；图片请用 view_image。",
        risk: Risk::Read,
        params: read_document::PARAMS,
        preview: read_document::preview,
        run: read_document::run,
    },
    Tool {
        name: "view_image",
        title: "查看图片",
        purpose: "查看工作区内图片的基本信息（格式 / 尺寸 / 体积），必要时取回原始数据。",
        risk: Risk::Read,
        params: view_image::PARAMS,
        preview: view_image::preview,
        run: view_image::run,
    },
    // 联网读取：只出网、不进工作区，所以排在本地「读」之后、「打开」之前。
    Tool {
        name: "web_search",
        title: "联网搜索",
        purpose: "用 Bing 搜索互联网：解析结果页，把标题 / 链接 / 摘要回给你（不需要任何 key）。\
                 \n时效或课外事实（新闻、近况、没把握的知识）用它核实，不要凭记忆硬答；\
                 用户想自己看网页时设 open_browser=true，在其浏览器里打开搜索页。",
        risk: Risk::Read,
        params: web_search::PARAMS,
        preview: web_search::preview,
        run: web_search::run,
    },
    Tool {
        name: "open_file",
        title: "打开文件",
        purpose: "在用户机器上用默认程序打开某个文件，或在文件管理器里定位它，让用户亲眼看。",
        risk: Risk::Open,
        params: open_file::PARAMS,
        preview: open_file::preview,
        run: open_file::run,
    },
    Tool {
        name: "open_app",
        title: "打开主界面",
        purpose: "把 Neo 的主窗口唤回到屏幕上（后台静默处理时用户说「打开主界面 / 让我看看」就用它）。",
        risk: Risk::Open,
        params: &[],
        preview: open_app::preview,
        run: open_app::run,
    },
    // 交互工具：不碰机器，只向用户要一个决定 —— 与「打开」同类相邻。
    Tool {
        name: "ask_user",
        title: "向用户提问",
        purpose: "拿不准用户意图时弹窗提问：给问题 + 2~4 个候选答案，用户点选后答案回给你。\
                 \n题没拍全、要求有歧义、下一步有多种合理做法时用它确认，不要自己猜；\
                 能合理假设的琐事不要打扰用户。",
        risk: Risk::Read,
        params: ask_user::PARAMS,
        preview: ask_user::preview,
        run: ask_user::run,
    },
    Tool {
        name: "write_file",
        title: "写入文件",
        purpose: "写入或整体覆盖一个文件。新建文件、整篇重写时用它；只改一小段请用 edit_file。",
        risk: Risk::Write,
        params: write_file::PARAMS,
        preview: write_file::preview,
        run: write_file::run,
    },
    Tool {
        name: "edit_file",
        title: "修改文件",
        purpose: "把文件里一段确定的原文替换成新文本。比 write_file 安全：不重写整个文件。",
        risk: Risk::Write,
        params: edit_file::PARAMS,
        preview: edit_file::preview,
        run: edit_file::run,
    },
    Tool {
        name: "powershell",
        title: "执行命令",
        purpose: "在工作区内执行 PowerShell 命令（编译、测试、搜索、批量处理）。专用文件、文档或图像工具能做的事不要用它。",
        risk: Risk::Exec,
        params: powershell::PARAMS,
        preview: powershell::preview,
        run: powershell::run,
    },
    Tool {
        name: "bash",
        title: "执行命令",
        purpose: "在工作区内的类 Unix 环境（随包的 Git Bash）里执行命令：\
                 \\nls / grep / sed / find / git 等文本与版本控制工具。\
                 \\n专用文件、文档或图像工具能做的事不要用它；要写另一种 shell 的语法请换用姊妹工具。",
        risk: Risk::Exec,
        params: bash::PARAMS,
        preview: bash::preview,
        run: bash::run,
    },
    // 长期记忆：改的是 AI 自己的记忆文件，不动用户数据 —— 归 open 档放行。
    Tool {
        name: "remember",
        title: "记住",
        purpose: "把值得长期记住的事（用户偏好、身份、常用设定）写进长期记忆，以后的对话都带在身上。一次性的任务指令不要记。",
        risk: Risk::Open,
        params: memory::REMEMBER_PARAMS,
        preview: memory::remember_preview,
        run: memory::remember_run,
    },
    Tool {
        name: "forget",
        title: "忘掉",
        purpose: "删除一条长期记忆（#id 或内容关键词）。记忆过期、记错、或用户要求忘掉时用。",
        risk: Risk::Open,
        params: memory::FORGET_PARAMS,
        preview: memory::forget_preview,
        run: memory::forget_run,
    },
    // 每日记忆：只对当天/某天有意义的速记（用户明确要求的记录），与长期记忆相邻。
    Tool {
        name: "recall_day",
        title: "回想某天",
        purpose: "读取每日记忆。不给日期 = 返回「哪天有什么」的一句话索引（先查它）；\
                 \n给 date = 读那天的全部速记和当天课堂总结。用户问「今天/某天发生了什么」时用它。",
        risk: Risk::Read,
        params: daily::RECALL_PARAMS,
        preview: daily::recall_preview,
        run: daily::recall_run,
    },
    Tool {
        name: "note_day",
        title: "记当天",
        purpose: "仅在用户明确要求记录时，往今天的每日记忆追加速记（时间自动盖章）。不得静默观察或记录学生行为；长期偏好用 remember。",
        risk: Risk::Open,
        params: daily::NOTE_PARAMS,
        preview: daily::note_preview,
        run: daily::note_run,
    },
    // 屏幕交互：比"执行命令"更贴近直接操作这台机器，排在最后。
    Tool {
        name: "screenshot",
        title: "截取屏幕",
        purpose: "截取整个屏幕或其中一块，并把图直接交给模型看（认界面、读屏幕上的文字）。\n仅在用户请求且需要视觉信息时使用，不要求每次操作前后截图。",
        risk: Risk::Read,
        params: screenshot::PARAMS,
        preview: screenshot::preview,
        run: screenshot::run,
    },
    Tool {
        name: "screen_elements",
        title: "屏幕元素",
        purpose: "用户请求桌面操作时先 mode=overview 获取窗口概览，再用 window_id 或 query 局部枚举。返回 snapshot_id + element_id，点击时必须一起传入，操作后刷新局部元素。不要默认扫描整桌面。",
        risk: Risk::Read,
        params: screen_elements::PARAMS,
        preview: screen_elements::preview,
        run: screen_elements::run,
    },
    Tool {
        name: "screen_element_search",
        title: "搜索屏幕元素",
        purpose: "在最近一次屏幕元素清单中按名称、控件类型或屏幕位置搜索按钮和控件，并返回 element_id、精确矩形、中心点与保守用途提示。找不到按钮时先用它，不要凭截图猜坐标。refresh=false 只查缓存，无缓存时拒绝；只有 refresh=true 才重新枚举。点击时同时传返回的 snapshot_id 和 element_id。",
        risk: Risk::Read,
        params: screen_element_search::PARAMS,
        preview: screen_element_search::preview,
        run: screen_element_search::run,
    },
    Tool {
        name: "click",
        title: "点击鼠标",
        purpose: "在屏幕上某个位置点击鼠标（左键/右键，可双击）。优先传局部元素枚举返回的 snapshot_id + element_id，操作后刷新元素。",
        risk: Risk::Exec,
        params: click::PARAMS,
        preview: click::preview,
        run: click::run,
    },
    Tool {
        name: "drag",
        title: "拖动鼠标",
        purpose: "按住鼠标从一处拖到另一处（选中文字、拖滑块、框选）。先用局部元素确定目标，仅在必要时请求截图。",
        risk: Risk::Exec,
        params: drag::PARAMS,
        preview: drag::preview,
        run: drag::run,
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
