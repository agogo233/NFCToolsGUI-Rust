//! 右栏状态面板：设备 / 当前任务 / 事件 / 控制台（docs/06 §5.5）。
//!
//! 全部由原底部控制台区迁入：`log_view` 尾部窗口渲染、自动滚动、
//! 事件上限（`EVENT_MAX`，定义在 `app.rs` 的 `NfcApp::push_event` 侧）等行为保持不变。

use egui::{Align, Frame, Layout, RichText, ScrollArea, Ui};

use nfc_core::i18n::t;
use nfc_core::state::DeviceState;
use nfc_core::theme;

use crate::app::NfcApp;
use crate::dialogs_ui;
use crate::theme_ui::{self, event_color};

/// 事件时间线最高显示高度（px）。原 S16 事件展开高度，沿用。
pub const EVENT_LIST_MAX_H: f32 = 96.0;
/// 控制台日志最小可见高度（px）。原 S16 `LOG_MIN_H`，沿用。
pub const LOG_MIN_H: f32 = 120.0;

/// 画右栏全部状态卡。
pub fn draw(ui: &mut Ui, app: &mut NfcApp, stacked: bool) {
    device_card(ui, app);
    ui.add_space(8.0);
    task_card(ui, app);
    ui.add_space(8.0);
    events_card(ui, app);
    ui.add_space(8.0);
    console_card(ui, app, stacked);
}

/// 右栏卡片统一外观。paint 由调用方先取好（避免闭包与 `app` 借用冲突）。
fn card(ui: &mut Ui, paint: theme_ui::Paint, inner: impl FnOnce(&mut Ui)) {
    Frame::NONE
        .fill(paint.color(paint.surface))
        .stroke(egui::Stroke::new(1.0, paint.color(paint.border)))
        .corner_radius(egui::CornerRadius::same(theme::RADIUS[1] as u8))
        .inner_margin(egui::Margin::symmetric(12, 10))
        .show(ui, inner);
}

fn device_card(ui: &mut Ui, app: &mut NfcApp) {
    card(ui, app.paint(), |ui| {
        let paint = app.paint();
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(t("html_rail_device"))
                    .strong()
                    .size(12.0)
                    .color(paint.color(paint.text_secondary)),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.small_button(t("html_refresh_device_list")).clicked() {
                    app.exec("scan-usb-devices", None);
                }
            });
        });
        ui.horizontal(|ui| {
            let dot = paint.device_dot(app.state.device_state);
            ui.colored_label(dot, RichText::new("●").size(10.0));
            let text = match app.state.device_state {
                DeviceState::None => t("html_no_device"),
                DeviceState::Connecting => t("html_device_connecting"),
                DeviceState::Connected => t("html_device_connected"),
                DeviceState::Failed => t("html_device_failed"),
            };
            ui.label(RichText::new(text).size(13.0));
        });
    });
}

fn task_card(ui: &mut Ui, app: &mut NfcApp) {
    card(ui, app.paint(), |ui| {
        let paint = app.paint();
        ui.label(
            RichText::new(t("html_rail_task"))
                .strong()
                .size(12.0)
                .color(paint.color(paint.text_secondary)),
        );
        ui.horizontal(|ui| {
            let dot = paint.status_dot(app.state.status.indicator);
            ui.colored_label(dot, RichText::new("●").size(10.0));
            let text = if app.state.status.text.is_empty() {
                t("indicator_free")
            } else {
                app.state.status.text.clone()
            };
            ui.label(text);
            if let Some(p) = app.dialogs_host.pending_purpose() {
                dialogs_ui::pending_hint(ui, p.pending_text());
            }
        });
        let frac = app
            .state
            .progress
            .map_or(0.0, |p| p.clamp(0.0, 100.0) / 100.0);
        let mut bar = egui::ProgressBar::new(frac).fill(paint.color(paint.primary));
        if app.state.progress.is_some() {
            bar = bar.show_percentage();
        }
        ui.add(bar);
        ui.horizontal(|ui| {
            let timer = app.timer_text().unwrap_or_else(|| "--:--:--".to_owned());
            ui.label(
                RichText::new(format!("{} {timer}", t("html_timer")))
                    .size(12.0)
                    .color(paint.color(paint.text_secondary)),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let stop = egui::Button::new(
                    RichText::new(t("html_stop_task"))
                        .color(paint.color(paint.danger))
                        .strong(),
                );
                if ui
                    .add_enabled(app.state.is_running_task, stop)
                    .on_hover_text(t("html_tip_stop_task"))
                    .on_disabled_hover_text(t("html_tip_stop_idle"))
                    .clicked()
                {
                    app.exec("cancel-task", None);
                }
            });
        });
    });
}

fn events_card(ui: &mut Ui, app: &mut NfcApp) {
    card(ui, app.paint(), |ui| {
        let paint = app.paint();
        ui.label(
            RichText::new(t("html_events"))
                .strong()
                .size(12.0)
                .color(paint.color(paint.text_secondary)),
        );
        ScrollArea::vertical()
            .id_salt("rail-events")
            .max_height(EVENT_LIST_MAX_H)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                if app.state.events.is_empty() {
                    ui.label(
                        RichText::new(t("html_no_events"))
                            .size(12.0)
                            .color(paint.color(paint.text_secondary)),
                    );
                    return;
                }
                // 最新事件在最上（原事件条按时间正序追加，这里倒序展示）
                for e in app.state.events.iter().rev() {
                    let color = event_color(app.theme, e.kind);
                    ui.horizontal(|ui| {
                        ui.colored_label(color, RichText::new("●").size(7.0));
                        ui.label(RichText::new(&e.text).size(12.0));
                    })
                    .response
                    .on_hover_text(&e.text);
                }
            });
    });
}

fn console_card(ui: &mut Ui, app: &mut NfcApp, stacked: bool) {
    let paint = app.paint();
    let avail = ui.available_height();
    let body_h = if stacked {
        220.0
    } else {
        (avail - 34.0).max(LOG_MIN_H)
    };
    Frame::NONE
        .fill(paint.color(paint.console_bg))
        .corner_radius(egui::CornerRadius::same(theme::RADIUS[0] as u8))
        .inner_margin(egui::Margin::symmetric(8, 6))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(t("html_console"))
                        .strong()
                        .size(12.0)
                        .color(paint.color(paint.console_fg)),
                );
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let mut auto = !app.lock_scroll;
                    if ui
                        .checkbox(&mut auto, RichText::new(t("html_unlock_scroll")).size(11.0))
                        .on_hover_text(t("html_tip_autoscroll"))
                        .changed()
                    {
                        app.lock_scroll = !auto;
                    }
                    if ui
                        .small_button(t("html_copy_log"))
                        .on_hover_text(t("html_tip_copy_log"))
                        .clicked()
                    {
                        ui.ctx().copy_text(app.state.log_buffer.clone());
                    }
                    if ui
                        .small_button(t("html_save_log"))
                        .on_hover_text(t("html_tip_save_log"))
                        .clicked()
                    {
                        let text = app.state.log_buffer.clone();
                        app.exec("save-log", Some(text));
                    }
                    if ui
                        .small_button(t("html_clear_log"))
                        .on_hover_text(t("html_tip_clear_log"))
                        .clicked()
                    {
                        app.state.log_buffer.clear();
                    }
                });
            });
            ScrollArea::vertical()
                .id_salt("rail-log")
                .stick_to_bottom(!app.lock_scroll)
                .max_height(body_h)
                .show(ui, |ui| {
                    let mut editable = app.state.log_view();
                    ui.add(
                        egui::TextEdit::multiline(&mut editable)
                            .desired_width(f32::INFINITY)
                            .font(egui::TextStyle::Monospace)
                            .interactive(true),
                    );
                });
        });
}
