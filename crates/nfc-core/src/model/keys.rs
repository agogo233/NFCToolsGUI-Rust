//! 密钥数据模型。对标 `command.js:50-64`（默认密钥表）与 `command.js:566-580`（`saveKeys`）。
//!
//! 偏离登记见 docs/02 §6。

use std::fs;
use std::path::PathBuf;
use std::sync::OnceLock;

use regex::Regex;
use thiserror::Error;

use crate::paths::Paths;

/// 内置默认密钥，13 个**小写 12-hex 裸串**，逐字对标 `command.js:50-64`。
///
/// 两点真相：
/// - 原版此处**不含类型名**，不是 `{type, value}` 结构。
/// - 这 13 把**从未传给 mfoc**（mfoc 有自己的内置表），只用于 ETA 分母估算
///   （`command.js:633`）与本模块的「不写入 keys.txt」剔除集合。
pub const DEFAULT_KEYS: [&str; 13] = [
    "ffffffffffff",
    "a0a1a2a3a4a5",
    "d3f7d3f7d3f7",
    "000000000000",
    "b0b1b2b3b4b5",
    "4d3a99c351dd",
    "1a982c7e459a",
    "aabbccddeeff",
    "714c5c886e97",
    "587ee5f9350f",
    "a0478cc39091",
    "533cb6c723f6",
    "8fd0a4f256e9",
];

/// 块内密钥类型，取值全集仅 A / B。
///
/// 来源：mfdetect/mfoc 行的 `matchStr[13]`（`command.js:738,740`）与
/// `hardNested.html:26-31` 的 radio `value="A"` / `value="B"`。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum KeyType {
    #[default]
    A,
    B,
}

impl KeyType {
    pub fn as_str(self) -> &'static str {
        match self {
            KeyType::A => "A",
            KeyType::B => "B",
        }
    }

    /// 单字符形式，供 mfoc / nfc-mfcollect 的位置参数使用。
    pub fn as_char(self) -> char {
        match self {
            KeyType::A => 'A',
            KeyType::B => 'B',
        }
    }

    /// 原版不校验直接落库（`command.js:738`），本实现对非 A/B 返回 `None`。
    pub fn from_char(c: char) -> Option<Self> {
        match c {
            'A' => Some(KeyType::A),
            'B' => Some(KeyType::B),
            _ => None,
        }
    }

    /// 严格解析：非 A/B 返回 `None`。
    pub fn parse(s: &str) -> Option<Self> {
        s.chars().next().and_then(KeyType::from_char)
    }

    /// 容错解析：非 `B` 一律视为 `A`。对标原版「只有 B 与其它」的两分口径。
    pub fn parse_lenient(s: &str) -> Self {
        match s.trim().to_ascii_uppercase().as_str() {
            "B" => KeyType::B,
            _ => KeyType::A,
        }
    }
}

/// 已破解的扇区密钥，对标 `knownKeyInfo` 的三元组 `[key, sector, type]`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyInfo {
    pub key: String,
    pub sector: u8,
    pub key_type: KeyType,
}

/// 未破解的扇区密钥，对标 `unknownKeyInfo` 的二元组 `[sector, type]`（原版不含 key 字段）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnknownKeyInfo {
    pub sector: u8,
    pub key_type: KeyType,
}

#[derive(Debug, Error)]
pub enum KeysError {
    #[error("密钥文件 {path} 操作失败：{source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
}

/// 复刻 `command.js:568` 的 `/[0-9A-Fa-f]{12}/g`：在**任意位置**非重叠扫描 12 位 hex。
///
/// 因不按行切分，`"ff ff ff ff ff ff"`（含空格）与 `"ffffffffffffffffff"`（14 个 f，
/// 抽出 1 个、剩余 2 个丢弃）都能得到结果，与原版一致。
pub fn parse_12hex(text: &str) -> Vec<String> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"[0-9A-Fa-f]{12}").expect("静态正则"));
    re.find_iter(text).map(|m| m.as_str().to_string()).collect()
}

/// 是否为默认密钥。
///
/// 偏离（原版 bug #6，`command.js:571-573`）：原版用 `indexOf` 与全小写的
/// `defaultKeys` 比对，工具/文件产出的大写 `FFFFFFFFFFFF` 剔不掉，会被写进
/// `keys.txt` 并让 mfoc 白跑 13 把。本实现按 ASCII 大小写折叠比对。
pub fn is_default_key(key: &str) -> bool {
    DEFAULT_KEYS.iter().any(|d| d.eq_ignore_ascii_case(key))
}

/// 合并去重并剔除默认密钥。纯函数，便于穷举单测。
///
/// 顺序语义对标 `command.js:570`：`new Set(knownKeys.concat(keys))` 保持首次出现
/// 顺序（旧文件在前、新 key 在后）。去重本身**仍按原版大小写敏感**。
pub fn merge_keys(existing: &[String], incoming: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    // Vec 保插入序 + HashSet 保 O(1) 去重；keys.txt 长期累积可达数千条，
    // save_keys 在 autoRun 循环里每命中一次就全量重扫
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for k in existing.iter().chain(incoming.iter()) {
        if seen.insert(k.as_str()) {
            out.push(k.clone());
        }
    }
    out.retain(|k| !is_default_key(k));
    out
}

/// 合并写入 `keys.txt`，返回本次**新增**的密钥（供事件条展示）。
///
/// 写回格式对标 `command.js:575` 的 `keys.join("\n")`：换行分隔裸 12-hex、**无结尾换行**。
///
/// 偏离（原版 bug #12）：原版 `writeFileSync` 直写，autoRun 循环中高频调用
/// （`command.js:345` 每命中一次就调一次）可能留下半截文件。本实现先写同目录
/// 临时文件再 `rename` 原子替换。
///
/// 偏离（原版 bug #14 的一部分，`command.js:631`）：原版 `readFileSync(knownKeysFile)`
/// 无 try/catch，文件丢失即崩，靠 `checkKeyFileExist()`（`command.js:746-749`）预建兜底。
/// 本实现把「缺失即空集」内联进来，调用方不必再预建。
pub fn save_keys(paths: &Paths, incoming: &[String]) -> Result<Vec<String>, KeysError> {
    let path = paths.keys_file();
    // 原版靠 Electron 保证 userData 存在；Rust 侧 Paths 不建目录，这里补上
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| KeysError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let existing = match fs::read_to_string(&path) {
        Ok(text) => parse_12hex(&text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(source) => return Err(KeysError::Io { path, source }),
    };

    let merged = merge_keys(&existing, incoming);
    // 显式追加后缀：with_extension 会把 .txt 替换掉，keys_file() 一旦改名就出错
    let tmp = PathBuf::from(format!("{}.tmp", path.display()));
    fs::write(&tmp, merged.join("\n")).map_err(|source| KeysError::Io {
        path: tmp.clone(),
        source,
    })?;
    fs::rename(&tmp, &path).map_err(|source| KeysError::Io { path, source })?;

    Ok(merged
        .iter()
        .filter(|k| !existing.contains(*k))
        .cloned()
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_keys_match_original_13_entries_verbatim() {
        assert_eq!(DEFAULT_KEYS.len(), 13);
        assert_eq!(DEFAULT_KEYS[0], "ffffffffffff");
        assert_eq!(DEFAULT_KEYS[1], "a0a1a2a3a4a5");
        assert_eq!(DEFAULT_KEYS[2], "d3f7d3f7d3f7");
        assert_eq!(DEFAULT_KEYS[3], "000000000000");
        assert_eq!(DEFAULT_KEYS[12], "8fd0a4f256e9");
        assert!(DEFAULT_KEYS.iter().all(|k| k.len() == 12));
        // 原版 defaultKeys 全小写（command.js:571）。注意断言要写成
        // "无大写字母"而非 `is_ascii_lowercase`——后者对数字也返回 false，
        // 会把 000000000000 这类纯数字 key 误判为失败。
        assert!(DEFAULT_KEYS
            .iter()
            .all(|k| k.chars().all(|c| !c.is_ascii_uppercase())));
        assert!(DEFAULT_KEYS
            .iter()
            .all(|k| k.chars().all(|c| c.is_ascii_hexdigit())));
    }

    #[test]
    fn key_type_roundtrip_and_rejects_others() {
        assert_eq!(KeyType::parse("A"), Some(KeyType::A));
        assert_eq!(KeyType::parse("B"), Some(KeyType::B));
        assert_eq!(KeyType::A.as_str(), "A");
        assert_eq!(KeyType::B.as_str(), "B");
        assert_eq!(KeyType::parse("C"), None);
        assert_eq!(KeyType::parse(""), None);
        assert_eq!(KeyType::from_char(' '), None);
    }

    #[test]
    fn parse_12hex_scans_anywhere_not_line_based() {
        // 原版 command.js:568 —— fs.readFileSync(f).toString().match(/[0-9A-Fa-f]{12}/g)
        assert_eq!(parse_12hex("a1b2c3d4e5f6"), vec!["a1b2c3d4e5f6"]);
        assert_eq!(parse_12hex("前缀a1b2c3d4e5f6后缀"), vec!["a1b2c3d4e5f6"]);
        // 正则不跨分隔符：空格分隔的 6 段各 2 hex 凑不出 12 连串
        assert_eq!(parse_12hex("ff ff ff ff ff ff"), Vec::<String>::new());
        // 保留原始大小写（不做 toLowerCase），与原版 match 语义一致
        assert_eq!(parse_12hex("A1B2C3D4E5F6"), vec!["A1B2C3D4E5F6"]);
        // 13 位只取前 12（正则贪婪但不越界）
        assert_eq!(parse_12hex("a1b2c3d4e5f6a7b8"), vec!["a1b2c3d4e5f6"]);
    }

    #[test]
    fn parse_12hex_drops_ragged_tail_like_original() {
        // 14 个 f：抽出 1 个 12-hex，剩余 2 个丢弃
        assert_eq!(parse_12hex("ffffffffffffffffff"), vec!["ffffffffffff"]);
        assert!(parse_12hex("fff").is_empty());
        assert!(parse_12hex("").is_empty());
    }

    #[test]
    fn parse_12hex_keeps_original_case() {
        assert_eq!(parse_12hex("AABBCCDDEEFF"), vec!["AABBCCDDEEFF"]);
        assert_eq!(parse_12hex("aabbccddeeff"), vec!["aabbccddeeff"]);
    }

    #[test]
    fn is_default_key_is_case_insensitive_but_merge_dedup_is_not() {
        assert!(is_default_key("FFFFFFFFFFFF"));
        assert!(is_default_key("A0A1A2A3A4A5"));
        assert!(!is_default_key("deadbeef1234"));
        // 去重保持原版大小写敏感：两种大小写都留。
        // 用非默认密钥对，否则 aabbccddeeff 是 DEFAULT_KEYS[7]，会被 retain 剔掉。
        let merged = merge_keys(&[], &["DEADBEEF1234".into(), "deadbeef1234".into()]);
        assert_eq!(merged, vec!["DEADBEEF1234", "deadbeef1234"]);
    }

    #[test]
    fn merge_strips_defaults_regardless_of_case() {
        let merged = merge_keys(
            &["FFFFFFFFFFFF".into()],
            &["A0A1A2A3A4A5".into(), "deadbeef1234".into()],
        );
        assert_eq!(merged, vec!["deadbeef1234"]);
    }

    #[test]
    fn merge_preserves_first_occurrence_order_old_file_first() {
        let merged = merge_keys(
            &["111111111111".into(), "222222222222".into()],
            &["222222222222".into(), "333333333333".into()],
        );
        assert_eq!(merged, vec!["111111111111", "222222222222", "333333333333"]);
    }

    #[test]
    fn merge_on_empty_inputs_yields_empty() {
        assert!(merge_keys(&[], &[]).is_empty());
    }
}
