//! # neo-theme
//!
//! Neo 的设计系统。三层结构：
//!
//! 1. [`palette`] —— 语义色板，逐条对应 DeepSeek Harness 的 `--dsw-*` token；
//! 2. [`metrics`] —— 度量与字号，基准值取自 Harness 组件 CSS，再按
//!    「像素密度 × 观看距离」统一放大（Neo 面向教室大屏的核心差异）；
//! 3. [`squircle`] —— Harness 的 `corner-shape: superellipse(1.5)`，
//!    egui 原生只有正圆圆角，这里自行生成超椭圆路径。
//!
//! 上游规范里 `--dsw-alias-brand-primary` 解析为「墨色」而非蓝色，
//! 真正的强调蓝是 `--dsw-alias-button-info-fill`。这一点容易踩坑，已在
//! [`Palette::brand_ink`] 与 [`Palette::accent`] 上分别标注。

pub mod fonts;
pub mod metrics;
pub mod palette;
pub mod squircle;
pub mod svg;

pub use egui::FontFamily;
pub use metrics::{Distance, Metrics, Typography};
pub use palette::Palette;
pub use squircle::HARNESS_SUPERELLIPSE;

use egui::{
    epaint::{PathShape, PathStroke},
    Color32, Context, CornerRadius, Painter, Rect, Shape, Stroke, StrokeKind, TextStyle,
};

/// 明暗主题。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ThemeMode {
    /// Harness 的 `body[data-ds-dark-theme]`。
    #[default]
    Dark,
    /// Harness 的默认（浅色）主题。
    Light,
}

impl ThemeMode {
    /// 供 UI 展示的中文名。
    pub const fn label(self) -> &'static str {
        match self {
            Self::Dark => "暗色",
            Self::Light => "亮色",
        }
    }

    /// 一键切换。
    pub const fn toggled(self) -> Self {
        match self {
            Self::Dark => Self::Light,
            Self::Light => Self::Dark,
        }
    }

    /// 解析色板。
    pub const fn palette(self) -> Palette {
        match self {
            Self::Dark => Palette::DARK,
            Self::Light => Palette::LIGHT,
        }
    }
}

/// 一套完整的主题：色板 + 度量 + 字号，三者由同一个 `scale` 派生。
#[derive(Clone, Copy, Debug)]
pub struct Theme {
    pub mode: ThemeMode,
    pub palette: Palette,
    pub metrics: Metrics,
    pub typo: Typography,
}

impl Theme {
    /// 由主题模式、视口逻辑高度与观看距离构造。
    pub fn new(mode: ThemeMode, viewport_height: f32, distance: Distance) -> Self {
        let metrics = Metrics::for_viewport(viewport_height, distance);
        Self::from_metrics(mode, metrics)
    }

    /// 由已有度量构造（用于保持 scale 稳定，避免窗口拖动时抖动）。
    pub fn from_metrics(mode: ThemeMode, metrics: Metrics) -> Self {
        Self {
            mode,
            palette: mode.palette(),
            metrics,
            typo: Typography::new(&metrics),
        }
    }

    /// 把主题注入 egui 全局样式。
    ///
    /// 只做「原生控件对齐」这一件事：Neo 的主界面全部由自己绘制，
    /// 但 `TextEdit` 的选择高亮、光标、滚动条仍然来自 egui，需要一起改。
    pub fn apply(&self, ctx: &Context) {
        let p = &self.palette;
        let theme = match self.mode {
            ThemeMode::Dark => egui::Theme::Dark,
            ThemeMode::Light => egui::Theme::Light,
        };
        // 大屏是固定使用场景，主题由 Neo 自己决定，不跟随系统。
        ctx.set_theme(theme);

        let mut visuals = match self.mode {
            ThemeMode::Dark => egui::Visuals::dark(),
            ThemeMode::Light => egui::Visuals::light(),
        };

        visuals.panel_fill = p.bg_base;
        visuals.window_fill = p.bg_layer_1;
        visuals.extreme_bg_color = p.bg_layer_1;
        visuals.faint_bg_color = p.hover;
        visuals.text_edit_bg_color = Some(Color32::TRANSPARENT);
        visuals.override_text_color = Some(p.label_primary);
        visuals.hyperlink_color = p.link;
        visuals.warn_fg_color = p.warn;
        visuals.error_fg_color = p.error;

        visuals.selection.bg_fill = p.accent.gamma_multiply(0.35);
        visuals.selection.stroke = Stroke::new(1.0, p.accent);

        // Harness 里光标用的是业务蓝，不是品牌色。
        visuals.text_cursor.stroke = Stroke::new(2.0, p.accent);

        let radius = CornerRadius::same(self.metrics.radius_chip().round() as u8);
        visuals.widgets.noninteractive.corner_radius = radius;
        visuals.widgets.noninteractive.bg_fill = p.bg_layer_2;
        visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, p.border_l2);
        visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, p.label_primary);

        visuals.widgets.inactive.corner_radius = radius;
        visuals.widgets.inactive.weak_bg_fill = p.hover;
        visuals.widgets.inactive.bg_fill = p.hover;
        visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, Color32::TRANSPARENT);
        visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, p.label_secondary);

        visuals.widgets.hovered.corner_radius = radius;
        visuals.widgets.hovered.weak_bg_fill = p.hover;
        visuals.widgets.hovered.bg_fill = p.hover_solid;
        visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, p.border_l2);
        visuals.widgets.hovered.fg_stroke = Stroke::new(1.0, p.label_primary);

        visuals.widgets.active.corner_radius = radius;
        visuals.widgets.active.weak_bg_fill = p.active;
        visuals.widgets.active.bg_fill = p.active;
        visuals.widgets.active.bg_stroke = Stroke::new(1.0, p.border_l3);
        visuals.widgets.active.fg_stroke = Stroke::new(1.0, p.label_primary);

        visuals.widgets.open.corner_radius = radius;

        ctx.set_visuals_of(theme, visuals);

        // 文本样式表 + 原生滚动条：egui 内置控件（TextEdit 等）按这些值走。
        let metrics = self.metrics;
        let typo = self.typo;
        ctx.style_mut_of(theme, |style| {
            style.spacing.scroll.bar_width = metrics.s(6.0);
            style.spacing.scroll.floating = true;

            style.text_styles.insert(
                TextStyle::Body,
                egui::FontId::new(typo.body, FontFamily::Proportional),
            );
            style.text_styles.insert(
                TextStyle::Button,
                egui::FontId::new(typo.label, fonts::bold()),
            );
            style.text_styles.insert(
                TextStyle::Small,
                egui::FontId::new(typo.caption, FontFamily::Proportional),
            );
            style.text_styles.insert(
                TextStyle::Heading,
                egui::FontId::new(typo.headline, FontFamily::Proportional),
            );
            style.text_styles.insert(
                TextStyle::Monospace,
                egui::FontId::new(typo.body, fonts::mono()),
            );
        });
    }
}

/// 超椭圆圆角绘制扩展。
///
/// egui 的 `rect_filled` 只能画正圆角；设计规范要的是
/// `superellipse(1.5)`，因此这里补一条路径绘制通道。
pub trait SquirclePaint {
    /// 填充一个超椭圆圆角矩形。
    fn squircle_filled(&self, rect: Rect, radius: f32, fill: impl Into<Color32>);
    /// 描边一个超椭圆圆角矩形（描边压在边界内侧）。
    fn squircle_stroked(&self, rect: Rect, radius: f32, stroke: impl Into<Stroke>);
    /// 同时填充与描边，保证描边覆盖在填充之上。
    fn squircle(
        &self,
        rect: Rect,
        radius: f32,
        fill: impl Into<Color32>,
        stroke: impl Into<Stroke>,
    );
}

impl SquirclePaint for Painter {
    fn squircle_filled(&self, rect: Rect, radius: f32, fill: impl Into<Color32>) {
        if rect.width() <= 0.0 || rect.height() <= 0.0 {
            return;
        }
        let pts = squircle::squircle_points(
            rect,
            radius,
            HARNESS_SUPERELLIPSE,
            squircle::DEFAULT_SEGMENTS,
        );
        self.add(Shape::Path(PathShape::convex_polygon(
            pts,
            fill.into(),
            PathStroke::NONE,
        )));
    }

    fn squircle_stroked(&self, rect: Rect, radius: f32, stroke: impl Into<Stroke>) {
        if rect.width() <= 0.0 || rect.height() <= 0.0 {
            return;
        }
        let stroke = stroke.into();
        if stroke.width <= 0.0 || stroke.color == Color32::TRANSPARENT {
            return;
        }
        let pts = squircle::squircle_points(
            rect,
            radius,
            HARNESS_SUPERELLIPSE,
            squircle::DEFAULT_SEGMENTS,
        );
        let mut path = PathShape::closed_line(pts, stroke);
        path.stroke.kind = StrokeKind::Inside;
        self.add(Shape::Path(path));
    }

    fn squircle(
        &self,
        rect: Rect,
        radius: f32,
        fill: impl Into<Color32>,
        stroke: impl Into<Stroke>,
    ) {
        self.squircle_filled(rect, radius, fill);
        self.squircle_stroked(rect, radius, stroke);
    }
}
