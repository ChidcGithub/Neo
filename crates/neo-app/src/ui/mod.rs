//! Neo 主界面的绘制层。
//!
//! 界面对齐 Harness 的 `ConversationRoot` 骨架：左侧会话栏 + 右侧主区；
//! 主区在「空态 hero」与「对话态」之间切换，输入卡（composer）在两种状态下共用。
//!
//! ## 组件归口
//!
//! **本层不再持有任何控件实现。** 所有可复用控件（按钮 / Chip / 对话框 /
//! 表单 / 列表行 / 反馈态）都收在 [`neo_ui`]；页面只做编排与业务状态。
//! 这里保留的是两类东西：
//!
//! 1. [`Skin`]：设计上下文 + 品牌资源的合体，供一次绘制共享；
//! 2. 少数以 `Skin` 为参的转发函数（新代码优先直接用 `neo_ui::`）。
//!
//! 凡是"以后还会再用"的控件，写到 `neo-ui`，不要写进页面。

pub mod composer;
pub mod conversation;
pub mod hero;
pub mod markdown;
pub mod math;
pub mod settings;
pub mod sidebar;
pub mod tools;

use neo_theme::Theme;
use neo_ui::Design;

use crate::brand::WhaleMark;

/// 输入卡的固定 id。
///
/// 空态与对话态共用同一个 id：同一时刻只会有其中一个被绘制，
/// 而复用 id 能让「在空态打完字 → 发送 → 切到对话态」时输入焦点不中断。
pub const COMPOSER_ID: &str = "neo-composer";

/// 当前帧是否有输入法（中文拼音 / 日文假名 / 韩文）事件。
///
/// 输入法组合期间，`winit` 会把按键交给输入法，组合中的字以
/// [`egui::Event::Ime`] 逐帧送进来，选词确认那一下是 `Ime::Commit`。
/// 这些事件出现的帧里，Enter 的归属是「输入法」而不是「发送」——
/// 用 Enter 选词却把消息发出去（或把草稿里的拼音原样发走），
/// 是中文用户最容易踩的坑。
pub fn ime_active(ctx: &egui::Context) -> bool {
    ctx.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Ime(_))))
}

/// 一次绘制共享的「皮肤」：设计上下文 + 品牌资源。
///
/// `Design` 来自 [`neo_ui`]，持有色板 / 度量 / 字号 / 组件 token；
/// `whale` 是应用级品牌资源，组件库不携带。
#[derive(Clone, Copy)]
pub struct Skin<'a> {
    pub design: Design,
    pub whale: &'a WhaleMark,
}

impl<'a> Skin<'a> {
    pub fn new(theme: Theme, whale: &'a WhaleMark) -> Self {
        Self {
            design: Design::new(theme),
            whale,
        }
    }

    /// 组件库的设计上下文（绝大多数控件只需要它）。
    pub fn d(&self) -> Design {
        self.design
    }

    // ---- 便捷取用：等价于 design 的同名方法 ----
    pub fn p(&self) -> neo_theme::Palette {
        self.design.p()
    }
    pub fn m(&self) -> neo_theme::Metrics {
        self.design.m()
    }
    pub fn t(&self) -> neo_theme::Typography {
        self.design.t()
    }
    pub fn prop(&self, size: f32) -> egui::FontId {
        self.design.font(size)
    }
    pub fn bold(&self, size: f32) -> egui::FontId {
        self.design.font_bold(size)
    }
    pub fn mono(&self, size: f32) -> egui::FontId {
        self.design.font_mono(size)
    }
}

// ---- 组件库转发：让页面 `use crate::ui::…` 一处拿到常用接口 ----

pub use neo_ui::{
    at, bottom_fade, ease, elide, hover_area, inset, tap, text_center, text_left, translucent,
    State,
};

/// 区块小标题（`Design` → `Skin` 包装）。
pub fn section_label(painter: &egui::Painter, skin: &Skin<'_>, rect: egui::Rect, label: &str) {
    neo_ui::section_label(painter, &skin.design, rect, label);
}
/// 1px 分隔线（`Design` → `Skin` 包装）。
pub fn divider(painter: &egui::Painter, skin: &Skin<'_>, rect: egui::Rect) {
    neo_ui::divider(painter, &skin.design, rect);
}
/// 卡片投影（`Design` → `Skin` 包装）。
pub fn elevation_soft(skin: &Skin<'_>) -> egui::epaint::Shadow {
    neo_ui::elevation_soft(&skin.design)
}
/// 分段控件（`Design` → `Skin` 包装）。
pub fn segmented(
    ui: &mut egui::Ui,
    skin: &Skin<'_>,
    width: f32,
    options: &[&str],
    selected: usize,
) -> Option<usize> {
    neo_ui::Segmented::new(options, selected).show(ui, &skin.design, width)
}
