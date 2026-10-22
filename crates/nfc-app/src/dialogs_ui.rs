//! 危险操作的应用内确认窗（S11-2）。
//!
//! # 为什么不用系统原生消息框
//!
//! 原版的删除 / 写卡 / 锁卡确认用 `dialog.showMessageBoxSync`（`command.js:507`、
//! `command.js:112`、`command.js:222`），而我们的 `DialogProvider` 是**同步**
//! trait，由 `Engine::dispatch` 在调用栈上直接取值。在 eframe 的 UI 线程上弹
//! 同步原生框会让主窗口在用户点「确定」之前完全无法重绘。
//!
//! 改法：确认框在 app 层自绘。流程随之变成两段式——
//!
//! 1. 此处弹出模态窗，用户点「确定」；
//! 2. app 把「已确认」预置进 [`nfc_core::dialogs::ScriptedDialog`] 的队列，
//!    再派发动作；engine 侧 [`nfc_core::tasks::card_write::prepare_write`]
//!    等函数照常调 `confirm_danger`，只是拿到的是预置值，不再弹框。
//!
//! **代价**：确认框外观与原版的系统消息框不同。刻意偏离，登记于 docs/02 §6。
//!
//! # 安全性
//!
//! 预置的「已确认」只对**紧接着的那一次** `dispatch` 有效。`consume_pending`
//! 在每次派发后清空队列，故一次批准绝不会泄漏到下一个动作。未点确定时
//! 队列里没有任何预置值，engine 拿到 `confirm_danger` 的安全默认值 `false`，
//! 写卡 / 格式化 / 锁卡 / 删除一律拒绝。

use egui::{Context, RichText, Ui};

use nfc_core::i18n::t;

/// 一次确认的用途。点「确定」后据它决定派发什么。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfirmKind {
    /// `write-IC`：写卡。已预先选好源 dump。
    WriteDump { dump: String },
    /// `write-ufuid-uid`：已预先选好 dump，block0 hex 由 engine 重读后拼进确认。
    WriteUid { dump: String },
    /// `lock-ufuid`：锁死卡片的 UFUID。
    Lock,
    /// `delete-dump`：删除若干转储。
    DeleteDumps(Vec<String>),
    /// R1：运行中关窗二次确认。
    CloseWhileRunning,
    /// `phw-write-uid`：手机/手环「写 UID 位」。block0 由「读卡号」缓存。
    PhwWriteUid { block0: String },
    /// `phw-write-data`：手机/手环「写数据扇区」。block0 + 目标扇区/块。
    PhwWriteData {
        block0: String,
        sector: u32,
        block: u32,
    },
    /// `format-card`：格式化卡片。S17 安全加固新增（原版无确认框）。
    FormatCard,
    /// S18 `cuid-write`：CUID 空卡写入。源 dump 已选好。
    CuidWrite { dump: String },
    /// S18 `done-input-uid-write`：手输卡号写 UFUID。载荷为原始输入文本。
    UidInputWrite { text: String },
}

/// 待确认的模态窗内容。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfirmModal {
    pub kind: ConfirmKind,
    /// `i18n("dialog_title_danger_operation")` 等。
    pub title: String,
    /// 正文。
    pub message: String,
    /// 原版 `detail:`（`command.js:511` 的 `files.join('\n')`）。
    pub detail: Option<String>,
    /// S18 B6：危险操作随机码。非 `None` 时须输入正确码才放行（对标
    /// MifareOneTool `ConfirmDangerousOperation` 的随机确认码）。
    pub code: Option<String>,
}

impl ConfirmModal {
    /// 构造一个只有标题 + 正文的确认窗。
    pub fn new(kind: ConfirmKind, title: &str, message: String) -> Self {
        Self {
            kind,
            title: title.to_owned(),
            message,
            detail: None,
            code: None,
        }
    }

    /// 追加 `detail:` 区（删除列表用）。
    pub fn with_detail(mut self, detail: String) -> Self {
        self.detail = Some(detail);
        self
    }

    /// S18 B6：加一道随机码门槛。返回的 `code` 应同时存进 `self.code` 与
    /// 渲染层的输入框初值。
    pub fn with_code(mut self, code: String) -> Self {
        self.code = Some(code);
        self
    }
}

/// 生成一个 4 位十六进制随机码（B6 危险操作确认码）。
///
/// 用时间 + pid 派生，不引入 `rand` 依赖（对标 MifareOneTool 的
/// `RNG 双字节 hex` 但零依赖）。
pub fn random_code() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0);
    let pid = std::process::id() as u64;
    format!(
        "{:04X}",
        ((nanos ^ pid.wrapping_mul(0x9E37_79B9)) & 0xFFFF) as u16
    )
}

/// 危险操作确认窗的公共标题（`i18n("dialog_title_danger_operation")`）。
pub fn danger_title() -> String {
    t("dialog_title_danger_operation")
}

/// 删除转储的确认窗，对标 `command.js:505-513`。
pub fn delete_dumps_modal(names: &[String]) -> ConfirmModal {
    ConfirmModal::new(
        ConfirmKind::DeleteDumps(names.to_vec()),
        &danger_title(),
        t("dialog_msg_are_you_sure_to_delete_these_dumps"),
    )
    .with_detail(names.join("\n"))
}

/// R1：任务运行中关窗的确认窗。
pub fn close_while_running_modal() -> ConfirmModal {
    ConfirmModal::new(
        ConfirmKind::CloseWhileRunning,
        &danger_title(),
        t("dialog_msg_are_you_sure_to_exit"),
    )
}

/// 绘制确认窗。返回 `Some(点击了确定)`。
///
/// 用 [`egui::Modal`] 而非普通 `egui::Window`：Modal 会盖一层遮罩并吃掉
/// 背后所有输入，这才是「危险确认」该有的样子——否则它在浮层位置，
/// 用户仍能在它面前继续点主窗口的写卡按钮。
pub fn show(ctx: &Context, m: &ConfirmModal, danger_color: egui::Color32) -> Option<bool> {
    let mut out: Option<bool> = None;
    // B6 随机码输入框的持久状态（跨帧保留）
    let code_id = egui::Id::new("nfc-danger-confirm-code-input");
    let r = egui::Modal::new(egui::Id::new("nfc-danger-confirm"))
        .area(egui::containers::modal::Modal::default_area(egui::Id::new(
            "nfc-danger-confirm-area",
        )))
        .show(ctx, |ui| {
            ui.set_min_width(320.0);
            ui.heading(&m.title);
            ui.separator();
            ui.label(&m.message);
            if let Some(detail) = &m.detail {
                ui.separator();
                // 逐行显示，文件多时也能滚
                egui::ScrollArea::vertical()
                    .max_height(140.0)
                    .show(ui, |ui| {
                        for line in detail.lines() {
                            ui.label(RichText::new(line).monospace());
                        }
                    });
            }
            // S18 B6：随机码门槛——输入匹配才放行
            let code_ok = match &m.code {
                Some(want) => {
                    let cur = ctx
                        .data_mut(|d| d.get_persisted::<String>(code_id))
                        .unwrap_or_default();
                    let ok = cur.trim().eq_ignore_ascii_case(want);
                    ui.horizontal(|ui| {
                        // 确认码随标签明示（独立于输入框，始终可见）：此前把
                        // 码塞进输入框的占位提示，输入后即消失、且与占位提示
                        // 同一视觉层级，用户容易忽略；改标签后码一直在场，
                        // 输入框只留字段名提示。
                        ui.label(
                            RichText::new(format!("{}：{}", t("html_confirm_code"), want))
                                .strong()
                                .monospace(),
                        );
                        let mut buf = cur.clone();
                        let resp = ui.add(
                            egui::TextEdit::singleline(&mut buf)
                                .desired_width(120.0)
                                .hint_text(t("html_confirm_code")),
                        );
                        if resp.changed() {
                            ctx.data_mut(|d| d.insert_persisted(code_id, buf));
                        }
                    });
                    ok
                }
                None => true,
            };
            ui.separator();
            ui.horizontal(|ui| {
                // 原版按钮序为 `[确定, 取消]`（command.js:509），此处保持
                // 「取消」在左、「确定」在右：确定是危险动作，放右侧不易误点。
                //
                // 点击后必须显式关闭本 Modal：`should_close()` 只由遮罩
                // 点击 / Esc / `ui.close()` 触发，若不关，按钮帧会被判为
                // 「尚未关闭」而丢掉结果（弹窗不消失、确认/取消无响应）。
                if ui.button(t("dialog_button_cancel")).clicked() {
                    out = Some(false);
                    ui.close();
                }
                let ok_btn = RichText::new(t("dialog_button_ok")).color(danger_color);
                if ui.add_enabled(code_ok, egui::Button::new(ok_btn)).clicked() {
                    out = Some(true);
                    ui.close();
                }
            });
        });
    // 出结果就返回：按钮路径本帧 `out` 已置位（配合 `ui.close()`），
    // 遮罩 / Esc 路径则由 `should_close()` 收尾。
    if out.is_some() || r.should_close() {
        // 关闭即清残留：否则下次弹新码时输入框带着上次的内容，
        // 占位提示又只在空输入时显示，用户看不到新码。
        ctx.data_mut(|d| d.insert_persisted(code_id, String::new()));
        out
    } else {
        None
    }
}

/// 底栏 / 状态条上显示的「系统弹窗正在等待」提示。
///
/// 没有它的话，用户点了「写卡」之后界面上毫无变化，容易以为程序卡死。
pub fn pending_hint(ui: &mut Ui, text: &str) {
    // S16：避免 `⏳` 在 Windows 上渲染成彩色表情，与主题割裂。
    ui.colored_label(
        ui.visuals().warn_fg_color,
        format!("{} {text}", t("html_waiting_file")),
    );
}
