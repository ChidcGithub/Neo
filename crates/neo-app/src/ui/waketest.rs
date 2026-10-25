//! 复用唤醒引擎的本机测试页，不启动独立采集或转写。

use super::{
    settings::{kv_row, section_label_row},
    Skin,
};
use crate::i18n::{tf, tr};
use crate::state::AppState;

pub fn draw(ui: &mut egui::Ui, skin: &Skin<'_>, width: f32, state: &mut AppState) {
    ui.set_max_width(width);
    ui.spacing_mut().item_spacing.y = skin.m().s(8.0);
    let test = &state.wake_test;
    let blocked = if state.classroom_safe {
        Some(tr("安全模式下不可用"))
    } else if !state.wake_enabled {
        Some(tr("请先开启语音唤醒"))
    } else if test.busy
        || state.generating
        || state.tool_open
        || state.tool_round
        || state.compaction.is_some()
        || state.compaction_resume
        || state.wants_demo_reply
    {
        Some(tr("请结束后再测试"))
    } else if test.broken {
        Some(tr("唤醒引擎故障，请重启语音唤醒"))
    } else {
        None
    };
    if let Some(reason) = blocked {
        ui.add(egui::Label::new(reason).wrap());
    }
    let active = test.requested || test.running;
    let d = skin.d();
    let button = neo_ui::Button::new(if active {
        tr("停止测试")
    } else {
        tr("开始测试")
    })
    .elevated()
    .enabled(active || blocked.is_none())
    .wrap()
    .id_salt("wake-test-button")
    .show(ui, &d);
    #[cfg(test)]
    ui.ctx().data_mut(|data| {
        data.insert_temp(
            egui::Id::new("wake-test-button"),
            (button.rect, button.enabled()),
        )
    });
    if button.clicked() {
        state.wake_test.requested = !active;
        ui.ctx().request_repaint();
    }
    let test = &state.wake_test;
    let status = if test.running {
        tr("测试中")
    } else if test.requested {
        tr("等待开始")
    } else {
        tr("已停止")
    };
    ui.add(egui::Label::new(egui::RichText::new(status).strong()).wrap());
    let number = |value: Option<f32>| {
        value
            .map(|v| format!("{v:.4}"))
            .unwrap_or_else(|| tr("暂无").into())
    };
    if let Some(s) = &test.snapshot {
        let phase = match s.phase {
            neo_wake::WakePhase::Loading => tr("加载模型 / 等待麦克风"),
            neo_wake::WakePhase::Ready => tr("采集已启动"),
            neo_wake::WakePhase::Error => tr("故障"),
            neo_wake::WakePhase::Stopped => tr("引擎停止"),
        };
        ui.add_space(skin.m().s(8.0));
        section_label_row(ui, skin, width, tr("输入设备"));
        kv_row(ui, skin, width, tr("阶段"), phase);
        kv_row(
            ui,
            skin,
            width,
            tr("默认输入设备"),
            s.device_name.as_deref().unwrap_or(tr("尚未选定")),
        );
        kv_row(
            ui,
            skin,
            width,
            tr("采样率"),
            &s.sample_rate
                .map(|v| format!("{v} Hz"))
                .unwrap_or_else(|| tr("暂无").into()),
        );
        kv_row(
            ui,
            skin,
            width,
            tr("推理输入"),
            &tf(
                "{rate} Hz 单声道",
                &[("rate", neo_wake::SAMPLE_RATE.to_string())],
            ),
        );
        ui.separator();
        section_label_row(ui, skin, width, tr("音频电平"));
        kv_row(ui, skin, width, tr("电平 RMS"), &number(s.rms));
        kv_row(ui, skin, width, "dBFS", &number(s.dbfs));
        kv_row(ui, skin, width, tr("音频峰值"), &number(s.peak));
        if let Some(dbfs) = s.dbfs.filter(|v| v.is_finite()) {
            // 仅绘制快照，不额外采集。
            let level = ((dbfs + 60.0) / 60.0).clamp(0.0, 1.0);
            let (rect, _) =
                ui.allocate_exact_size(egui::vec2(width, skin.m().s(6.0)), egui::Sense::hover());
            ui.painter()
                .rect_filled(rect, skin.m().s(3.0), skin.p().border_l1);
            let filled = egui::Rect::from_min_size(
                rect.min,
                egui::vec2(rect.width() * level, rect.height()),
            );
            ui.painter()
                .rect_filled(filled, skin.m().s(3.0), skin.p().label_secondary);
        }
        ui.separator();
        section_label_row(ui, skin, width, tr("唤醒识别"));
        kv_row(ui, skin, width, tr("当前得分"), &number(s.score));
        kv_row(ui, skin, width, tr("近 2 秒最高"), &number(s.score_peak_2s));
        kv_row(
            ui,
            skin,
            width,
            tr("实际阈值"),
            &format!("{:.4}", s.threshold),
        );
        kv_row(
            ui,
            skin,
            width,
            tr("预热进度"),
            &tf(
                "{frames} / {total} 帧",
                &[
                    ("frames", s.warmup_frames.to_string()),
                    ("total", s.warmup_total.to_string()),
                ],
            ),
        );
        kv_row(
            ui,
            skin,
            width,
            tr("引擎累计确认命中"),
            &tf(
                "{count} 次",
                &[("count", s.hit_count.to_string())],
            ),
        );
    } else {
        ui.add(egui::Label::new(tr("暂无测试快照。")).wrap());
    }
    if let Some(error) = &test.error {
        ui.add(
            egui::Label::new(
                egui::RichText::new(tf("最近错误：{error}", &[("error", error.clone())]))
                    .color(skin.p().error),
            )
            .wrap(),
        );
    } else {
        ui.add(egui::Label::new(tr("最近错误：无")).wrap());
    }
}

#[cfg(test)]
#[path = "waketest_tests.rs"]
mod tests;
