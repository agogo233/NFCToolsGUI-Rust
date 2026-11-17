//! 单实例锁（§6.2 #34，原版无此机制，刻意新增）。
//!
//! `paths.rs` 把 `libnfc.conf` / `temp.mfd` / `nonces.bin` / `keys.txt` 等
//! 全部落在**按用户聚合**的同一目录：`libnfc.conf` 每次连接都整文件覆盖写、
//! `temp.mfd` 是写卡备份闸的唯一载体——双开时后连实例会改写配置、覆盖对方
//! 正在用的备份，最坏可绕过「失败可恢复」保障把错误数据写进卡。
//!
//! 故 GUI 启动时对数据目录下的 `instance.lock` 加**非阻塞独占锁**：拿到即
//! 唯一实例；拿不到说明已有实例在跑，提示后退出。锁随句柄存活，进程退出
//! （含崩溃）即释放，无残留文件问题；锁文件本体是 0 字节哨兵，不清理——
//! 删除会与「新实例已打开同一文件」竞争，反而破坏锁语义。

use std::fs;
use std::path::Path;

/// 持有期即锁期：保活到进程退出。
pub struct InstanceLock {
    _file: fs::File,
}

impl InstanceLock {
    /// 尝试对 `path` 加非阻塞独占锁。
    ///
    /// - `Ok(Some(lock))`：拿到锁，本进程是唯一实例；
    /// - `Ok(None)`：锁被占（已有实例在跑）；
    /// - `Err(e)`：锁文件打不开（数据目录不可写等）——与「已有实例」是
    ///   两回事，调用方不应把基础设施故障误报成「重复启动」。
    pub fn acquire(path: &Path) -> std::io::Result<Option<Self>> {
        // 锁文件是 0 字节哨兵，内容无关紧要；truncate 声明仅为满足
        // clippy::suspicious_open_options（create+write 必须显式表态）。
        let file = fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(path)?;
        if try_lock_exclusive(&file) {
            Ok(Some(Self { _file: file }))
        } else {
            Ok(None)
        }
    }
}

/// 检测到第二实例时的用户提示。Windows 走原生 MessageBox（GUI 子系统下
/// stderr 不可见）；其余平台退回 stderr（终端 / journal 可见）。
/// 不用 `rfd::MessageDialog`：本 crate 的 rfd 特性集（xdg-portal + wayland，
/// 无 gtk3）不保证它在 Linux 可用。
pub fn notify_already_running() {
    let msg = nfc_core::i18n::t("dialog_msg_instance_running");
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONWARNING, MB_OK};
        let wide: Vec<u16> = msg.encode_utf16().chain(std::iter::once(0)).collect();
        let title: Vec<u16> = "NFCToolsGUI"
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        // windows-sys 0.52 的 HWND 是 isize（不是指针），空句柄 = 0。
        // SAFETY: 两个宽字符串均以 NUL 结尾；消息框是模态阻塞调用。
        unsafe {
            MessageBoxW(0, wide.as_ptr(), title.as_ptr(), MB_OK | MB_ICONWARNING);
        }
    }
    #[cfg(not(windows))]
    {
        eprintln!("{msg}");
    }
}

#[cfg(unix)]
fn try_lock_exclusive(file: &fs::File) -> bool {
    use std::os::unix::io::AsRawFd;
    // SAFETY: flock 只依赖合法 fd；LOCK_NB 使锁被占时立即失败（EWOULDBLOCK）。
    unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) == 0 }
}

#[cfg(windows)]
fn try_lock_exclusive(file: &fs::File) -> bool {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        LockFileEx, LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY,
    };
    // windows-sys 0.52 的 HANDLE 是 isize（不是指针），故从 RawHandle 转换。
    let handle = file.as_raw_handle() as isize;
    let mut overlapped: windows_sys::Win32::System::IO::OVERLAPPED = unsafe { std::mem::zeroed() };
    // SAFETY: handle 合法；锁区间 [0, 2^64-1) 覆盖整个哨兵文件。
    // 同进程第二个句柄同样会被拒（字节范围锁按句柄排他），测试依赖此语义。
    unsafe {
        LockFileEx(
            handle,
            LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
            0,
            u32::MAX,
            u32::MAX,
            &mut overlapped,
        ) != 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_acquire_is_rejected_while_first_holds() {
        let path = std::env::temp_dir().join(format!(
            "nfcgui-instance-lock-test-{}.lock",
            std::process::id()
        ));
        let _guard = InstanceLock::acquire(&path).expect("首实例应拿到锁");
        assert!(
            matches!(InstanceLock::acquire(&path), Ok(None)),
            "锁被持有时第二实例必须拿不到"
        );
        let _ = fs::remove_file(&path);
    }
}
