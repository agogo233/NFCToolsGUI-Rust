//! 对话框抽象。core 只持 trait，系统弹窗（rfd）在 `nfc-app` 侧实现。
//!
//! 对标原版 `dialog.showSaveDialogSync`（`command.js:660-666`）与
//! `dialog.showOpenDialog`（`command.js:186-192`）。
//!
//! S11-2 起本模块在**生产路径**上参与运行：系统文件框是阻塞调用，必须放到
//! app 侧的工作线程（`nfc-app::dialogs_host`），而 engine 的 `dispatch` 是
//! 同步的。折中办法是 app 预选好路径再作为载荷传进来，`prepare_write` 等
//! 函数便会跳过文件框；危险确认则由 app 的应用内模态窗完成，结果预置进
//! [`ScriptedDialog::confirm_results`]。详见 docs/02 §6。
//!
//! # 「预选路径」对**流程中途**的保存框不成立
//!
//! 上面那套只对「动作发起时就能问」的框有效（写卡选源 dump、字典、比较器…）。
//! 但有两处保存框是**流程跑到一半**才弹的，路径事先无从知晓：
//!
//! - `read-IC` 收尾：默认文件名含刚读出的 UID（`command.js:660`），
//!   而 UID 要等 mfoc 跑完才知道；
//! - HardNested `collectOnly`：文件名含本轮 sector（`command.js:310`）。
//!
//! 这两处无法预选。S11-7 用 [`PendingSave`] 把请求**挂起**：engine 记下
//! 「要保存成什么样子」，返回控制权；app 在自己的帧循环里用 rfd 弹框，
//! 拿到路径后回调 `Engine::resolve_save` 续接。core 全程不碰 GUI。
//!
//! 偏离登记见 docs/02 §6。

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

/// 文件过滤器。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FileFilter {
    /// 展示名（`i18n("file_type_dump")` → "转储文件"）。
    pub name: String,
    /// 扩展名，不含点（`["dump", "mfd"]`）。
    pub extensions: Vec<String>,
}

/// 「保存到...」对话框的入参。
///
/// 逐字对标 `command.js:661-666` 的 options：`title` / `defaultPath` / `filters` / `message`。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SaveDialogRequest {
    /// `i18n("dialog_title_save_to")`
    pub title: String,
    /// `path.join(dumpsFolder, defaultName)`
    pub default_path: PathBuf,
    pub filters: Vec<FileFilter>,
    /// `i18n("dialog_msg_choose_save_path")`
    pub message: String,
}

/// 「打开文件」对话框的入参。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OpenDialogRequest {
    pub title: String,
    pub default_path: PathBuf,
    /// `i18n("dialog_button_open")`
    pub button_label: String,
    pub filters: Vec<FileFilter>,
    pub message: String,
}

/// 危险操作确认框的入参，对标 `command.js:111-116` 的 `showMessageBox`。
///
/// 原版用 `type: "warning"` + `[取消, 确定]` 两个按钮，并按 `response === 1`
/// 判定「确定」。此处只保留「是 / 否」语义，不暴露按钮文案（`MessageBoxKind`
/// 决定标题栏图标，按钮文案由平台本地化）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConfirmRequest {
    /// `i18n("dialog_title_danger_operation")`
    pub title: String,
    /// 正文。`write-ufuid-uid` 会拼上 block0 hex（`command.js:211`）
    pub message: String,
}

/// 对话框提供方。
///
/// core 不链接任何 GUI 库，故以 trait 注入。`Send` 不要求：所有调用都发生在 UI 线程。
pub trait DialogProvider {
    /// 对标 `dialog.showSaveDialogSync`。返回 `None` 表示用户取消。
    fn show_save_dialog(&mut self, req: &SaveDialogRequest) -> Option<PathBuf>;

    /// 对标 `dialog.showOpenDialog` 的 `filePaths[0]`。返回 `None` 表示取消。
    fn show_open_dialog(&mut self, req: &OpenDialogRequest) -> Option<PathBuf>;

    /// 危险操作确认框。返回 `true` 表示用户点「确定」。
    ///
    /// 默认实现返回 `false`（拒绝执行）——安全默认值：未接入真实实现者时
    /// 写卡/格式化/锁卡一律不执行。
    fn confirm_danger(&mut self, _req: &ConfirmRequest) -> bool {
        false
    }
}

/// 转储文件过滤器，`i18n("file_type_dump")` + `["dump", "mfd"]`。
pub fn dump_filter(name: &str) -> FileFilter {
    FileFilter {
        name: name.to_string(),
        extensions: vec!["dump".to_string(), "mfd".to_string()],
    }
}

/// 按预设脚本作答的对话框。
///
/// 两个用途：
/// - 测试与 `--selftest`：预设返回值 + 记录收到的请求，用于断言文案与顺序。
/// - **生产路径**（S11-2）：app 层的应用内确认窗点「确定」后，把 `true`
///   预置进 `confirm_results`，engine 侧的 `prepare_write` 等照常调
///   `confirm_danger`，拿到的就是预置值——于是 engine 全程不弹任何系统框。
///   因为它不再只是测试替身，故正名为 `ScriptedDialog`。
#[derive(Clone, Debug, Default)]
pub struct ScriptedDialog {
    /// 依次弹出的保存对话框返回值；用尽后返回 `None`（模拟取消）。
    pub save_results: Vec<Option<PathBuf>>,
    /// 依次弹出的打开对话框返回值。
    pub open_results: Vec<Option<PathBuf>>,
    /// 依次弹出的危险确认框返回值；用尽后返回 `false`（拒绝执行）。
    pub confirm_results: Vec<bool>,
    /// 收到的保存请求，用于断言 title / defaultPath / filters。
    pub save_requests: Vec<SaveDialogRequest>,
    pub open_requests: Vec<OpenDialogRequest>,
    /// 收到的确认请求，用于断言 title / message。
    pub confirm_requests: Vec<ConfirmRequest>,
}

/// 测试期的旧名，保留以免大量测试代码改动。
pub type MockDialog = ScriptedDialog;

impl ScriptedDialog {
    pub fn new() -> Self {
        Self::default()
    }

    /// 预设下一次「保存到」的结果。
    pub fn with_save(mut self, r: Option<PathBuf>) -> Self {
        self.save_results.push(r);
        self
    }

    /// 预设下一次「打开文件」的结果。
    pub fn with_open(mut self, r: Option<PathBuf>) -> Self {
        self.open_results.push(r);
        self
    }

    /// 预设下一次危险确认框的返回值。
    pub fn with_confirm(mut self, ok: bool) -> Self {
        self.confirm_results.push(ok);
        self
    }

    /// 危险确认框被调用过几次。
    pub fn confirm_calls(&self) -> usize {
        self.confirm_requests.len()
    }

    /// 弹出保存对话框的次数。
    pub fn save_calls(&self) -> usize {
        self.save_requests.len()
    }

    /// 弹出打开对话框的次数。
    pub fn open_calls(&self) -> usize {
        self.open_requests.len()
    }
}

impl DialogProvider for ScriptedDialog {
    fn show_save_dialog(&mut self, req: &SaveDialogRequest) -> Option<PathBuf> {
        self.save_requests.push(req.clone());
        if self.save_results.is_empty() {
            None
        } else {
            self.save_results.remove(0)
        }
    }

    fn show_open_dialog(&mut self, req: &OpenDialogRequest) -> Option<PathBuf> {
        self.open_requests.push(req.clone());
        if self.open_results.is_empty() {
            None
        } else {
            self.open_results.remove(0)
        }
    }

    fn confirm_danger(&mut self, req: &ConfirmRequest) -> bool {
        self.confirm_requests.push(req.clone());
        // 用尽后返回 false：未预设即拒绝，符合安全默认值
        if self.confirm_results.is_empty() {
            false
        } else {
            self.confirm_results.remove(0)
        }
    }
}

/// 内部可变共享的对话框句柄，便于把 `&mut dyn DialogProvider` 借给任务函数。
pub type SharedDialog = Rc<RefCell<dyn DialogProvider>>;

/// 一个**挂起中**的「保存到...」请求（S11-7）。
///
/// engine 在流程中途需要保存路径时不能自己弹框（见模块文档），于是把它
/// 打包成这个值挂进 [`crate::engine::Engine`]，由 app 侧弹完框后送回
/// `Engine::resolve_save`。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingSave {
    /// 已经算好的默认路径（含 UID / sector，故只能在流程中途才确定）。
    pub default_path: PathBuf,
    /// 要传给 rfd 的入参。
    pub request: SaveDialogRequest,
    /// 要移动的源文件（`temp.mfd` 或 `nonces.bin`）。
    pub source: PathBuf,
    /// 收尾时要额外刷新的东西：读卡存盘要重刷历史列表。
    pub refresh_history: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dump_req() -> SaveDialogRequest {
        SaveDialogRequest {
            title: "保存到...".into(),
            default_path: PathBuf::from("/data/dumpfiles/112233445566_2024_01_02.mfd"),
            filters: vec![dump_filter("转储文件")],
            message: "请选择保存路径".into(),
        }
    }

    #[test]
    fn dump_filter_has_both_extensions() {
        let f = dump_filter("转储文件");
        assert_eq!(f.name, "转储文件");
        assert_eq!(f.extensions, vec!["dump".to_string(), "mfd".to_string()]);
    }

    #[test]
    fn mock_returns_preset_results_in_order() {
        let mut d = MockDialog::new()
            .with_save(Some(PathBuf::from("/a.mfd")))
            .with_save(None)
            .with_open(Some(PathBuf::from("/b.mfd")));
        assert_eq!(
            d.show_save_dialog(&dump_req()),
            Some(PathBuf::from("/a.mfd"))
        );
        // 第二个预设是取消
        assert_eq!(d.show_save_dialog(&dump_req()), None);
        assert_eq!(
            d.show_open_dialog(&OpenDialogRequest::default()),
            Some(PathBuf::from("/b.mfd"))
        );
    }

    #[test]
    fn mock_records_requests_verbatim() {
        let mut d = MockDialog::new();
        let req = dump_req();
        d.show_save_dialog(&req);
        assert_eq!(d.save_calls(), 1);
        assert_eq!(d.save_requests[0], req);
        assert_eq!(d.save_requests[0].default_path.extension().unwrap(), "mfd");
        assert_eq!(d.save_requests[0].filters[0].extensions.len(), 2);
    }

    #[test]
    fn mock_exhausted_results_behave_as_cancel() {
        let mut d = MockDialog::new();
        assert_eq!(d.show_save_dialog(&dump_req()), None);
        assert_eq!(d.show_open_dialog(&OpenDialogRequest::default()), None);
        assert_eq!(d.save_calls(), 1);
        assert_eq!(d.open_requests.len(), 1);
    }

    #[test]
    fn shared_dialog_allows_interior_mutation() {
        let d = Rc::new(RefCell::new(
            MockDialog::new().with_save(Some(PathBuf::from("/x.mfd"))),
        ));
        let shared: SharedDialog = d.clone();
        let got = shared.borrow_mut().show_save_dialog(&dump_req());
        assert_eq!(got, Some(PathBuf::from("/x.mfd")));
        assert_eq!(d.borrow().save_calls(), 1);
    }

    #[test]
    fn save_request_defaults_are_empty() {
        let r = SaveDialogRequest::default();
        assert!(r.title.is_empty());
        assert!(r.message.is_empty());
        assert!(r.filters.is_empty());
        assert_eq!(r.default_path, PathBuf::new());
    }

    #[test]
    fn mock_confirm_returns_preset_in_order() {
        let mut d = MockDialog::new().with_confirm(true).with_confirm(false);
        let req = ConfirmRequest {
            title: "危险操作警告".into(),
            message: "确定要继续吗？".into(),
        };
        assert!(d.confirm_danger(&req));
        assert!(!d.confirm_danger(&req));
        assert_eq!(d.confirm_calls(), 2);
    }

    /// 安全默认值：未预设确认结果时必须拒绝执行。
    #[test]
    fn mock_confirm_defaults_to_reject() {
        let mut d = MockDialog::new();
        assert!(!d.confirm_danger(&ConfirmRequest::default()));
        assert_eq!(d.confirm_calls(), 1);
    }

    /// 只实现 save/open 的旧实现者也应继承「确认=false」的安全默认。
    #[test]
    fn bare_impl_default_confirm_is_reject() {
        struct Bare;
        impl DialogProvider for Bare {
            fn show_save_dialog(&mut self, _: &SaveDialogRequest) -> Option<PathBuf> {
                None
            }
            fn show_open_dialog(&mut self, _: &OpenDialogRequest) -> Option<PathBuf> {
                None
            }
        }
        let mut b = Bare;
        assert!(!b.confirm_danger(&ConfirmRequest::default()));
    }

    #[test]
    fn mock_confirm_records_request_verbatim() {
        let mut d = MockDialog::new().with_confirm(true);
        let req = ConfirmRequest {
            title: "危险操作警告".into(),
            message: "即将写入的0块：\nAABBCCDDEEFF0011223344556677".into(),
        };
        assert!(d.confirm_danger(&req));
        assert_eq!(d.confirm_requests[0], req);
        assert!(d.confirm_requests[0].message.contains('A'));
    }
}
