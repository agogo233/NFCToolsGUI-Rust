//! `dumpfiles/` 目录的文件操作，对标 `command.js` 的 `dump-history` /
//! `delete-dump` / `rename-dump-file` / `updateDumpFiles`。
//!
//! 只做纯文件操作，不弹任何对话框——确认框由 app 层经
//! `DialogProvider::confirm_danger` 发起，与 `tasks/*` 的分层一致。
//! [`delete_confirm`] 只负责**构造**确认请求，不负责询问。

use std::path::{Path, PathBuf};

use crate::dialogs::ConfirmRequest;
use crate::i18n;

/// 原版只列 `.mfd`（`command.js:500` `filter(file => file.endsWith('.mfd'))`）。
pub const DUMP_EXT: &str = "mfd";

/// 删除转储的确认框，对标 `command.js:507-513` 的 `showMessageBoxSync`。
///
/// `message` 取 `i18n("dialog_msg_are_you_sure_to_delete_these_dumps")`，
/// 待删文件名列表由原版的 `detail: files.join('\n')` 承载——`ConfirmRequest`
/// 没有单独的 detail 字段，故拼在 message 末尾换行。
pub fn delete_confirm(names: &[String]) -> ConfirmRequest {
    let mut message = i18n::t("dialog_msg_are_you_sure_to_delete_these_dumps");
    if !names.is_empty() {
        message.push('\n');
        message.push_str(&names.join("\n"));
    }
    ConfirmRequest {
        title: i18n::t("dialog_title_danger_operation"),
        message,
    }
}

/// 列出 dump 目录下的 `.mfd` 文件名（不含路径），按文件名升序。
///
/// 对标 `command.js:499` `fs.readdirSync(dumpsFolder).filter(...endsWith('.mfd'))`。
///
/// 排序说明：原版 `readdirSync` 的返回顺序由文件系统决定（多数实现是
/// 目录项顺序，非字典序），UI 每次刷新顺序可能跳变。此处显式按文件名排序，
/// 属确定性改进，登记于 docs/02 §6.9。
///
/// 目录不存在时返回空 vec——原版 `fs.mkdir(dumpsFolder, ...)` 先建目录，
/// 此处不建（建目录是 app 启动时的职责，见 `main.rs`），缺失即当空。
pub fn list(dir: &Path) -> Vec<String> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = rd
        .filter_map(|e| e.ok())
        // 只取文件，跳过子目录（原版 readdirSync 也会把目录名混进来，
        // 但那些名字不以 .mfd 结尾，故实际被 filter 排除）
        .filter(|e| e.path().is_file())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(DUMP_EXT))
        .collect();
    names.sort();
    names
}

/// 删除若干 dump 文件，返回成功删除的名字。
///
/// 对标 `command.js:515-520` 的 `files.forEach(file => fs.unlink(...))`。
///
/// 原版每个 `unlink` 失败就 `throw`，且用异步回调，故实际是"部分删除后抛"。
/// 此处改为逐个尝试、跳过失败项并把成功项返回——调用方据此刷新列表，
/// 语义上更接近原版的最终状态（`updateDumpFiles()` 在每个成功回调里调用）。
///
/// **路径安全**：`name` 若含分隔符或 `..`，一律拒绝。这比原版多一道防线：
/// 原版 `path.join(dumpsFolder, file)` 在 `file` 为绝对路径时会被 Node
/// 直接丢弃 `dumpsFolder`，等于可以删任意路径下的文件。
pub fn delete(dir: &Path, names: &[String]) -> Vec<String> {
    names
        .iter()
        .filter(|n| is_plain_name(n))
        .filter_map(|n| {
            std::fs::remove_file(dir.join(n))
                .ok()
                .map(|()| (*n).clone())
        })
        .collect()
}

/// 重命名一个 dump 文件（仍在同一目录内）。
///
/// 对标 `command.js:526` 的 `fs.rename(join(dir, old), join(dir, new))`。
/// 同样拒绝含分隔符的名字，理由见 [`delete`]。
pub fn rename(dir: &Path, old: &str, new: &str) -> Result<PathBuf, DumpFileError> {
    if !is_plain_name(old) {
        return Err(DumpFileError::BadName(old.to_owned()));
    }
    if !is_plain_name(new) {
        return Err(DumpFileError::BadName(new.to_owned()));
    }
    let (from, to) = (dir.join(old), dir.join(new));
    std::fs::rename(&from, &to).map_err(|e| DumpFileError::Io {
        name: old.to_owned(),
        source: e,
    })?;
    Ok(to)
}

/// 文件名必须是单一文件名：非空、不含路径分隔符、不是 `.` / `..`。
///
/// 除本模块的 [`delete`] / [`rename`] 外，app 侧编辑器载入、比较器两侧
/// 载入、编辑器保存写回也用它做入口防线（纵深防御：所有把用户可见文件名
/// 拼到 dumpfiles 下的入口都过同一道校验）。
pub fn is_plain_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains('/')
        && !name.contains('\\')
        && !name.contains('\0')
}

#[derive(Debug)]
pub enum DumpFileError {
    /// 名字含路径分隔符或为空，拒绝操作。
    BadName(String),
    Io {
        name: String,
        source: std::io::Error,
    },
}

impl std::fmt::Display for DumpFileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DumpFileError::BadName(n) => write!(f, "非法文件名: {n}"),
            DumpFileError::Io { name, source } => write!(f, "{name}: {source}"),
        }
    }
}

impl std::error::Error for DumpFileError {}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试自持临时目录：不用全局表，避免 libtest 多线程下互删
    /// （S8 曾因此把 `data.rs` / `mfoc.rs` 的 TempGuard 改成自持）。
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let p = std::env::temp_dir().join(format!(
                "nfc-dumpfiles-{tag}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).unwrap();
            Self(p)
        }
        fn write(&self, name: &str) {
            std::fs::write(self.0.join(name), b"x").unwrap();
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn list_only_returns_mfd_sorted() {
        let d = TempDir::new("list");
        for n in ["b.mfd", "a.mfd", "c.dump", "notes.txt", "d.mfd"] {
            d.write(n);
        }
        // 子目录即使叫 x.mfd 也不该被当成 dump（原版 readdirSync 会混进来，
        // 但本实现显式跳过目录——更安全）
        std::fs::create_dir_all(d.path().join("sub.mfd")).unwrap();
        assert_eq!(list(d.path()), vec!["a.mfd", "b.mfd", "d.mfd"]);
    }

    #[test]
    fn list_missing_dir_is_empty_not_error() {
        let p = std::env::temp_dir().join("nfc-dumpfiles-nonexistent-xyz");
        assert!(list(&p).is_empty());
    }

    #[test]
    fn delete_removes_and_reports() {
        let d = TempDir::new("del");
        d.write("a.mfd");
        d.write("b.mfd");
        let got = delete(d.path(), &["a.mfd".to_owned(), "b.mfd".to_owned()]);
        assert_eq!(got, vec!["a.mfd", "b.mfd"]);
        assert!(list(d.path()).is_empty());
    }

    #[test]
    fn delete_skips_missing_and_bad_names() {
        let d = TempDir::new("del2");
        d.write("a.mfd");
        // 不存在的文件不该报错，只是没进成功列表
        let got = delete(d.path(), &["a.mfd".to_owned(), "ghost.mfd".to_owned()]);
        assert_eq!(got, vec!["a.mfd"]);
        // 路径穿越必须被拒
        let got = delete(
            d.path(),
            &["../../etc/passwd".to_owned(), "a/../b.mfd".to_owned()],
        );
        assert!(got.is_empty(), "路径穿越竟被接受: {got:?}");
    }

    #[test]
    fn rename_moves_file() {
        let d = TempDir::new("ren");
        d.write("old.mfd");
        let to = rename(d.path(), "old.mfd", "new.mfd").unwrap();
        assert_eq!(to, d.path().join("new.mfd"));
        assert_eq!(list(d.path()), vec!["new.mfd"]);
    }

    #[test]
    fn rename_rejects_traversal() {
        let d = TempDir::new("ren2");
        d.write("a.mfd");
        assert!(matches!(
            rename(d.path(), "a.mfd", "../evil.mfd"),
            Err(DumpFileError::BadName(_))
        ));
        assert!(matches!(
            rename(d.path(), "../evil.mfd", "b.mfd"),
            Err(DumpFileError::BadName(_))
        ));
        // 拒绝后原文件应还在
        assert_eq!(list(d.path()), vec!["a.mfd"]);
    }

    #[test]
    fn rename_missing_source_is_io_error() {
        let d = TempDir::new("ren3");
        assert!(matches!(
            rename(d.path(), "ghost.mfd", "x.mfd"),
            Err(DumpFileError::Io { .. })
        ));
    }

    #[test]
    fn is_plain_name_rejects_edge_cases() {
        assert!(is_plain_name("a.mfd"));
        assert!(!is_plain_name(""));
        assert!(!is_plain_name("."));
        assert!(!is_plain_name(".."));
        assert!(!is_plain_name("a/b"));
        assert!(!is_plain_name("a\\b"));
        assert!(!is_plain_name("a\0b"));
        // 非 ASCII 名字允许（原版 dump 文件名含 UID，仍是 ASCII，
        // 但用户可能手动改，故不额外设限）
        assert!(is_plain_name("中文.mfd"));
    }
}
