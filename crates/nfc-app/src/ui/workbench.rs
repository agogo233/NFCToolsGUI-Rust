//! 中栏工作台：当前卡片档案 + 卡片扇区编辑器（docs/06 §5.2-5.3）。
//!
//! 数据源是解卡落盘后构建的内存快照（[`CardSnapshot`]），渲染不依赖
//! 磁盘文件仍在与否；密钥已知 / 未知状态按逻辑扇区从 `AppState` 派生，
//! 破解命中后（`unknown_key_info` 移除）行色与明文随之刷新，零额外状态。

use egui::{Align, Color32, Frame, Label, Layout, RichText, ScrollArea, Ui};

use nfc_core::i18n::t;
use nfc_core::model::{dump, dumpcheck, hex};
use nfc_core::state::KeyType;
use nfc_core::theme;

use crate::app::NfcApp;

/// 解卡成功后的卡片快照。
#[derive(Clone, Debug)]
pub struct CardSnapshot {
    /// 块 0 前 4 字节（大写冒号分隔）；更长 UID 以展开详情里的完整块 0 为准。
    pub uid: String,
    /// 容量 → 卡型。
    pub card_type: String,
    /// [`dump::to_sector_lines`] 的编辑器组（1K=16、4K=64、Mini=5；每组 4 行 32-hex）。
    pub groups: Vec<dump::Sector>,
    /// 转储完整路径字符串——「写这张卡」直接沿用该参数走原确认链。
    pub dump_arg: String,
    /// 转储文件名。
    pub file_name: String,
}

/// 从落盘的转储文件构建快照；失败（文件缺失 / 容量不合法）返回 `None`
/// 并保持静默——selftest 会用假路径走到这里，不能惊动既有事件断言。
pub fn build_snapshot(path: &std::path::Path) -> Option<CardSnapshot> {
    let bytes = std::fs::read(path).ok()?;
    let dump_arg = path.to_string_lossy().into_owned();
    let file_name = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    snapshot_from_bytes(&bytes, dump_arg, file_name)
}

/// 从 dump 字节构建快照（selftest 直接喂字节，不经磁盘）。
pub fn snapshot_from_bytes(
    bytes: &[u8],
    dump_arg: String,
    file_name: String,
) -> Option<CardSnapshot> {
    let groups = dump::to_sector_lines(bytes).ok()?;
    let uid = groups
        .first()
        .and_then(|g| g.first())
        .map(|line| uid_from_block0(line))
        .unwrap_or_else(|| String::from("--"));
    Some(CardSnapshot {
        uid,
        card_type: card_type(bytes.len()),
        groups,
        dump_arg,
        file_name,
    })
}

/// 未知密钥的占位显示。
const NK: &str = "?? ?? ?? ?? ?? ??";

fn uid_from_block0(line: &str) -> String {
    match hex::hex_decode(line) {
        Ok(b) if b.len() >= 4 => b[..4]
            .iter()
            .map(|x| format!("{x:02X}"))
            .collect::<Vec<_>>()
            .join(":"),
        _ => String::from("--"),
    }
}

fn card_type(len: usize) -> String {
    match len {
        320 => "MIFARE Mini".to_owned(),
        1024 => "MIFARE Classic 1K".to_owned(),
        2048 => "MIFARE Classic 2K".to_owned(),
        4096 => "MIFARE Classic 4K".to_owned(),
        _ => t("html_card_type_unknown"),
    }
}

/// 中英混排的 hex 展示：两位一组、空格分隔、大写。
fn spaced(hex: &str) -> String {
    let up = hex.to_uppercase();
    up.as_bytes()
        .chunks(2)
        .map(|c| std::str::from_utf8(c).unwrap_or(""))
        .collect::<Vec<_>>()
        .join(" ")
}

fn spaced_bytes(b: &[u8]) -> String {
    b.iter()
        .map(|x| format!("{x:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn draw(ui: &mut Ui, app: &mut NfcApp, stacked: bool) {
    card_context(ui, app);
    ui.add_space(8.0);
    sector_editor(ui, app, stacked);
}

/// 当前卡片档案条：UID / 卡型 / 密钥统计 / 转储文件 + 直达操作。
fn card_context(ui: &mut Ui, app: &mut NfcApp) {
    let paint = app.paint();
    let mut write_clicked = false;
    let mut editor_clicked = false;
    let mut copied = false;
    Frame::NONE
        .fill(paint.color(paint.surface))
        .stroke(egui::Stroke::new(1.0, paint.color(paint.border)))
        .corner_radius(egui::CornerRadius::same(theme::RADIUS[1] as u8))
        .inner_margin(egui::Margin::symmetric(12, 10))
        .show(ui, |ui| {
            let Some(snap) = app.card_snapshot.as_ref() else {
                // 空态按连接状态分支：未连接时引导先连设备，而不是让用户
                // 点了「一键解卡」再被门禁拦下。
                ui.label(RichText::new(t("html_card_none")).strong());
                let hint = if app.state.is_device_connected {
                    t("html_card_none_hint")
                } else {
                    t("html_card_none_hint_not_connected")
                };
                ui.label(
                    RichText::new(hint)
                        .size(12.0)
                        .color(paint.color(paint.text_secondary)),
                );
                return;
            };
            let uid = snap.uid.clone();
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(t("html_card_context"))
                        .strong()
                        .size(13.0)
                        .color(paint.color(paint.text_secondary)),
                );
                ui.separator();
                ui.vertical(|ui| {
                    ui.label(
                        RichText::new("UID")
                            .size(10.0)
                            .color(paint.color(paint.text_secondary)),
                    );
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(&uid)
                                .size(16.0)
                                .strong()
                                .color(paint.color(paint.uid))
                                .font(egui::FontId::monospace(16.0)),
                        );
                        // 一键复制卡号：UID 是写卡/报障/记录最常被抄走的数据，
                        // 之前只能手动选中逐字符拖选。
                        if ui
                            .small_button("⧉")
                            .on_hover_text(t("html_tip_copy_uid"))
                            .clicked()
                        {
                            ui.ctx().copy_text(uid.clone());
                            copied = true;
                        }
                    });
                });
                ui.vertical(|ui| {
                    ui.label(
                        RichText::new(t("html_card_type"))
                            .size(10.0)
                            .color(paint.color(paint.text_secondary)),
                    );
                    ui.label(RichText::new(&snap.card_type).size(13.0).strong());
                });
                ui.vertical(|ui| {
                    let known = app.state.known_key_info.len();
                    let unknown = app.state.unknown_key_info.len();
                    ui.label(
                        RichText::new(t("html_detect_summary"))
                            .size(10.0)
                            .color(paint.color(paint.text_secondary)),
                    );
                    ui.label(
                        RichText::new(format!(
                            "{} {known} · {} {unknown}",
                            t("html_detect_known"),
                            t("html_detect_unknown")
                        ))
                        .size(13.0)
                        .strong(),
                    );
                    let frac = if known + unknown == 0 {
                        0.0
                    } else {
                        known as f32 / (known + unknown) as f32
                    };
                    ui.add_sized([110.0, 6.0], egui::ProgressBar::new(frac));
                });
                ui.vertical(|ui| {
                    ui.label(
                        RichText::new(t("html_current_file"))
                            .size(10.0)
                            .color(paint.color(paint.text_secondary)),
                    );
                    ui.label(
                        RichText::new(&snap.file_name)
                            .size(12.0)
                            .font(egui::FontId::monospace(12.0)),
                    );
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui
                        .button(RichText::new(t("html_edit_dump")).size(12.0))
                        .clicked()
                    {
                        editor_clicked = true;
                    }
                    let write = egui::Button::new(
                        RichText::new(t("html_write_this_dump"))
                            .size(12.0)
                            .strong()
                            .color(paint.color(paint.text_inverse)),
                    )
                    .fill(paint.color(paint.primary));
                    if ui.add(write).clicked() {
                        write_clicked = true;
                    }
                });
            });
        });
    if editor_clicked {
        app.exec("open-dump-editor", None);
    }
    if write_clicked {
        let arg = app.card_snapshot.as_ref().map(|s| s.dump_arg.clone());
        if let Some(arg) = arg {
            // 与 DumpSaved 引导条完全同参：走 write-IC 的确认框 + 备份安全闸。
            app.exec("write-IC", Some(arg));
        }
    }
    if copied {
        app.push_event(
            nfc_core::state::EventKind::Success,
            &t("log_msg_uid_copied"),
        );
    }
}

/// 扇区编辑器：组行（扇区 / KeyA / ACL / KeyB / 状态）+ 点击展开块数据与 ACL 解读。
fn sector_editor(ui: &mut Ui, app: &mut NfcApp, stacked: bool) {
    let paint = app.paint();
    let avail = ui.available_height();
    let editor_h = if stacked {
        480.0
    } else {
        (avail - 8.0).max(260.0)
    };
    Frame::NONE
        .fill(paint.color(paint.surface))
        .stroke(egui::Stroke::new(1.0, paint.color(paint.border)))
        .corner_radius(egui::CornerRadius::same(theme::RADIUS[1] as u8))
        .show(ui, |ui| {
            ui.allocate_ui_with_layout(
                egui::vec2(ui.available_width(), editor_h),
                egui::Layout::top_down(Align::Min),
                |ui| editor_body(ui, app),
            );
        });
}

fn editor_body(ui: &mut Ui, app: &mut NfcApp) {
    let paint = app.paint();
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(t("html_sector_editor"))
                .strong()
                .size(12.0)
                .color(paint.color(paint.text_secondary)),
        );
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(
                RichText::new(format!("● {}", t("html_known_key")))
                    .size(11.0)
                    .color(paint.color(theme::STATUS_OK)),
            );
            ui.label(
                RichText::new(format!("● {}", t("html_legend_unknown")))
                    .size(11.0)
                    .color(paint.color(theme::STATUS_WARN)),
            );
        });
    });
    let key_w = ((ui.available_width() - 24.0 - 90.0 - 44.0 - 32.0) / 2.0).max(110.0);
    ui.horizontal(|ui| {
        cell(
            ui,
            24.0,
            &t("html_sector"),
            paint.color(paint.text_secondary),
            false,
        );
        cell(
            ui,
            key_w,
            &t("html_key_a"),
            paint.color(paint.text_secondary),
            false,
        );
        cell(
            ui,
            90.0,
            &t("html_acl"),
            paint.color(paint.text_secondary),
            false,
        );
        cell(
            ui,
            key_w,
            &t("html_key_b"),
            paint.color(paint.text_secondary),
            false,
        );
        cell(
            ui,
            44.0,
            &t("html_detect_summary"),
            paint.color(paint.text_secondary),
            false,
        );
    });
    ui.separator();
    let scroll_h = (ui.available_height() - 4.0).max(120.0);
    ScrollArea::vertical()
        .id_salt("sector-table")
        .max_height(scroll_h)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            let Some(snap) = app.card_snapshot.as_ref() else {
                ui.label(
                    RichText::new(t("html_card_none"))
                        .size(12.0)
                        .color(paint.color(paint.text_secondary)),
                );
                return;
            };
            let n = snap.groups.len();
            let mut toggle: Option<usize> = None;
            // P2-11：detail 需要 &mut app（复制事件），而 snap 借用了
            // app.card_snapshot。先把展开组的行数据 clone 出来，借用即断开。
            let open_detail = app.open_sector.and_then(|i| {
                snap.groups
                    .get(i)
                    .map(|g| (i, g.clone(), dump::group_has_trailer(i, n)))
            });
            for (i, g) in snap.groups.iter().enumerate() {
                let logical = dump::group_to_sector(i, n);
                let has_trailer = dump::group_has_trailer(i, n);
                let unknown = app
                    .state
                    .unknown_key_info
                    .iter()
                    .any(|(s, _)| *s as usize == logical);
                let key_a = app
                    .state
                    .known_key_info
                    .iter()
                    .find(|(_, s, ty)| *s as usize == logical && *ty == KeyType::A);
                let key_b = app
                    .state
                    .known_key_info
                    .iter()
                    .find(|(_, s, ty)| *s as usize == logical && *ty == KeyType::B);
                let ka_text = key_a
                    .map(|(k, _, _)| spaced(k))
                    .unwrap_or_else(|| NK.to_owned());
                let kb_text = key_b
                    .map(|(k, _, _)| spaced(k))
                    .unwrap_or_else(|| NK.to_owned());
                let ka_color = if key_a.is_some() {
                    theme::STATUS_OK
                } else {
                    theme::STATUS_WARN
                };
                let kb_color = if key_b.is_some() {
                    theme::STATUS_OK
                } else {
                    theme::STATUS_WARN
                };
                let acl_text = if has_trailer {
                    hex::hex_decode(&g[3])
                        .ok()
                        .filter(|b| b.len() >= 9)
                        .map(|b| spaced_bytes(&b[6..9]))
                        .unwrap_or_else(|| "-".to_owned())
                } else {
                    "-".to_owned()
                };
                let (tag, tag_color) = if unknown {
                    (t("html_detect_unknown"), theme::STATUS_WARN)
                } else if key_a.is_some() || key_b.is_some() {
                    (t("html_detect_known"), theme::STATUS_OK)
                } else {
                    ("-".to_owned(), paint.text_secondary)
                };
                let row = ui.horizontal(|ui| {
                    cell(
                        ui,
                        24.0,
                        &format!("{i:02}"),
                        paint.color(paint.text_secondary),
                        true,
                    );
                    cell(ui, key_w, &ka_text, paint.color(ka_color), true);
                    cell(ui, 90.0, &acl_text, paint.color(paint.text_secondary), true);
                    cell(ui, key_w, &kb_text, paint.color(kb_color), true);
                    cell(ui, 44.0, &tag, paint.color(tag_color), false);
                });
                if row.response.clicked() {
                    toggle = Some(i);
                }
            }
            if let Some(i) = toggle {
                app.open_sector = if app.open_sector == Some(i) {
                    None
                } else {
                    Some(i)
                };
            }
            // 展开详情放在循环外：此时 snap 的借用已结束，可拿 &mut app。
            if let Some((i, g, has_trailer)) = open_detail {
                detail(ui, app, i, n, &g, has_trailer);
            }
        });
}

/// 单元格：定宽文本（等宽可选），超出宽度截断。
fn cell(ui: &mut Ui, w: f32, text: &str, color: Color32, mono: bool) {
    let mut rich = RichText::new(text).size(12.0).color(color);
    if mono {
        rich = rich.font(egui::FontId::monospace(12.0));
    }
    ui.add_sized([w, 18.0], Label::new(rich).truncate());
}

/// 展开详情：该组 4 行的完整 hex + （真尾块组）ACL 权限解读 + 行级快捷操作。
fn detail(ui: &mut Ui, app: &mut NfcApp, i: usize, groups: usize, g: &[String], has_trailer: bool) {
    let paint = app.paint();
    let base = dump::group_first_block(i, groups);
    // P2-11：行级快捷操作（复制单块 / 整组 hex）。egui 闭包里拿不到 &mut app，
    // 先记下要复制的文本，Frame 结束后统一 copy + 记事件。
    let mut copied: Option<String> = None;
    Frame::NONE
        .fill(paint.color(paint.surface2))
        .show(ui, |ui| {
            for (j, line) in g.iter().enumerate() {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(format!("{} {}", t("html_block"), base + j))
                            .size(11.0)
                            .color(paint.color(paint.text_secondary)),
                    );
                    ui.label(
                        RichText::new(spaced(line))
                            .size(12.0)
                            .font(egui::FontId::monospace(12.0)),
                    );
                    if ui
                        .small_button("⧉")
                        .on_hover_text(t("html_tip_copy_hex"))
                        .clicked()
                    {
                        copied = Some(line.clone());
                    }
                });
            }
            if ui
                .small_button(RichText::new(t("html_copy_group")).size(11.0))
                .on_hover_text(t("html_tip_copy_group"))
                .clicked()
            {
                copied = Some(g.join(""));
            }
            if has_trailer {
                acl_box(ui, app, &g[3]);
            }
        });
    if let Some(hexline) = copied {
        ui.ctx().copy_text(hexline);
        app.push_event(
            nfc_core::state::EventKind::Success,
            &t("log_msg_hex_copied"),
        );
    }
}

/// ACL 权限解读：算法与 `dumpcheck::check_trailer_access` 同源
/// （前向 + 反向解码，一致才有效）；逐块给出人话权限。
fn acl_box(ui: &mut Ui, app: &NfcApp, trailer: &str) {
    let paint = app.paint();
    let Some(b) = hex::hex_decode(trailer).ok().filter(|b| b.len() >= 10) else {
        return;
    };
    let ac = [b[6], b[7], b[8], b[9]];
    let d = dumpcheck::decode_access(ac);
    ui.separator();
    if !d.consistent() {
        ui.label(
            RichText::new(t("html_acl_corrupt"))
                .size(11.0)
                .strong()
                .color(paint.color(theme::STATUS_ERR)),
        );
        ui.label(
            RichText::new(format!("[{:?}] ≠ [{:?}]", d.forward, d.reversed))
                .size(11.0)
                .color(paint.color(theme::STATUS_ERR))
                .font(egui::FontId::monospace(11.0)),
        );
        return;
    }
    ui.label(
        RichText::new(format!(
            "{} {} · {}",
            t("html_acl"),
            spaced_bytes(&ac),
            t("html_acl_ok")
        ))
        .size(11.0)
        .color(paint.color(paint.text_secondary)),
    );
    for (j, &c) in d.forward.iter().enumerate() {
        let perm = if j < 3 {
            t(&format!("html_acl_data_perm_{c}"))
        } else {
            t(&format!("html_acl_trailer_perm_{c}"))
        };
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(format!("{}{} C={}", t("html_block"), j, c))
                    .size(11.0)
                    .color(paint.color(paint.text_secondary))
                    .font(egui::FontId::monospace(11.0)),
            );
            ui.label(RichText::new(perm).size(11.0));
        });
    }
}
