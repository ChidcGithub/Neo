//! # neo-ui —— Neo 的组件库
//!
//! 把"界面长什么样"从每个页面里抽出来，收成**一套可复用的设计接口**。
//! 目标很直接：以后加一个按钮、开一个对话框，只调用这里的组件，
//! 不再各自手搓一遍绘制代码 —— 也就不再各自跑偏。
//!
//! ## 分层
//!
//! ```text
//! neo-theme   语义 token（色板 / 度量 / 字号 / 超椭圆）   ← 数值的唯一来源
//!    ↓
//! neo-ui      组件（本 crate）                          ← 造型与交互的唯一来源
//!    ↓
//! neo-app     页面（hero / conversation / settings …）   ← 只做编排与业务
//! ```
//!
//! ## 用起来是什么样
//!
//! ```no_run
//! # use neo_ui::{Design, Button, Icon};
//! # fn demo(ui: &mut egui::Ui, d: &Design) {
//! // 一次按钮调用：自动带悬停过渡、按下反馈、禁用态、触控命中区扩展。
//! if Button::new("开始讲解").icon(Icon::Board).primary().show(ui, d).clicked() {
//!     // …
//! }
//! # }
//! ```
//!
//! ## 两条贯穿全库的约定
//!
//! 1. **动作只回报，不执行**。组件返回 [`egui::Response`] 或一个 `Outcome` 结构，
//!    由页面决定改什么状态。组件内部不碰业务状态。
//! 2. **视觉尺寸保真，命中区扩展**。所有可点控件的绘制尺寸沿用 Harness 的原值，
//!    但交互矩形会扩到触控下限（见 [`Metrics::hit_target`]）—— 教室一体机上
//!    手指点得中，造型比例又不变。

pub mod badge;
pub mod button;
pub mod container;
pub mod feedback;
pub mod field;
pub mod icons;
pub mod list;
pub mod modal;

mod base;
mod design;

pub use base::{
    at, bottom_fade, divider, ease, elevation_soft, elide, hash_id, hover_area, inset, inset_all,
    section_label, tap, text_center, text_left, text_right, translucent, State, HOVER_EASE,
    MEASURE_EPSILON,
};
pub use design::{Design, Size};

pub use badge::{Badge, BadgeTone};
pub use button::{Button, Chip, IconButton, IconButtonStyle, Segmented, Variant};
pub use container::{Card, CardSurface, Panel, Section};
pub use feedback::{EmptyState, Spinner, Toast, ToastKind, Tooltip};
pub use field::{FieldRow, Switch, TextField};
pub use icons::Icon;
pub use list::{ListRow, NavItem};
pub use modal::{Modal, ModalSize};

/// 组件库的公共绘制先导 —— 所有组件都接受 `&Design`。
///
/// 有它就不再需要每个页面自己算 `skin.p()` / `skin.m()`，
/// 也不会出现"这个页面用错色板"这类问题。
pub type P = neo_theme::Palette;
