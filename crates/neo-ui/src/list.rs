//! 列表行：导航项与会话行。
//!
//! 侧栏的会话行承载了三种形态（常规 / 行内重命名 / 删除确认），
//! 过去散在页面里；收进组件后，任何"可重命名可删除的列表"都能直接复用。
//!
//! 关键约定：**动作按钮恒显而非仅悬停** —— 教室一体机没有 hover，
//! 靠悬停才能露出的操作在目标设备上等于不存在。

use egui::{Id, Rect, Ui, Vec2};
use neo_theme::SquirclePaint;

use crate::base::{at, elide, inset, tap, text_center, text_left, State};
use crate::icons::Icon;
use crate::Design;

/// 一个导航项（侧栏的「新对话」「设置」）。
pub struct NavItem<'a> {
    label: &'a str,
    icon: Icon,
    active: bool,
    id_salt: Option<Id>,
}

impl<'a> NavItem<'a> {
    pub fn new(label: &'a str, icon: Icon) -> Self {
        Self {
            label,
            icon,
            active: false,
            id_salt: None,
        }
    }
    pub fn active(mut self, on: bool) -> Self {
        self.active = on;
        self
    }
    pub fn id_salt(mut self, s: impl std::hash::Hash) -> Self {
        self.id_salt = Some(crate::base::hash_id(s));
        self
    }

    pub fn height(d: &Design) -> f32 {
        d.m().hit_target(d.m().s(40.0))
    }

    pub fn show(self, ui: &mut Ui, d: &Design, width: f32) -> egui::Response {
        let m = d.m();
        let p = d.p();
        let h = Self::height(d);
        let (rect, _) = ui.allocate_exact_size(Vec2::new(width, h), egui::Sense::hover());
        // 默认 Id 带上位置：只按标签区分时，两条同名行（例如两个同标题的会话）
        // 会撞成一个 Id，点一条同时命中另一条。
        let id = self.id_salt.unwrap_or_else(|| {
            ui.id()
                .with(("nav", self.label, rect.left() as i32, rect.top() as i32))
        });
        let resp = tap(ui, rect, id);
        let st = State::of(&resp);

        if self.active {
            ui.painter()
                .squircle_filled(rect, m.radius_chip(), p.nav_active);
        } else if st.hovered {
            ui.painter().squircle_filled(rect, m.radius_chip(), p.hover);
        }

        let hot = st.hovered || self.active;
        let icon_rect = Rect::from_center_size(
            egui::pos2(rect.left() + m.s(20.0), rect.center().y),
            Vec2::splat(m.s(17.0)),
        );
        self.icon.paint(
            ui.painter(),
            icon_rect,
            if hot { p.accent } else { p.label_secondary },
        );
        text_left(
            ui.painter(),
            inset(rect, m.s(38.0), 0.0, m.s(8.0), 0.0),
            self.label,
            d.font_bold(d.t().label),
            if hot {
                p.label_primary
            } else {
                p.label_secondary
            },
        );
        resp
    }
}

/// 会话行上可用的行内动作。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowAction {
    /// 打开这条会话。
    Open,
    /// 请求重命名。
    Rename,
    /// 请求删除（进入确认）。
    Delete,
}

/// 会话行的显示形态。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowMode {
    /// 常规：标题 + 时间 + 恒显动作钮。
    Normal,
    /// 行内重命名。
    Rename,
    /// 删除确认条。
    Confirm,
}

/// 一条会话行。
pub struct ListRow<'a> {
    id: i64,
    title: &'a str,
    meta: &'a str,
    active: bool,
    mode: RowMode,
}

impl<'a> ListRow<'a> {
    pub fn new(id: i64, title: &'a str, meta: &'a str) -> Self {
        Self {
            id,
            title,
            meta,
            active: false,
            mode: RowMode::Normal,
        }
    }
    pub fn active(mut self, on: bool) -> Self {
        self.active = on;
        self
    }
    pub fn mode(mut self, m: RowMode) -> Self {
        self.mode = m;
        self
    }

    pub fn height(d: &Design) -> f32 {
        d.m().nav_item_h()
    }

    /// 常规行的绘制（动作钮恒显）。
    ///
    /// 重命名 / 确认两种模式由调用方接管内容（分别需要 `TextEdit`
    /// 与两个按钮，组件不替调用方持有草稿状态）。
    pub fn show_normal(self, ui: &mut Ui, d: &Design, width: f32) -> Option<RowAction> {
        let m = d.m();
        let p = d.p();
        let h = Self::height(d);
        let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, h), egui::Sense::click());
        let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
        let st = State::of(&resp);

        if self.active {
            ui.painter()
                .squircle_filled(rect, m.radius_chip(), p.nav_active);
        } else if st.hovered {
            ui.painter()
                .squircle_filled(rect, m.radius_chip(), p.nav_hover);
        }

        // 行内动作：重命名 / 删除（恒显）。
        let target = m.hit_target(m.s(24.0));
        let act_size = Vec2::splat(target);
        let glyph = Vec2::splat(m.s(15.0));
        let pen_rect = Rect::from_center_size(
            egui::pos2(rect.right() - target * 1.5, rect.center().y),
            act_size,
        );
        let trash_rect = Rect::from_center_size(
            egui::pos2(rect.right() - target * 0.5, rect.center().y),
            act_size,
        );
        let pen = tap(ui, pen_rect, Id::new(("neo-row-pen", self.id))).on_hover_text("重命名会话");
        let trash =
            tap(ui, trash_rect, Id::new(("neo-row-trash", self.id))).on_hover_text("删除会话");
        let idle = if self.active {
            p.label_secondary
        } else {
            p.label_caption
        };
        Icon::Pen.paint(
            ui.painter(),
            Rect::from_center_size(pen_rect.center(), glyph),
            if pen.hovered() { p.label_primary } else { idle },
        );
        Icon::Trash.paint(
            ui.painter(),
            Rect::from_center_size(trash_rect.center(), glyph),
            if trash.hovered() { d.c().error } else { idle },
        );

        let mut action = None;
        if pen.clicked() {
            action = Some(RowAction::Rename);
        } else if trash.clicked() {
            action = Some(RowAction::Delete);
        } else if resp.clicked() {
            action = Some(RowAction::Open);
        }

        let text_right_edge = pen_rect.left() - m.s(4.0);
        let title_rect = Rect::from_min_max(
            egui::pos2(rect.left() + m.s(10.0), rect.top() + m.s(4.0)),
            egui::pos2(text_right_edge, rect.center().y - m.s(2.0)),
        );
        let meta_rect = Rect::from_min_max(
            egui::pos2(rect.left() + m.s(10.0), rect.center().y - m.s(1.0)),
            egui::pos2(text_right_edge, rect.bottom() - m.s(4.0)),
        );
        let font = d.font(d.t().label);
        let shown = elide(ui.painter(), self.title, &font, title_rect.width());
        ui.painter().text(
            title_rect.left_bottom(),
            egui::Align2::LEFT_BOTTOM,
            shown,
            font,
            if self.active {
                p.label_primary
            } else {
                p.label_secondary
            },
        );
        ui.painter().text(
            meta_rect.left_top(),
            egui::Align2::LEFT_TOP,
            self.meta,
            d.font(d.t().caption - m.s(1.0)),
            p.label_caption,
        );
        action
    }
}

/// 删除确认条的两个按钮。
pub struct ConfirmBar<'a> {
    pub question: &'a str,
    pub confirm: &'a str,
}

impl<'a> ConfirmBar<'a> {
    pub fn new(question: &'a str) -> Self {
        Self {
            question,
            confirm: "删除",
        }
    }

    /// 布局流入口；`opening` 表示本次确认首次展示，该帧不会提交删除。
    pub fn show(&self, ui: &mut Ui, d: &Design, id: impl std::hash::Hash, opening: bool) -> ConfirmResponse {
        let id = crate::hash_id(id);
        let inner = ui.push_id(id, |ui| {
            ui.spacing_mut().item_spacing = Vec2::splat(d.m().s(8.0));
            let question = ui.add(egui::Label::new(egui::RichText::new(self.question)
                .font(d.font(d.t().label)).color(d.p().label_primary)).wrap());
            // 整行或两行按钮都按实际热区排版，不再外扩覆盖问句。
            let available = ui.available_width();
            let button_w = ["取消", self.confirm].map(|text| {
                d.m().hit_target(ui.painter().layout_no_wrap(text.to_owned(),
                    d.font_bold(d.t().label), d.p().label_primary).size().x + d.m().s(32.0))
            });
            let stacked = button_w[0] + button_w[1] + d.m().s(8.0) > available;
            let buttons = ui.with_layout(if stacked { egui::Layout::top_down(egui::Align::Min) }
                else { egui::Layout::left_to_right(egui::Align::Center) }, |ui| {
                let cancel = crate::Button::new("取消").elevated().touch_layout()
                    .id_salt(id.with("cancel")).show(ui, d);
                let confirm = crate::Button::new(self.confirm).danger().touch_layout()
                    .id_salt(id.with("confirm")).show(ui, d);
                (cancel, confirm)
            }).inner;
            (question, buttons.0, buttons.1)
        });
        let (question, cancel, confirm) = inner.inner;
        if opening { cancel.request_focus(); }
        let escaped = ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
        let outcome = if escaped || (!opening && cancel.clicked()) {
            ConfirmOutcome::Cancel
        } else if !opening && confirm.clicked() {
            ConfirmOutcome::Confirm
        } else { ConfirmOutcome::None };
        ConfirmResponse { response: inner.response, question, cancel, confirm, outcome }
    }
}

pub struct ConfirmResponse {
    pub response: egui::Response,
    pub question: egui::Response,
    pub cancel: egui::Response,
    pub confirm: egui::Response,
    pub outcome: ConfirmOutcome,
}

/// 删除确认行的绘制结果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfirmOutcome {
    Confirm,
    Cancel,
    None,
}

/// 确认行的两段几何：文案可排区域 + 删除键。
///
/// 抽成独立函数是为了**能被测试盯住**：文案的右缘必须严格落在按钮左缘的左边。
/// 早先这里是写死的"右边留 132"，而两个按钮实际占了 154 —— 删除键直接压在了
/// 「删除这条会话？」上面（截图里一眼能看到）。这类"两个数字各自对、合起来错"
/// 的问题，只有把关系写成断言才挡得住。
pub fn confirm_geometry(d: &Design, rect: Rect) -> (Rect, Rect) {
    let m = d.m();
    let pad_x = m.s(12.0);
    let gap = m.s(8.0);

    // 可用宽度扣掉左右内边距；按钮**最多占满可用宽度**（窄行时收缩），
    // 这样"按钮留在行内"与"文案区非负"两件事都是构造保证的，不靠调用方守规矩。
    let avail = (rect.width() - pad_x * 2.0).max(0.0);
    let btn_w = m.s(56.0).min(avail).max(m.s(16.0).min(avail));
    let chip_h = m.s(24.0);

    let btn = Rect::from_center_size(
        egui::pos2(rect.right() - pad_x - btn_w * 0.5, rect.center().y),
        Vec2::new(btn_w, chip_h),
    );
    // 文案区：左缘与按钮左缘取小，右缘再留一个间隙 —— 于是
    // `label.right() <= btn.left()` 恒成立（间隙挤没了就退化成零宽）。
    let left = (rect.left() + pad_x).min(btn.left());
    let right = (btn.left() - gap).max(left);
    let label = Rect::from_min_max(
        egui::pos2(left, rect.top()),
        egui::pos2(right, rect.bottom()),
    );
    (label, btn)
}

/// 绘制一条删除确认行。
///
/// ## 只有一个按键，取消靠"点别处"
///
/// 这一行问的是"要不要删"，不是一个需要权衡的选择：想删就点删除，不想删就
/// 点（或按 Esc）别的地方。所以**没有取消键** —— 多一个按钮反而把行挤满，
/// 也让"我正在确认"这件事看起来像个小表单。删除键放在原取消键的位置（最右），
/// 视线不用在行里跳。
///
/// ## 文案与按钮不许重叠
///
/// 文案的可排宽度是从**按钮的实际左缘**倒推的，不是写死的数字 ——
/// 早先写死"右边留 132"，而两个按钮实际占了 154，于是删除键压在了
/// "删除这条会话？"上面（截图里能直接看到）。
/// 现在只剩一个按钮，文案用 [`elide`] 截断，再窄也不会压上去。
pub fn confirm_row(
    ui: &mut Ui,
    d: &Design,
    rect: Rect,
    id: i64,
    bar: &ConfirmBar<'_>,
) -> ConfirmOutcome {
    let m = d.m();
    let p = d.p();
    let c = d.c();

    ui.painter().squircle_filled(
        rect,
        m.radius_chip(),
        crate::base::translucent(c.error, 0.12),
    );
    ui.painter().squircle_stroked(
        rect,
        m.radius_chip(),
        egui::Stroke::new(1.0, crate::base::translucent(c.error, 0.45)),
    );

    // ---- 几何：文案区 + 唯一的按键（删除，占原取消键的位置）----
    let (label_rect, del_rect) = confirm_geometry(d, rect);

    // ---- 文案：按算出来的宽度截断，再窄也压不到按钮上 ----
    let font = d.font(d.t().label);
    let shown = elide(ui.painter(), bar.question, &font, label_rect.width());
    text_left(ui.painter(), label_rect, &shown, font, p.label_primary);

    // 保留紧凑的视觉按钮，命中高度补足触控下限且不跨到相邻会话行。
    let hit_rect = Rect::from_center_size(
        del_rect.center(),
        Vec2::new(m.hit_target(del_rect.width()), m.hit_target(del_rect.height())),
    ).intersect(rect);
    let del = tap(ui, hit_rect, Id::new(("neo-confirm-del", id)));
    ui.painter().squircle_filled(
        del_rect,
        m.radius_chip(),
        if del.hovered() {
            c.error
        } else {
            crate::base::translucent(c.error, 0.85)
        },
    );
    text_center(
        ui.painter(),
        del_rect,
        bar.confirm,
        d.font_bold(d.t().caption),
        c.on_primary,
    );

    if del.clicked() {
        return ConfirmOutcome::Confirm;
    }

    // ---- 取消：点这一行以外的任何地方，或按 Esc ----
    let (any_click, click_pos, escaped) = ui.input(|i| {
        (
            i.pointer.any_click(),
            i.pointer.interact_pos(),
            i.key_pressed(egui::Key::Escape),
        )
    });
    if should_cancel(any_click, click_pos, rect, escaped) {
        return ConfirmOutcome::Cancel;
    }

    ConfirmOutcome::None
}

/// 这一次输入是不是"取消删除"。
///
/// 规则只有两条：**点在这一行之外**，或按下 Esc。抽成纯函数是为了能被测试盯住 ——
/// "点别处取消"是个容易做歪的交互（点行内空白、点到别的行、点到主区，行为都该一致），
/// 埋在 `ui.input` 的闭包里就只剩肉眼验证了。
pub fn should_cancel(
    any_click: bool,
    click_pos: Option<egui::Pos2>,
    rect: Rect,
    escaped: bool,
) -> bool {
    if escaped {
        return true;
    }
    any_click && click_pos.is_some_and(|pos| !rect.contains(pos))
}

/// 行内重命名编辑框的容器（背景 + 内部 `TextEdit` 由调用方提供）。
pub fn rename_frame(ui: &Ui, d: &Design, rect: Rect) -> Rect {
    let m = d.m();
    ui.painter()
        .squircle_filled(rect, m.radius_chip(), d.p().nav_active);
    let line_h = d.t().label + m.s(8.0);
    Rect::from_center_size(rect.center(), Vec2::new(rect.width() - m.s(20.0), line_h))
}

/// 在 `rect` 内建立子 `Ui`（供行内编辑框复用）。
pub fn row_child<R>(ui: &mut Ui, rect: Rect, add: impl FnOnce(&mut Ui) -> R) -> R {
    at(ui, rect, add)
}

#[cfg(test)]
mod confirm_tests {
    use super::*;
    use neo_theme::{Distance, Theme, ThemeMode};

    fn design(mode: ThemeMode, viewport_h: f32, distance: Distance) -> Design {
        Design::new(Theme::new(mode, viewport_h, distance))
    }

    #[test]
    fn flow_confirmation_wraps_targets_and_keyboard_is_safe() {
        for mode in [ThemeMode::Light, ThemeMode::Dark] {
            for scale in [0.85, 1.0, 1.75, 2.8] {
                for width in [240.0, 320.0, 560.0] {
                    let ctx = egui::Context::default();
                    neo_theme::fonts::install(&ctx);
                    let theme = Theme::from_metrics(mode, neo_theme::Metrics::from_scale(scale));
                    theme.apply(&ctx);
                    let d = Design::new(theme);
                    let question = "删除这条记忆？此操作无法撤销。Long question ".repeat(5);
                    let draw = |opening, events| {
                        let mut result = None;
                        let mut output = ctx.run_ui(egui::RawInput {
                            screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(width, 4000.0))),
                            events, ..Default::default()
                        }, |ui| {
                            let parent = Rect::from_min_size(egui::pos2(8.0, 8.0), Vec2::new(width - 16.0, 3984.0));
                            crate::at(ui, parent, |ui| {
                                ui.shrink_clip_rect(parent);
                                let response = ConfirmBar::new(&question).show(ui, &d, "safe-confirm", opening);
                                for rect in [response.cancel.rect, response.confirm.rect] {
                                    assert!(ui.clip_rect().contains_rect(rect), "确认热区不得越过父级裁剪");
                                }
                                result = Some(response);
                            });
                        });
                        output.textures_delta.clear();
                        result.unwrap()
                    };
                    let key = |key| vec![egui::Event::Key { key, physical_key: None, pressed: true,
                        repeat: false, modifiers: egui::Modifiers::NONE }];
                    let first = draw(true, vec![]);
                    assert_eq!(first.outcome, ConfirmOutcome::None);
                    let settled = draw(false, vec![]);
                    assert!(settled.cancel.has_focus());
                    assert_eq!(first.cancel.id, settled.cancel.id);
                    assert!(settled.question.rect.bottom() <= settled.cancel.rect.top());
                    assert!(!settled.cancel.rect.shrink(0.1).intersects(settled.confirm.rect.shrink(0.1)));
                    for rect in [settled.cancel.rect, settled.confirm.rect] {
                        assert!(rect.right() <= width);
                        assert!(rect.height() >= d.m().hit_target(0.0));
                    }
                    assert_eq!(draw(false, key(egui::Key::Enter)).outcome, ConfirmOutcome::Cancel);
                    draw(true, vec![]);
                    draw(false, key(egui::Key::Tab));
                    let focused = draw(false, vec![]);
                    assert!(focused.confirm.has_focus(), "Tab {width}/{scale}");
                    assert_eq!(draw(false, key(egui::Key::Enter)).outcome, ConfirmOutcome::Confirm);
                    assert_eq!(draw(false, key(egui::Key::Escape)).outcome, ConfirmOutcome::Cancel);
                    assert_eq!(draw(true, key(egui::Key::Enter)).outcome, ConfirmOutcome::None);
                }
            }
        }
    }

    /// **回归**：文案区必须严格在按钮左边（曾经重叠 22pt，把「删除」压在问句上）。
    #[test]
    fn label_never_overlaps_the_button() {
        let bar = Rect::from_min_size(egui::pos2(100.0, 50.0), Vec2::new(220.0, 32.0));
        for mode in [ThemeMode::Dark, ThemeMode::Light] {
            for h in [768.0f32, 1080.0, 2160.0] {
                for dist in [
                    Distance::Standard,
                    Distance::Classroom,
                    Distance::Auditorium,
                ] {
                    let d = design(mode, h, dist);
                    let (label, btn) = confirm_geometry(&d, bar);
                    assert!(
                        label.right() <= btn.left(),
                        "文案压到按钮上了：label.right={:.1} btn.left={:.1}（{mode:?} {h} {dist:?}）",
                        label.right(),
                        btn.left()
                    );
                    // 极端放大下文案区会退化成零宽（而不是负宽）—— 这也是对的，
                    // 至少不会画到按钮上去。
                    assert!(label.right() >= label.left(), "文案区出现了负宽度");
                }
            }
        }
    }

    /// 按钮贴着右边距，与原来的取消键同位。
    #[test]
    fn button_sits_at_the_right_edge() {
        let d = design(ThemeMode::Dark, 1080.0, Distance::Classroom);
        let bar = Rect::from_min_size(egui::pos2(0.0, 0.0), Vec2::new(220.0, 32.0));
        let (_, btn) = confirm_geometry(&d, bar);
        assert!((bar.right() - btn.right() - d.m().s(12.0)).abs() < 0.01);
        // 竖直居中
        assert!((btn.center().y - bar.center().y).abs() < 0.01);
    }

    /// 窄到放不下时，文案区退化成零宽（而不是"负宽"或跑到按钮右边）。
    #[test]
    fn degenerate_width_does_not_overflow() {
        let d = design(ThemeMode::Dark, 1080.0, Distance::Classroom);
        let bar = Rect::from_min_size(egui::pos2(0.0, 0.0), Vec2::new(40.0, 32.0));
        let (label, btn) = confirm_geometry(&d, bar);
        assert!(label.right() <= btn.left(), "窄行也不许压上去");
        assert!(label.width() >= 0.0);
    }
}

#[cfg(test)]
mod confirm_cancel_tests {
    use super::*;
    use egui::pos2;

    fn bar() -> Rect {
        Rect::from_min_size(pos2(100.0, 50.0), Vec2::new(220.0, 32.0))
    }

    #[test]
    fn clicking_inside_the_row_keeps_the_confirmation() {
        // 点在行内（包括空白处和删除键上）→ 不取消
        assert!(!should_cancel(true, Some(bar().center()), bar(), false));
        assert!(!should_cancel(true, Some(pos2(101.0, 51.0)), bar(), false));
    }

    #[test]
    fn clicking_anywhere_else_cancels() {
        // 上、下、左、右四个方向都算"别处"
        for pos in [
            pos2(100.0, 20.0),  // 侧栏上方
            pos2(100.0, 400.0), // 侧栏下方
            pos2(5.0, 60.0),    // 左侧边缘
            pos2(900.0, 60.0),  // 主区（会话区）
        ] {
            assert!(
                should_cancel(true, Some(pos), bar(), false),
                "{pos:?} 应当被当作「点别处」"
            );
        }
    }

    #[test]
    fn no_click_means_no_cancel() {
        // 只是移动鼠标/什么都没点 → 保持确认态
        assert!(!should_cancel(false, Some(pos2(900.0, 60.0)), bar(), false));
        assert!(!should_cancel(false, None, bar(), false));
        // 有点击但拿不到坐标（极罕见）→ 保守地不取消
        assert!(!should_cancel(true, None, bar(), false));
    }

    #[test]
    fn escape_always_cancels() {
        assert!(should_cancel(false, None, bar(), true));
        assert!(should_cancel(true, Some(bar().center()), bar(), true));
    }
}
