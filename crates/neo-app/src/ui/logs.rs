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
            ui.add(egui::Label::new(RichText::new(
                "id 为本次进程内操作编号，parent 关联任务；elapsed_ms 包含等待确认时间。cancel_requested 不代表已经终止；background=true 仅表示后台启动，不跟踪进程退出。"
            ).font(skin.prop(skin.t().caption)).color(p.label_secondary)).wrap());
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
                .hint("筛选组件、id、parent、tool 或 kind")
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

fn relative_time(ms: u64) -> String {
    format!("+{:02}:{:02}:{:02}.{:03}", ms / 3_600_000, ms / 60_000 % 60, ms / 1_000 % 60, ms % 1_000)
}

fn format_entry(entry: &Entry) -> String {
    let mut text = format!("{} [{}] {} · {}", relative_time(entry.last_ms), entry.level.label(), entry.component, entry.message);
    if entry.occurrences > 1 {
        let _ = write!(text, "  ×{}（首次 {}）", entry.occurrences, relative_time(entry.first_ms));
    }
    text
}

fn export<'a>(entries: impl IntoIterator<Item = &'a Entry>) -> String {
    let mut text = format!("Neo {} · {} / {} · diagnostics v1\n进程内相对时间；筛选结果可能不完整；不含配置、正文或命令。\n", env!("CARGO_PKG_VERSION"), std::env::consts::OS, std::env::consts::ARCH);
    for entry in entries {
        let _ = writeln!(text, "{}", format_entry(entry));
    }
    text
}

#[cfg(test)]
#[path = "logs_tests.rs"]
mod tests;
