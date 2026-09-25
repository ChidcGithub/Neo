//! 字体装配。
//!
//! 复刻 Harness 的字体栈（`ui-theme/src/styles/base.css`）：
//!
//! ```text
//! --dsw-font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', 'PingFang SC',
//!                    'Hiragino Sans GB', 'Microsoft YaHei', 'Helvetica Neue',
//!                    Helvetica, Arial, sans-serif
//! --ds-font-family-code: 'SF Mono', 'JetBrains Mono', 'Fira Code', Consolas,
//!                        'Liberation Mono', Menlo, Courier, 'PingFang SC', 'Microsoft YaHei'
//! ```
//!
//! 上游注释特别指出：等宽栈**故意不接**裸 `monospace` —— Windows 下中文会掉到
//! SimSun（宋体）。这里同样把 Microsoft YaHei 显式接在 Consolas 之后。
//!
//! Linux / macOS 上按同一优先级顺序探测，探测不到时安静降级到 egui 内置字体。

use std::sync::Arc;

use egui::{Context, FontData, FontDefinitions, FontFamily, FontTweak};

/// 粗体族名（Harness 里 `font-weight: 500/600` 的位置都用它）。
pub const FAMILY_BOLD: &str = "neo-bold";
/// 等宽族名。
pub const FAMILY_MONO: &str = "neo-mono";

/// 粗体字族句柄。
pub fn bold() -> FontFamily {
    FontFamily::Name(FAMILY_BOLD.into())
}

/// 等宽字族句柄。
pub fn mono() -> FontFamily {
    FontFamily::Name(FAMILY_MONO.into())
}

/// 一组候选字体文件：按优先级尝试，取第一个存在的。
struct Candidate {
    /// 注入 egui 时使用的逻辑名。
    key: &'static str,
    /// 候选文件路径（按平台优先级）。
    paths: &'static [&'static str],
    /// TTC/OTC 集合内的 face 序号。
    index: u32,
    /// 额外缩放微调。
    tweak: FontTweak,
}

/// 所有平台通用的探测表。
///
/// 顺序即优先级：`Segoe UI → Microsoft YaHei`，与 Harness 在 Windows 上的实际落点一致。
fn candidates() -> Vec<Candidate> {
    vec![
        Candidate {
            key: "neo-ui",
            paths: &[
                r"C:\Windows\Fonts\segoeui.ttf",
                "/System/Library/Fonts/SFNS.ttf",
                "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
                "/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf",
            ],
            index: 0,
            tweak: FontTweak::default(),
        },
        Candidate {
            key: "neo-ui-cjk",
            paths: &[
                r"C:\Windows\Fonts\msyh.ttc",
                r"C:\Windows\Fonts\simhei.ttf",
                r"C:\Windows\Fonts\Deng.ttf",
                "/System/Library/Fonts/PingFang.ttc",
                "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
                "/usr/share/fonts/truetype/wqy/wqy-microhei.ttc",
            ],
            index: 0,
            // 中文字面在同样的磅值下视觉偏大，收一点与拉丁文对齐基线节奏。
            tweak: FontTweak {
                scale: 0.97,
                ..Default::default()
            },
        },
        Candidate {
            key: "neo-bold",
            paths: &[
                r"C:\Windows\Fonts\seguibl.ttf",
                "/System/Library/Fonts/SFNS.ttf",
                "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf",
            ],
            index: 0,
            tweak: FontTweak::default(),
        },
        Candidate {
            key: "neo-bold-cjk",
            paths: &[
                r"C:\Windows\Fonts\msyhbd.ttc",
                r"C:\Windows\Fonts\simhei.ttf",
                r"C:\Windows\Fonts\Deng.ttf",
                "/System/Library/Fonts/PingFang.ttc",
                "/usr/share/fonts/opentype/noto/NotoSansCJK-Bold.ttc",
                "/usr/share/fonts/truetype/wqy/wqy-microhei.ttc",
            ],
            index: 0,
            tweak: FontTweak {
                scale: 0.97,
                ..Default::default()
            },
        },
        Candidate {
            key: "neo-mono",
            paths: &[
                r"C:\Windows\Fonts\consola.ttf",
                "/System/Library/Fonts/SFNSMono.ttf",
                "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
            ],
            index: 0,
            tweak: FontTweak::default(),
        },
    ]
}

/// KaTeX 字体面（内嵌）。
///
/// 名字与 `ratex` 的 `FontId::as_str()` **一一对应** —— 排版器给出哪个名字，
/// 这里就得有哪个族，否则数学公式会缺字。
/// 共 20 个面 / 约 540 KB（KaTeX 0.18.7，OFL 许可）。
pub mod katex {
    /// （族名, TTF 字节）。
    pub const AMS_REGULAR: &[u8] = include_bytes!("../assets/katex/KaTeX_AMS-Regular.ttf");
    pub const CALIGRAPHIC_BOLD: &[u8] =
        include_bytes!("../assets/katex/KaTeX_Caligraphic-Bold.ttf");
    pub const CALIGRAPHIC_REGULAR: &[u8] =
        include_bytes!("../assets/katex/KaTeX_Caligraphic-Regular.ttf");
    pub const FRAKTUR_BOLD: &[u8] = include_bytes!("../assets/katex/KaTeX_Fraktur-Bold.ttf");
    pub const FRAKTUR_REGULAR: &[u8] = include_bytes!("../assets/katex/KaTeX_Fraktur-Regular.ttf");
    pub const MAIN_BOLD: &[u8] = include_bytes!("../assets/katex/KaTeX_Main-Bold.ttf");
    pub const MAIN_BOLDITALIC: &[u8] = include_bytes!("../assets/katex/KaTeX_Main-BoldItalic.ttf");
    pub const MAIN_ITALIC: &[u8] = include_bytes!("../assets/katex/KaTeX_Main-Italic.ttf");
    pub const MAIN_REGULAR: &[u8] = include_bytes!("../assets/katex/KaTeX_Main-Regular.ttf");
    pub const MATH_BOLDITALIC: &[u8] = include_bytes!("../assets/katex/KaTeX_Math-BoldItalic.ttf");
    pub const MATH_ITALIC: &[u8] = include_bytes!("../assets/katex/KaTeX_Math-Italic.ttf");
    pub const SANSSERIF_BOLD: &[u8] = include_bytes!("../assets/katex/KaTeX_SansSerif-Bold.ttf");
    pub const SANSSERIF_ITALIC: &[u8] =
        include_bytes!("../assets/katex/KaTeX_SansSerif-Italic.ttf");
    pub const SANSSERIF_REGULAR: &[u8] =
        include_bytes!("../assets/katex/KaTeX_SansSerif-Regular.ttf");
    pub const SCRIPT_REGULAR: &[u8] = include_bytes!("../assets/katex/KaTeX_Script-Regular.ttf");
    pub const SIZE1_REGULAR: &[u8] = include_bytes!("../assets/katex/KaTeX_Size1-Regular.ttf");
    pub const SIZE2_REGULAR: &[u8] = include_bytes!("../assets/katex/KaTeX_Size2-Regular.ttf");
    pub const SIZE3_REGULAR: &[u8] = include_bytes!("../assets/katex/KaTeX_Size3-Regular.ttf");
    pub const SIZE4_REGULAR: &[u8] = include_bytes!("../assets/katex/KaTeX_Size4-Regular.ttf");
    pub const TYPEWRITER_REGULAR: &[u8] =
        include_bytes!("../assets/katex/KaTeX_Typewriter-Regular.ttf");

    /// 全部字体面：(族名, 字节)。
    pub const FACES: &[(&str, &[u8])] = &[
        ("AMS-Regular", AMS_REGULAR),
        ("Caligraphic-Bold", CALIGRAPHIC_BOLD),
        ("Caligraphic-Regular", CALIGRAPHIC_REGULAR),
        ("Fraktur-Bold", FRAKTUR_BOLD),
        ("Fraktur-Regular", FRAKTUR_REGULAR),
        ("Main-Bold", MAIN_BOLD),
        ("Main-BoldItalic", MAIN_BOLDITALIC),
        ("Main-Italic", MAIN_ITALIC),
        ("Main-Regular", MAIN_REGULAR),
        ("Math-BoldItalic", MATH_BOLDITALIC),
        ("Math-Italic", MATH_ITALIC),
        ("SansSerif-Bold", SANSSERIF_BOLD),
        ("SansSerif-Italic", SANSSERIF_ITALIC),
        ("SansSerif-Regular", SANSSERIF_REGULAR),
        ("Script-Regular", SCRIPT_REGULAR),
        ("Size1-Regular", SIZE1_REGULAR),
        ("Size2-Regular", SIZE2_REGULAR),
        ("Size3-Regular", SIZE3_REGULAR),
        ("Size4-Regular", SIZE4_REGULAR),
        ("Typewriter-Regular", TYPEWRITER_REGULAR),
    ];
}

/// 装配结果：实际找到的字体名，供诊断面板展示。
#[derive(Clone, Debug, Default)]
pub struct LoadedFonts {
    pub ui: Option<String>,
    pub cjk: Option<String>,
    pub bold: Option<String>,
    pub mono: Option<String>,
}

impl LoadedFonts {
    /// 是否拿到了可用的中文字体（决定界面能否正常显示中文）。
    pub fn has_cjk(&self) -> bool {
        self.cjk.is_some()
    }
}

/// 把 Neo 字体装进 egui 上下文，返回实际加载结果。
///
/// 内置字体（NotoEmoji / emoji-icon-font）保留在链尾，保证 emoji 与冷僻符号不变成豆腐块。
pub fn install(ctx: &Context) -> LoadedFonts {
    let mut defs = FontDefinitions::default();
    let mut loaded = LoadedFonts::default();

    for c in candidates() {
        let Some(path) = c.paths.iter().find(|p| std::path::Path::new(p).is_file()) else {
            continue;
        };
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        let data = FontData {
            font: bytes.into(),
            index: c.index,
            tweak: c.tweak,
        };
        defs.font_data.insert(c.key.to_owned(), Arc::new(data));

        let slot = match c.key {
            "neo-ui" => &mut loaded.ui,
            "neo-ui-cjk" => &mut loaded.cjk,
            "neo-bold" => &mut loaded.bold,
            "neo-mono" => &mut loaded.mono,
            _ => continue,
        };
        *slot = Some((*path).to_owned());
    }

    // ---- 族链 ----
    // 顺序即 Harness 的 fallback 优先级；内置字体永远留在末尾。
    let mut proportional: Vec<String> = Vec::new();
    if loaded.ui.is_some() {
        proportional.push("neo-ui".to_owned());
    }
    if loaded.cjk.is_some() {
        proportional.push("neo-ui-cjk".to_owned());
    }
    proportional.extend([
        "Ubuntu-Light".to_owned(),
        "NotoEmoji-Regular".to_owned(),
        "emoji-icon-font".to_owned(),
    ]);

    let mut monospace: Vec<String> = Vec::new();
    if loaded.mono.is_some() {
        monospace.push("neo-mono".to_owned());
    }
    if loaded.cjk.is_some() {
        monospace.push("neo-ui-cjk".to_owned());
    }
    monospace.extend(["Hack".to_owned(), "Ubuntu-Light".to_owned()]);

    let mut bold_chain: Vec<String> = Vec::new();
    if loaded.bold.is_some() {
        bold_chain.push("neo-bold".to_owned());
    }
    if loaded.cjk.is_some() {
        bold_chain.push("neo-bold-cjk".to_owned());
    }
    if loaded.ui.is_some() {
        bold_chain.push("neo-ui".to_owned());
    }
    if loaded.cjk.is_some() {
        bold_chain.push("neo-ui-cjk".to_owned());
    }
    bold_chain.extend(["Ubuntu-Light".to_owned(), "NotoEmoji-Regular".to_owned()]);

    defs.families
        .insert(FontFamily::Proportional, proportional.clone());
    defs.families.insert(FontFamily::Monospace, monospace);
    defs.families.insert(bold(), bold_chain);
    defs.families.insert(mono(), monospace_family(&loaded));

    // ---- KaTeX（数学公式）----
    // 每个字体面单独成一个族：`ratex` 的排版结果给出族名（`Main-Regular` 等），
    // 这里就得有同名族可取。族链尾接正文字体 —— 万一缺字，宁可用正文兜底，
    // 也不要让公式变成一排豆腐块。
    for (name, bytes) in katex::FACES {
        let key = format!("{KATEX_PREFIX}{name}");
        defs.font_data.insert(
            key.clone(),
            Arc::new(FontData {
                font: bytes.to_vec().into(),
                index: 0,
                tweak: FontTweak::default(),
            }),
        );
        let mut chain = vec![key];
        chain.extend(proportional.iter().cloned());
        defs.families.insert(katex_family(name), chain);
    }

    ctx.set_fonts(defs);
    loaded
}

/// KaTeX 族名前缀。加了前缀是为了和内置族（`Proportional`/`Monospace`）
/// 以及上游字体 key 彻底分开，出问题时一眼能看出是数学字体。
const KATEX_PREFIX: &str = "katex:";

/// 这个字体面是否已内嵌（排版器给出的族名逐个核对时用）。
pub fn has_katex_face(name: &str) -> bool {
    katex::FACES.iter().any(|(n, _)| *n == name)
}

/// 取一个 KaTeX 字体族。
///
/// `name` 用 `ratex` 给的族名（`Main-Regular` / `Size2-Regular` …）。
/// **认不出来就退回正文字体** —— 公式会难看到看得见的程度，而不是整块消失。
pub fn katex_family(name: &str) -> FontFamily {
    if katex::FACES.iter().any(|(n, _)| *n == name) {
        FontFamily::Name(format!("{KATEX_PREFIX}{name}").into())
    } else {
        FontFamily::Proportional
    }
}

/// 等宽族链（与 `Monospace` 保持一致）。
fn monospace_family(loaded: &LoadedFonts) -> Vec<String> {
    let mut chain: Vec<String> = Vec::new();
    if loaded.mono.is_some() {
        chain.push("neo-mono".to_owned());
    }
    if loaded.cjk.is_some() {
        chain.push("neo-ui-cjk".to_owned());
    }
    chain.extend(["Hack".to_owned(), "Ubuntu-Light".to_owned()]);
    chain
}
