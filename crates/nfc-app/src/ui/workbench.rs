//! 中栏工作台：当前卡片档案 + 扇区编辑器（docs/06 §5.2-5.3）。
//!
//! 扇区编辑器为**始终可编辑**形态——原 720×640 浮窗「转储编辑器」已并入
//! 中栏（docs/02 §6.48）。编辑数据源是 [`crate::panels::EditorForm`]（可编辑
//! 草稿，保存 / 写卡均以其为准）；档案条数据源是解卡落盘后构建的
//! [`CardSnapshot`]。两者职责分离：档案条展示「卡片是谁」，编辑器展示并
//! 修改「卡里有什么」。

use egui::{Align, Frame, Label, Layout, RichText, ScrollArea, Ui};

use nfc_core::i18n::t;
use nfc_core::model::{dump, dumpcheck, hex};
use nfc_core::theme;

use crate::app::NfcApp;
use crate::panels::ViewMode;

/// 解卡成功后的卡片快照。
#[derive(Clone, Debug)]
pub struct CardSnapshot {
    /// 块 0 前 4 字节（大写冒号分隔）。
    pub uid: String,
    /// 容量 → 卡型。
    pub card_type: String,
    /// [`dump::to_sector_lines`] 的编辑器组（1K=16、4K=64、Mini=5；每组 4 行 32-hex）。
    /// UI 不再直接渲染（编辑器以 `forms.editor` 为准），保留供 selftest 容量断言。
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

pub fn draw(ui: &mut Ui, app: &mut NfcApp, stacked: bool) {
    card_context(ui, app);
    ui.add_space(8.0);
    sector_editor(ui, app, stacked);
}

/// 当前卡片档案条：UID / 卡型 / 密钥统计 / 转储文件 + 直达操作。
///
/// 信息与操作拆两行（docs/02 §6.44）：解卡后单行放不下（长文件名 +
/// 按钮组），右对齐按钮会从容器右缘向左压到「当前文件」上；信息行
/// 自动换行 + 文件名限宽截断，按钮行独占一行贴右，任何窗宽都不重叠。
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
                // 空态只留一行：连接引导上移到标题栏状态胶囊的悬停提示
                // （app::device_status_chip），这里不再重复占用一整行。
                ui.label(RichText::new(t("html_card_none")).strong());
                return;
            };
            let uid = snap.uid.clone();
            ui.horizontal_wrapped(|ui| {
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
                            .small_button(RichText::new(t("html_copy")).size(11.0))
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
                });
                ui.vertical(|ui| {
                    ui.label(
                        RichText::new(t("html_current_file"))
                            .size(10.0)
                            .color(paint.color(paint.text_secondary)),
                    );
                    // 限宽截断 + 悬停给全名：长文件名不再把同行列顶出边界。
                    ui.add_sized(
                        [160.0, 18.0],
                        Label::new(
                            RichText::new(&snap.file_name)
                                .size(12.0)
                                .font(egui::FontId::monospace(12.0)),
                        )
                        .truncate(),
                    )
                    .on_hover_text(snap.file_name.clone());
                });
            });
            ui.add_space(6.0);
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
    if editor_clicked {
        // 带**完整路径**：要载入的 dump 可能在 dumpfiles/ 之外（用户自选
        // 目录），exec 分流按是否含路径分隔符决定走哪条载入链。
        let arg = app.card_snapshot.as_ref().map(|s| s.dump_arg.clone());
        app.exec("open-dump-editor", arg);
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

/// 扇区编辑器卡片：**始终可编辑**（docs/02 §6.48，原浮窗内容平移至此）。
///
/// stack 档（`stacked`）取**自然高度**且不套内层滚动：固定高度 + 内嵌滚动
/// 会把扇区框压在外层视口折叠线下（真机呈现「表头可见、行空白」），改由
/// 外层 `stacked-main` 统一滚动（docs/02 §6.47）；其余档位占满中栏剩余
/// 高度，扇区列表走内层滚动。
///
/// 落盘 / 各入口载入后置位的 `app.scroll_to_editor` 在此消费：把编辑器
/// 卡片顶部对齐到视口——stack 档必需（编辑器在操作区之后、折叠线下）；
/// 宽档无外层滚动区，`scroll_to_me` 自然 no-op。**渲染过才清标志**：
/// 该帧没画到（理论上不会）就留到下一帧。
fn sector_editor(ui: &mut Ui, app: &mut NfcApp, stacked: bool) {
    let paint = app.paint();
    let editor_h = if stacked {
        None
    } else {
        Some((ui.available_height() - 8.0).max(260.0))
    };
    let resp = Frame::NONE
        .fill(paint.color(paint.surface))
        .stroke(egui::Stroke::new(1.0, paint.color(paint.border)))
        .corner_radius(egui::CornerRadius::same(theme::RADIUS[1] as u8))
        .inner_margin(egui::Margin::symmetric(12, 10))
        .show(ui, |ui| {
            if let Some(h) = editor_h {
                ui.allocate_ui_with_layout(
                    egui::vec2(ui.available_width(), h),
                    Layout::top_down(Align::Min),
                    |ui| editor_body(ui, app, false),
                );
            } else {
                editor_body(ui, app, true);
            }
        });
    if app.scroll_to_editor {
        app.scroll_to_editor = false;
        resp.response.scroll_to_me(Some(Align::TOP));
    }
}

/// 编辑器正文：标题 + 图例 →（空态 | 逐扇区编辑框 + 实时体检）→ 文件条 → 按钮行。
fn editor_body(ui: &mut Ui, app: &mut NfcApp, stacked: bool) {
    let paint = app.paint();
    let legend = [
        (t("html_uid_manufacture_info"), 0u8),
        (t("html_key_a"), 1),
        (t("html_key_b"), 3),
        (t("html_access_control"), 2),
    ];
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(t("html_sector_editor"))
                .strong()
                .size(12.0)
                .color(paint.color(paint.text_secondary)),
        );
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            // 图例（`dumpEditor.html:29-39`）——原版的「更新颜色」是重算高亮，
            // egui 每帧按 token 着色本就实时，故只显示图例不给按钮。
            ui.label(
                RichText::new(format!("{}{}", t("html_caption"), ""))
                    .size(11.0)
                    .color(paint.color(paint.text_secondary)),
            );
            for (label, w) in legend.iter() {
                ui.colored_label(
                    legend_color(paint, *w),
                    RichText::new(format!("{label} | ")).size(11.0),
                );
            }
        });
    });
    if app.forms.editor.sectors.is_empty() {
        // 空态：与原浮窗一致显示「当前文件-」，补一句去向提示；
        // 按钮行照常画出（「打开文件」是空态下的唯一入口）。
        ui.vertical_centered(|ui| {
            ui.add_space(18.0);
            ui.label(format!("{}-", t("html_current_file")));
            ui.add_space(4.0);
            ui.label(
                RichText::new(t("html_card_none"))
                    .size(12.0)
                    .color(paint.color(paint.text_secondary)),
            );
            ui.add_space(6.0);
        });
        editor_footer(ui, app);
        return;
    }
    // 编辑粒度：原版每扇区一个 contenteditable div，此处每扇区一个
    // 4 行 multiline 输入（verify_parity S10 守卫锚点 `desired_rows(4)`）。
    // 字节视图改用显示缓存 + 独立控件 id（docs/02 §6.40）：缓存里是
    // 「用户上次打出来的原文」，下一帧原样画回去，egui 的光标才不会
    // 每次击键后跳回行首。
    let editor_is_1k = app.forms.editor.sectors.len() == 16;
    let in_byte = app.forms.editor.view_mode == ViewMode::Byte;
    let mut byte_view_buf = if in_byte {
        app.forms.editor.byte_view.clone()
    } else {
        Vec::new()
    };
    if stacked {
        // stack 档：自然高度直接画，外层 stacked-main 统一滚动。
        let dirty = sector_edit_rows(ui, app, editor_is_1k, in_byte, &mut byte_view_buf);
        if dirty {
            app.forms.editor.dirty = true;
        }
    } else {
        let scroll_h = (ui.available_height() - 4.0).max(120.0);
        let inner = ScrollArea::vertical()
            .id_salt("sector-edit")
            .max_height(scroll_h)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                sector_edit_rows(ui, app, editor_is_1k, in_byte, &mut byte_view_buf)
            });
        if inner.inner {
            app.forms.editor.dirty = true;
        }
    }
    if in_byte {
        app.forms.editor.byte_view = byte_view_buf;
    }
    // S18：体检结果区——逐条列出问题 / 值块 / 未发现。
    //
    // 这里**每帧按当前 `sectors` 实时计算**，不缓存「上次点按钮的结果」：
    // 编辑内容一变结论就变，缓存反而会让用户看到过期结论。代价是每帧一次
    // O(卡容量) 的纯计算，对 1K/4K 可忽略。
    let check_lines = nfc_core::model::dumpcheck::check_report(&app.forms.editor.sectors);
    if !check_lines.is_empty() {
        ui.add_space(4.0);
        Frame::NONE
            .fill(paint.color(paint.surface2))
            .corner_radius(egui::CornerRadius::same(4))
            .inner_margin(egui::Margin::symmetric(8, 6))
            .show(ui, |ui| {
                for line in &check_lines {
                    // 以「!」开头的问题行用危险色，其余（值块 / 未发现）正常色
                    let color = if line.starts_with('!') {
                        paint.color(paint.danger)
                    } else {
                        paint.color(paint.text)
                    };
                    ui.label(RichText::new(line.clone()).size(12.0).color(color));
                }
            });
    }
    editor_footer(ui, app);
}

/// 逐扇区编辑框（原浮窗 `dump_editor` 的编辑区）。返回本帧是否有改动。
fn sector_edit_rows(
    ui: &mut Ui,
    app: &mut NfcApp,
    editor_is_1k: bool,
    in_byte: bool,
    byte_view_buf: &mut Vec<String>,
) -> bool {
    let paint = app.paint();
    let danger = paint.color(paint.danger);
    let mut dirty = false;
    for (si, sector) in app.forms.editor.sectors.iter_mut().enumerate() {
        // 校验（`dumpEditor.js:98-118` 的 checkData）：行数 / 行长任一不符
        // 即整块红框包裹（原版 styles.invalid 的做法），比行尾「!」更难看漏。
        let invalid = sector.len() != dump::BLOCKS_PER_SECTOR
            || sector
                .iter()
                .any(|l| l.chars().count() != dump::BLOCK_SIZE * 2);
        let mut frame = Frame::NONE
            .inner_margin(egui::Margin::symmetric(6, 4))
            .corner_radius(egui::CornerRadius::same(theme::RADIUS[0] as u8));
        if invalid {
            frame = frame.stroke(egui::Stroke::new(1.5, danger));
        }
        frame.show(ui, |ui| {
            ui.horizontal(|ui| {
                // 扇区号固定窄列，右对齐贴输入框
                ui.set_width(56.0);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new(format!("{}{si}", t("html_sector")))
                            .strong()
                            .color(if invalid {
                                danger
                            } else {
                                paint.color(paint.text)
                            }),
                    );
                });
                // S18：值块标记——该扇区含值块时在号后贴一个小徽章
                // （对标 MyPN532 DumpSectorEditor 的 value block 高亮）。
                // 只用当前这一扇区构造单元素切片，避免在 `iter_mut`
                // 存续期间再不可变借用整个 `sectors`（E0502）。
                if !dumpcheck::value_block_lines(si, std::slice::from_ref(&*sector), editor_is_1k)
                    .is_empty()
                {
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new(t("html_value_block"))
                            .size(10.0)
                            .color(paint.color(paint.key_b)),
                    );
                }
            });
            // 把 4 行拼成带换行的单串，交给一个 multiline 输入。
            let mut text = if in_byte {
                byte_view_buf.get(si).cloned().unwrap_or_default()
            } else {
                sector.join("\n")
            };
            let builder = egui::TextEdit::multiline(&mut text)
                .desired_rows(4)
                .desired_width(f32::INFINITY)
                .font(egui::TextStyle::Monospace);
            // 字节视图的控件 id 按扇区区分：共用一个 id 会让多扇区的
            // 交互状态互相覆盖（原浮窗遗留问题，顺手修正）。
            let resp = if in_byte {
                ui.add(builder.id_salt(("byte-view", si)))
            } else {
                ui.add(builder)
            };
            if resp.changed() {
                dirty = true;
                if in_byte {
                    if let Some(slot) = byte_view_buf.get_mut(si) {
                        *slot = text.clone();
                    }
                    *sector = from_byte_view_text(&text);
                } else {
                    *sector = text.split('\n').map(str::to_owned).collect();
                }
            }
        });
        ui.add_space(4.0);
    }
    dirty
}

/// 底部文件条（当前文件 + 显示视图切换）+ 操作按钮行。
///
/// 7 个按钮在窄窗口下装不下一行，`horizontal_wrapped` 自动换行而不横向
/// 溢出（docs/02 §6.29）；视图切换属「怎么显示」，与处理操作分开放在
/// 文件条上。
fn editor_footer(ui: &mut Ui, app: &mut NfcApp) {
    let paint = app.paint();
    ui.separator();
    let name = app
        .forms
        .editor
        .file_name
        .clone()
        .unwrap_or_else(|| "-".to_owned());
    ui.horizontal(|ui| {
        // 文件名可能很长（含用户自定义名），截断防溢出，全名悬停可见。
        ui.add(Label::new(RichText::new(format!("{}{name}", t("html_current_file")))).truncate())
            .on_hover_text(name);
        // 显示方式切换（docs/02 §6.40）：连续 hex ↔ 每字节空一格的大写。
        // 按钮文字是**切换后的目标**视图，故两种状态各需一条文案。
        let label = if app.forms.editor.view_mode == ViewMode::Continuous {
            t("html_dump_view_byte")
        } else {
            t("html_dump_view_continuous")
        };
        if ui
            .button(label)
            .on_hover_text(t("html_tip_dump_view"))
            .clicked()
        {
            app.forms.editor.toggle_view();
        }
    });
    ui.horizontal_wrapped(|ui| {
        if ui.button(t("html_open_file")).clicked() {
            // 必须走 exec：engine 不认这个名字（不在纯 UI 名单里），
            // 直接 push 会弹出 NotWired 错误事件。
            app.exec("dump-editor-choose-file", None);
        }
        // 校验通过才允许保存（原版 checkData(true) 失败会 alert）
        let ok = app.forms.editor.can_save();
        if ui
            .add_enabled(ok, egui::Button::new(t("html_save_file")))
            .clicked()
        {
            // "0" = 非另存为；hex 由 app 侧从编辑器表单现取
            app.exec("dump-editor-save", Some("0".to_owned()));
        }
        if ui
            .add_enabled(ok, egui::Button::new(t("html_save_as_file")))
            .clicked()
        {
            // "1" = 另存为，弹系统保存框
            app.exec("dump-editor-save", Some("1".to_owned()));
        }
        // 写卡入口（原版 dumpEditor 没有）：先另存一份副本，再走既有
        // write-IC 确认链。按 `command` 门禁置灰——未连接 / 有任务在跑时
        // 悬停说明原因，与主面板写卡按钮同一套规则。
        let gate =
            nfc_core::command::spec("write-IC").map(|s| nfc_core::command::gate(s, &app.state));
        let can_write = ok && matches!(gate, Some(Ok(())) | None);
        let write_tip = match &gate {
            Some(Err(e)) => t(e.i18n_key()),
            _ => t("html_write_this_dump"),
        };
        if ui
            .add_enabled(can_write, egui::Button::new(t("html_write_this_dump")))
            .on_hover_text(write_tip)
            .clicked()
        {
            app.write_editor_to_card();
        }
        // S18：体检 / 修复 / 导出密钥（纯 UI 动作，不进 engine 队列）
        if ui.button(t("html_check_dump")).clicked() {
            // 体检结论每帧已实时显示在下方，这里把同样的结论写进控制台——
            // 点按钮要有可见反馈，且日志可随「保存日志」留存。
            for line in dumpcheck::check_report(&app.forms.editor.sectors) {
                app.state
                    .apply(nfc_core::state::Event::Log(format!("\n{line}\n")));
            }
        }
        if ui.button(t("html_fix_dump")).clicked() {
            match dumpcheck::fix(&app.forms.editor.sectors) {
                Ok((fixed, items)) => {
                    app.forms.editor.sectors = fixed;
                    // 修复改写了扇区，字节视图的显示缓存必须跟着重建，
                    // 否则停在字节视图时会继续看到修复前的内容（docs/02 §6.40）
                    app.forms.editor.rebuild_byte_view();
                    app.forms.editor.dirty = true;
                    // 修复日志逐条写进控制台
                    for line in dumpcheck::fix_report(&items) {
                        app.state
                            .apply(nfc_core::state::Event::Log(format!("\n{line}\n")));
                    }
                }
                Err(issues) => {
                    // 修复失败（结构/容量不合法）：逐条写进控制台，让用户看见原因。
                    for line in issues.iter().map(dumpcheck::issue_text) {
                        app.state
                            .apply(nfc_core::state::Event::Log(format!("\n{line}\n")));
                    }
                }
            }
        }
        if ui.button(t("html_export_keys")).clicked() {
            app.exec("dump-editor-export-keys", None);
        }
    });
}

/// 原版图例的四段配色 token（`dumpEditor.html:31-37` + `tokens.css`）。
/// egui 无 `<span class="uid">`，故按段着色，token 与原版一致。
fn legend_color(paint: crate::theme_ui::Paint, which: u8) -> egui::Color32 {
    match which {
        0 => paint.color(paint.uid),
        1 => paint.color(paint.key_a),
        2 => paint.color(paint.access),
        _ => paint.color(paint.key_b),
    }
}

/// 字节视图的编辑内容 → 4 行 × 32 小写 hex。
///
/// 空白（含换行）一律当分隔符，因此整段粘贴连续 hex 同样可用。
/// 尾部**不补零**：短了就让 `validate` 报 `LineLength`，而不是静默产出坏数据。
fn from_byte_view_text(text: &str) -> Vec<String> {
    let flat: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    flat.to_ascii_lowercase()
        .chars()
        .collect::<Vec<char>>()
        .chunks(32)
        .map(|row| row.iter().copied().collect::<String>())
        .collect()
}
