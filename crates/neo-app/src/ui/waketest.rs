//! 复用唤醒引擎的本机测试页，不启动独立采集或转写。

use crate::state::AppState;
use super::{settings::{kv_row, section_label_row}, Skin};

pub fn draw(ui: &mut egui::Ui, skin: &Skin<'_>, width: f32, state: &mut AppState) {
    ui.set_max_width(width);
    ui.spacing_mut().item_spacing.y = skin.m().s(8.0);
    let test = &state.wake_test;
    let blocked = if state.classroom_safe {
        Some("课堂安全模式已开启，禁止开始测试。请先在通用页明确关闭安全模式。")
    } else if !state.wake_enabled {
        Some("语音唤醒已关闭。请先在通用页打开语音唤醒，再手动开始测试。")
    } else if test.busy || state.generating || state.tool_open || state.tool_round
        || state.compaction.is_some() || state.compaction_resume || state.wants_demo_reply {
        Some("任务或听写进行中，不能开始测试；请先停止或等待结束。")
    } else if test.broken {
        Some("唤醒引擎故障已锁存。修复后在通用页关闭语音唤醒，等界面更新后再打开，即可手动重试；本页不会自动重建模型。")
    } else {
        None
    };
    if let Some(reason) = blocked {
        ui.add(egui::Label::new(reason).wrap());
    }
    let active = test.requested || test.running;
    let button = ui.add_enabled(active || blocked.is_none(),
        egui::Button::new(if active { "停止测试" } else { "开始测试" }).wrap());
    #[cfg(test)]
    ui.ctx().data_mut(|data| data.insert_temp(egui::Id::new("wake-test-button"), (button.rect, button.enabled())));
    if button.clicked() {
        state.wake_test.requested = !active;
        ui.ctx().request_repaint();
    }
    let test = &state.wake_test;
    let status = if test.running { "测试运行中：命中仅计数，不会开始听写或发送任务" }
        else if test.requested { "等待现有唤醒引擎就绪" }
        else { "测试已停止：下方保留上次快照，不代表当前实时状态" };
    ui.add(egui::Label::new(egui::RichText::new(status).strong()).wrap());
    let number = |value: Option<f32>| value.map(|v| format!("{v:.4}")).unwrap_or_else(|| "暂无".into());
    if let Some(s) = &test.snapshot {
        let phase = match s.phase {
            neo_wake::WakePhase::Loading => "加载模型 / 等待麦克风",
            neo_wake::WakePhase::Ready => "采集已启动",
            neo_wake::WakePhase::Error => "故障",
            neo_wake::WakePhase::Stopped => "引擎停止",
        };
        ui.add_space(skin.m().s(8.0));
        section_label_row(ui, skin, width, "输入设备");
        kv_row(ui, skin, width, "阶段", phase);
        kv_row(ui, skin, width, "默认输入设备", s.device_name.as_deref().unwrap_or("尚未选定"));
        kv_row(ui, skin, width, "采样率", &s.sample_rate.map(|v| format!("{v} Hz")).unwrap_or_else(|| "暂无".into()));
        kv_row(ui, skin, width, "推理输入", &format!("{} Hz 单声道", neo_wake::SAMPLE_RATE));
        ui.separator();
        section_label_row(ui, skin, width, "音频电平");
        kv_row(ui, skin, width, "电平 RMS", &number(s.rms));
        kv_row(ui, skin, width, "dBFS", &number(s.dbfs));
        kv_row(ui, skin, width, "音频峰值", &number(s.peak));
        if let Some(dbfs) = s.dbfs.filter(|v| v.is_finite()) {
            // 仅绘制现有快照，无动画、无额外采集或刷新定时器。
            let level = ((dbfs + 60.0) / 60.0).clamp(0.0, 1.0);
            let (rect, _) = ui.allocate_exact_size(egui::vec2(width, skin.m().s(6.0)), egui::Sense::hover());
            ui.painter().rect_filled(rect, skin.m().s(3.0), skin.p().border_l1);
            let filled = egui::Rect::from_min_size(rect.min, egui::vec2(rect.width() * level, rect.height()));
            ui.painter().rect_filled(filled, skin.m().s(3.0), skin.p().label_secondary);
            ui.add(egui::Label::new(egui::RichText::new("电平刻度 −60 至 0 dBFS · 停止后保留快照")
                .font(skin.prop(skin.t().caption)).color(skin.p().label_caption)).wrap());
        }
        ui.separator();
        section_label_row(ui, skin, width, "唤醒识别");
        kv_row(ui, skin, width, "当前得分", &number(s.score));
        kv_row(ui, skin, width, "近 2 秒最高", &number(s.score_peak_2s));
        kv_row(ui, skin, width, "实际阈值", &format!("{:.4}（只读）", s.threshold));
        kv_row(ui, skin, width, "预热进度", &format!("{} / {} 帧", s.warmup_frames, s.warmup_total));
        kv_row(ui, skin, width, "引擎累计确认命中", &format!("{} 次（含正常监听，并非本轮测试独有）", s.hit_count));
    } else {
        ui.add(egui::Label::new("阶段、设备、采样率、电平、峰值、得分、阈值、预热与命中次数：尚无测试快照。").wrap());
    }
    if let Some(error) = &test.error {
        ui.add(egui::Label::new(egui::RichText::new(format!("最近错误（保留）：{error}")).color(skin.p().error)).wrap());
    } else {
        ui.add(egui::Label::new("最近错误：无").wrap());
    }
    ui.separator();
    section_label_row(ui, skin, width, "隐私与排查指引");
    for text in [
        "仅复用当前唤醒模型和麦克风。测试时只观测，不听写、不发送；只在临时内存保留诊断，不上传、不写入日志或对话。",
        "离开本页、关闭设置、隐藏主窗或切换安全模式会停止测试；停止测试不会卸载正常唤醒模型，返回正常监听前会重新预热。停止测试不等于关闭正常语音唤醒。",
        "设备由 Windows 默认输入设备决定。请在 Windows 设置 → 系统 → 声音 → 输入选择正确麦克风并检查应用权限。显示的是启动时设备；变更后可关闭再打开语音唤醒。",
        "学校电脑若提示模型、ONNX Runtime 或资源缺失，请联系管理员核对部署资源与权限，不要反复点击开始，也不要擅自降低阈值。",
        "先在近距离、安静环境说 Hi, Neo，等待预热完成，对比静音电平和得分；再回到教室距离与背景噪声条件比较。若近距正常而远距失败，优先检查麦克风位置、增益及噪声，不擅调阈值。",
    ] {
        ui.add(egui::Label::new(text).wrap());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wake_test_narrow_page_wraps_and_blocks_unsafe_start() {
        for width in [240.0, 320.0, 460.0] {
            for scale in [0.85, 1.0, 1.75, 2.8] {
                let ctx = crate::ui::composer::ui_regression::context();
                let theme = neo_theme::Theme::from_metrics(neo_theme::ThemeMode::Dark,
                    neo_theme::Metrics::from_scale(scale));
                theme.apply(&ctx);
                let whale = crate::brand::WhaleMark::cached(&ctx);
                let skin = Skin::new(theme, &whale);
                let mut state = AppState::default();
                state.wake_test.error = Some("学校模型资源缺失，请联系管理员核对部署资源与权限".repeat(3));
                state.wake_test.snapshot = Some(neo_wake::WakeDiagnostics {
                    enabled: true, test_mode: true, phase: neo_wake::WakePhase::Ready,
                    device_name: Some("Windows 默认输入设备名称很长的学校教室麦克风".repeat(3)),
                    sample_rate: Some(48_000), rms: Some(0.02), dbfs: Some(-34.0),
                    peak: Some(0.3), score: Some(0.4), score_peak_2s: Some(0.5),
                    threshold: 0.25, warmup_frames: 25, warmup_total: 25, dictating: false,
                    hit_count: 3, last_hit_ms: 0, updated_at_ms: 0, last_audio_ms: 0,
                    error: None,
                });
                let mut output = ctx.run_ui(egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, 12000.0))),
                    ..Default::default()
                }, |ui| {
                    draw(ui, &skin, width - 16.0, &mut state);
                    assert!(ui.min_rect().width() <= width, "overflow {width}/{scale}");
                });
                output.textures_delta.clear();
                let (_, enabled): (egui::Rect, bool) = ctx.data(|d| d.get_temp(egui::Id::new("wake-test-button")).unwrap());
                assert!(!enabled);
                for clipped in &output.shapes {
                    if let egui::Shape::Text(text) = &clipped.shape {
                        assert!(!text.galley.elided);
                        let bounds = egui::Rect::from_min_size(text.pos, text.galley.size());
                        assert!(clipped.clip_rect.expand(1.0).contains_rect(bounds),
                            "clipped {width}/{scale}: {:?}, clip={:?}, bounds={bounds:?}",
                            text.galley.text(), clipped.clip_rect);
                    }
                }
                assert!(output.platform_output.commands.is_empty());
                assert!(!state.wake_test.requested);
            }
        }
    }
}
