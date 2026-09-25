//! 工具的实现与注册表。
//!
//! 每个工具一个文件，导出 `PARAMS`（参数声明）、`preview`（给人看的摘要）
//! 与 `run`（执行体），由 [`REGISTRY`] 汇总。
//! 加工具 = 加一个文件 + 在 [`REGISTRY`] 里加一行 + 补 `docs/tools.md`。
//!
//! 两个 shell 工具只差方言和宿主，执行骨架共用 [`shell`]。

pub(crate) mod base64_lite;
pub mod bash;
pub mod click;
pub mod drag;
pub mod edit_file;
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
    // 屏幕交互：比"执行命令"更贴近直接操作这台机器，排在最后。
    Tool {
        name: "screenshot",
        title: "截取屏幕",
        purpose: "截取整个屏幕或其中一块，并把图直接交给模型看（认界面、读屏幕上的文字）。\n要确认某个操作的结果时也用它。",
        risk: Risk::Read,
        params: screenshot::PARAMS,
        preview: screenshot::preview,
        run: screenshot::run,
    },
    Tool {
        name: "screen_elements",
        title: "屏幕元素",
        purpose: "枚举屏幕上所有可交互元素并编号（按钮 / 菜单 / 输入框…，含名称与位置）。\
                 要在界面上操作时**先用它**拿到编号，再用 click 的 element_id 点选 ——\
                 比对着截图猜坐标准得多。",
        risk: Risk::Read,
        params: screen_elements::PARAMS,
        preview: screen_elements::preview,
        run: screen_elements::run,
    },
    Tool {
        name: "screen_element_search",
        title: "搜索屏幕元素",
        purpose: "在最近一次屏幕元素清单中按名称、控件类型或屏幕位置搜索按钮和控件，并返回 element_id、精确矩形、中心点与保守用途提示。找不到按钮时先用它，不要凭截图猜坐标。没有缓存时自动枚举当前焦点窗口。",
        risk: Risk::Read,
        params: screen_element_search::PARAMS,
        preview: screen_element_search::preview,
        run: screen_element_search::run,
    },
    Tool {
        name: "click",
        title: "点击鼠标",
        purpose: "在屏幕上某个位置点击鼠标（左键/右键，可双击）。用之前先 `screenshot` 看清位置。",
        risk: Risk::Exec,
        params: click::PARAMS,
        preview: click::preview,
        run: click::run,
    },
    Tool {
        name: "drag",
        title: "拖动鼠标",
        purpose: "按住鼠标从一处拖到另一处（选中文字、拖滑块、框选）。用之前先 `screenshot`。",
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
