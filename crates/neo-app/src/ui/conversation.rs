//! 对话态：顶栏 + 消息流 + 停靠的输入卡。
//!
//! 顶栏内边距直接抄 Harness `ConversationRoot` 的 `10px 28px 0 20px`；
//! 消息流的内容列在**主区中轴居中**，与输入卡共用同一条左基线
//! （`--dsh-chat-content-width`，输入卡再宽 32px），
//! 这样眼睛在两条边缘之间不需要重新找基准。
//!
//! ⚠️ 内容列不能靠 `ui.add_space` 横向让位 —— 它只作用于纵向。
//! 横向偏移必须放进一次左到右布局（见 `draw_messages`），
//! 且要用 `set_max_width(content_w)` 收窄，否则 Markdown 的代码块会撑到屏边。

use egui::{Color32, Rect, ScrollArea, Sense, Ui, Vec2};
use neo_theme::SquirclePaint;
use neo_ui::{Icon, IconButton};

use super::{composer, elide, markdown, text_left, translucent, Skin};
use crate::state::{AppState, Role};

/// 消息入场淡入时长（对应 spec `--ds-transition-duration` 标准档 0.2s）。
const ENTER_FADE: f32 = 0.2;

/// 对话态上发生的用户动作。
#[derive(Default, Clone, Copy)]
pub struct Outcome {
    pub composer: composer::Outcome,
    pub new_session: bool,
}

/// 绘制对话态。`topbar` 已由调用方从 `area` 切出。
pub fn draw(
    ui: &mut Ui,
    skin: &Skin<'_>,
    topbar: Rect,
    body: Rect,
    state: &mut AppState,
) -> Outcome {
    let mut out = Outcome::default();

    // ---- 顶栏 ----
    let pad = skin.m().topbar_pad();
    let header = egui::Rect::from_min_max(
        egui::pos2(topbar.left() + pad[3], topbar.top() + pad[0]),
        egui::pos2(topbar.right() - pad[1], topbar.bottom() - pad[2]),
    );
    let title_font = skin.bold(skin.t().label + skin.m().s(1.0));
    let title = state.current_title();
    let shown = elide(ui.painter(), &title, &title_font, header.width() * 0.55);
    text_left(
        ui.painter(),
        Rect::from_min_size(
            egui::pos2(header.left(), header.top()),
            Vec2::new(header.width(), skin.m().s(20.0)),
        ),
        &shown,
        title_font,
        skin.p().label_primary,
    );
    text_left(
        ui.painter(),
        Rect::from_min_size(
            egui::pos2(header.left(), header.top() + skin.m().s(20.0)),
            Vec2::new(header.width(), skin.m().s(16.0)),
        ),
        &format!(
            "{} · {} 条消息",
            state.model_display(),
            state.messages.len()
        ),
        skin.prop(skin.t().caption),
        skin.p().label_caption,
    );

    // 顶栏右侧只留「新对话」：设置入口统一收在侧栏底部，顶栏不再重复一个。
    let d = skin.d();
    let m = skin.m();
    let p = skin.p();
    let btn_d = m.s(30.0);
    let new_center = egui::pos2(header.right() - btn_d * 0.5, header.top() + m.s(16.0));
    if IconButton::new(Icon::Plus)
        .elevated()
        .id_salt("neo-topbar-new")
        .show_at(ui, &d, new_center)
        .clicked()
    {
        out.new_session = true;
    }

    // ---- 消息流 → 渐隐 → 输入卡 ----
    // 绘制顺序有讲究：渐隐遮罩必须盖在消息流之上、输入卡之下，
    // 否则会把卡片底边也一起糊掉。
    let card_w = m.card_max(body.width());
    let card_h = composer::block_height(ui, skin, state, card_w, false);
    let card_bottom_pad = m.s(14.0);
    let card = Rect::from_min_size(
        egui::pos2(
            body.center().x - card_w * 0.5,
            body.bottom() - card_bottom_pad - card_h,
        ),
        Vec2::new(card_w, card_h),
    );

    let list_rect = Rect::from_min_max(body.min, egui::pos2(body.right(), card.top() - m.s(16.0)));
    if list_rect.height() > m.s(40.0) {
        draw_messages(ui, skin, list_rect, state);
        // 消息被裁掉时不该出现生硬切边 —— 沉入背景。
        let fade_h = m.s(52.0).min(list_rect.height() * 0.4);
        super::bottom_fade(
            ui.painter(),
            Rect::from_min_max(
                egui::pos2(list_rect.left(), list_rect.bottom() - fade_h),
                egui::pos2(list_rect.right(), list_rect.bottom()),
            ),
            p.bg_base,
        );
    }

    out.composer = composer::draw(ui, skin, card, state, false);

    out
}

/// 消息流。
fn draw_messages(ui: &mut Ui, skin: &Skin<'_>, rect: Rect, state: &mut AppState) {
    let m = skin.m();
    let content_w = m.content_max(rect.width());
    let gap = m.s(22.0);
    // 内容列整体右移到主区正中，与输入卡共用同一条左基线 ——
    // 上游 `--dsh-chat-content-width` 同样是居中的（输入卡再宽 32px）。
    // 注意 `add_space` 只在纵向生效：横向让位必须在左到右布局里放一个空白项。
    let x_off = ((rect.width() - content_w) * 0.5).max(0.0);
    // 入场淡入所需的跨帧状态（本帧之前已播过的条数 + 会话代次）。
    let entered = state.entered_count.min(state.messages.len());
    let seq = state.session_seq;
    let fade = skin.p().bg_base;

    super::at(ui, rect, |ui| {
        let scroll = ScrollArea::vertical()
            .id_salt("neo-thread")
            .auto_shrink([false, false])
            .stick_to_bottom(true)
            .show(ui, |ui| {
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Min), |ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    ui.add_space(x_off);
                    ui.vertical(|ui| {
                        // 收窄到内容列：Markdown / 代码块按列宽换行，不会撑到屏边。
                        ui.set_max_width(content_w);
                        ui.spacing_mut().item_spacing.y = gap;
                        for (i, msg) in state.messages.iter().enumerate() {
                            // egui 动画首调直接返回目标值 —— 新消息必须先播种 0
                            // 再启动到 1 才有过渡；老消息每帧续播 1，收敛后是纯查表。
                            let id = egui::Id::new(("neo-msg-enter", seq, i));
                            let k = if i < entered {
                                ui.ctx().animate_value_with_time(id, 1.0, ENTER_FADE)
                            } else {
                                ui.ctx().animate_value_with_time(id, 0.0, ENTER_FADE);
                                ui.ctx().animate_value_with_time(id, 1.0, ENTER_FADE)
                            };
                            let top = ui.cursor().min;
                            draw_one(ui, skin, state, msg, content_w);
                            if k < 1.0 {
                                // 用背景色遮罩"盖住再掀开"，等价于整条淡入，
                                // 不必逐形状穿透 markdown / 工具卡里的子 Ui。
                                let cover = Rect::from_min_max(
                                    top,
                                    egui::pos2(top.x + content_w, ui.cursor().top()),
                                );
                                ui.painter()
                                    .rect_filled(cover, 0.0, translucent(fade, 1.0 - k));
                            }
                        }
                        ui.add_space(m.s(8.0));
                        // Expand the existing rect, not set_min_height: in egui 0.36
                        // set_min_height reserves space from the CURRENT cursor and
                        // would append the whole message height again.
                        let mut aligned = ui.min_rect();
                        aligned.max.y = aligned.max.y.ceil();
                        ui.expand_to_include_rect(aligned);
                    });
                });
            });
        #[cfg(test)]
        ui.ctx().data_mut(|data| {
            data.insert_temp(
                egui::Id::new("neo-test-thread-geometry"),
                (scroll.inner_rect, scroll.content_size, scroll.state.offset),
            )
        });
        let _ = scroll;
    });
    // 本帧起新出现的消息都已播种并启动淡入，后续帧走"续播 1"分支。
    state.entered_count = state.messages.len();
}

/// 单条消息：用户气泡 / 助手（思考 + 正文 + 错误 + 元信息）。
fn draw_one(
    ui: &mut Ui,
    skin: &Skin<'_>,
    state: &AppState,
    msg: &crate::state::ChatMessage,
    content_w: f32,
) {
    let p = skin.p();
    let m = skin.m();

    match msg.role {
        Role::Tool => draw_tool_card(ui, skin, msg, content_w),
        Role::User => {
            ui.scope(|ui| {
                ui.spacing_mut().item_spacing.y = m.s(8.0);
                let max_bubble = content_w * 0.76;
                for attachment in &msg.attachments {
                    let (row, _) =
                        ui.allocate_exact_size(Vec2::new(content_w, m.s(80.0)), Sense::hover());
                    let width = m.s(360.0).min(max_bubble);
                    let card = Rect::from_min_size(
                        egui::pos2(row.right() - width, row.top()),
                        Vec2::new(width, row.height()),
                    );
                    if ui.is_rect_visible(card) {
                        composer::attachment_card(ui, skin, card, attachment, None);
                    }
                }
                if !msg.content.trim().is_empty() {
                    // 用户气泡：右侧对齐，最大宽度取内容列的 76%。
                    let font = skin.prop(skin.t().body);
                    let galley = ui.painter().layout(
                        msg.content.clone(),
                        font,
                        p.label_primary,
                        (max_bubble - m.s(32.0)).max(1.0),
                    );
                    let w = (galley.size().x + m.s(32.0)).min(max_bubble);
                    let h = galley.size().y + m.s(22.0);
                    let (r, _) = ui.allocate_exact_size(Vec2::new(content_w, h), Sense::hover());
                    let bubble =
                        Rect::from_min_size(egui::pos2(r.right() - w, r.top()), Vec2::new(w, h));
                    ui.painter().squircle_filled(bubble, m.s(16.0), p.bubble);
                    ui.painter().galley(
                        egui::pos2(bubble.left() + m.s(16.0), bubble.top() + m.s(11.0)),
                        galley,
                        p.label_primary,
                    );
                }
            });
        }
        Role::Assistant => {
            // 思考过程（可折叠的一小块）。
            if !msg.reasoning.is_empty() && state.show_reasoning {
                draw_reasoning(ui, skin, content_w, &msg.reasoning);
                ui.add_space(m.s(8.0));
            }

            if msg.content.is_empty() && msg.error.is_none() {
                // 还没有任何内容：脉动点。
                pulsing_dots(ui, skin, content_w);
            } else if !msg.content.is_empty() {
                // 正文：Markdown。
                markdown::render(ui, skin, &msg.content, msg.streaming);
                ui.add_space(m.s(8.0));
            }

            if let Some(err) = &msg.error {
                let font = skin.prop(skin.t().caption);
                let galley =
                    ui.painter()
                        .layout(format!("⚠ {err}"), font, p.error, content_w - m.s(16.0));
                let h = galley.size().y + m.s(12.0);
                let (r, _) = ui.allocate_exact_size(Vec2::new(content_w, h), Sense::hover());
                ui.painter()
                    .squircle_filled(r, m.s(8.0), p.error.gamma_multiply(0.12));
                ui.painter().galley(
                    egui::pos2(r.left() + m.s(8.0), r.top() + m.s(6.0)),
                    galley,
                    p.error,
                );
            }

            // 元信息（模型 · 耗时 / 已停止）。
            if !msg.meta.is_empty() && !msg.streaming {
                let meta_font = skin.prop(skin.t().caption);
                let (r, _) =
                    ui.allocate_exact_size(Vec2::new(content_w, m.s(16.0)), Sense::hover());
                text_left(ui.painter(), r, &msg.meta, meta_font, p.label_caption);
            }
        }
    }
}

/// 思考过程：浅色小字，带左侧竖条。
fn draw_reasoning(ui: &mut Ui, skin: &Skin<'_>, content_w: f32, text: &str) {
    let p = skin.p();
    let m = skin.m();
    let font = skin.prop(skin.t().caption);
    // 排版宽度必须**等于**实际绘制宽度：文字从 `left + pad_l` 起画，
    // 早先排版用 `content_w - 16` 而行首缩进 12，右缘会多出 4pt（放大后可见），
    // 结果是"该换行的地方没换、不该换的地方先换"。
    let pad_l = m.s(12.0);
    let pad_r = m.s(12.0);
    let text_w = (content_w - pad_l - pad_r).max(m.s(40.0));
    let galley = ui
        .painter()
        .layout(text.to_owned(), font, p.label_tertiary, text_w);
    let h = galley.size().y + m.s(16.0);
    let (r, _) = ui.allocate_exact_size(Vec2::new(content_w, h), Sense::hover());
    ui.painter().vline(
        r.left(),
        r.y_range(),
        egui::Stroke::new(m.s(2.0), p.border_l3),
    );
    ui.painter().galley(
        egui::pos2(r.left() + pad_l, r.top() + m.s(8.0)),
        galley,
        p.label_tertiary,
    );
}

/// 生成中的三个相位错开的脉动点（对应 Harness `.pending` 的呼吸）。
fn pulsing_dots(ui: &mut Ui, skin: &Skin<'_>, content_w: f32) {
    let p = skin.p();
    let m = skin.m();
    let (r, _) = ui.allocate_exact_size(Vec2::new(content_w, m.s(24.0)), Sense::hover());
    let cy = r.center().y;
    let d = m.s(5.0);
    let gap = m.s(14.0);
    let t = ui.ctx().time();
    for i in 0..3usize {
        // 1s 一轮，每个点错开 0.18s。
        let phase = (t - i as f64 * 0.18) as f32;
        let k = 0.3 + 0.7 * (0.5 + 0.5 * (phase * std::f32::consts::TAU).sin());
        ui.painter().circle_filled(
            egui::pos2(r.left() + gap + i as f32 * gap, cy),
            d * 0.5,
            super::translucent(p.accent, k),
        );
    }
    // 动画期间保持重绘。
    ui.ctx()
        .request_repaint_after(std::time::Duration::from_millis(33));
}

/// 顶栏高度。
pub fn header_height(skin: &Skin<'_>) -> f32 {
    let pad = skin.m().topbar_pad();
    pad[0] + skin.m().s(40.0)
}

/// 工具调用行。
///
/// 结构照搬上游 Harness（`dsh-client-ui-tool`，从它的内联 CSS 与 `tool-call-model`
/// 抽出来的）：
///
/// ```text
/// [图标/状态点] 标题 · 摘要…………………………  [退出码]
/// ```
///
/// - **标题是"变体名"**（读取 / 写入 / 修改 / 执行 / 搜索 / 工具），不是工具自己的名字 ——
///   一行工具调用的信息密度靠"读了什么、跑没跑通"就够（上游也是这么做的，
///   它把变体名写成设计字面量；Neo 换了中文词，结构不变）；
/// - **摘要与标题同一行**，中间隔一个 2pt 圆点，摘要用三级色 + 省略号；
/// - **行首**平时是变体图标，失败时换成红点、待确认时换成强调色点
///   （上游 `leadingFor`：error → 红点、stopped → warning 点）；
/// - **执行中**整行扫过一道柔和高光（上游 2.6s 循环的 sweep）；
/// - 命令**非零退出**时摘要后跟一个红色退出码胶囊（上游 `terminalFailed` 的红 pill）。
///
/// 失败时下面补一行原因 —— 卡片本身就是给老师看的"为什么没成"。
fn draw_tool_card(ui: &mut Ui, skin: &Skin<'_>, msg: &crate::state::ChatMessage, content_w: f32) {
    use crate::state::ToolState;
    use neo_tools::present::{self, Variant};

    let p = skin.p();
    let m = skin.m();
    let t = skin.t();
    let Some(tool) = msg.tool.as_ref() else {
        return;
    };

    let variant = Variant::of(&tool.name);
    let awaiting = tool.state == ToolState::AwaitingConfirm;
    let denied = tool.state == ToolState::Denied;
    let cancelled = tool.state == ToolState::Cancelled;
    let running = tool.state == ToolState::Running;
    // 取消不是失败：中性灰，不上错误色。
    let failed = denied || (tool.outcome.is_some() && !tool.ok());

    // 标题：变体名；认不出来的工具退回自己的名字（上游：`工具名 · 摘要`）。
    let title = if variant == Variant::Others {
        present::others_title(&tool.name)
    } else {
        variant.title().to_owned()
    };
    // 摘要：待确认时说的是"将要做什么"（工具自己写的 preview），
    // 其余用参数派生（上游的 `deriveSummary`：模型写的一句话 > 命令本身）。
    let summary = if awaiting {
        tool.preview.clone()
    } else {
        let s = present::summary(variant, &tool.args);
        if s.is_empty() {
            tool.line()
        } else {
            s
        }
    };

    // 命令的退出码（非零才显示胶囊）。
    let exit_code = tool
        .outcome
        .as_ref()
        .and_then(|o| o.data.get("exit_code"))
        .and_then(serde_json::Value::as_i64);

    // 失败原因 / 待确认说明（单行）；取消的给一句中性交代。
    let err_text: Option<String> = if awaiting {
        None
    } else if cancelled {
        Some("已取消".to_owned())
    } else if failed {
        tool.outcome
            .as_ref()
            .and_then(|o| o.error.as_ref())
            .map(|e| match &e.hint {
                Some(h) => format!("{}（建议：{h}）", e.message),
                None => e.message.clone(),
            })
    } else {
        None
    };
    let err_color = if failed {
        p.error
    } else if cancelled {
        p.label_caption
    } else {
        p.accent
    };
    let err_galley = err_text.as_ref().map(|txt| {
        ui.painter()
            .layout(txt.clone(), skin.prop(t.caption), err_color, content_w - m.s(12.0))
    });

    // 上游：一行 14px/24px；leading 16px，右侧 6px。
    let lh = t.label_lh.max(t.label * 1.5);
    let pad_x = m.s(6.0);
    let lead = m.s(16.0);
    let sep = m.s(2.0);
    let gap = m.s(8.0);

    let err_h = err_galley
        .as_ref()
        .map(|g| g.size().y + m.s(4.0))
        .unwrap_or(0.0);
    let h = lh + m.s(6.0) * 2.0 + err_h;
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(content_w, h), Sense::hover());

    // 行本身透明（上游 `_row` 无底色）；悬停给一层极淡的底，表明可点。
    if resp.hovered() {
        ui.painter().squircle_filled(rect, m.s(8.0), p.hover);
    }

    let row = Rect::from_min_size(
        egui::pos2(rect.left(), rect.top() + m.s(6.0)),
        Vec2::new(rect.width(), lh),
    );
    let cy = row.center().y;
    let painter = ui.painter();

    // ---- leading：变体图标，或状态点（上游 leadingFor）----
    let lead_rect = Rect::from_center_size(
        egui::pos2(row.left() + pad_x + lead * 0.5, cy),
        Vec2::splat(lead),
    );
    if failed || denied {
        painter.circle_filled(lead_rect.center(), m.s(4.0), p.error);
    } else if awaiting {
        // 上游 stopped 用 warning 点；Neo 没有 warning token，用强调色代替。
        painter.circle_filled(lead_rect.center(), m.s(4.0), p.accent);
    } else if cancelled {
        painter.circle_filled(lead_rect.center(), m.s(4.0), p.label_caption);
    } else {
        icon_for(variant).paint(painter, lead_rect, p.label_tertiary, 1.5);
    }

    // ---- 标题 · 摘要 ----
    let title_font = skin.prop(t.label);
    let summary_font = skin.prop(t.label);
    let mut x = lead_rect.right() + m.s(6.0);

    let title_galley = painter.layout_no_wrap(title.clone(), title_font, p.label_primary);
    // 右侧要留出退出码胶囊的位置
    let pill_w = exit_code
        .filter(|c| *c != 0)
        .map(|_| m.s(30.0))
        .unwrap_or(0.0);
    let avail_end = row.right() - pad_x - pill_w;
    let title_w = title_galley.size().x.min((avail_end - x).max(0.0));

    painter.galley(
        egui::pos2(x, cy - title_galley.size().y * 0.5),
        title_galley.clone(),
        p.label_primary,
    );
    x += title_w;

    // 2pt 圆点分隔符（上游 `.sep`：2px 圆点，左右各 8px，label-caption 色）
    if x + gap * 2.0 + m.s(12.0) < avail_end {
        painter.circle_filled(egui::pos2(x + gap, cy), sep * 0.5, p.label_caption);
        x += gap * 2.0;
    }

    if x < avail_end {
        let shown = super::elide(painter, &summary, &summary_font, avail_end - x);
        painter.text(
            egui::pos2(x, cy),
            egui::Align2::LEFT_CENTER,
            shown,
            summary_font,
            p.label_tertiary,
        );
    }

    // ---- 退出码胶囊（非零）----
    if let Some(code) = exit_code.filter(|c| *c != 0) {
        let pill = Rect::from_min_size(
            egui::pos2(avail_end, cy - m.s(9.0)),
            Vec2::new(m.s(30.0), m.s(18.0)),
        );
        painter.squircle_filled(pill, m.s(6.0), p.error.gamma_multiply(0.18));
        painter.text(
            pill.center(),
            egui::Align2::CENTER_CENTER,
            code.to_string(),
            skin.mono(t.caption),
            p.error,
        );
    }

    // ---- 执行中：扫光 ----
    if running {
        sweep(ui, skin, row);
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(16));
    }

    // ---- 失败：第二行说明 ----
    if let Some(g) = err_galley {
        painter.galley(
            egui::pos2(row.left() + pad_x, row.bottom() + m.s(4.0)),
            g,
            p.error,
        );
    }
}

/// 变体 → 行首图标。
///
/// 上游的 leading 图标是**插件自己给的**（`keyed per-tool presentation`），
/// 它那套图标里没有终端、也没有"写入"；这里用 Neo 现有的图标挑最贴近的语义。
fn icon_for(variant: neo_tools::present::Variant) -> Icon {
    use neo_tools::present::Variant;
    match variant {
        Variant::Read => Icon::Folder,
        Variant::Write => Icon::Plus,
        Variant::Edit => Icon::Pen,
        Variant::Bash => Icon::ArrowRight,
        Variant::Search => Icon::Search,
        Variant::Code => Icon::Sparkle,
        // 屏幕交互：拿「面板」这个矩形图标表示一块屏幕。上游图标集里
        // 没有指针/鼠标，硬造一个反而会和整套几何不搭（见 icons.rs 的说明）。
        Variant::Screen => Icon::Board,
        Variant::Others => Icon::Dots,
    }
}

/// 执行中的扫光：一条柔和高光从左扫到右（上游 300px 宽、2.6s、ease-out 循环）。
fn sweep(ui: &Ui, skin: &Skin<'_>, row: egui::Rect) {
    let m = skin.m();
    let band = m.s(300.0);
    let period = 2.6f32;
    let time = ui.input(|i| i.time) as f32;
    let phase = (time / period) % 1.0;
    // 上游关键帧：0% → left = -300px；90%…100% → left = 100%（越界后不可见）。
    let p = (phase / 0.9).min(1.0);
    let travel = row.width() + band;
    let x = row.left() - band + p * travel;

    let tex = sweep_texture(ui.ctx(), skin);
    let target =
        egui::Rect::from_min_size(egui::pos2(x, row.top()), egui::vec2(band, row.height()));
    let p = ui.painter().with_clip_rect(row);
    p.image(
        tex.id(),
        target,
        egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
        Color32::WHITE,
    );
}

/// 扫光贴图：一条两端透明、中间稍亮的横向渐变（缓存一份）。
fn sweep_texture(ctx: &egui::Context, skin: &Skin<'_>) -> egui::TextureHandle {
    let id = egui::Id::new("neo-tool-sweep");
    if let Some(t) = ctx.data(|d| d.get_temp::<egui::TextureHandle>(id)) {
        return t;
    }
    const W: usize = 128;
    // 上游用的是「底色 60% 混合透明」；这里取正文色的低透明度高光，观感一致。
    let base = skin.p().label_primary;
    let pixels: Vec<Color32> = (0..W)
        .map(|i| {
            let t = i as f32 / (W - 1) as f32;
            // 两端为 0，中间 0.55 处最亮
            let a = if t < 0.55 { t / 0.55 } else { (1.0 - t) / 0.45 };
            base.gamma_multiply(a.clamp(0.0, 1.0) * 0.10)
        })
        .collect();
    let tex = ctx.load_texture(
        "neo-tool-sweep",
        egui::ColorImage::new([W, 1], pixels),
        egui::TextureOptions::LINEAR,
    );
    ctx.data_mut(|d| d.insert_temp(id, tex.clone()));
    tex
}
