//! Neo 调色板。
//!
//! 数值直接取自 DeepSeek Harness 的 `packages/client/ui-theme/src/styles/design-platform.css`
//! 语义 token（`--dsw-*`），保留原始的 light / dark 两套取值，未做二次调色。
//!
//! 命名保留了 Harness 的语义分层（base / layer / label / border / interactive），
//! 便于对照上游规范继续演进。

use egui::Color32;

/// 不透明 RGB。
pub const fn rgb(r: u8, g: u8, b: u8) -> Color32 {
    Color32::from_rgb(r, g, b)
}

/// 预乘 alpha 的 RGBA —— egui 的 `Color32` 期望预乘值。
pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Color32 {
    Color32::from_rgba_premultiplied(
        (r as u32 * a as u32 / 255) as u8,
        (g as u32 * a as u32 / 255) as u8,
        (b as u32 * a as u32 / 255) as u8,
        a,
    )
}

/// 白色叠加层（Harness 用 `rgba(255,255,255,α)` 表达暗色下的 hover/描边）。
pub const fn white_a(a: u8) -> Color32 {
    rgba(255, 255, 255, a)
}

/// 黑色叠加层（浅色主题下的 hover / 描边）。
pub const fn black_a(a: u8) -> Color32 {
    rgba(0, 0, 0, a)
}

/// DeepSeek 品牌蓝阶梯（`--dsw-static-deepseek-*`）。
pub mod deepseek {
    use super::{rgb, Color32};

    pub const D50: Color32 = rgb(237, 243, 254);
    pub const D100: Color32 = rgb(228, 237, 253);
    pub const D200: Color32 = rgb(211, 226, 255);
    pub const D300: Color32 = rgb(183, 200, 254);
    pub const D400: Color32 = rgb(103, 158, 254);
    pub const D450: Color32 = rgb(86, 134, 254);
    pub const D500: Color32 = rgb(65, 118, 230);
    pub const D600: Color32 = rgb(72, 104, 178);
    pub const D800: Color32 = rgb(52, 65, 91);
}

/// 上游 `--dsw-static-neutral-bluish-*` 中性偏蓝阶梯（组件 token 的取值来源）。
///
/// 单独抽出来是因为组件级 token 在上游是 `var(--dsw-static-…)` 引用而非字面量，
/// 必须保留这一层才能"改一处、两边同变"。
pub mod neutral {
    use super::{rgb, Color32};

    pub const N_00: Color32 = rgb(255, 255, 255);
    pub const N_50: Color32 = rgb(249, 250, 251);
    pub const N_60: Color32 = rgb(249, 250, 251);
    pub const N_75: Color32 = rgb(241, 243, 245);
    pub const N_100: Color32 = rgb(235, 238, 242);
    pub const N_150: Color32 = rgb(233, 236, 242);
    pub const N_200: Color32 = rgb(225, 229, 238);
    pub const N_300: Color32 = rgb(207, 211, 214);
    pub const N_400: Color32 = rgb(173, 178, 184);
    pub const N_500: Color32 = rgb(151, 157, 166);
    pub const N_600: Color32 = rgb(129, 133, 140);
    pub const N_700: Color32 = rgb(97, 102, 107);
    pub const N_750: Color32 = rgb(67, 69, 74);
    pub const N_800: Color32 = rgb(53, 54, 56);
    pub const N_850: Color32 = rgb(44, 44, 46);
    /// `--dsw-static-neutral-bluish-875`：暗色下 `bg-layer-1` 的实际取值。
    pub const N_875: Color32 = rgb(35, 35, 36);
    pub const N_900: Color32 = rgb(27, 27, 28);
    pub const N_950: Color32 = rgb(21, 21, 23);
    pub const N_1000: Color32 = rgb(15, 17, 21);

    /// `--dsw-static-neutral-50/800`：markdown 行内代码底。
    pub const NEUTRAL_50: Color32 = rgb(250, 250, 250);
    pub const NEUTRAL_800: Color32 = rgb(41, 41, 41);

    // ---- 状态色阶梯 ----
    pub const RED_400: Color32 = rgb(242, 90, 90);
    pub const RED_600: Color32 = rgb(236, 19, 19);
    pub const GREEN_400: Color32 = rgb(78, 209, 126);
    pub const GREEN_500: Color32 = rgb(34, 197, 94);
    pub const AMBER_400: Color32 = rgb(247, 173, 49);
    pub const AMBER_500: Color32 = rgb(245, 158, 11);
    pub const AMBER_600: Color32 = rgb(221, 134, 41);
    pub const BLUE_900: Color32 = rgb(14, 48, 116);

    /// `--dsw-static-deepseek-50`
    pub const DS_50: Color32 = rgb(237, 243, 254);
}

/// 组件级 token —— 上游 `--dsw-alias-button-*` / `bg-mask-*` / `bg-overlay` / `toast-bg`
/// 等的语义封装。
///
/// [`Palette`] 负责"基底表面与文字"，这里负责"控件怎么长"：
/// 按钮的三种填充、遮罩的四个层级、浮层底色。分开的理由是
/// 前者被几乎所有绘制调用到，后者只在组件库里出现。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Components {
    // ---- 按钮 ----
    /// `--dsw-alias-button-primary-fill`：主按钮（= brand-primary，上游是墨色）。
    pub btn_primary: Color32,
    /// `--dsw-alias-button-primary-hover`
    pub btn_primary_hover: Color32,
    /// `--dsw-alias-button-primary-dimmed`：主按钮禁用态。
    pub btn_primary_dimmed: Color32,
    /// `--dsw-alias-button-elevated-fill`：次级按钮（抬升表面）。
    pub btn_elevated: Color32,
    /// `--dsw-alias-button-contrast-fill`：反色按钮（深底浅字 / 浅底深字）。
    pub btn_contrast: Color32,
    /// `--dsw-alias-button-floating-fill` / `-hover`：浮动按钮（工具条上的圆钮）。
    pub btn_floating: Color32,
    pub btn_floating_hover: Color32,
    /// `--dsw-alias-button-info-fill` / `-hover`：业务蓝按钮（发送）。
    pub btn_info: Color32,
    pub btn_info_hover: Color32,
    /// `--dsw-alias-button-ghost-active-fill` / `-border` / `-hover`：幽灵按钮选中态。
    pub btn_ghost_active: Color32,
    pub btn_ghost_border: Color32,
    pub btn_ghost_active_hover: Color32,
    /// 业务蓝按钮上的前景色（发送按钮）。
    pub on_info: Color32,
    /// 危险红按钮上的前景色。
    pub on_danger: Color32,
    /// 主按钮/反色按钮上的前景色（`label-primary-foreground`）。
    pub on_primary: Color32,
    /// 反色按钮上的前景色（`label-primary-inverted`）。
    pub on_contrast: Color32,

    // ---- 遮罩 / 浮层 ----
    /// `--dsw-alias-bg-mask-1`：模态遮罩（最重）。
    pub mask_modal: Color32,
    /// `--dsw-alias-bg-mask-2`：轻度遮罩。
    pub mask_soft: Color32,
    /// `--dsw-alias-bg-mask-3`：图库 / 沉浸态遮罩。
    pub mask_deep: Color32,
    /// `--dsw-alias-bg-overlay`：浮层底（popover / menu 之外的轻浮层）。
    pub overlay: Color32,
    /// `--dsw-alias-toast-bg`
    pub toast: Color32,
    /// `--dsw-alias-tooltip-bg`
    pub tooltip: Color32,
    /// `--dsw-alias-border-inverted`：反色块上的描边。
    pub border_inverted: Color32,

    // ---- 状态语义 ----
    pub error: Color32,
    pub error_soft: Color32,
    pub success: Color32,
    pub success_soft: Color32,
    pub warn: Color32,
    pub warn_label: Color32,
    pub warn_soft: Color32,
    pub business: Color32,
    pub business_soft: Color32,

    // ---- 交互态（组件内的 hover / active 底） ----
    pub hover: Color32,
    pub hover_solid: Color32,
    pub hover_danger: Color32,
    pub hover_accent: Color32,
    pub active: Color32,

    // ---- markdown ----
    pub code_inline: Color32,
    pub code_block: Color32,
    pub code_banner: Color32,
    pub citation: Color32,
    pub md_tag: Color32,
    pub placeholder: Color32,

    // ---- 侧栏 ----
    pub nav_hover: Color32,
    pub nav_active: Color32,
}

impl Components {
    /// 亮色：逐条对应上游 `body { }` 的组件 token。
    pub const LIGHT: Self = Self {
        btn_primary: crate::palette::neutral::N_1000, // = alias-brand-primary（墨色）
        btn_primary_hover: neutral::N_750,
        btn_primary_dimmed: neutral::N_100,
        btn_elevated: neutral::N_00,
        btn_contrast: neutral::N_700,
        btn_floating: neutral::N_00,
        btn_floating_hover: neutral::N_75,
        btn_info: crate::palette::deepseek::D500,
        btn_info_hover: crate::palette::deepseek::D400,
        btn_ghost_active: neutral::N_100,
        btn_ghost_border: neutral::N_500,
        btn_ghost_active_hover: neutral::N_150,
        on_info: neutral::N_00,
        on_danger: neutral::N_00,
        on_primary: neutral::N_00,
        on_contrast: neutral::N_00,

        mask_modal: rgba(0, 0, 0, 61), // 0.24
        mask_soft: rgba(0, 0, 0, 31),  // 0.12
        mask_deep: rgba(0, 0, 0, 122), // 0.48
        overlay: neutral::N_150,
        toast: neutral::N_800,
        tooltip: neutral::N_850,
        border_inverted: rgba(0, 0, 0, 0),

        error: neutral::RED_600,
        error_soft: rgba(236, 19, 19, 13), // .05
        success: neutral::GREEN_500,
        success_soft: rgba(34, 197, 94, 26),
        warn: neutral::AMBER_500,
        warn_label: neutral::AMBER_600,
        warn_soft: rgba(245, 158, 11, 26),
        business: crate::palette::deepseek::D500,
        business_soft: crate::palette::deepseek::D100,

        hover: rgba(38, 49, 72, 15), // .06
        hover_solid: neutral::N_75,
        hover_danger: rgba(236, 19, 19, 13),
        hover_accent: rgba(38, 49, 72, 36), // .14
        active: rgba(38, 49, 72, 26),       // .10

        code_inline: neutral::NEUTRAL_50,
        code_block: neutral::N_50,
        code_banner: neutral::N_50,
        citation: neutral::N_100,
        md_tag: neutral::N_75,
        placeholder: neutral::N_60,

        nav_hover: neutral::N_75,
        nav_active: neutral::N_100,
    };

    /// 暗色：逐条对应上游 `body[data-ds-dark-theme] { }`。
    pub const DARK: Self = Self {
        btn_primary: crate::palette::neutral::N_00, // = alias-brand-primary（暗色下是白）
        btn_primary_hover: neutral::N_100,
        btn_primary_dimmed: neutral::N_750,
        btn_elevated: neutral::N_750,
        btn_contrast: neutral::N_50,
        btn_floating: neutral::N_850,
        btn_floating_hover: neutral::N_800,
        btn_info: crate::palette::deepseek::D400,
        btn_info_hover: crate::palette::deepseek::D500,
        btn_ghost_active: neutral::N_750,
        btn_ghost_border: neutral::N_600,
        btn_ghost_active_hover: neutral::N_700,
        on_info: neutral::N_1000,
        on_danger: neutral::N_00,
        on_primary: neutral::N_00,
        on_contrast: neutral::N_1000,

        mask_modal: rgba(0, 0, 0, 128), // 0.5
        mask_soft: rgba(0, 0, 0, 51),   // 0.2
        mask_deep: rgba(0, 0, 0, 122),  // 0.48
        overlay: neutral::N_700,
        toast: neutral::N_750,
        tooltip: neutral::N_750,
        border_inverted: rgba(255, 255, 255, 15),

        error: neutral::RED_400,
        error_soft: rgba(242, 90, 90, 38), // .15
        success: neutral::GREEN_500,
        success_soft: rgba(34, 197, 94, 38),
        warn: neutral::AMBER_500,
        warn_label: neutral::AMBER_600,
        warn_soft: rgba(245, 158, 11, 38),
        business: crate::palette::deepseek::D400,
        business_soft: crate::palette::deepseek::D800,

        hover: rgba(255, 255, 255, 20), // .08
        hover_solid: neutral::N_800,
        hover_danger: rgba(242, 90, 90, 38),
        hover_accent: rgba(255, 255, 255, 61), // .24
        active: rgba(255, 255, 255, 36),       // .14

        code_inline: neutral::NEUTRAL_800,
        code_block: neutral::N_900,
        code_banner: neutral::N_850,
        citation: neutral::N_800,
        md_tag: neutral::N_850,
        placeholder: neutral::N_850,

        nav_hover: neutral::N_850,
        nav_active: neutral::N_750,
    };
}

impl Palette {
    /// 该色板对应的组件 token。
    ///
    /// 判别明暗看 `bg_base` 的亮度：暗色底 < 中灰即取暗色组件 token。
    /// 不依赖某个具体 RGB，避免未来微调基底值后判别失效。
    pub const fn components(self) -> Components {
        let l = self.bg_base.r() as u32 + self.bg_base.g() as u32 + self.bg_base.b() as u32;
        if l < 3 * 128 {
            Components::DARK
        } else {
            Components::LIGHT
        }
    }
}

/// 一套完整的语义色板。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Palette {
    // ---- 表面 / 层级 ----
    /// `--dsw-alias-bg-base`：应用最底层背景。
    pub bg_base: Color32,
    /// `--dsw-alias-bg-layer-1/2/3`：抬升层级（弹层、菜单、卡片）。
    pub bg_layer_1: Color32,
    pub bg_layer_2: Color32,
    pub bg_layer_3: Color32,
    /// `--dsw-specific-sidebar-fill`：侧栏底色。
    pub sidebar_fill: Color32,
    /// `--dsw-specific-input-major`：输入卡表面。
    pub input_surface: Color32,
    /// `--dsw-specific-selector`：圆形控件 / 选择器填充。
    pub selector: Color32,
    /// `--dsw-specific-bubble`：消息气泡底。
    pub bubble: Color32,
    /// `--dsw-specific-tip` / 次级区块底。
    pub tip: Color32,

    // ---- 文字 ----
    /// `--dsw-alias-label-primary`
    pub label_primary: Color32,
    /// `--dsw-alias-label-secondary`
    pub label_secondary: Color32,
    /// `--dsw-alias-label-tertiary`
    pub label_tertiary: Color32,
    /// `--dsw-alias-label-caption`
    pub label_caption: Color32,
    /// `--dsw-alias-label-primary-foreground`：填充块上的反色文字。
    pub label_on_accent: Color32,

    // ---- 品牌 / 强调 ----
    /// `--dsw-alias-brand-primary`：该 token 在本套规范里解析为「墨色」（不是蓝）。
    pub brand_ink: Color32,
    /// `--dsw-alias-button-info-fill`：发送按钮 / 主强调蓝。
    pub accent: Color32,
    /// `--dsw-alias-button-info-hover`
    pub accent_hover: Color32,
    /// `--dsw-alias-state-business-tertiary`：强调蓝的弱底。
    pub accent_soft: Color32,
    /// `--dsw-alias-link`
    pub link: Color32,

    // ---- 状态 ----
    pub success: Color32,
    pub warn: Color32,
    pub error: Color32,

    // ---- 描边 ----
    pub border_l1: Color32,
    pub border_l2: Color32,
    pub border_l3: Color32,
    pub border_l4: Color32,

    // ---- 交互态 ----
    /// `--dsw-alias-interactive-bg-hover`
    pub hover: Color32,
    /// `--dsw-alias-interactive-bg-hover-solid`
    pub hover_solid: Color32,
    /// `--dsw-alias-interactive-bg-active`
    pub active: Color32,
    /// 侧栏导航项 hover / active
    pub nav_hover: Color32,
    pub nav_active: Color32,

    // ---- 杂项 ----
    pub scrollbar: Color32,
    pub scrollbar_hover: Color32,
    pub tooltip_bg: Color32,
}

impl Palette {
    /// 暗色主题 —— 与 Harness `body[data-ds-dark-theme]` 一一对应。
    ///
    /// 层次方向容易记反：**暗色下 `bg-layer-1` 比 `bg-layer-2` 更暗**（875 → 850 → 800），
    /// 越靠近用户的表面越亮。
    pub const DARK: Self = Self {
        bg_base: neutral::N_950,
        bg_layer_1: neutral::N_875,
        bg_layer_2: neutral::N_850,
        bg_layer_3: neutral::N_800,
        sidebar_fill: rgb(27, 27, 28),
        input_surface: neutral::N_850,
        selector: neutral::N_800,
        bubble: neutral::N_850,
        tip: neutral::N_800,

        label_primary: neutral::N_50,
        label_secondary: neutral::N_300,
        label_tertiary: neutral::N_400,
        label_caption: neutral::N_600,
        label_on_accent: neutral::N_1000,

        brand_ink: neutral::N_50,
        accent: deepseek::D400,
        accent_hover: deepseek::D500,
        accent_soft: deepseek::D800,
        link: deepseek::D400,

        success: neutral::GREEN_500,
        warn: neutral::AMBER_500,
        error: neutral::RED_400,

        border_l1: white_a(15),
        border_l2: white_a(31),
        border_l3: white_a(41),
        border_l4: white_a(51),

        hover: white_a(20),
        hover_solid: neutral::N_800,
        active: white_a(36),
        nav_hover: neutral::N_850,
        nav_active: neutral::N_750,

        scrollbar: rgb(60, 60, 61),
        scrollbar_hover: rgb(84, 85, 87),
        tooltip_bg: neutral::N_750,
    };

    /// 亮色主题 —— 与 Harness 默认（无 `data-ds-dark-theme`）一一对应。
    /// 教室大屏常在明亮环境下使用，因此这一套是首等公民而非附属。
    pub const LIGHT: Self = Self {
        bg_base: neutral::N_00,
        bg_layer_1: neutral::N_00,
        bg_layer_2: neutral::N_00,
        bg_layer_3: neutral::N_00,
        sidebar_fill: rgb(249, 250, 251),
        input_surface: neutral::N_00,
        selector: neutral::N_60,
        bubble: deepseek::D50,
        tip: neutral::N_60,

        label_primary: neutral::N_1000,
        label_secondary: neutral::N_700,
        label_tertiary: neutral::N_600,
        label_caption: neutral::N_400,
        label_on_accent: neutral::N_00,

        brand_ink: neutral::N_1000,
        accent: deepseek::D500,
        accent_hover: deepseek::D400,
        accent_soft: deepseek::D100,
        link: deepseek::D500,

        success: neutral::GREEN_500,
        warn: neutral::AMBER_500,
        error: neutral::RED_600,

        border_l1: black_a(10),
        border_l2: black_a(26),
        border_l3: black_a(31),
        border_l4: black_a(41),

        hover: rgba(38, 49, 72, 15),
        hover_solid: neutral::N_75,
        active: rgba(38, 49, 72, 26),
        nav_hover: neutral::N_75,
        nav_active: neutral::N_100,

        scrollbar: rgb(229, 229, 229),
        scrollbar_hover: rgb(212, 212, 212),
        tooltip_bg: neutral::N_850,
    };

    /// 适用于教室大屏的默认主题：亮环境优先选亮色。
    pub const fn for_surface(is_dark_room: bool) -> Self {
        if is_dark_room {
            Self::DARK
        } else {
            Self::LIGHT
        }
    }
}
