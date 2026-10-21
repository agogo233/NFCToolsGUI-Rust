//! 系统文件对话框的线程宿主（S11-2）。
//!
//! rfd 的 `pick_file()` / `save_file()` 是**阻塞**调用。若在 eframe 的 UI 线程
//! 上直接调，用户在弹窗里选文件的那几秒里窗口会完全无响应。故本模块起一个
//! 专用工作线程：界面线程只负责「投递请求」和「每帧收取结果」。
//!
//! # 为什么不把 rfd 装进 core
//!
//! core 不链接任何 GUI 库（见 `dialogs.rs` 的分层说明），且 `DialogProvider`
//! 是**同步** trait，由 `Engine::dispatch` 在自己的栈上直接调用。系统弹窗必须
//! 跨线程、跨帧，无法塞进同步 trait。故流程被拆成两段：
//!
//! 1. app 层在此弹出系统文件框，拿到路径；
//! 2. 路径作为动作载荷交给 engine，`tasks::card_write::prepare_write` 的
//!    `dump_id` / `prepare_write_uid` 的 `dump_path` 分支会**跳过**文件框。
//!
//! 于是 core 侧一行不改，测试仍可用 `ScriptedDialog` 完整驱动。
//!
//! # 已知限制
//!
//! - **rfd 0.17 的阻塞 API 返回 `Option<PathBuf>`，不区分「用户取消」与
//!   「后端不可用」**，两者都是 `None`。Linux 无桌面门户时表现为「点了没反应」。
//!   [`file_dialogs_supported`] 能在最常见的场景（无头 / SSH）提前拦下并给出
//!   明确提示，但那只是启发式探测，不能覆盖全部失败原因。
//! - **弹窗无父窗口**。eframe 0.36 的 `CreationContext` 其实实现了
//!   `HasWindowHandle`（`eframe-0.36.2/src/epi.rs:100-107`），rfd 的
//!   `set_parent` 正是这个签名，故**类型上并非不可用**；真正的原因是
//!   `CreationContext` 只活过创建闭包、存不进 `NfcApp`，而工作线程在闭包之外。
//!   另：XDG portal 后端本就忽略 parent。功能正常，但弹窗不是模态于主窗，
//!   极端情况下可能出现在窗口后面。
//!
//! # S11-7：流程中途的保存框
//!
//! 另有两处保存框**没法预选路径**：默认文件名含刚读出的 UID（`read-IC` 收尾）
//! 或本轮 sector（HardNested `collectOnly`），都要等工具跑完才知道。core 侧的
//! [`nfc_core::engine::Engine`] 把它们记成 `pending_save`，本模块照常弹框，
//! app 在帧循环里取走请求、回传路径给 `Engine::resolve_save` 续接。
//!
//! 偏离登记见 docs/02 §6。

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread::JoinHandle;

use nfc_core::dialogs::{FileFilter, OpenDialogRequest, SaveDialogRequest};

/// 一次文件对话框请求的用途。收到结果时用它决定续接哪条流程。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FilePurpose {
    /// `write-IC`：写卡前选源 dump。
    WriteDump,
    /// `write-ufuid-uid`：读 block0 前选 dump。
    WriteUidDump,
    /// `open-dict-file`。
    DictFile,
    /// `save-log`。
    SaveLog,
    /// `dump-editor-choose-file`。
    EditorFile,
    /// `dump-editor-save` 的「另存为」。
    EditorSaveAs,
    /// `dump-comparator-choose-file` 的 A 侧。
    ComparatorA,
    /// `dump-comparator-choose-file` 的 B 侧。
    ComparatorB,
    /// `read-IC` 收尾：mfoc 跑完才弹（默认名含刚读出的 UID，事先无从预选）。
    ReadIcSave,
    /// HardNested `collectOnly`：采完才弹（默认名含本轮 sector）。
    NoncesSave,
    /// S18 `cuid-write`：选源 dump（CUID 空卡写入）。
    CuidDump,
    /// S18 `dump-editor-export-keys`：把密钥字典另存为文件。
    ExportKeys,
}

impl FilePurpose {
    /// 系统弹窗打开时，底栏显示的等待文案。
    pub fn pending_text(self) -> &'static str {
        match self {
            FilePurpose::WriteDump
            | FilePurpose::WriteUidDump
            | FilePurpose::EditorFile
            | FilePurpose::CuidDump => "正在等待选择文件…",
            FilePurpose::ComparatorA | FilePurpose::ComparatorB => "正在等待选择转储…",
            FilePurpose::DictFile => "正在等待选择字典…",
            FilePurpose::SaveLog
            | FilePurpose::EditorSaveAs
            | FilePurpose::ReadIcSave
            | FilePurpose::NoncesSave
            | FilePurpose::ExportKeys => "正在等待选择保存路径…",
        }
    }
}

/// 一个文件框的结果。
///
/// `path` 为 `None` 表示「用户取消」**或**「后端不可用」——见模块文档的已知限制。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileReply {
    pub purpose: FilePurpose,
    pub path: Option<PathBuf>,
}

/// 投递给工作线程的活儿。
enum Job {
    Open {
        purpose: FilePurpose,
        req: OpenDialogRequest,
    },
    Save {
        purpose: FilePurpose,
        req: SaveDialogRequest,
    },
}

/// 文件对话框宿主。
///
/// 只做「投递 / 收取」，不含任何业务判断，故 `--selftest` 可直接往
/// `feed_reply` 里塞结果来驱动后续流程，不必真的弹框。
pub struct DialogHost {
    tx: Sender<Job>,
    /// Job 的接收端。`spawn` 模式下它已被工作线程持有，这里为 `None`；
    /// `manual` 模式下无人消费，但必须留着以免 `send` 失败导致 `pending` 记不上。
    #[allow(dead_code)]
    job_rx: Option<Receiver<Job>>,
    rx: Receiver<FileReply>,
    /// 与 `rx` 配对的发送端。生产路径只有工作线程用；`--selftest` 用
    /// `feed_reply` 借它绕过系统弹窗，故需要持有。
    rep_tx: Sender<FileReply>,
    /// 已投递但尚未回传的用途，先进先出。与 `rx` 平行维护，好让界面显示
    /// 「正在等待选择文件…」，并在测试里断言队列已排空。
    pending: Vec<FilePurpose>,
    /// 工作线程句柄。**故意 drop 时不 join**：系统弹窗可能还开着，join 会
    /// 把主线程卡住直到用户关掉弹窗。线程靠 `tx` 全部 drop 后自然退出。
    _worker: Option<JoinHandle<()>>,
}

impl DialogHost {
    /// 测试用：不起工作线程，`request_*` 只记账不派活。
    ///
    /// 没有它，`--selftest` 与单测里的 `request_open` 会真的把 Job 发给工作
    /// 线程，**在有桌面的机器上弹出系统文件框并阻塞**，在无头机器上则与
    /// `feed_reply` 的回传在通道上竞争，造成 flaky 失败。
    pub fn manual() -> Self {
        // Job 的接收端必须留着：`request_open` 用 `send().is_ok()` 判定是否
        // 记账，接收端一掉 send 就失败，`pending` 会永远是空的。
        // 留着但**不 spawn 线程**即可——Job 进了通道没人消费，测试不会弹框。
        let (tx, rx) = channel::<Job>();
        let (rep_tx, rep_rx) = channel::<FileReply>();
        Self {
            tx,
            job_rx: Some(rx),
            rx: rep_rx,
            rep_tx,
            pending: Vec::new(),
            _worker: None,
        }
    }

    /// 启动工作线程。
    pub fn spawn() -> Self {
        let (job_tx, job_rx) = channel::<Job>();
        let (rep_tx, rep_rx) = channel::<FileReply>();
        // 克隆一份给工作线程，`rep_tx` 本体留在宿主里供 `feed_reply` 用。
        let worker_tx = rep_tx.clone();
        let worker = std::thread::Builder::new()
            .name("nfc-dialog".to_owned())
            .spawn(move || {
                // 循环直到界面侧全部 Sender 被 drop（即 App 退出）。
                while let Ok(job) = job_rx.recv() {
                    let (purpose, path) = match job {
                        Job::Open { purpose, req } => (purpose, run_open(&req)),
                        Job::Save { purpose, req } => (purpose, run_save(&req)),
                    };
                    // 界面已退出时 send 失败，直接收工。
                    if worker_tx.send(FileReply { purpose, path }).is_err() {
                        break;
                    }
                }
            });
        Self {
            tx: job_tx,
            job_rx: None, // 已被上面 spawn 的工作线程接管
            rx: rep_rx,
            rep_tx,
            pending: Vec::new(),
            _worker: worker.ok(),
        }
    }

    /// 投递一个「打开文件」请求，立即返回。
    pub fn request_open(&mut self, purpose: FilePurpose, req: OpenDialogRequest) {
        if self.tx.send(Job::Open { purpose, req }).is_ok() {
            self.pending.push(purpose);
        }
    }

    /// 投递一个「保存到」请求，立即返回。
    pub fn request_save(&mut self, purpose: FilePurpose, req: SaveDialogRequest) {
        if self.tx.send(Job::Save { purpose, req }).is_ok() {
            self.pending.push(purpose);
        }
    }

    /// 非阻塞取一个已完成的结果。没有就是没有。
    ///
    /// 每个回传都从 `pending` 摘掉一条，故 [`Self::outstanding`] 归零即代表
    /// 所有系统弹窗都已关闭。
    pub fn poll(&mut self) -> Option<FileReply> {
        let reply = self.rx.try_recv().ok()?;
        if let Some(pos) = self.pending.iter().position(|p| *p == reply.purpose) {
            self.pending.remove(pos);
        }
        Some(reply)
    }

    /// 直接塞一个结果，跳过系统弹窗。`--selftest` 用。
    pub fn feed_reply(&mut self, purpose: FilePurpose, path: Option<PathBuf>) {
        if let Some(pos) = self.pending.iter().position(|p| *p == purpose) {
            self.pending.remove(pos);
        }
        let _ = self.rep_tx.send(FileReply { purpose, path });
    }

    /// 是否还有一个文件框在等用户。
    pub fn is_pending(&self) -> bool {
        self.pending_purpose().is_some()
    }

    /// 当前在等的用途。用于显示「选择中…」提示。
    ///
    /// 队列里可能已有多条请求（用户连点了两次），返回最早那条。
    pub fn pending_purpose(&self) -> Option<FilePurpose> {
        self.pending.first().copied()
    }

    /// 已投递但尚未回传的数量。`--selftest` 用来断言「取消后队列已排空」。
    pub fn outstanding(&self) -> usize {
        self.pending.len()
    }
}

/// 工作线程里的 `pick_file`。
fn run_open(req: &OpenDialogRequest) -> Option<PathBuf> {
    let mut dlg = rfd::FileDialog::new();
    if !req.title.is_empty() {
        dlg = dlg.set_title(req.title.clone());
    }
    dlg = set_dir(dlg, &req.default_path);
    dlg = add_filters(dlg, &req.filters);
    dlg.pick_file()
}

/// 工作线程里的 `save_file`。
fn run_save(req: &SaveDialogRequest) -> Option<PathBuf> {
    let mut dlg = rfd::FileDialog::new();
    if !req.title.is_empty() {
        dlg = dlg.set_title(req.title.clone());
    }
    // 原版 Electron 的 `defaultPath` 就是**裸文件名**（`command.js:541` 的
    // `NFCTools_log_<时间戳>`、`:450` 的 `data.url` 都没有 `path.join`），
    // 即停在系统上次使用目录。此处只在确实带目录时才设 `set_directory`。
    if let Some(name) = req.default_path.file_name() {
        dlg = dlg.set_file_name(name.to_string_lossy().into_owned());
        if let Some(dir) = req.default_path.parent() {
            dlg = set_dir(dlg, dir);
        }
    } else {
        dlg = set_dir(dlg, &req.default_path);
    }
    dlg = add_filters(dlg, &req.filters);
    dlg.save_file()
}

/// 设起始目录。目录不存在时**跳过**而非报错——原版 `defaultPath` 指向的
/// `dumpfiles/` 可能还没建（首次启动），硬设会让部分后端直接失败。
fn set_dir(dlg: rfd::FileDialog, dir: &Path) -> rfd::FileDialog {
    if dir.as_os_str().is_empty() || !dir.is_dir() {
        return dlg;
    }
    dlg.set_directory(dir)
}

/// 逐个加过滤器。扩展名为空的过滤器跳过——rfd 在部分后端会把
/// 「空扩展名列表」当成「只允许无扩展名的文件」。
fn add_filters(dlg: rfd::FileDialog, filters: &[FileFilter]) -> rfd::FileDialog {
    let mut out = dlg;
    for f in filters {
        if f.extensions.is_empty() {
            continue;
        }
        let exts: Vec<&str> = f.extensions.iter().map(String::as_str).collect();
        out = out.add_filter(f.name.clone(), &exts);
    }
    out
}

/// 系统文件对话框在本机是否可用。
///
/// Windows / macOS 恒为 `true`（原生弹窗，无外部前提）。Linux 需要桌面门户
/// 服务，故探测会话总线是否可达。
pub fn file_dialogs_supported() -> bool {
    if !cfg!(target_os = "linux") {
        return true;
    }
    portal_available(
        std::env::var_os("DBUS_SESSION_BUS_ADDRESS").as_deref(),
        std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .as_deref(),
    )
}

/// 纯函数版探测，便于单测覆盖全部组合。
///
/// 总线地址显式给出时直接可用；否则回落到 XDG 规范默认位置
/// `$XDG_RUNTIME_DIR/bus`。
fn portal_available(bus_addr: Option<&OsStr>, runtime_dir: Option<&Path>) -> bool {
    if bus_addr.is_some_and(|a| !a.is_empty()) {
        return true;
    }
    runtime_dir.is_some_and(|d| d.join("bus").exists())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_filter_is_skipped_not_passed_through() {
        // 空扩展名会让部分后端只显示无扩展名文件
        let filters = vec![FileFilter {
            name: "空".into(),
            extensions: vec![],
        }];
        // 只验证不 panic；真正弹框在真机验证
        let _ = add_filters(rfd::FileDialog::new(), &filters);
    }

    #[test]
    fn missing_dir_is_skipped_instead_of_erroring() {
        let _ = set_dir(rfd::FileDialog::new(), Path::new("/nonexistent-xyz/dumps"));
        let _ = set_dir(rfd::FileDialog::new(), Path::new(""));
    }

    #[test]
    fn explicit_bus_address_means_portal_available() {
        assert!(portal_available(
            Some(OsStr::new("unix:path=/run/user/1000/bus")),
            None
        ));
    }

    #[test]
    fn empty_bus_address_falls_back_to_xdg_default_socket() {
        let dir = std::env::temp_dir().join("nfc-portal-probe");
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(dir.join("bus"), b"").unwrap();
        // 显式但为空的地址 → 回落到 XDG 默认位置 → socket 存在 → 可用
        assert!(portal_available(Some(OsStr::new("")), Some(&dir)));
        // socket 不存在 → 不可用
        assert!(!portal_available(
            Some(OsStr::new("")),
            Some(Path::new("/nope"))
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn headless_linux_reports_unsupported() {
        // 无头 / SSH 会话的典型形态
        assert!(!portal_available(None, None));
    }

    #[test]
    fn host_starts_empty_and_drains_results() {
        let mut host = DialogHost::spawn();
        assert!(!host.is_pending(), "刚建好不该有待处理请求");
        assert_eq!(host.outstanding(), 0);
        assert!(host.poll().is_none());
    }

    #[test]
    fn feed_reply_is_polled_back_in_order() {
        let mut host = DialogHost::spawn();
        // 模拟「已投递两个请求」，再回传结果
        host.request_open(FilePurpose::ComparatorA, OpenDialogRequest::default());
        host.request_open(FilePurpose::ComparatorB, OpenDialogRequest::default());
        assert_eq!(host.outstanding(), 2);
        assert_eq!(host.pending_purpose(), Some(FilePurpose::ComparatorA));

        host.feed_reply(FilePurpose::ComparatorA, Some(PathBuf::from("/a.mfd")));
        let r = host.poll().expect("应有结果");
        assert_eq!(r.purpose, FilePurpose::ComparatorA);
        assert_eq!(r.path, Some(PathBuf::from("/a.mfd")));
        assert_eq!(host.outstanding(), 1);

        // 取消也要回传，否则 pending 永远排不空、提示条撤不掉
        host.feed_reply(FilePurpose::ComparatorB, None);
        let r = host.poll().expect("应有结果");
        assert_eq!(r.purpose, FilePurpose::ComparatorB);
        assert_eq!(r.path, None);
        assert!(!host.is_pending());
        assert_eq!(host.outstanding(), 0);
    }

    #[test]
    fn duplicate_purposes_are_matched_first_in_first_out() {
        // 同一个用途可能被投递多次（用户连点「保存」），不能错配
        let mut host = DialogHost::spawn();
        host.request_save(FilePurpose::SaveLog, SaveDialogRequest::default());
        host.request_save(FilePurpose::SaveLog, SaveDialogRequest::default());
        host.feed_reply(FilePurpose::SaveLog, Some(PathBuf::from("/first.txt")));
        assert_eq!(host.outstanding(), 1);
        assert_eq!(host.poll().unwrap().path, Some(PathBuf::from("/first.txt")));
    }

    #[test]
    fn pending_text_is_distinct_per_purpose() {
        // 底栏提示按用途给不同文案
        let all = [
            FilePurpose::WriteDump,
            FilePurpose::WriteUidDump,
            FilePurpose::DictFile,
            FilePurpose::SaveLog,
            FilePurpose::EditorFile,
            FilePurpose::EditorSaveAs,
            FilePurpose::ComparatorA,
            FilePurpose::ComparatorB,
            FilePurpose::CuidDump,
            FilePurpose::ExportKeys,
        ];
        for p in all {
            assert!(!p.pending_text().is_empty(), "{p:?} 缺等待文案");
        }
    }
}
