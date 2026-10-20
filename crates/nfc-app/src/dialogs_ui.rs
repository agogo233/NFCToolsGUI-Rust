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
}

impl ConfirmModal {
    /// 构造一个只有标题 + 正文的确认窗。
    pub fn new(kind: ConfirmKind, title: &str, message: String) -> Self {
        Self {
            kind,
            title: title.to_owned(),
            message,
            detail: None,
        }
    }

    /// 追加 `detail:` 区（删除列表用）。
    pub fn with_detail(mut self, detail: String) -> Self {
        self.detail = Some(detail);
        self
    }
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
            ui.separator();
            ui.horizontal(|ui| {
                // 原版按钮序为 `[确定, 取消]`（command.js:509），此处保持
                // 「取消」在左、「确定」在右：确定是危险动作，放右侧不易误点。
                if ui.button(t("dialog_button_cancel")).clicked() {
                    out = Some(false);
                }
                if ui
                    .button(RichText::new(t("dialog_button_ok")).color(danger_color))
                    .clicked()
                {
                    out = Some(true);
                }
            });
        });
    if r.should_close() {
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
