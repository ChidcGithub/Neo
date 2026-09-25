//! Neo 度量体系。
//!
//! 基准值（`scale = 1.0`）逐条对应 DeepSeek Harness 的组件样式：
//! `InputBar.module.css` / `HeroShell.module.css` / `ConversationRoot.module.css`。
//!
//! Neo 的差异点是**教室大屏**：所有基准值乘以一个统一的 `scale`，
//! 该系数由「物理像素密度」与「观看距离」两个因子共同决定。
//! 这样 1080p 近距与 4K 远距下，界面占据的**视角**保持一致。

/// 观看距离档位 —— 决定额外的放大倍率。
///
/// 教室一体机通常 86~98 吋、学生座位距屏幕 4~10 米，同样字号
/// 在远距下辨识度会骤降，因此需要按距离补偿。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Distance {
    /// 讲台近距离操作 / 普通桌面显示器。
    Standard,
    /// 教室大屏默认档：兼顾后排可读与一屏信息量。
    #[default]
    Classroom,
    /// 阶梯教室 / 报告厅后排。
    Auditorium,
}

impl Distance {
    /// 距离补偿倍率。
    pub const fn factor(self) -> f32 {
        match self {
            Self::Standard => 1.0,
            Self::Classroom => 1.25,
            Self::Auditorium => 1.6,
        }
    }

    /// 供 UI 展示的中文名。
    pub const fn label(self) -> &'static str {
        match self {
            Self::Standard => "近距",
            Self::Classroom => "教室",
            Self::Auditorium => "远距",
        }
    }

    /// 三档循环，供一键切换。
    pub const fn next(self) -> Self {
        match self {
            Self::Standard => Self::Classroom,
            Self::Classroom => Self::Auditorium,
            Self::Auditorium => Self::Standard,
        }
    }
}

// ---------------------------------------------------------------------------
// Harness 基准常量（1x，单位：逻辑像素）
// ---------------------------------------------------------------------------

/// 输入卡圆角（`InputBar .card { border-radius: 22px }`）。
pub const R_BASE_CARD: f32 = 22.0;
/// chip / 下拉控件圆角（`.select { border-radius: 8px }`）。
pub const R_BASE_CHIP: f32 = 8.0;
/// workspace chip 圆角（`.workspace { border-radius: 16px }`）。
pub const R_BASE_WORKSPACE: f32 = 16.0;

/// 输入卡内部纵向间距（`.card { gap: 12px; padding-top: 8px }`）。
pub const BASE_CARD_GAP: f32 = 12.0;
pub const BASE_CARD_PAD_TOP: f32 = 8.0;

/// 输入区行高与上下内边距（`.input { padding: 4px 8px 0 14px }`）。
pub const BASE_LINE_HEIGHT: f32 = 24.0;
pub const BASE_INPUT_PAD_TOP: f32 = 4.0;
pub const BASE_INPUT_PAD_LEFT: f32 = 14.0;
pub const BASE_INPUT_PAD_RIGHT: f32 = 8.0;

/// hero 态两行地板 / 停靠态一行地板（`.hero .input { min-height: 52px }`）。
pub const BASE_INPUT_MIN_HERO: f32 = 52.0;
pub const BASE_INPUT_MIN_DOCKED: f32 = 36.0;

/// 工具栏行（`.row { padding: 2px 8px 6px; gap: 12px }`）。
pub const BASE_TOOLBAR_PAD_TOP: f32 = 2.0;
pub const BASE_TOOLBAR_PAD_BOTTOM: f32 = 6.0;
pub const BASE_TOOLBAR_PAD_X: f32 = 8.0;
pub const BASE_TOOLBAR_GAP: f32 = 12.0;

/// 圆形控件直径（`.add` 28px / `.primary` 34px）。
pub const BASE_BTN_ADD: f32 = 28.0;
pub const BASE_BTN_SEND: f32 = 34.0;
/// 下拉 chip 高度（`.select { height: 28px }`）。
pub const BASE_CHIP_H: f32 = 28.0;
/// chip 的水平内边距（单侧；`.select { padding: 0 20px 0 8px }` 的左侧 8px）。
pub const BASE_CHIP_PAD_X: f32 = 8.0;
/// chip 右侧为折叠箭头预留的额外宽度。
pub const BASE_CHIP_CHEVRON: f32 = 12.0;

/// hero 标题（`.headline { font-size: 26px; line-height: 32px }`）。
pub const BASE_HEADLINE: f32 = 26.0;
pub const BASE_HEADLINE_LH: f32 = 32.0;
/// hero 鲸鱼标志宽度（figma 34×25）。
pub const BASE_FISH: f32 = 34.0;

/// 正文 / 草稿字号（`.card { font-size: 14px; line-height: 24px }`）。
pub const BASE_BODY: f32 = 14.0;
/// chip / 侧栏 / workspace 标签字号（13/20 wt500）。
pub const BASE_LABEL: f32 = 13.0;
pub const BASE_LABEL_LH: f32 = 20.0;
/// 提示 / notice 字号（12/18）。
pub const BASE_CAPTION: f32 = 12.0;

/// 会话内容列宽上界与输入卡上界（`--dsh-chat-content-width` / `+32px`）。
pub const BASE_CONTENT_MAX: f32 = 920.0;
pub const BASE_CARD_MAX: f32 = 952.0;
/// 输入卡侧向留白（`--dsh-composer-side-clearance: 16px`）。
pub const BASE_SIDE_CLEARANCE: f32 = 16.0;
/// hero 纵向堆叠间距（`.stack { gap: 12px }`）。
pub const BASE_STACK_GAP: f32 = 12.0;
/// workspace 行左缩进（`.workspaceRow { padding-left: 8px }`）。
pub const BASE_WORKSPACE_ROW_PAD: f32 = 8.0;

/// 顶栏内边距（`ConversationRoot` 头部 `10px 28px 0 20px`）。
pub const BASE_TOPBAR_PAD: [f32; 4] = [10.0, 28.0, 0.0, 20.0];

// ---------------------------------------------------------------------------
// 触控：视觉尺寸保真，命中区扩展
// ---------------------------------------------------------------------------

/// 教室一体机触控最小命中边长（WCAG 2.5.5 / Material 触控规范）。
///
/// Neo 不放大 Harness 的 28/34px 圆形控件（那会破坏造型比例），
/// 而是把它们的不透明命中区向外扩到该尺寸。
pub const TOUCH_TARGET_MIN: f32 = 48.0;

/// 手指在玻璃上的物理接触半径，用于判定「近距误触」。
pub const TOUCH_SLOP: f32 = 10.0;

/// 缩放后的度量集合。
#[derive(Clone, Copy, Debug)]
pub struct Metrics {
    /// 统一放大系数（已含像素密度与距离补偿）。
    scale: f32,
}

impl Metrics {
    /// 由视口逻辑高度与观看距离推导。
    ///
    /// - 像素密度因子：`height / 1080`，即 1080p→1.0、4K→2.0；
    /// - 距离因子：见 [`Distance::factor`]；
    /// - 最终收敛到 `[0.85, 2.8]`，避免超宽屏或异常 DPI 把界面撑爆。
    pub fn for_viewport(viewport_height: f32, distance: Distance) -> Self {
        let density = (viewport_height / 1080.0).max(0.1);
        let raw = density * distance.factor();
        Self {
            scale: raw.clamp(0.85, 2.8),
        }
    }

    /// 直接用显式系数构造（用于测试与预设）。
    pub fn from_scale(scale: f32) -> Self {
        Self {
            scale: scale.clamp(0.85, 2.8),
        }
    }

    /// 原始放大系数。
    pub fn scale(&self) -> f32 {
        self.scale
    }

    /// 按系数换算任意基准值。
    pub fn s(&self, base: f32) -> f32 {
        base * self.scale
    }

    // ---- 圆角 ----
    /// 输入卡圆角。22px 的「近方圆形」在放大后会显得太圆，因此上限锁定在 28。
    pub fn radius_card(&self) -> f32 {
        self.s(R_BASE_CARD).min(28.0)
    }
    pub fn radius_chip(&self) -> f32 {
        self.s(R_BASE_CHIP).min(12.0)
    }
    pub fn radius_workspace(&self) -> f32 {
        self.s(R_BASE_WORKSPACE).min(22.0)
    }

    // ---- 间距 ----
    pub fn card_gap(&self) -> f32 {
        self.s(BASE_CARD_GAP)
    }
    pub fn card_pad_top(&self) -> f32 {
        self.s(BASE_CARD_PAD_TOP)
    }
    pub fn stack_gap(&self) -> f32 {
        self.s(BASE_STACK_GAP)
    }
    pub fn toolbar_gap(&self) -> f32 {
        self.s(BASE_TOOLBAR_GAP)
    }
    pub fn side_clearance(&self) -> f32 {
        self.s(BASE_SIDE_CLEARANCE)
    }
    pub fn workspace_row_pad(&self) -> f32 {
        self.s(BASE_WORKSPACE_ROW_PAD)
    }

    // ---- 输入区 ----
    pub fn line_height(&self) -> f32 {
        self.s(BASE_LINE_HEIGHT)
    }
    pub fn input_min_hero(&self) -> f32 {
        self.s(BASE_INPUT_MIN_HERO)
    }
    pub fn input_min_docked(&self) -> f32 {
        self.s(BASE_INPUT_MIN_DOCKED)
    }
    /// 输入区最多长到多少像素后开始内滚（Harness `--dsh-composer-text-max-height: 336px`）。
    pub fn input_max_height(&self) -> f32 {
        self.s(336.0)
    }

    // ---- 控件 ----
    pub fn btn_add(&self) -> f32 {
        self.s(BASE_BTN_ADD)
    }
    pub fn btn_send(&self) -> f32 {
        self.s(BASE_BTN_SEND)
    }
    pub fn chip_h(&self) -> f32 {
        self.s(BASE_CHIP_H)
    }
    /// chip 的水平内边距（单侧）。
    pub fn chip_pad_x(&self) -> f32 {
        self.s(BASE_CHIP_PAD_X)
    }
    /// chip 右侧为箭头预留的宽度。
    pub fn chip_chevron(&self) -> f32 {
        self.s(BASE_CHIP_CHEVRON)
    }
    /// 给定文本宽度时 chip 的最小宽度。
    ///
    /// 布局（算宽度）与绘制（算内区）必须共用这一个式子 —— 两边各写一遍
    /// 迟早会因为舍入差出 1e-4，而那个差值恰好能让文本被误判为放不下。
    pub fn chip_width(&self, text_w: f32, chevron: bool) -> f32 {
        text_w + 2.0 * self.chip_pad_x() + if chevron { self.chip_chevron() } else { 0.0 }
    }
    /// 圆形/胶囊控件的命中区边长：视觉尺寸保真，命中区按触控规范下限补足。
    pub fn hit_target(&self, visual: f32) -> f32 {
        visual.max(self.s(TOUCH_TARGET_MIN))
    }
    /// 命中区相对视觉尺寸每侧需要补出的空白。
    pub fn hit_pad(&self, visual: f32) -> f32 {
        (self.hit_target(visual) - visual) * 0.5
    }

    // ---- 尺寸 ----
    /// 会话内容列宽上界。
    ///
    /// 取「基准上界 × scale」与「可用宽度减去两侧各 2 倍 clearance」的较小者：
    /// 既保留上游 920px 的扫读上界，又不会在窄屏上把列宽压到比可用空间还小。
    pub fn content_max(&self, viewport_width: f32) -> f32 {
        let available = viewport_width - 4.0 * self.s(BASE_SIDE_CLEARANCE);
        self.s(BASE_CONTENT_MAX).min(available.max(0.0))
    }
    /// 输入卡上界，始终保持比内容列多 32px（Harness 的 `+32px` 关系）。
    pub fn card_max(&self, viewport_width: f32) -> f32 {
        self.content_max(viewport_width) + self.s(32.0)
    }
    /// 侧栏宽度。Harness 的侧栏承载会话列表；Neo 额外容纳大屏场景入口。
    pub fn sidebar_w(&self) -> f32 {
        self.s(268.0)
    }
    /// 侧栏导航项高度与圆角。
    pub fn nav_item_h(&self) -> f32 {
        self.s(38.0)
    }
    pub fn nav_item_gap(&self) -> f32 {
        self.s(2.0)
    }
    /// 顶栏内边距。
    pub fn topbar_pad(&self) -> [f32; 4] {
        [
            self.s(BASE_TOPBAR_PAD[0]),
            self.s(BASE_TOPBAR_PAD[1]),
            self.s(BASE_TOPBAR_PAD[2]),
            self.s(BASE_TOPBAR_PAD[3]),
        ]
    }
    /// 顶栏高度（内容 + 上下 padding 的下界）。
    pub fn topbar_h(&self) -> f32 {
        self.s(BASE_TOPBAR_PAD[0]) + self.s(40.0)
    }
}

/// 字号 / 行高（已缩放）。
#[derive(Clone, Copy, Debug)]
pub struct Typography {
    pub headline: f32,
    pub headline_lh: f32,
    pub body: f32,
    pub label: f32,
    pub label_lh: f32,
    pub caption: f32,
    pub caption_lh: f32,
    pub fish: f32,
}

impl Typography {
    /// 由度量系数推导字号。
    pub fn new(m: &Metrics) -> Self {
        Self {
            headline: m.s(BASE_HEADLINE),
            headline_lh: m.s(BASE_HEADLINE_LH),
            body: m.s(BASE_BODY),
            label: m.s(BASE_LABEL),
            label_lh: m.s(BASE_LABEL_LH),
            caption: m.s(BASE_CAPTION),
            caption_lh: m.s(18.0),
            fish: m.s(BASE_FISH),
        }
    }
}
