//! 设计上下文：组件库对外的唯一入口。

use egui::FontId;
use neo_theme::{fonts, Metrics, Palette, Theme, Typography};

/// 一次绘制所需的全部"设计上下文"。
///
/// 页面把当前 [`Theme`] 包一层得到它，然后传给所有组件。
/// 组件只认 `Design` —— 这保证同一帧里所有控件用的是同一套色板与度量，
/// 不会出现"侧栏是暗色、主区是亮色"这类脏状态。
///
/// 不持有品牌资源：鲸鱼标志这类**应用级素材**留在 app 侧，
/// 组件库只关心"造型系统"（色 / 度量 / 字号 / 形状），不关心"画的是哪个 logo"。
#[derive(Clone, Copy)]
pub struct Design {
    /// 色板 + 度量 + 字号（三者同源）。
    pub theme: Theme,
}

/// 通用尺寸档。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Size {
    /// 紧凑（工具条 / 行内动作）。
    Sm,
    /// 常规。
    #[default]
    Md,
    /// 大（主操作 / 触控优先位）。
    Lg,
}

impl Design {
    pub fn new(theme: Theme) -> Self {
        Self { theme }
    }

    /// 表面语义色板（`bg-*` / `label-*` / `border-*`）。
    pub fn palette(&self) -> Palette {
        self.theme.palette
    }

    /// 组件级 token（`button-*` / `bg-mask-*` / `state-*` …）。
    ///
    /// **组件颜色一律从这里取**，不从 [`Palette`] 猜 —— 两者的取值来源不同
    /// （`Palette` 是表面层，`Components` 是控件层），混用会在明暗主题下跑偏。
    pub fn comps(&self) -> neo_theme::palette::Components {
        self.theme.palette.components()
    }

    /// 度量（已按视口与观看距离缩放）。
    pub fn metrics(&self) -> Metrics {
        self.theme.metrics
    }

    /// 字号。
    pub fn typo(&self) -> Typography {
        self.theme.typo
    }

    /// 常规字重。
    pub fn font(&self, size: f32) -> FontId {
        FontId::new(size, egui::FontFamily::Proportional)
    }
    /// 粗字重（对应上游 `font-weight: 500`）。
    pub fn font_bold(&self, size: f32) -> FontId {
        FontId::new(size, fonts::bold())
    }
    /// 等宽（版本号、路径、快捷键）。
    pub fn font_mono(&self, size: f32) -> FontId {
        FontId::new(size, fonts::mono())
    }

    /// 快捷：表面色板别名（少敲 `.theme`）。
    pub fn p(&self) -> Palette {
        self.palette()
    }
    /// 快捷：度量别名。
    pub fn m(&self) -> Metrics {
        self.metrics()
    }
    /// 快捷：字号别名。
    pub fn t(&self) -> Typography {
        self.typo()
    }
    /// 快捷：组件 token 别名。
    pub fn c(&self) -> neo_theme::palette::Components {
        self.comps()
    }

    /// 是否暗色主题（决定投影/遮罩的观感取向）。
    pub fn is_dark(&self) -> bool {
        self.theme.mode == neo_theme::ThemeMode::Dark
    }
}
