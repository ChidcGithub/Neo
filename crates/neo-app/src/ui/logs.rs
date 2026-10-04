//! 本地诊断页：默认脱敏概括，显式开启后查看敏感错误详情；不进入模型上下文。

use crate::i18n::{tf, tr};
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
    selected: Option<u64>,
    copy_details_confirmed: bool,
    details_were_enabled: bool,
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
            selected: None,
            copy_details_confirmed: false,
            details_were_enabled: false,
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
            indices: view
                .entries
                .iter()
                .enumerate()
                .filter_map(|(index, entry)| {
                    matches_filter(entry, self.level, &lower).then_some(index)
                })
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
        let enabled = diagnostics::details_enabled();
        if enabled != self.details_were_enabled {
            self.reset_details();
            self.details_were_enabled = enabled;
        }
        let d = skin.d();
        let m = skin.m();
        let p = skin.p();
        let width = width.min(ui.available_width()).max(1.0);
        ui.push_id("neo-diagnostics-page", |ui| {
            ui.set_max_width(width);
            ui.spacing_mut().item_spacing = egui::vec2(m.s(8.0), m.s(8.0));
            ui.add(egui::Label::new(
                RichText::new(tr("诊断仅存内存，重启清空，不上传、不发送给模型。启动故障另记本地概括日志，不含聊天或工具原文，也不受此处清空影响。"))
                    .font(skin.prop(skin.t().caption)).color(p.label_secondary),
            ).wrap());
            ui.add(egui::Label::new(RichText::new(
                tr("id：操作编号；parent：关联任务；elapsed_ms 含等待确认时间。cancel_requested ≠ 已终止；background=true 不跟踪进程退出。")
            ).font(skin.prop(skin.t().caption)).color(p.label_secondary)).wrap());
            let mut enabled = diagnostics::details_enabled();
            let toggle = ui.checkbox(&mut enabled, tr("详细错误诊断（仅本次运行）"));
            #[cfg(test)]
            ui.ctx().data_mut(|data| data.insert_temp(egui::Id::new("neo-logs-details-toggle"), toggle.rect));
            ui.add(egui::Label::new(RichText::new(
                tr("详情可能含密钥等秘密，仅记录开启后的新错误。关闭后清除已保留详情，但不能撤回复制内容或擦除在途错误；不上传、不落盘、不发送给模型。")
            ).font(skin.prop(skin.t().caption)).color(p.warn)).wrap());
            if toggle.changed() {
                diagnostics::set_details_enabled(enabled);
                self.reset_details();
                self.details_were_enabled = enabled;
                view = diagnostics::snapshot();
            }
            if let Some(dir) = crate::startup::log_dir() {
                super::settings::kv_row(ui, skin, width, tr("启动日志目录"), &dir.display().to_string());
            }
            let selected = self.level.and_then(|level| Level::ALL.iter().position(|value| *value == level))
                .map_or(0, |index| index + 1);
            let options = [tr("全部"), tr("调试"), tr("信息"), tr("警告"), tr("错误")];
            let mut selection = None;
            let segment_width = options.iter().map(|label| {
                ui.painter().layout_no_wrap((*label).to_owned(), d.font_bold(d.t().label), p.label_primary)
                    .size().x + m.s(24.0)
            }).fold(0.0_f32, f32::max);
            if width >= (segment_width * options.len() as f32).max(m.s(240.0)) {
                selection = Segmented::new(&options, selected).id_salt("neo-logs-level").show(ui, &d, width);
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
                .hint(tr("筛选组件、id、parent、tool 或 kind"))
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
                    let label_width = ui.painter().layout_no_wrap(
                                            label.to_owned(), d.font_bold(d.t().label), p.label_primary,
                                        ).size().x + m.s(32.0);
                                        let response = if width < label_width.max(m.s(180.0)) {
                        ui.add(egui::Button::new(label).wrap())
                    } else {
                        Button::new(label).ghost().small().id_salt(salt).show(ui, &d)
                    };
                    #[cfg(test)]
                    ui.ctx().data_mut(|data| data.insert_temp(egui::Id::new(salt), response.rect));
                    response.clicked()
                };
                if action(if self.follow { tr("暂停跟随") } else { tr("恢复跟随") }, "neo-logs-follow") {
                    self.follow = !self.follow;
                    resume_follow = self.follow;
                }
                if action(tr("清空"), "neo-logs-clear") {
                    diagnostics::clear();
                    self.reset_details();
                    view = diagnostics::snapshot();
                }
                copy = action(tr("复制筛选结果"), "neo-logs-copy");
            });
            let filtered = self.filtered_entries(&view);
            let entries = &filtered.indices;
            if copy {
                // 只复制当前快照中匹配的脱敏条目，不复制输入条件或原始错误。
                ui.ctx().copy_text(export(entries.iter().map(|&index| &view.entries[index])));
            }
            ui.add(egui::Label::new(RichText::new(tf(
                "显示 {shown} / {total} 条 · 文本 {bytes} / {max_bytes} KiB · 上限 {max_entries} 条、单条 {max_entry_bytes} 字节\n收到 {received} 次 · 聚合重复 {merged} 次 · 容量丢弃 {dropped} 次 · 截断 {truncated} 次（自清空起）",
                &[
                    ("shown", entries.len().to_string()), ("total", view.entries.len().to_string()),
                    ("bytes", view.stats.bytes.div_ceil(1024).to_string()), ("max_bytes", (view.max_bytes / 1024).to_string()),
                    ("max_entries", view.max_entries.to_string()), ("max_entry_bytes", view.max_entry_bytes.to_string()),
                    ("received", view.stats.received.to_string()), ("merged", view.stats.merged.to_string()),
                    ("dropped", view.stats.dropped.to_string()), ("truncated", view.stats.truncated.to_string()),
                ],
            )).font(skin.prop(skin.t().caption)).color(p.label_secondary)).wrap());
            ui.add(egui::Label::new(tf("详情 {bytes} / {max_bytes} KiB · 单份上限 {max_entry_bytes} KiB · 已释放 {dropped} 份", &[
                ("bytes", view.stats.trace_bytes.div_ceil(1024).to_string()),
                ("max_bytes", (diagnostics::MAX_TRACE_BYTES / 1024).to_string()),
                ("max_entry_bytes", (diagnostics::MAX_TRACE_ENTRY_BYTES / 1024).to_string()),
                ("dropped", view.stats.traces_dropped.to_string()),
            ])).wrap());
            if view.stats.dropped > 0 {
                ui.add(egui::Label::new(RichText::new(tr("容量已满，旧记录已丢弃；历史不完整。"))
                    .font(skin.prop(skin.t().caption)).color(p.warn)).wrap());
            }
            if !self.follow {
                ui.add(egui::Label::new(RichText::new(tr("滚动已暂停，仍继续记录。"))
                    .font(skin.prop(skin.t().caption)).color(p.label_secondary)).wrap());
            }
            ui.add(egui::Label::new(RichText::new(tr("脱敏可能遗漏秘密；复制会写入系统剪贴板，请先检查。"))
                .font(skin.prop(skin.t().caption)).color(p.label_tertiary)).wrap());
            if entries.is_empty() {
                ui.add(egui::Label::new(RichText::new(if view.entries.is_empty() {
                    tr("暂无日志；启用记录后显示诊断概括。")
                } else {
                    tr("无匹配日志。")
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
            if let Some(entry) = self.selected.and_then(|id| view.entries.iter().find(|entry| entry.id == id)) {
                self.draw_detail(ui, entry);
            } else if self.selected.take().is_some() {
                self.copy_details_confirmed = false;
                ui.label(tr("所选记录已被淘汰或清空。"));
            } else {
                ui.label(tr("选择记录查看位置与已捕获堆栈。"));
            }
        });
    }

    fn reset_details(&mut self) {
        self.filtered = None;
        self.selected = None;
        self.copy_details_confirmed = false;
    }

    fn draw_detail(&mut self, ui: &mut Ui, entry: &Entry) {
        ui.separator();
        ui.label(tr(
            "错误详情 · 编译优化或缺少调试符号时，部分帧可能显示 unknown / <unknown>",
        ));
        let enabled = diagnostics::details_enabled();
        if enabled {
            let consent = ui.checkbox(
                &mut self.copy_details_confirmed,
                tr("我理解详细报告可能含秘密，并会进入系统剪贴板"),
            );
            #[cfg(test)]
            ui.ctx().data_mut(|data| {
                data.insert_temp(egui::Id::new("neo-logs-detail-consent"), consent.rect)
            });
            #[cfg(not(test))]
            let _ = consent;
            let copy = ui.add_enabled(
                self.copy_details_confirmed,
                egui::Button::new(tr("复制详细报告（含敏感原文）")).wrap(),
            );
            #[cfg(test)]
            ui.ctx().data_mut(|data| {
                data.insert_temp(egui::Id::new("neo-logs-copy-detail"), copy.rect)
            });
            if copy.clicked() && diagnostics::details_enabled() {
                ui.ctx().copy_text(detail_report(entry));
            }
        }
        // No cached String or selected Arc: revocation clears every snapshot handle.
        // Default label Copy/Cut bypasses consent; only the explicit button may export.
        egui::ScrollArea::both()
            .id_salt("neo-logs-detail-scroll")
            .max_height(260.0)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                let report = ui.add(
                    egui::Label::new(RichText::new(detail_report(entry)).monospace())
                        .selectable(false)
                        .extend(),
                );
                #[cfg(test)]
                ui.ctx().data_mut(|data| {
                    data.insert_temp(egui::Id::new("neo-logs-detail-text"), report.rect)
                });
                #[cfg(not(test))]
                let _ = report;
            });
    }

    fn draw_entries(
        &mut self,
        ui: &mut Ui,
        skin: &Skin<'_>,
        width: f32,
        filtered: &FilteredEntries,
        resume_follow: bool,
    ) -> egui::scroll_area::ScrollAreaOutput<usize> {
        let entries = &filtered.indices;
        let font = skin.prop(skin.t().caption);
        let row_height = (ui.fonts_mut(|fonts| fonts.row_height(&font))
            + 2.0 * ui.spacing().button_padding.y)
            .max(ui.spacing().interact_size.y);
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
            ui.set_max_width(
                (width - skin.m().s(12.0))
                    .min(ui.available_width())
                    .max(1.0),
            );
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
                let response = ui
                    .add(
                        egui::Button::selectable(
                            self.selected == Some(entry.id),
                            RichText::new(&text).font(font.clone()).color(color),
                        )
                        .truncate(),
                    )
                    .on_hover_text(text);
                #[cfg(test)]
                ui.ctx().data_mut(|data| {
                    data.insert_temp(egui::Id::new(("neo-logs-row", entry.id)), response.rect)
                });
                if response.clicked() {
                    self.selected = Some(entry.id);
                    self.copy_details_confirmed = false;
                }
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
    format!(
        "+{:02}:{:02}:{:02}.{:03}",
        ms / 3_600_000,
        ms / 60_000 % 60,
        ms / 1_000 % 60,
        ms % 1_000
    )
}

fn format_entry(entry: &Entry) -> String {
    let mut text = format!(
        "{} [{}] {} · {}",
        relative_time(entry.last_ms),
        entry.level.label(),
        entry.component,
        entry.message
    );
    if entry.occurrences > 1 {
        text.push_str(&tf(
            "  ×{count}（首次 {time}）",
            &[
                ("count", entry.occurrences.to_string()),
                ("time", relative_time(entry.first_ms)),
            ],
        ));
    }
    text
}

fn detail_report(entry: &Entry) -> String {
    let mut text = tf(
        "{entry}\n记录位置（非上游错误源）: {file}:{line}:{column}\n",
        &[
            ("entry", format_entry(entry)),
            ("file", entry.source.file.to_owned()),
            ("line", entry.source.line.to_string()),
            ("column", entry.source.column.to_string()),
        ],
    );
    if !diagnostics::details_enabled() {
        text.push_str(tr("未捕获详情或详情已清除；详细诊断已关闭。\n"));
        return text;
    }
    let Some(retained) = &entry.trace else {
        text.push_str(tr(
            "无已捕获堆栈：错误可能发生于开启前，或详情超过保留上限。\n",
        ));
        return text;
    };
    retained.inspect(|trace| {
        let Some(trace) = trace else {
            text.push_str(tr("详情已被容量淘汰、清空或关闭诊断移除。\n"));
            return;
        };
        let label = match retained.kind {
            diagnostics::TraceKind::Creation => {
                tr("错误创建位置 / creation stack（不是 OS 原始失败栈）")
            }
            diagnostics::TraceKind::Observation => {
                tr("错误观察位置 / observation stack（上游真实源未知）")
            }
        };
        let _ = writeln!(
            text,
            "{label}: {}:{}:{}",
            trace.location.file, trace.location.line, trace.location.column
        );
        text.push_str(tr("错误及 source 链（仅实际提供的错误）：\n"));
        if trace.causes.is_empty() {
            text.push_str(tr("未提供 source 链；不从字符串推测上游原因。\n"));
        }
        for (index, cause) in trace.causes.iter().enumerate() {
            let _ = writeln!(text, "[{index}] {cause}");
        }
        text.push_str(tr("已捕获的全部堆栈帧：\n"));
        if trace.backtrace.is_empty() {
            text.push_str(tr("无可用堆栈 / unknown symbols\n"));
        } else {
            text.push_str(&trace.backtrace);
            text.push('\n');
        }
        if trace.truncated {
            text.push_str(tr(
                "[已截断：64 KiB 总上限、原因链上限、循环或格式化失败；非完整堆栈报告]\n",
            ));
        }
    });
    text
}

fn export<'a>(entries: impl IntoIterator<Item = &'a Entry>) -> String {
    let mut text = tf("Neo {version} · {os} / {arch} · diagnostics v1\n进程内相对时间；筛选结果可能不完整；不含配置、正文或命令。\n", &[
            ("version", env!("CARGO_PKG_VERSION").to_owned()),
            ("os", std::env::consts::OS.to_owned()), ("arch", std::env::consts::ARCH.to_owned()),
        ]);
    for entry in entries {
        let _ = writeln!(text, "{}", format_entry(entry));
    }
    text
}

#[cfg(test)]
#[path = "logs_tests.rs"]
mod tests;
