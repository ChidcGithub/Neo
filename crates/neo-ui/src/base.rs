//! 基础绘制原语。
//!
//! 组件库的地基：几何收缩、命中区、过渡、文本。它们不构成"控件"，
//! 但每个控件都在用，因此也统一收在这里，避免各页面各写一份。

use egui::{Align2, Color32, FontId, Id, Painter, Rect, Response, Sense, Ui, UiBuilder};

use crate::Design;

/// 文本测量的容差（逻辑像素）。
///
/// `max_w` 往往来自"总宽减内边距"这类减法，末位会有 1e-4 量级舍入损失；
/// 没有容差时恰好等宽的文本会被判为放不下、整段退化成孤零零一个省略号。
pub const MEASURE_EPSILON: f32 = 0.5;

/// 从任意可哈希值造一个 `egui::Id`。
///
/// egui 0.36 的 `Id::new` 要求 `AsId`（它不直接接受 `impl Hash`），
/// 组件的 `id_salt` 想给用户传 `&str` / 元组都可以，所以在这里过一道 `Hash`。
pub fn hash_id(source: impl std::hash::Hash) -> Id {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    std::hash::Hash::hash(&source, &mut hasher);
    Id::new(std::hash::Hasher::finish(&hasher))
}

/// 过渡时长，对应上游 `--ds-transition-duration-fast: 0.1s`。
pub const HOVER_EASE: f32 = 0.1;

/// 建立一个从 `rect` 左上角开始的子 `Ui`。
///
/// 弹层/面板内的控件**必须**走它：直接用父级 `Ui` 的游标会让控件跑到
/// 布局流的下一个位置（历史上踩过：显示面板的分段控件跑到了窗口左下角）。
pub fn at<R>(ui: &mut Ui, rect: Rect, add: impl FnOnce(&mut Ui) -> R) -> R {
    ui.scope_builder(
        UiBuilder::new()
            .max_rect(rect)
            .layout(egui::Layout::top_down(egui::Align::Min)),
        add,
    )
    .inner
}

/// 让整个矩形可点。命中区由调用方决定，便于"视觉小、热区大"的触控适配。
pub fn tap(ui: &Ui, rect: Rect, id: Id) -> Response {
    ui.interact(rect, id, Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// 只感知悬停、不响应点击的区域（给纯装饰性的 hover 反馈用）。
pub fn hover_area(ui: &Ui, rect: Rect, id: Id) -> Response {
    ui.interact(rect, id, Sense::hover())
}

/// 把 `rect` 按内边距收缩。
pub fn inset(rect: Rect, l: f32, t: f32, r: f32, b: f32) -> Rect {
    Rect::from_min_max(
        egui::pos2(rect.left() + l, rect.top() + t),
        egui::pos2(rect.right() - r, rect.bottom() - b),
    )
}

/// 四边等距收缩。
pub fn inset_all(rect: Rect, v: f32) -> Rect {
    inset(rect, v, v, v, v)
}

/// 柔和过渡到 `target`。`id` 必须稳定 —— egui 按它缓存动画状态。
pub fn ease(ui: &Ui, id: Id, target: f32) -> f32 {
    ui.ctx().animate_value_with_time(id, target, HOVER_EASE)
}

/// 把颜色压到 `k` 倍不透明度（0..1）。
///
/// `Color32` 是预乘的：等比缩放 RGB 与 alpha 就是正确的降透明度方式，
/// 不要自己去算混合结果。
pub fn translucent(c: Color32, k: f32) -> Color32 {
    c.gamma_multiply(k.clamp(0.0, 1.0))
}

/// 自绘控件的状态。
#[derive(Clone, Copy, Debug, Default)]
pub struct State {
    pub hovered: bool,
    pub pressed: bool,
    pub focused: bool,
    pub enabled: bool,
}

impl State {
    /// 从交互结果推导。`focused` 由调用方按需覆盖（`Response` 不直接给）。
    pub fn of(r: &Response) -> Self {
        Self {
            hovered: r.hovered(),
            pressed: r.is_pointer_button_down_on(),
            focused: r.has_focus(),
            enabled: r.enabled(),
        }
    }

    /// 当前帧相对静止态的不透明度增益（0..1），供"叠一层"式过渡使用。
    pub fn emphasis(&self) -> f32 {
        match (self.pressed, self.hovered) {
            (true, _) => 1.0,
            (false, true) => 0.6,
            _ => 0.0,
        }
    }
}

/// 底部渐隐遮罩 —— 对应上游
/// "the bottom gradient mask is owned by the chat scroller"。
///
/// 内容被滚动区裁掉时不该出现生硬切边；这层从透明渐变到 `color`，
/// 让列表"沉"进后面的背景里。用顶点色网格实现，16 行足够平滑。
pub fn bottom_fade(painter: &Painter, rect: Rect, color: Color32) {
    let steps = 16usize;
    let mut mesh = egui::Mesh::default();
    for i in 0..=steps {
        let t = i as f32 / steps as f32;
        let y = rect.top() + rect.height() * t;
        let a = t * color.a() as f32;
        let c = Color32::from_rgba_premultiplied(
            (color.r() as f32 * a / 255.0) as u8,
            (color.g() as f32 * a / 255.0) as u8,
            (color.b() as f32 * a / 255.0) as u8,
            a as u8,
        );
        mesh.colored_vertex(egui::pos2(rect.left(), y), c);
        mesh.colored_vertex(egui::pos2(rect.right(), y), c);
    }
    for i in 0..steps {
        let a = (i * 2) as u32;
        mesh.add_triangle(a, a + 1, a + 2);
        mesh.add_triangle(a + 1, a + 3, a + 2);
    }
    painter.add(egui::Shape::mesh(mesh));
}

// ---------------------------------------------------------------------------
// 文本
// ---------------------------------------------------------------------------

/// 左对齐、垂直居中的单行文本。
pub fn text_left(painter: &Painter, rect: Rect, s: &str, font: FontId, color: Color32) {
    painter.text(
        egui::pos2(rect.left(), rect.center().y),
        Align2::LEFT_CENTER,
        s,
        font,
        color,
    );
}

/// 右对齐、垂直居中的单行文本。
pub fn text_right(painter: &Painter, rect: Rect, s: &str, font: FontId, color: Color32) {
    painter.text(
        egui::pos2(rect.right(), rect.center().y),
        Align2::RIGHT_CENTER,
        s,
        font,
        color,
    );
}

/// 水平与垂直都居中。
pub fn text_center(painter: &Painter, rect: Rect, s: &str, font: FontId, color: Color32) {
    painter.text(rect.center(), Align2::CENTER_CENTER, s, font, color);
}

/// 当文本超出 `max_w` 时用省略号截断。
///
/// egui 的 `Galley` 不做省略号，这里手工二分。相同输入会命中 egui 的字形缓存，
/// 因此每帧重复调用不构成性能问题。
pub fn elide(painter: &Painter, s: &str, font: &FontId, max_w: f32) -> String {
    if max_w <= 0.0 {
        return String::new();
    }
    let width_of = |t: &str| -> f32 {
        painter
            .layout_no_wrap(t.to_owned(), font.clone(), Color32::WHITE)
            .size()
            .x
    };
    if width_of(s) <= max_w + MEASURE_EPSILON {
        return s.to_owned();
    }

    let chars: Vec<char> = s.chars().collect();
    let mut lo = 0usize;
    let mut hi = chars.len();
    let mut best = String::new();
    while lo < hi {
        let mid = (lo + hi) / 2;
        let cand: String = chars[..mid].iter().collect::<String>() + "…";
        if width_of(&cand) <= max_w + MEASURE_EPSILON {
            best = cand;
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    if best.is_empty() {
        "…".to_owned()
    } else {
        best
    }
}

// ---------------------------------------------------------------------------
// 装饰
// ---------------------------------------------------------------------------

/// 区块小标题（左侧对齐的 caption）。
pub fn section_label(painter: &Painter, d: &Design, rect: Rect, label: &str) {
    text_left(
        painter,
        rect,
        label,
        d.font(d.t().caption),
        d.p().label_caption,
    );
}

/// 1px 分隔线。
pub fn divider(painter: &Painter, d: &Design, rect: Rect) {
    painter.hline(
        rect.left()..=rect.right(),
        rect.center().y,
        egui::Stroke::new(1.0, d.p().border_l1),
    );
}

/// 卡片投影 —— 对应上游 `--dsw-elevation-soft`。
///
/// 亮色主题下投影要更淡，否则白底上的灰边会显脏。
pub fn elevation_soft(d: &Design) -> egui::epaint::Shadow {
    if d.is_dark() {
        egui::epaint::Shadow {
            offset: [0, 6],
            blur: 22,
            spread: 0,
            color: neo_theme::palette::black_a(100),
        }
    } else {
        egui::epaint::Shadow {
            offset: [0, 6],
            blur: 20,
            spread: 0,
            color: neo_theme::palette::rgba(15, 17, 21, 28),
        }
    }
}
