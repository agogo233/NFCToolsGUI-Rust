//! 右栏状态面板：设备 / 当前任务 / 事件 / 控制台（docs/06 §5.5）。
//!
//! 全部由原底部控制台区迁入：`log_view` 尾部窗口渲染、自动滚动、
//! 事件上限（`EVENT_MAX`，定义在 `app.rs` 的 `NfcApp::push_event` 侧）等行为保持不变。

use egui::{Align, Frame, Label, Layout, RichText, ScrollArea, Sense, Ui};

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
            // 圆点与 13pt 状态文字同行：字号随文本走，避免小圆点相对
            // 大字号文本「浮」在行中央（docs/02 §6.50）
            ui.colored_label(dot, RichText::new("●").size(12.0));
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
        // P1-7：autoRun 的更具体进度（第 n/total 扇区 · 目标扇区 X）。
        if let Some(line) = app.hard_nested_progress_line() {
            ui.label(
                RichText::new(line)
                    .size(11.0)
                    .color(paint.color(paint.text_secondary)),
            );
        }
        // P2-10：mfcuk 扇区游标进度（任务本身无进度行，从游标推导）。
        if let Some((cur, total)) = app.mfcuk_progress {
            ui.label(
                RichText::new(format!("{} {cur}/{total}", t("html_mfcuk_sector_progress")))
                    .size(11.0)
                    .color(paint.color(paint.text_secondary)),
            );
        }
        ui.horizontal(|ui| {
            let dot = paint.status_dot(app.state.status.indicator);
            // 圆点与 13pt 状态文字同行：字号随文本走，避免小圆点相对
            // 大字号文本「浮」在行中央（docs/02 §6.50）
            ui.colored_label(dot, RichText::new("●").size(12.0));
            let text = if app.state.status.text.is_empty() {
                t("indicator_free")
            } else {
                app.state.status.text.clone()
            };
            // 工具输出文本长度不可控，截断防溢出，全名走悬停提示。
            ui.add(Label::new(RichText::new(&text)).truncate())
                .on_hover_text(text);
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
                // 最新事件在最上（原事件条按时间正序追加，这里倒序展示）。
                // P2-12：转储保存类事件可点击 → 打开转储编辑器载入该 dump。
                let mut open_dump: Option<String> = None;
                let saved_prefix = t("event_dump_saved");
                for (k, e) in app.state.events.iter().rev().enumerate() {
                    let color = event_color(app.theme, e.kind);
                    let row = ui.horizontal(|ui| {
                        // 7pt 圆点在 12pt 事件文本旁近不可见，随文本放大到 10pt（docs/02 §6.50）
                        ui.colored_label(color, RichText::new("●").size(10.0));
                        // 事件文本长度不可控，截断防溢出；全名走悬停提示。
                        ui.add(Label::new(RichText::new(&e.text).size(12.0)).truncate());
                    });
                    // 子 Ui 的 response 固定 `Sense::hover`，`clicked()` 恒为
                    // false（P2-12 的点击因此从未生效）；用 interact 以整行
                    // 矩形注册点击区。
                    let resp = ui
                        .interact(
                            row.response.rect,
                            egui::Id::new("rail-event").with(k),
                            Sense::click(),
                        )
                        .on_hover_text(&e.text);
                    // 只有「转储文件已保存: <名>」这类 success 事件可点；
                    // 文件名取前缀之后的部分，且必须仍在 dump_history 里
                    // （防过期事件里的名字已被删除/改名）。
                    let name = e.text.strip_prefix(&saved_prefix).filter(|n| {
                        e.kind == nfc_core::state::EventKind::Success
                            && app.state.dump_history.iter().any(|h| h == n)
                    });
                    if let Some(name) = name {
                        let name = name.to_owned();
                        if resp.clicked() {
                            open_dump = Some(name);
                        }
                        resp.on_hover_text(t("html_tip_open_event_dump"));
                    }
                }
                if let Some(name) = open_dump {
                    app.exec("open-dump-editor", Some(name));
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
    // 对齐响应式提案的 `.console`：终端深色底上加 1px 描边、圆角与其余卡片（RADIUS[1]）统一。
    Frame::NONE
        .fill(paint.color(paint.console_bg))
        .stroke(egui::Stroke::new(1.0, paint.color(paint.border)))
        .corner_radius(egui::CornerRadius::same(theme::RADIUS[1] as u8))
        .inner_margin(egui::Margin::symmetric(8, 6))
        .show(ui, |ui| {
            // 自动换行：窄档右栏放不下「标题 + 复选框 + 3 个按钮」一行。
            // 右对齐子布局必须 `with_main_wrap`——否则装不下时会从卡片左缘
            // 向外画出界（真机复现过：按钮排「左边出去了」）。
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    RichText::new(t("html_console"))
                        .strong()
                        .size(12.0)
                        .color(paint.color(paint.console_fg)),
                );
                ui.with_layout(
                    Layout::right_to_left(Align::Center).with_main_wrap(true),
                    |ui| {
                        let mut auto = !app.lock_scroll;
                        // 复选框文案与三个 13pt 默认按钮同行：字号统一，避免
                        // 同一行内控件高度参差（docs/02 §6.50）
                        if ui
                            .checkbox(&mut auto, RichText::new(t("html_unlock_scroll")).size(13.0))
                            .on_hover_text(t("html_tip_autoscroll"))
                            .changed()
                        {
                            app.lock_scroll = !auto;
                        }
                        // 三个操作钮与任务卡「停止任务」同用默认按钮（同 padding/底/描边），
                        // 此前 `small_button` 半 padding 落在终端底色上，观感与其余卡片不一。
                        if ui
                            .button(t("html_copy_log"))
                            .on_hover_text(t("html_tip_copy_log"))
                            .clicked()
                        {
                            ui.ctx().copy_text(app.state.log_buffer.clone());
                        }
                        if ui
                            .button(t("html_save_log"))
                            .on_hover_text(t("html_tip_save_log"))
                            .clicked()
                        {
                            let text = app.state.log_buffer.clone();
                            app.exec("save-log", Some(text));
                        }
                        if ui
                            .button(t("html_clear_log"))
                            .on_hover_text(t("html_tip_clear_log"))
                            .clicked()
                        {
                            app.state.log_buffer.clear();
                        }
                    },
                );
            });
            ScrollArea::vertical()
                .id_salt("rail-log")
                .stick_to_bottom(!app.lock_scroll)
                .max_height(body_h)
                .show(ui, |ui| {
                    // 只读但仍可拖选复制：此前 `TextEdit::interactive(false)` 把
                    // 文本选择也一并禁用（真机反馈「日志不能选择复制」），换
                    // selectable Label——不可输入，保持控制台「工具输出/回显」
                    // 只读语义，拖选/双击选词/Ctrl+C 复制片段；整段复制仍走
                    // 「复制日志」按钮，编辑入口只有清空/复制/保存。
                    let view = app.state.log_view();
                    ui.add(
                        egui::Label::new(RichText::new(view).font(egui::FontId::monospace(12.0)))
                            .selectable(true),
                    );
                });
        });
}
