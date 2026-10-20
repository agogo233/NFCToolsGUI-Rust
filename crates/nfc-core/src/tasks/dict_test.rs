//! 字典测试。对标 `command.js:359-397`（`open-dict-file` / `dict-test` /
//! `dict-test-config-done`）。
//!
//! 偏离登记见 docs/02 §6。
//!
//! 流程：选字典文件 → `nfc-mfdict -s<扇区> -t<类型> -l<起始位置> -d<字典>` →
//! 解析 `Found Key: ` → 落盘密钥 → 移除队首 unknown。

use std::path::{Path, PathBuf};

use crate::dialogs::{FileFilter, OpenDialogRequest};
use crate::i18n;
use crate::model::keys::KeyType;
use crate::state::{AppState, DictTestConfig, Event};

/// 工具名，对标 `command.js:387`。
pub const TOOL_DICT: &str = "nfc-mfdict";

/// `nfc-mfdict` 的参数，对标 `command.js:388`。
///
/// 四个参数：`-s<扇区>`、`-t<类型>`、`-l<起始位置>`、`-d<字典路径>`。
pub fn dict_args(config: &DictTestConfig, dict_path: &Path) -> Vec<String> {
    vec![
        format!("-s{}", config.sector),
        format!("-t{}", config.key_type.as_char()),
        format!("-l{}", config.start_position),
        format!("-d{}", dict_path.display()),
    ]
}

/// 选字典文件的对话框，对标 `command.js:363-371`。
pub fn open_dict_request(current: &Path) -> OpenDialogRequest {
    OpenDialogRequest {
        title: i18n::t("dialog_title_choose_dictionary_file"),
        default_path: current.to_path_buf(),
        button_label: String::new(),
        filters: vec![FileFilter {
            name: i18n::t("file_type_dict"),
            extensions: vec!["txt".to_string(), "dic".to_string()],
        }],
        message: i18n::t("dialog_msg_choose_dictionary_file"),
    }
}

/// 从字典路径取文件名（跨平台分隔符），对标 `command.js:375`。
///
/// 原版 `dictPath.split(/[\/\\]/g)` 后取最后一段。
pub fn dict_file_name(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

/// 从 `paths` 解析出字典的默认配置，对标 `command.js:388-392`。
///
/// 起始位置 `0` 是原版 `DictTestConfig` 的缺省（面板里用户可改）。
pub fn default_config(unknown_head: Option<(u8, KeyType)>) -> DictTestConfig {
    match unknown_head {
        Some((sector, key_type)) => DictTestConfig {
            sector,
            key_type,
            start_position: 0,
        },
        None => DictTestConfig::default(),
    }
}

/// 字典测试的状态条，对标 `command.js:385`。
pub fn status_text() -> String {
    i18n::t("indicator_testing_dictionary")
}

/// 命中后把新密钥落盘，对标 `command.js:389-390` 的 `saveKeys([key])`。
///
/// 返回是否有新密钥需要落盘（`fresh` 非空即有）。
pub fn report_found_key(
    state: &mut AppState,
    key: &str,
    paths: &crate::paths::Paths,
) -> Result<bool, String> {
    let fresh =
        crate::model::keys::save_keys(paths, &[key.to_string()]).map_err(|e| e.to_string())?;
    if !fresh.is_empty() {
        state.apply(Event::EventItem {
            kind: crate::state::EventKind::Key,
            text: format!("{}{}", i18n::t("event_keys_saved"), fresh.join(", ")),
        });
    }
    Ok(!fresh.is_empty())
}

/// `open-dict-file` 的结果：更新路径并回传文件名。
///
/// 对标 `command.js:372-377`：取消则不动状态。
pub fn apply_dict_choice(state: &mut AppState, path: Option<PathBuf>) -> Option<String> {
    let p = path?;
    let name = dict_file_name(&p.to_string_lossy()).to_string();
    state.dict_path = p;
    state.apply(Event::DictFileName(name.clone()));
    Some(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(sector: u8, key_type: KeyType, start: u32) -> DictTestConfig {
        DictTestConfig {
            sector,
            key_type,
            start_position: start,
        }
    }

    // ---------------------------------------------------------- 参数

    #[test]
    fn dict_args_match_original_four_flags() {
        let a = dict_args(&cfg(3, KeyType::B, 0), Path::new("/d/dict.dic"));
        assert_eq!(a.len(), 4);
        assert_eq!(a, vec!["-s3", "-tB", "-l0", "-d/d/dict.dic"]);
    }

    #[test]
    fn dict_args_with_start_position() {
        let a = dict_args(&cfg(15, KeyType::A, 4096), Path::new("/x.dic"));
        assert_eq!(a[2], "-l4096");
        assert_eq!(a[0], "-s15");
        assert_eq!(a[1], "-tA");
    }

    #[test]
    fn dict_args_tolerates_spaces_in_path() {
        // 参数是单字符串，路径含空格时由 exec 层负责传参，不在此处拆分
        let a = dict_args(&cfg(0, KeyType::A, 0), Path::new("/my dict/d.dic"));
        assert_eq!(a[3], "-d/my dict/d.dic");
    }

    // ---------------------------------------------------------- 文件名

    #[test]
    fn dict_file_name_splits_both_separators() {
        assert_eq!(dict_file_name("/a/b/c.dic"), "c.dic");
        assert_eq!(dict_file_name("a\\b\\c.dic"), "c.dic");
        assert_eq!(dict_file_name("c.dic"), "c.dic");
        assert_eq!(dict_file_name("/a/b/"), "");
    }

    // ---------------------------------------------------------- 配置

    #[test]
    fn default_config_from_unknown_head() {
        let c = default_config(Some((5, KeyType::A)));
        assert_eq!(c.sector, 5);
        assert_eq!(c.key_type, KeyType::A);
        assert_eq!(c.start_position, 0);
    }

    #[test]
    fn default_config_without_unknown_is_zeroed() {
        let c = default_config(None);
        assert_eq!(c, DictTestConfig::default());
        assert_eq!(c.start_position, 0);
    }

    // ---------------------------------------------------------- 对话框

    #[test]
    fn open_dict_request_uses_dict_filter() {
        let req = open_dict_request(Path::new("/current/dict.dic"));
        assert_eq!(req.default_path, PathBuf::from("/current/dict.dic"));
        assert_eq!(req.filters[0].name, "字典文件");
        assert_eq!(req.filters[0].extensions, vec!["txt", "dic"]);
        assert_eq!(req.title, "选择字典文件");
    }

    // ---------------------------------------------------------- 选文件

    #[test]
    fn apply_dict_choice_updates_path_and_pushes_name() {
        // 只需一个 PathBuf，无需真的建目录
        let picked = std::env::temp_dir()
            .join("nfc-s7-dict-choice")
            .join("my.dic");
        let mut s = AppState::new();
        let name = apply_dict_choice(&mut s, Some(picked.clone())).expect("应返回文件名");
        assert_eq!(name, "my.dic");
        assert_eq!(s.dict_path, picked);
        // DictFileName 是 UI-only 事件，但 dict_path 必须落状态
        assert_eq!(s.dict_path.file_name().unwrap(), "my.dic");
    }

    #[test]
    fn apply_dict_choice_cancel_keeps_old_path() {
        let mut s = AppState::new();
        let old = PathBuf::from("/old/dict.dic");
        s.dict_path = old.clone();
        assert_eq!(apply_dict_choice(&mut s, None), None);
        assert_eq!(s.dict_path, old, "取消不应改路径");
    }

    // ---------------------------------------------------------- 落盘

    #[test]
    fn report_found_key_saves_to_keys_file() {
        let dir = std::env::temp_dir().join(format!("nfc-s7-dt2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let paths = crate::paths::Paths::new(&dir);
        let mut s = AppState::new();
        let fresh = report_found_key(&mut s, "deadbeef1234", &paths).unwrap();
        assert!(fresh);
        let saved = std::fs::read_to_string(paths.keys_file()).unwrap();
        assert!(saved.contains("deadbeef1234"), "实际: {saved:?}");
        assert!(s
            .events
            .iter()
            .any(|e| e.kind == crate::state::EventKind::Key));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 默认密钥命中时不落盘，也不推事件（`save_keys` 内部已剔除）。
    #[test]
    fn report_found_key_skips_default_keys() {
        let dir = std::env::temp_dir().join(format!("nfc-s7-dt3-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let paths = crate::paths::Paths::new(&dir);
        let mut s = AppState::new();
        let fresh = report_found_key(&mut s, "ffffffffffff", &paths).unwrap();
        assert!(!fresh, "默认密钥应被剔除");
        assert!(s.events.is_empty());
        let saved = std::fs::read_to_string(paths.keys_file()).unwrap();
        assert!(!saved.contains("ffffffffffff"), "实际: {saved:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn status_text_uses_original_wording() {
        assert_eq!(status_text(), "正在进行字典测试");
    }
}
