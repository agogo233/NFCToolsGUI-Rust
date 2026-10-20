//! 写卡类任务。对标 `command.js:107-146`（`write-IC` / `format-card`）与
//! `command.js:185-241`（`write-ufuid-uid` / `lock-ufuid`）。
//!
//! 偏离登记见 docs/02 §6。
//!
//! 三类写卡工具（`nfc-mfclassic` / `nfc-mfsetuid` / `nfc-mflock`）经核实
//! **完全不解析 stdout**，只按退出码判定（见 docs/04 §2），故本模块只负责：
//! 参数拼装、危险确认、block0 读取与校验、结果文案。运行编排在 S11 接 `ToolRunner`。

use std::path::{Path, PathBuf};

use crate::dialogs::{DialogProvider, FileFilter, OpenDialogRequest};
use crate::i18n;
use crate::paths::Paths;
use crate::state::{AppState, Event};

/// `nfc-mfclassic` 子命令。
pub const MFCLASSIC_WRITE: &str = "w";
pub const MFCLASSIC_FORMAT: &str = "f";

/// 写卡前的处置结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PrepareOutcome {
    /// 可以执行写卡，携带已解析的 dump 路径。
    ///
    /// 路径必须随结果返回：`write-IC` 选了哪个文件是执行阶段的唯一输入，
    /// 丢弃它等于让调用方无从构造 `nfc-mfclassic` 的第 4 个参数。
    Ready(PathBuf),
    /// 可以执行，且无需选文件（`lock-ufuid` / `format-card`）。
    ReadyNoFile,
    /// 用户取消（关闭确认框 / 关闭文件选择框）
    Cancelled,
    /// 出错并已落日志，应 `printExitLog(1)`
    Failed { reason: WriteFailure },
}

/// 写卡准备阶段的失败原因。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WriteFailure {
    /// 转储文件读取失败，对标 `command.js:194`
    ReadFailed,
    /// 转储文件不足 16 字节，读不出完整 block0，对标 `command.js:200`
    TooShort,
}

impl WriteFailure {
    /// 对应 i18n key。
    pub fn i18n_key(self) -> &'static str {
        match self {
            WriteFailure::ReadFailed => "log_msg_read_dump_file_failed",
            WriteFailure::TooShort => "log_msg_dump_file_too_short",
        }
    }
}

/// 读取 dump 的 block0 并转大写 hex，对标 `command.js:193-196`。
///
/// 原版 `fs.readFileSync(p).subarray(0, 16).toString("hex").toUpperCase()`：
/// 不足 16 字节时 `subarray` 静默截短，随后靠 `length !== 32` 拦下。
/// 此处直接 `min(16)` 取值，长度校验交给调用方，与原版等价。
pub fn read_block0_hex(path: &Path) -> std::io::Result<String> {
    let data = std::fs::read(path)?;
    let n = data.len().min(16);
    Ok(crate::model::hex::hex_encode_upper(&data[..n]))
}

/// block0 是否足够写 UFUID（需满 16 字节 = 32 hex）。
pub fn block0_is_valid(hex: &str) -> bool {
    hex.len() == 32
}

/// 危险确认框的通用构造，对标 `command.js:111-116` / `command.js:214-219`。
pub fn danger_request(message_key: &str) -> crate::dialogs::ConfirmRequest {
    crate::dialogs::ConfirmRequest {
        title: i18n::t("dialog_title_danger_operation"),
        message: i18n::t(message_key),
    }
}

/// `write-IC` 的危险确认框，`command.js:112`。
pub fn write_dump_confirm() -> crate::dialogs::ConfirmRequest {
    danger_request("dialog_msg_confirm_write_dump")
}

/// `lock-ufuid` 的危险确认框，`command.js:222`。
pub fn lock_confirm() -> crate::dialogs::ConfirmRequest {
    danger_request("dialog_msg_card_will_be_locked")
}

/// `write-ufuid-uid` 的危险确认框，`command.js:212-214`：**正文拼接 block0 hex**。
pub fn write_uid_confirm(block0_hex: &str) -> crate::dialogs::ConfirmRequest {
    crate::dialogs::ConfirmRequest {
        title: i18n::t("dialog_title_danger_operation"),
        message: format!("{}{}", i18n::t("dialog_msg_confirm_write_uid"), block0_hex),
    }
}

/// 选 dump 文件的对话框，`command.js:130-138`。
pub fn open_dump_request(dumps_dir: &Path) -> OpenDialogRequest {
    OpenDialogRequest {
        title: i18n::t("dialog_title_choose_dump_need_to_write"),
        default_path: dumps_dir.to_path_buf(),
        button_label: i18n::t("dialog_button_open"),
        filters: vec![FileFilter {
            name: i18n::t("file_type_dump"),
            extensions: vec!["dump".to_string(), "mfd".to_string()],
        }],
        message: i18n::t("dialog_msg_choose_dump_file"),
    }
}

/// 解析 `write-IC` 传入的 dumpId：绝对路径直接用，相对路径拼到 `dumpfiles/`。
///
/// 对标 `command.js:127`：`path.isAbsolute(dumpId) ? dumpId : path.join(dumpsFolder, dumpId)`。
pub fn resolve_dump_id(dump_id: &str, dumps_dir: &Path) -> PathBuf {
    let p = Path::new(dump_id);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        dumps_dir.join(dump_id)
    }
}

/// `nfc-mfclassic w A u <src> <temp> f`，对标 `command.js:117`。
///
/// 六个参数分别是：子命令、目标扇区、UID 模式、源 dump、临时读出 dump、扇区过滤标志。
pub fn mfclassic_write_args(src: &Path, temp: &Path) -> Vec<String> {
    vec![
        MFCLASSIC_WRITE.to_string(),
        "A".to_string(),
        "u".to_string(),
        src.display().to_string(),
        temp.display().to_string(),
        "f".to_string(),
    ]
}

/// `nfc-mfclassic f A u <temp> <temp> f`，对标 `command.js:145`。
///
/// 源与目标同为 `temp.mfd`——先读出当前卡内容到 temp，再把 temp 写回去。
pub fn mfclassic_format_args(temp: &Path) -> Vec<String> {
    vec![
        MFCLASSIC_FORMAT.to_string(),
        "A".to_string(),
        "u".to_string(),
        temp.display().to_string(),
        temp.display().to_string(),
        "f".to_string(),
    ]
}

/// `nfc-mfsetuid -q <block0_hex>`，对标 `command.js:220`。
pub fn mfsetuid_args(block0_hex: &str) -> Vec<String> {
    vec!["-q".to_string(), block0_hex.to_string()]
}

/// `nfc-mflock -q`，对标 `command.js:231`。无额外参数。
pub fn mflock_args() -> Vec<String> {
    vec!["-q".to_string()]
}

/// 把准备阶段的失败落进日志，对标 `command.js:195` / `command.js:201`。
pub fn report_failure(state: &mut AppState, reason: WriteFailure) {
    state.apply(Event::Log(format!("\n\n{}\n", i18n::t(reason.i18n_key()))));
}

/// `write-IC` 的完整准备流程：选文件 → 危险确认。
///
/// 对标 `command.js:108-140`。`dump_id` 为 `Some` 时跳过文件选择框（原版从
/// `dump-saved` 引导按钮带参调用），`None` 则弹框。
pub fn prepare_write(
    paths: &Paths,
    dump_id: Option<&str>,
    dialog: &mut dyn DialogProvider,
) -> PrepareOutcome {
    let chosen = match dump_id {
        Some(id) => resolve_dump_id(id, &paths.dumpfiles_dir()),
        None => match dialog.show_open_dialog(&open_dump_request(&paths.dumpfiles_dir())) {
            Some(p) => p,
            None => return PrepareOutcome::Cancelled,
        },
    };
    if dialog.confirm_danger(&write_dump_confirm()) {
        PrepareOutcome::Ready(chosen)
    } else {
        PrepareOutcome::Cancelled
    }
}

/// `write-ufuid-uid` 的完整准备流程：选文件 → 读 block0 → 校验 → 危险确认。
///
/// 对标 `command.js:186-222`。返回 `Ok(hex)` 表示可执行 `nfc-mfsetuid`。
pub fn prepare_write_uid(
    state: &mut AppState,
    paths: &Paths,
    dialog: &mut dyn DialogProvider,
    dump_path: Option<PathBuf>,
) -> Result<String, PrepareOutcome> {
    // 文件框默认落在 dumpfiles 目录（dump 的存放处），而非进程 CWD——
    // 后者取决于启动方式，用户得自己翻目录树才能找到自己的卡。
    let path = match dump_path
        .or_else(|| dialog.show_open_dialog(&open_dump_request(&paths.dumpfiles_dir())))
    {
        Some(p) => p,
        None => return Err(PrepareOutcome::Cancelled),
    };
    let hex = match read_block0_hex(&path) {
        Ok(h) => h,
        Err(_) => {
            report_failure(state, WriteFailure::ReadFailed);
            return Err(PrepareOutcome::Failed {
                reason: WriteFailure::ReadFailed,
            });
        }
    };
    if !block0_is_valid(&hex) {
        report_failure(state, WriteFailure::TooShort);
        return Err(PrepareOutcome::Failed {
            reason: WriteFailure::TooShort,
        });
    }
    if dialog.confirm_danger(&write_uid_confirm(&hex)) {
        Ok(hex)
    } else {
        Err(PrepareOutcome::Cancelled)
    }
}

/// `lock-ufuid` 的准备流程：只有危险确认。
pub fn prepare_lock(dialog: &mut dyn DialogProvider) -> PrepareOutcome {
    if dialog.confirm_danger(&lock_confirm()) {
        PrepareOutcome::ReadyNoFile
    } else {
        PrepareOutcome::Cancelled
    }
}

/// 写卡后的结果文案，对标 `command.js:121`（`code === 0` 判成败）。
pub fn report_write_result(state: &mut AppState, code: Option<i32>) {
    let key = if code == Some(0) {
        "log_msg_write_success"
    } else {
        "log_msg_write_failed"
    };
    state.apply(Event::Log(format!("\n\n{}\n", i18n::t(key))));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 每个测试**只删自己那一个**目录。
    ///
    /// 不能用「共享 Vec + drop 时 drain 全部」的写法：libtest 默认多线程并行，
    /// 任一测试结束就会连带删掉其他正在使用中的目录，导致随机失败。
    struct TempGuard(PathBuf);

    impl Drop for TempGuard {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// 目录名带测试名 + 进程 id，同一进程内不会撞名。
    fn tempdir(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("nfc-s6-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    // ---------------------------------------------------------- 参数拼装

    #[test]
    fn mfclassic_write_args_match_original_six_tokens() {
        let a = mfclassic_write_args(Path::new("/d/in.mfd"), Path::new("/d/temp.mfd"));
        assert_eq!(a.len(), 6);
        assert_eq!(a, vec!["w", "A", "u", "/d/in.mfd", "/d/temp.mfd", "f"]);
    }

    #[test]
    fn mfclassic_format_args_use_temp_twice() {
        let a = mfclassic_format_args(Path::new("/d/temp.mfd"));
        assert_eq!(a, vec!["f", "A", "u", "/d/temp.mfd", "/d/temp.mfd", "f"]);
        // 源与目标同路径
        assert_eq!(a[3], a[4]);
    }

    #[test]
    fn mfsetuid_and_mflock_args() {
        assert_eq!(
            mfsetuid_args("AABBCCDDEEFF0011223344556677"),
            vec!["-q", "AABBCCDDEEFF0011223344556677"]
        );
        assert_eq!(mflock_args(), vec!["-q"]);
    }

    #[test]
    fn resolve_dump_id_handles_absolute_and_relative() {
        let dir = Path::new("/data/dumpfiles");
        assert_eq!(
            resolve_dump_id("/abs/x.mfd", dir),
            PathBuf::from("/abs/x.mfd")
        );
        assert_eq!(
            resolve_dump_id("x.mfd", dir),
            PathBuf::from("/data/dumpfiles/x.mfd")
        );
        assert_eq!(
            resolve_dump_id("sub/x.mfd", dir),
            PathBuf::from("/data/dumpfiles/sub/x.mfd")
        );
    }

    // ---------------------------------------------------------- block0

    #[test]
    fn read_block0_hex_is_uppercase_and_16_bytes() {
        let dir = tempdir("block0");
        let _guard = TempGuard(dir.clone());
        let p = dir.join("a.mfd");
        let mut data = vec![0xab; 32];
        data[15] = 0xcd;
        std::fs::write(&p, &data).unwrap();
        let hex = read_block0_hex(&p).unwrap();
        assert_eq!(hex, "ABABABABABABABABABABABABABABABCD");
        assert_eq!(hex.len(), 32);
        assert!(block0_is_valid(&hex));
    }

    #[test]
    fn read_block0_hex_truncates_short_file_but_is_invalid() {
        let dir = tempdir("short");
        let _guard = TempGuard(dir.clone());
        let p = dir.join("s.mfd");
        std::fs::write(&p, [0xab; 10]).unwrap();
        let hex = read_block0_hex(&p).unwrap();
        assert_eq!(hex.len(), 20, "10 字节 → 20 hex");
        assert!(!block0_is_valid(&hex), "不足 16 字节应判无效");
    }

    #[test]
    fn read_block0_hex_missing_file_errors() {
        let dir = tempdir("nofile");
        let _guard = TempGuard(dir.clone());
        assert!(read_block0_hex(&dir.join("nope.mfd")).is_err());
    }

    #[test]
    fn block0_is_valid_rejects_wrong_lengths() {
        assert!(!block0_is_valid(""));
        assert!(!block0_is_valid("ABCD"));
        assert!(!block0_is_valid(&"A".repeat(31)));
        assert!(!block0_is_valid(&"A".repeat(33)));
        assert!(block0_is_valid(&"A".repeat(32)));
    }

    // ---------------------------------------------------------- 确认框

    #[test]
    fn write_uid_confirm_appends_block0_to_message() {
        let req = write_uid_confirm("AABBCCDDEEFF0011223344556677");
        assert_eq!(req.title, "危险操作警告");
        assert!(
            req.message.ends_with("AABBCCDDEEFF0011223344556677"),
            "正文应拼上 block0: {}",
            req.message
        );
        assert!(req.message.starts_with("该操作将会改写UFUID卡片的0块"));
    }

    #[test]
    fn lock_confirm_message_warns_irreversible() {
        let req = lock_confirm();
        assert!(req.message.contains("锁死"), "实际: {}", req.message);
        assert!(req.message.contains("不可恢复"));
    }

    #[test]
    fn open_dump_request_uses_dumpfiles_dir_and_filter() {
        let req = open_dump_request(Path::new("/data/dumpfiles"));
        assert_eq!(req.default_path, PathBuf::from("/data/dumpfiles"));
        assert_eq!(req.button_label, "打开");
        assert_eq!(req.filters[0].extensions, vec!["dump", "mfd"]);
    }

    // ---------------------------------------------------------- prepare

    #[test]
    fn prepare_write_confirmed_is_ready() {
        let dir = tempdir("pw-ok");
        let _guard = TempGuard(dir.clone());
        let paths = Paths::new(&dir);
        let mut dlg = crate::dialogs::MockDialog::new()
            .with_open(Some(dir.join("a.mfd")))
            .with_confirm(true);
        assert_eq!(
            prepare_write(&paths, None, &mut dlg),
            PrepareOutcome::Ready(dir.join("a.mfd"))
        );
        assert_eq!(dlg.confirm_calls(), 1);
        assert!(!dlg.confirm_requests.is_empty(), "确认请求应被记录");
    }

    #[test]
    fn prepare_write_cancelled_does_not_confirm() {
        let dir = tempdir("pw-cancel");
        let _guard = TempGuard(dir.clone());
        let paths = Paths::new(&dir);
        let mut dlg = crate::dialogs::MockDialog::new()
            .with_open(Some(dir.join("a.mfd")))
            .with_confirm(false);
        assert_eq!(
            prepare_write(&paths, None, &mut dlg),
            PrepareOutcome::Cancelled
        );
        // 确认框确实弹了，只是用户点了取消
        assert_eq!(dlg.confirm_calls(), 1);
    }

    #[test]
    fn prepare_write_open_cancelled_skips_confirm() {
        let dir = tempdir("pw-open-cancel");
        let _guard = TempGuard(dir.clone());
        let paths = Paths::new(&dir);
        let mut dlg = crate::dialogs::MockDialog::new()
            .with_open(None)
            .with_confirm(true);
        assert_eq!(
            prepare_write(&paths, None, &mut dlg),
            PrepareOutcome::Cancelled
        );
        assert_eq!(dlg.confirm_calls(), 0, "文件框就取消不该再弹确认框");
    }

    #[test]
    fn prepare_write_with_dump_id_skips_open_dialog() {
        let dir = tempdir("pw-id");
        let _guard = TempGuard(dir.clone());
        let paths = Paths::new(&dir);
        std::fs::create_dir_all(paths.dumpfiles_dir()).unwrap();
        let mut dlg = crate::dialogs::MockDialog::new().with_confirm(true);
        assert_eq!(
            prepare_write(&paths, Some("a.mfd"), &mut dlg),
            PrepareOutcome::Ready(paths.dumpfiles_dir().join("a.mfd")),
            "带 dumpId 时应解析为 dumpfiles 目录下的绝对路径"
        );
        assert!(dlg.open_requests.is_empty(), "带 dumpId 不应弹文件框");
    }

    #[test]
    fn prepare_write_uid_valid_flow() {
        let dir = tempdir("pwu-ok");
        let _guard = TempGuard(dir.clone());
        let paths = Paths::new(&dir);
        let p = dir.join("a.mfd");
        std::fs::write(&p, vec![0x5a; 32]).unwrap();
        let mut s = AppState::new();
        let mut dlg = crate::dialogs::MockDialog::new().with_confirm(true);
        let hex = prepare_write_uid(&mut s, &paths, &mut dlg, Some(p)).expect("应通过");
        assert!(s.log_buffer.is_empty(), "成功路径不该报错");
        assert_eq!(hex.len(), 32);
        assert_eq!(dlg.confirm_calls(), 1);
        assert!(dlg.confirm_requests[0].message.contains(&hex));
    }

    #[test]
    fn prepare_write_uid_short_file_reports_and_skips_confirm() {
        let dir = tempdir("pwu-short");
        let _guard = TempGuard(dir.clone());
        let paths = Paths::new(&dir);
        let p = dir.join("s.mfd");
        std::fs::write(&p, vec![0x5a; 8]).unwrap();
        let mut s = AppState::new();
        let mut dlg = crate::dialogs::MockDialog::new().with_confirm(true);
        let e = prepare_write_uid(&mut s, &paths, &mut dlg, Some(p)).unwrap_err();
        assert_eq!(
            e,
            PrepareOutcome::Failed {
                reason: WriteFailure::TooShort
            }
        );
        assert_eq!(dlg.confirm_calls(), 0, "过短不该弹确认框");
        assert!(
            s.log_buffer.contains("转储文件太短，无法读取0块"),
            "实际: {}",
            s.log_buffer
        );
    }

    #[test]
    fn prepare_write_uid_unreadable_reports_read_failed() {
        let dir = tempdir("pwu-nofile");
        let _guard = TempGuard(dir.clone());
        let mut s = AppState::new();
        let mut dlg = crate::dialogs::MockDialog::new().with_confirm(true);
        let paths = Paths::new(&dir);
        let e = prepare_write_uid(&mut s, &paths, &mut dlg, Some(dir.join("nonexistent.mfd")))
            .unwrap_err();
        assert_eq!(
            e,
            PrepareOutcome::Failed {
                reason: WriteFailure::ReadFailed
            }
        );
        assert!(s.log_buffer.contains("无法读取转储文件"));
        assert_eq!(dlg.confirm_calls(), 0);
    }

    #[test]
    fn prepare_write_uid_cancelled_at_confirm() {
        let dir = tempdir("pwu-cancel");
        let _guard = TempGuard(dir.clone());
        let paths = Paths::new(&dir);
        let p = dir.join("a.mfd");
        std::fs::write(&p, vec![0x5a; 32]).unwrap();
        let mut s = AppState::new();
        let mut dlg = crate::dialogs::MockDialog::new().with_confirm(false);
        assert_eq!(
            prepare_write_uid(&mut s, &paths, &mut dlg, Some(p)).unwrap_err(),
            PrepareOutcome::Cancelled
        );
        assert!(s.log_buffer.is_empty(), "取消不该报错");
    }

    #[test]
    fn prepare_lock_confirmed_and_cancelled() {
        let mut ok = crate::dialogs::MockDialog::new().with_confirm(true);
        assert_eq!(prepare_lock(&mut ok), PrepareOutcome::ReadyNoFile);
        let mut no = crate::dialogs::MockDialog::new().with_confirm(false);
        assert_eq!(prepare_lock(&mut no), PrepareOutcome::Cancelled);
        assert_eq!(no.confirm_calls(), 1);
    }

    /// 安全锁：任何未预设确认结果的实现者都必须拒绝写卡。
    #[test]
    fn unconfirmed_always_cancels() {
        let dir = tempdir("unsafe");
        let _guard = TempGuard(dir.clone());
        let paths = Paths::new(&dir);
        let mut dlg = crate::dialogs::MockDialog::new().with_open(Some(dir.join("a.mfd")));
        assert_eq!(
            prepare_write(&paths, None, &mut dlg),
            PrepareOutcome::Cancelled
        );
        assert_eq!(prepare_lock(&mut dlg), PrepareOutcome::Cancelled);
    }

    // ---------------------------------------------------------- 结果文案

    #[test]
    fn report_write_result_branches_on_zero() {
        let mut s = AppState::new();
        report_write_result(&mut s, Some(0));
        assert!(s.log_buffer.contains("写卡成功"));
        report_write_result(&mut s, Some(1));
        assert!(s.log_buffer.contains("写卡失败"));
        // 被 kill（code=None）也算失败
        report_write_result(&mut s, None);
        assert_eq!(s.log_buffer.matches("写卡失败").count(), 2);
    }

    #[test]
    fn report_failure_uses_reason_key() {
        let mut s = AppState::new();
        report_failure(&mut s, WriteFailure::TooShort);
        assert!(s.log_buffer.contains("转储文件太短"));
        let mut s2 = AppState::new();
        report_failure(&mut s2, WriteFailure::ReadFailed);
        assert!(s2.log_buffer.contains("无法读取转储文件"));
    }
}
