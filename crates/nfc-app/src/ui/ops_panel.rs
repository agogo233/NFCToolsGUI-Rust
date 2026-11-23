//! 左栏操作面板：按工作流步骤分组的动作按钮（docs/06 §5.1）。
//!
//! 按钮清单、门禁（`command::gate`）与悬停提示逐项继承原三列按钮区
//! （旧 `app.rs::button_area`），仅重排为「读卡 / 破解 / 写卡 / 更多」
//! 四组；不新增、不删减任何 action。危险操作改为红描边 + 警示图标的
//! 独立样式（已确认的设计决策）。

use egui::{Align, Frame, Layout, RichText, ScrollArea, Ui};

use nfc_core::i18n::t;
use nfc_core::theme;

use crate::app::NfcApp;

/// 按钮样式档位（沿用原三列按钮区的分级）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BtnStyle {
    /// 主操作（实心品牌色）。
    Primary,
    /// 普通操作。
    Normal,
    /// 次级操作（描边）。
    Ghost,
    /// 危险操作（红描边，不可逆，配合应用内危险确认窗）。
    Danger,
}

/// 单个动作按钮的静态描述。
pub struct BtnDef {
    pub action: &'static str,
    pub label_key: &'static str,
    pub tip_key: &'static str,
    pub style: BtnStyle,
}

/// 操作分组：步骤编号 + 组名 + 按钮列表。
pub struct GroupDef {
    pub no: &'static str,
    pub title_key: &'static str,
    pub btns: &'static [BtnDef],
}

const fn btn(
    action: &'static str,
    label_key: &'static str,
    tip_key: &'static str,
    style: BtnStyle,
) -> BtnDef {
    BtnDef {
        action,
        label_key,
        tip_key,
        style,
    }
}

/// 四组操作按钮（P1-8 重组）：13 个 action 与原三列完全一致，仅重排：
///   读卡组 3 个：read-IC=primary
///   破解组 2 个：无 primary / danger
///   写卡组 5 个：write-IC=primary、format-card=danger（CUID 写 / 手输卡号 /
///     写 UID 本质都是写卡，从「更多」并入，减少来回跳组）
///   更多组 3 个：手机/手环、全加密爆破、锁定 UFUID=danger
pub const GROUPS: &[GroupDef] = &[
    GroupDef {
        no: "1",
        title_key: "html_read_area",
        btns: &[
            btn(
                "read-IC",
                "html_read_card",
                "html_tip_read_ic",
                BtnStyle::Primary,
            ),
            btn(
                "input-keys-read-IC",
                "html_read_card_with_known_key",
                "html_tip_read_known_key",
                BtnStyle::Normal,
            ),
            btn(
                "detect-card-type",
                "html_detect_card",
                "html_tip_detect_card",
                BtnStyle::Ghost,
            ),
        ],
    },
    GroupDef {
        no: "2",
        title_key: "html_crack_area",
        btns: &[
            btn(
                "hard-nested",
                "html_hard_nested",
                "html_tip_hard_nested",
                BtnStyle::Normal,
            ),
            btn(
                "dict-test",
                "html_test_dictionary",
                "html_tip_dict_test",
                BtnStyle::Normal,
            ),
        ],
    },
    GroupDef {
        no: "3",
        title_key: "html_write_area",
        btns: &[
            btn(
                "write-IC",
                "html_write_card",
                "html_tip_write_ic",
                BtnStyle::Primary,
            ),
            btn(
                "cuid-write",
                "html_cuid_write",
                "html_tip_cuid_write",
                BtnStyle::Normal,
            ),
            btn(
                "input-uid-write",
                "html_write_uid_input",
                "html_tip_write_uid_input",
                BtnStyle::Normal,
            ),
            btn(
                "write-ufuid-uid",
                "html_write_ufuid_uid",
                "html_tip_write_ufuid_uid",
                BtnStyle::Normal,
            ),
            btn(
                "format-card",
                "html_format_card",
                "html_tip_format_card",
                BtnStyle::Danger,
            ),
        ],
    },
    GroupDef {
        no: "+",
        title_key: "html_ops_more",
        btns: &[
            btn(
                "open-phw-window",
                "html_phw_open",
                "html_tip_phw",
                BtnStyle::Normal,
            ),
            btn(
                "run-mfcuk",
                "html_mfcuk_run",
                "html_tip_mfcuk_run",
                BtnStyle::Normal,
            ),
            btn(
                "lock-ufuid",
                "html_lock_ufuid",
                "html_tip_lock_ufuid",
                BtnStyle::Danger,
            ),
        ],
    },
];

/// 画左栏全部分组。`icon_mode` 为真时压成 56px 图标条：
/// 按钮只留一个代表字（悬停给全名 + 提示），组头只留步骤徽标。
///
/// 非 stack 档整栏包滚动区：窗口偏矮时（标题栏 + 引导条占掉头部后）
/// 底部分组（如「更多操作」）不再被裁剪，可滚动查看；内容不超高时无滚动条。
/// stack 档（<820）**不套**内层滚动：外层 `stacked-main` 已统一滚动，再嵌
/// 一层会占满外层视口并优先吞掉滚轮，把中栏卡死在折叠线下（真机复现过）。
pub fn draw(ui: &mut Ui, app: &mut NfcApp, icon_mode: bool, stacked: bool) {
    if stacked {
        for g in GROUPS {
            group_card(ui, app, g, icon_mode);
            ui.add_space(8.0);
        }
        return;
    }
    ScrollArea::vertical()
        .id_salt("ops-panel-scroll")
        .auto_shrink([false, true])
        .show(ui, |ui| {
            for g in GROUPS {
                group_card(ui, app, g, icon_mode);
                ui.add_space(8.0);
            }
        });
}

/// 图标条模式下每个动作的代表字（窄窗 56px 左栏用；悬停仍给全名）。
fn icon_char(action: &str) -> &'static str {
    match action {
        "read-IC" => "读",
        "input-keys-read-IC" => "钥",
        "detect-card-type" => "检",
        "hard-nested" => "破",
        "dict-test" => "典",
        "write-IC" => "写",
        "cuid-write" => "C",
        "input-uid-write" => "输",
        "write-ufuid-uid" => "U",
        "format-card" => "格",
        "open-phw-window" => "手",
        "run-mfcuk" => "爆",
        "lock-ufuid" => "锁",
        _ => "•",
    }
}

fn group_card(ui: &mut Ui, app: &mut NfcApp, g: &GroupDef, icon_mode: bool) {
    let paint = app.paint();
    Frame::NONE
        .fill(paint.color(paint.surface))
        .stroke(egui::Stroke::new(1.0, paint.color(paint.border)))
        .corner_radius(egui::CornerRadius::same(nfc_core::theme::RADIUS[1] as u8))
        .inner_margin(egui::Margin::symmetric(if icon_mode { 6 } else { 10 }, 10))
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 6.0;
            group_head(ui, app, g, icon_mode);
            ui.separator();
            for b in g.btns {
                action_button(ui, app, b, icon_mode);
            }
        });
}

/// 组头：步骤徽标 + 组名 + 状态（待命 / 完成）。
///
/// 状态仅为视觉提示，不参与门禁；运行中的进度在右栏任务卡展示。
/// 图标条模式下只画居中的步骤徽标（组名/状态随全名进悬停提示）。
fn group_head(ui: &mut Ui, app: &NfcApp, g: &GroupDef, icon_mode: bool) {
    let paint = app.paint();
    let done = group_done(app, g);
    let (bg, fg) = if done {
        (theme::STATUS_OK, paint.text_inverse)
    } else {
        (paint.surface2, paint.text_secondary)
    };
    if icon_mode {
        ui.vertical_centered(|ui| {
            let (rect, _) = ui.allocate_exact_size(egui::vec2(20.0, 20.0), egui::Sense::hover());
            ui.painter()
                .rect_filled(rect, theme::RADIUS[0], paint.color(bg));
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                g.no,
                egui::FontId::proportional(11.0),
                paint.color(fg),
            );
        });
        return;
    }
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(20.0, 20.0), egui::Sense::hover());
        ui.painter()
            .rect_filled(rect, theme::RADIUS[0], paint.color(bg));
        ui.painter().text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            g.no,
            egui::FontId::proportional(11.0),
            paint.color(fg),
        );
        ui.label(RichText::new(t(g.title_key)).strong().size(13.0));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let state = if done {
                t("html_step_done")
            } else {
                t("html_step_idle")
            };
            ui.label(RichText::new(state).size(11.0).color(paint.color(if done {
                theme::STATUS_OK
            } else {
                paint.text_secondary
            })));
        });
    });
}

/// 分组完成条件（视觉提示）：
/// 读卡组 = 已有本次会话的卡片快照；破解组 = 做过检测且未知密钥清零；
/// 写卡组 = 本次会话写卡成功过（docs/06 §5.1；取消/失败不置位，新卡重置）。
fn group_done(app: &NfcApp, g: &GroupDef) -> bool {
    match g.no {
        "1" => app.card_snapshot.is_some(),
        "2" => !app.state.known_key_info.is_empty() && app.state.unknown_key_info.is_empty(),
        "3" => app.write_done,
        _ => false,
    }
}

fn action_button(ui: &mut Ui, app: &mut NfcApp, b: &BtnDef, icon_mode: bool) {
    let paint = app.paint();
    // 门禁：未连接设备 / 有任务运行中 / 正在连接 → 置灰，并在悬停时说明原因
    // （取代「点了之后才在日志里看到报错」，与原三列按钮区一致）。
    let gate = nfc_core::command::spec(b.action).map(|s| nfc_core::command::gate(s, &app.state));
    let enabled = match &gate {
        Some(Err(_)) => false,
        Some(Ok(())) | None => true,
    };
    let reason = match &gate {
        Some(Err(e)) => t(e.i18n_key()),
        _ => String::new(),
    };
    if icon_mode {
        // 56px 图标条：单字方块按钮，悬停给「全名 + 提示」，门禁与常规模一致。
        let (fill, fg, stroke) = match b.style {
            BtnStyle::Primary => (paint.primary, paint.text_inverse, None),
            BtnStyle::Normal => (paint.surface2, paint.text, None),
            BtnStyle::Ghost => (paint.surface, paint.text, Some((1.0, paint.border))),
            BtnStyle::Danger => (paint.surface, paint.danger, Some((1.5, paint.danger))),
        };
        let (rect, resp) = ui.allocate_exact_size(egui::vec2(38.0, 30.0), egui::Sense::click());
        let painter = ui.painter();
        painter.rect_filled(rect, theme::RADIUS[0], paint.color(fill));
        if let Some((sw, sc)) = stroke {
            painter.rect_stroke(
                rect,
                egui::CornerRadius::same(theme::RADIUS[0] as u8),
                egui::Stroke::new(sw, paint.color(sc)),
                egui::StrokeKind::Inside,
            );
        }
        let fg_color = if enabled {
            paint.color(fg)
        } else {
            paint.color(fg).gamma_multiply(0.45)
        };
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            icon_char(b.action),
            egui::FontId::proportional(15.0),
            fg_color,
        );
        let tip = format!("{}\n{}", t(b.label_key), t(b.tip_key));
        let resp = if enabled {
            resp.on_hover_text(tip)
        } else {
            resp.on_disabled_hover_text(reason)
        };
        if resp.clicked() {
            app.exec(b.action, None);
        }
        return;
    }
    let w = ui.available_width();
    let (h, fs, strong) = match b.style {
        BtnStyle::Primary => (38.0, 14.0, true),
        BtnStyle::Normal | BtnStyle::Ghost | BtnStyle::Danger => (30.0, 13.0, false),
    };
    let (fill, fg, stroke) = match b.style {
        BtnStyle::Primary => (paint.primary, paint.text_inverse, None),
        BtnStyle::Normal => (paint.surface2, paint.text, None),
        BtnStyle::Ghost => (paint.surface, paint.text, Some((1.0, paint.border))),
        BtnStyle::Danger => (paint.surface, paint.danger, Some((1.5, paint.danger))),
    };
    let mut rich = RichText::new(t(b.label_key))
        .size(fs)
        .color(paint.color(fg));
    if strong {
        rich = rich.strong();
    }
    let mut button = egui::Button::new(rich)
        .fill(paint.color(fill))
        .min_size(egui::vec2(w, h));
    if let Some((sw, sc)) = stroke {
        button = button.stroke(egui::Stroke::new(sw, paint.color(sc)));
    }
    let resp = if enabled {
        ui.add_enabled(true, button).on_hover_text(t(b.tip_key))
    } else {
        ui.add_enabled(false, button).on_disabled_hover_text(reason)
    };
    if resp.clicked() {
        app.exec(b.action, None);
    }
}
