//! 本地诊断页：只读取脱敏日志，不依赖 AppState，不把日志送入模型上下文。

use std::fmt::Write as _;
use std::sync::Arc;
use std::time::Duration;

use egui::{RichText, Ui};
use neo_ui::{Button, Segmented, TextField};

use super::Skin;
use crate::diagnostics::{self, Entry, Level, Snapshot};

/// 保存交互状态及当前筛选索引，共享一份有界脱敏快照；默认自动跟随。
#[derive(Clone)]
pub struct LogViewState {
    pub level: Option<Level>,
    pub keyword: String,
    pub follow: bool,
    filtered: Option<Arc<FilteredEntries>>,
}

struct FilteredEntries {
    entries: Arc<Vec<Entry>>,
    level: Option<Level>,
    keyword: String,
    indices: Vec<usize>,
}

impl Default for LogViewState {
    fn default() -> Self {
        Self {
            level: None,
            keyword: String::new(),
            follow: true,
            filtered: None,
        }
    }
}

/// 无需给 AppState 增加字段；筛选和跟随状态保存在当前 egui 上下文的临时数据中。
/// 调用方给出内容区宽度即可，不需要在模型消息或持久化配置中保存任何内容。
pub fn draw(ui: &mut Ui, skin: &Skin<'_>, width: f32) {
    let id = ui.id().with("neo-log-view-state");
    let mut state = ui
        .ctx()
        .data_mut(|data| data.get_temp::<LogViewState>(id).unwrap_or_default());
    state.draw(ui, skin, width);
    ui.ctx().data_mut(|data| data.insert_temp(id, state));
}

impl LogViewState {
    fn filtered_entries(&mut self, view: &Snapshot) -> Arc<FilteredEntries> {
        let keyword = self.keyword.trim();
        if let Some(cached) = &self.filtered {
            if Arc::ptr_eq(&cached.entries, &view.entries)
                && cached.level == self.level
                && cached.keyword == keyword
            {
                return Arc::clone(cached);
            }
        }
        let lower = keyword.to_lowercase();
        let filtered = Arc::new(FilteredEntries {
            entries: Arc::clone(&view.entries),
            level: self.level,
            keyword: keyword.to_owned(),
            indices: view.entries.iter().enumerate()
                .filter_map(|(index, entry)| matches_filter(entry, self.level, &lower).then_some(index))
                .collect(),
        });
        self.filtered = Some(Arc::clone(&filtered));
        filtered
    }

    /// 也可由调用方自行保存独立 LogViewState，并直接调用此方法。
    pub fn draw(&mut self, ui: &mut Ui, skin: &Skin<'_>, width: f32) {
        let view = diagnostics::snapshot();
        self.draw_snapshot(ui, skin, width, view);
        // 后台线程无须知道 egui；只有打开日志页时才定期刷新。
        ui.ctx().request_repaint_after(Duration::from_millis(250));
    }

    fn draw_snapshot(&mut self, ui: &mut Ui, skin: &Skin<'_>, width: f32, mut view: Snapshot) {
        let d = skin.d();
        let m = skin.m();
        let p = skin.p();
        let width = width.min(ui.available_width()).max(1.0);
        ui.push_id("neo-diagnostics-page", |ui| {
            ui.set_max_width(width);
            ui.spacing_mut().item_spacing = egui::vec2(m.s(8.0), m.s(8.0));
            ui.add(egui::Label::new(
                RichText::new("下方为进程内诊断，重启后丢失，不落盘、不上传、不进入模型上下文。启动故障另记本地安全概括日志，不含聊天或工具原文；此处清空不删除启动日志。")
                    .font(skin.prop(skin.t().caption)).color(p.label_secondary),
            ).wrap());
            if let Some(dir) = crate::startup::log_dir() {
                super::settings::kv_row(ui, skin, width, "启动日志目录", &dir.display().to_string());
            }
            let selected = self.level.and_then(|level| Level::ALL.iter().position(|value| *value == level))
                .map_or(0, |index| index + 1);
            let options = ["全部", "调试", "信息", "警告", "错误"];
            let mut selection = None;
            if width >= m.s(240.0) {
                selection = Segmented::new(&options, selected).show(ui, &d, width);
            } else {
                // 窄屏下分行，避免五段固定宽度把文字挤在一起。
                ui.horizontal_wrapped(|ui| {
                    for (index, label) in options.iter().enumerate() {
                        if ui.selectable_label(selected == index, *label).clicked() {
                            selection = Some(index);
                        }
                    }
                });
            }
            if let Some(index) = selection {
                self.level = index.checked_sub(1).and_then(|index| Level::ALL.get(index).copied());
            }
            TextField::new(&mut self.keyword)
                .hint("筛选组件或安全概括（忽略大小写）")
                .id_salt("neo-logs-keyword")
                .show(ui, &d, width);
            // 筛选条件不是日志内容；限制长度，避免每帧无界复制。
            if self.keyword.len() > 256 {
                let mut end = 256;
                while !self.keyword.is_char_boundary(end) {
                    end -= 1;
                }
                self.keyword.truncate(end);
            }
            let mut copy = false;
            let mut resume_follow = false;
            ui.horizontal_wrapped(|ui| {
                let mut action = |label: &str, salt: &str| {
                    let response = if width < m.s(180.0) {
                        ui.add(egui::Button::new(label).wrap())
                    } else {
                        Button::new(label).ghost().small().id_salt(salt).show(ui, &d)
                    };
                    #[cfg(test)]
                    ui.ctx().data_mut(|data| data.insert_temp(egui::Id::new(salt), response.rect));
                    response.clicked()
                };
                if action(if self.follow { "暂停跟随" } else { "恢复跟随" }, "neo-logs-follow") {
                    self.follow = !self.follow;
                    resume_follow = self.follow;
                }
                if action("清空", "neo-logs-clear") {
                    diagnostics::clear();
                    view = diagnostics::snapshot();
                }
                copy = action("复制筛选结果", "neo-logs-copy");
            });
            let filtered = self.filtered_entries(&view);
            let entries = &filtered.indices;
            if copy {
                // 只复制当前快照中匹配的脱敏条目，不复制输入条件或原始错误。
                ui.ctx().copy_text(export(entries.iter().map(|&index| &view.entries[index])));
            }
            ui.add(egui::Label::new(RichText::new(format!(
                "显示 {} / {} 条 · 文本 {} / {} KiB · 上限 {} 条、单条 {} 字节\n收到 {} 次 · 聚合重复 {} 次 · 容量丢弃 {} 次 · 截断 {} 次（自清空起）",
                entries.len(), view.entries.len(), view.stats.bytes.div_ceil(1024),
                view.max_bytes / 1024, view.max_entries, view.max_entry_bytes,
                view.stats.received, view.stats.merged, view.stats.dropped, view.stats.truncated,
            )).font(skin.prop(skin.t().caption)).color(p.label_secondary)).wrap());
            if view.stats.dropped > 0 {
                ui.add(egui::Label::new(RichText::new("容量已满，较旧记录已被丢弃；当前视图不是完整历史。")
                    .font(skin.prop(skin.t().caption)).color(p.warn)).wrap());
            }
            if !self.follow {
                ui.add(egui::Label::new(RichText::new("已暂停自动滚动，后台仍继续记录并更新计数。")
                    .font(skin.prop(skin.t().caption)).color(p.label_secondary)).wrap());
            }
            ui.add(egui::Label::new(RichText::new("凭据标记或 URL 会整字段隐藏；脱敏不保证识别所有秘密，请勿在调用点记录原文。复制后内容会进入系统剪贴板。")
                .font(skin.prop(skin.t().caption)).color(p.label_tertiary)).wrap());
            if entries.is_empty() {
                ui.add(egui::Label::new(RichText::new(if view.entries.is_empty() {
                    "暂无日志。这里仅展示启用记录后的安全诊断概括。"
                } else {
                    "没有匹配当前筛选条件的日志。"
                }).font(skin.prop(skin.t().body)).color(p.label_secondary)).wrap());
            } else {
                let scroll = self.draw_entries(ui, skin, width, &filtered, resume_follow);
                // egui 在用户滚离底部后解除吸附，按钮也必须同步为可恢复状态。
                if self.follow && scroll.state.offset.y + scroll.inner_rect.height() + 1.0 < scroll.content_size.y {
                    self.follow = false;
                }
                #[cfg(test)]
                ui.ctx().data_mut(|data| data.insert_temp(egui::Id::new("neo-logs-scroll-probe"),
                    (scroll.state.offset.y, scroll.inner_rect, scroll.content_size.y, scroll.inner)));
            }
        });
    }

    fn draw_entries(
        &self,
        ui: &mut Ui,
        skin: &Skin<'_>,
        width: f32,
        filtered: &FilteredEntries,
        resume_follow: bool,
    ) -> egui::scroll_area::ScrollAreaOutput<usize> {
        let entries = &filtered.indices;
        let font = skin.prop(skin.t().caption);
        let row_height = ui.fonts_mut(|fonts| fonts.row_height(&font));
        let mut scroll = egui::ScrollArea::vertical()
            .id_salt("neo-logs-entries")
            .max_height(skin.m().s(300.0))
            .auto_shrink([false, true])
            .stick_to_bottom(self.follow);
        if resume_follow {
            // stick_to_bottom 不会重新吸附手动滚离底部的视图，恢复时显式跳到底部。
            scroll = scroll.vertical_scroll_offset(
                (row_height + ui.spacing().item_spacing.y) * entries.len() as f32,
            );
        }
        // 仅排版可见行；长概括单行省略，悬停查看完整安全内容，复制仍保留全文。
        scroll.show_rows(ui, row_height, entries.len(), |ui, rows| {
            ui.set_max_width((width - skin.m().s(12.0)).min(ui.available_width()).max(1.0));
            let count = rows.len();
            for row in rows {
                let entry = &filtered.entries[entries[row]];
                let color = match entry.level {
                    Level::Error => skin.p().error,
                    Level::Warn => skin.p().warn,
                    Level::Info => skin.p().label_primary,
                    Level::Debug => skin.p().label_secondary,
                };
                let text = format_entry(entry);
                ui.add(egui::Label::new(RichText::new(&text).font(font.clone()).color(color))
                    .truncate()).on_hover_text(text);
            }
            count
        })
    }
}

fn matches_filter(entry: &Entry, level: Option<Level>, keyword: &str) -> bool {
    level.is_none_or(|level| entry.level == level)
        && (keyword.is_empty()
            || entry.component.to_lowercase().contains(keyword)
            || entry.message.to_lowercase().contains(keyword))
}

fn format_entry(entry: &Entry) -> String {
    format!(
        "+{}.{:03}s [{}] {} · {}  ×{}（首次 +{}.{:03}s）",
        entry.last_ms / 1000,
        entry.last_ms % 1000,
        entry.level.label(),
        entry.component,
        entry.message,
        entry.occurrences,
        entry.first_ms / 1000,
        entry.first_ms % 1000,
    )
}

fn export<'a>(entries: impl IntoIterator<Item = &'a Entry>) -> String {
    let mut text = String::new();
    for entry in entries {
        let _ = writeln!(text, "{}", format_entry(entry));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostics::{Stats, MAX_BYTES, MAX_ENTRIES, MAX_ENTRY_BYTES};

    fn sample() -> Snapshot {
        Snapshot {
            entries: Arc::new(vec![
                Entry {
                    level: Level::Error,
                    component: "Tool".into(),
                    message: "执行失败：timeout".into(),
                    first_ms: 1,
                    last_ms: 2,
                    occurrences: 3,
                },
                Entry {
                    level: Level::Info,
                    component: "app".into(),
                    message: "[已脱敏]".into(),
                    first_ms: 3,
                    last_ms: 3,
                    occurrences: 1,
                },
            ]),
            stats: Stats {
                received: 10,
                merged: 2,
                dropped: 6,
                truncated: 0,
                bytes: 50,
            },
            max_entries: MAX_ENTRIES,
            max_bytes: MAX_BYTES,
            max_entry_bytes: MAX_ENTRY_BYTES,
        }
    }

    #[test]
    fn filter_cache_reuses_indices_and_refreshes_for_conditions_and_snapshot() {
        let mut view = sample();
        view.entries = Arc::new((0..MAX_ENTRIES).map(|index| Entry {
            level: if index % 2 == 0 { Level::Info } else { Level::Error },
            component: "Tool".into(), message: format!("安全事件 {index}").into_boxed_str(),
            first_ms: index as u64, last_ms: index as u64, occurrences: 1,
        }).collect());
        let mut state = LogViewState { keyword: "安全事件".into(), ..Default::default() };
        let first = state.filtered_entries(&view);
        assert_eq!(first.indices.len(), MAX_ENTRIES);
        let mut legacy_matches = 0;
        let mut rebuilt = 0;
        for _ in 0..60 {
            let keyword = state.keyword.trim().to_lowercase();
            let legacy: Vec<_> = view.entries.iter().filter(|entry| {
                legacy_matches += 1;
                matches_filter(entry, state.level, &keyword)
            }).collect();
            assert_eq!(legacy.len(), first.indices.len());
            // egui get_temp 也会克隆状态：索引与日志文本都应继续共享。
            state = state.clone();
            let next = state.filtered_entries(&view.clone());
            rebuilt += usize::from(!Arc::ptr_eq(&first, &next));
        }
        assert_eq!(legacy_matches, 60 * MAX_ENTRIES);
        assert_eq!(rebuilt, 0);
        println!("1000项，预热后60次筛选：旧路径匹配次数={legacy_matches}，缓存索引重建={rebuilt}");
        state.level = Some(Level::Error);
        let errors = state.filtered_entries(&view);
        assert!(!Arc::ptr_eq(&first, &errors));
        assert_eq!(errors.indices.len(), 500);
        state.keyword = " TOOL ".into();
        let by_component = state.filtered_entries(&view);
        assert!(!Arc::ptr_eq(&errors, &by_component));
        assert_eq!(by_component.indices, errors.indices);
        state.keyword = "missing".into();
        assert!(state.filtered_entries(&view).indices.is_empty());
        state.keyword.clear();
        let old = Arc::downgrade(&state.filtered_entries(&view));
        let mut changed = view.clone();
        Arc::make_mut(&mut changed.entries)[1].occurrences = 9;
        let updated = state.filtered_entries(&changed);
        assert!(old.upgrade().is_none(), "仅保留当前筛选而非历史结果");
        assert_eq!(updated.entries[1].occurrences, 9);
        assert!(export(updated.indices.iter().map(|&index| &updated.entries[index])).contains("×9"));
        changed.entries = Arc::new(Vec::new());
        changed.stats = Stats::default();
        let cleared = state.filtered_entries(&changed);
        assert!(cleared.indices.is_empty());
        assert!(!Arc::ptr_eq(&updated, &cleared));
    }

    #[test]
    fn filters_and_copy_use_only_visible_safe_entries() {
        let view = sample();
        assert!(matches_filter(&view.entries[0], Some(Level::Error), "tool"));
        assert!(matches_filter(&view.entries[0], None, "失败"));
        assert!(!matches_filter(&view.entries[0], Some(Level::Warn), ""));
        assert!(!matches_filter(&view.entries[0], None, "missing"));
        let entries: Vec<_> = view
            .entries
            .iter()
            .filter(|entry| matches_filter(entry, Some(Level::Info), ""))
            .collect();
        let copied = export(entries);
        assert!(copied.contains("[已脱敏]"));
        assert!(!copied.contains("timeout"));
        assert!(export([]).is_empty());
    }

    #[test]
    fn virtual_rows_pause_and_resume_without_laying_out_the_entire_buffer() {
        let ctx = egui::Context::default();
        let mut fonts = egui::FontDefinitions::default();
        let family = fonts.families[&egui::FontFamily::Proportional].clone();
        fonts.families.insert(neo_theme::fonts::bold(), family.clone());
        fonts.families.insert(neo_theme::fonts::mono(), family);
        ctx.set_fonts(fonts);
        let theme = neo_theme::Theme::new(neo_theme::ThemeMode::Dark, 1080.0, neo_theme::Distance::Standard);
        theme.apply(&ctx);
        let whale = crate::brand::WhaleMark::cached(&ctx);
        let skin = Skin::new(theme, &whale);
        let mut entries = vec![sample().entries[0].clone(); MAX_ENTRIES];
        for entry in entries.iter_mut().take(120) {
            entry.message = "安全概括".repeat(150).into_boxed_str();
        }
        let entries = Arc::new(entries);
        let mut state = LogViewState { follow: false, ..Default::default() };
        let render = |state: &LogViewState, count: usize, resume: bool| {
            let mut scroll = None;
            let mut output = ctx.run_ui(egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(240.0, 700.0))),
                ..Default::default()
            }, |ui| {
                let visible = FilteredEntries {
                    entries: Arc::clone(&entries), level: None, keyword: String::new(),
                    indices: (0..count).collect(),
                };
                scroll = Some(state.draw_entries(ui, &skin, 224.0, &visible, resume));
                assert!(ui.min_rect().width() <= 240.0);
            });
            output.textures_delta.clear();
            assert!(output.platform_output.commands.is_empty());
            let scroll = scroll.unwrap();
            assert!(scroll.inner < 40, "不应排版全部 {count} 条日志");
            scroll
        };
        let initial = render(&state, 500, false);
        assert_eq!(initial.state.offset.y, 0.0);
        let grown = render(&state, MAX_ENTRIES, false);
        assert_eq!(grown.state.offset.y, 0.0);
        state.follow = true;
        let resumed = render(&state, MAX_ENTRIES, true);
        assert!(resumed.state.offset.y > 0.0);
        assert!((resumed.state.offset.y - (resumed.content_size.y - resumed.inner_rect.height())).abs() < 1.0);
        state.follow = false;
        let paused = render(&state, MAX_ENTRIES, false);
        assert!((paused.state.offset.y - resumed.state.offset.y).abs() < 1.0);
    }

    #[test]
    fn pointer_copy_pause_resume_and_wheel_match_the_visible_thousand_entry_view() {
        use crate::ui::composer::ui_regression::{context, frame, pointer, probe};
        let ctx = context();
        let size = egui::vec2(460.0, 900.0);
        let mut view = sample();
        view.entries = Arc::new((0..MAX_ENTRIES).map(|index| Entry {
            level: if index % 2 == 0 { Level::Info } else { Level::Error },
            component: "safe-test".into(), message: format!("安全事件 {index}").into_boxed_str(),
            first_ms: index as u64, last_ms: index as u64, occurrences: 1,
        }).collect());
        let mut state = LogViewState::default();
        let render = |state: &mut LogViewState, events| frame(&ctx, size, events,
            |ui, skin| state.draw_snapshot(ui, skin, 444.0, view.clone()));
        for _ in 0..3 { render(&mut state, vec![]); }
        let cached = state.filtered.clone().unwrap();
        for _ in 0..60 {
            render(&mut state, vec![]);
            assert!(Arc::ptr_eq(&cached, state.filtered.as_ref().unwrap()));
        }
        let (bottom, rect, content, count): (f32, egui::Rect, f32, usize) = probe(&ctx, "neo-logs-scroll-probe");
        assert!(state.follow && bottom > 0.0 && count < 40);
        assert!((bottom + rect.height() - content).abs() < 1.0);
        render(&mut state, vec![egui::Event::PointerMoved(rect.center()),
            egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Point,
                delta: egui::vec2(0.0, 150.0), modifiers: egui::Modifiers::NONE, phase: egui::TouchPhase::Move }]);
        for _ in 0..10 { render(&mut state, vec![]); }
        assert!(!state.follow, "manual scroll must expose the resume action");
        let button: egui::Rect = probe(&ctx, "neo-logs-follow");
        render(&mut state, pointer(button.center(), true));
        render(&mut state, pointer(button.center(), false));
        assert!(state.follow);
        let (offset, rect, content, _): (f32, egui::Rect, f32, usize) = probe(&ctx, "neo-logs-scroll-probe");
        assert!((offset + rect.height() - content).abs() < 1.0);
        render(&mut state, pointer(button.center(), true));
        render(&mut state, pointer(button.center(), false));
        assert!(!state.follow);
        state.level = Some(Level::Error);
        state.keyword = "SAFE-TEST".into();
        render(&mut state, vec![]);
        let copy: egui::Rect = probe(&ctx, "neo-logs-copy");
        render(&mut state, pointer(copy.center(), true));
        let output = render(&mut state, pointer(copy.center(), false));
        let copied = output.platform_output.commands.iter().find_map(|command| {
            if let egui::OutputCommand::CopyText(text) = command { Some(text) } else { None }
        }).expect("copy action must produce only a clipboard command");
        let filtered: Vec<_> = view.entries.iter().filter(|entry| entry.level == Level::Error).collect();
        assert_eq!(copied, &export(filtered));
        assert_eq!(copied.lines().count(), 500);
        assert!(!state.follow);
    }

    #[test]
    fn page_draws_headlessly_in_both_themes_and_narrow_widths() {
        for mode in [neo_theme::ThemeMode::Light, neo_theme::ThemeMode::Dark] {
            for width in [160.0, 240.0, 460.0] {
              for scale in [0.85, 1.0, 1.75, 2.8] {
                let ctx = egui::Context::default();
                let mut fonts = egui::FontDefinitions::default();
                let family = fonts.families[&egui::FontFamily::Proportional].clone();
                fonts
                    .families
                    .insert(neo_theme::fonts::bold(), family.clone());
                fonts.families.insert(neo_theme::fonts::mono(), family);
                ctx.set_fonts(fonts);
                let theme = neo_theme::Theme::from_metrics(mode, neo_theme::Metrics::from_scale(scale));
                theme.apply(&ctx);
                let whale = crate::brand::WhaleMark::cached(&ctx);
                let skin = Skin::new(theme, &whale);
                let mut state = LogViewState {
                    follow: false,
                    ..Default::default()
                };
                let mut output = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(width, 700.0),
                        )),
                        ..Default::default()
                    },
                    |ui| {
                        state.draw_snapshot(ui, &skin, width - 16.0, sample());
                        assert!(ui.min_rect().width() <= width, "窄屏布局不应横向溢出");
                    },
                );
                // 纯 egui 测试不提交 GPU 纹理，显式消费待上传列表。
                output.textures_delta.clear();
                assert!(!output.shapes.is_empty());
                assert!(!state.follow);
                assert!(output.platform_output.commands.is_empty());
              }
            }
        }
    }
}
