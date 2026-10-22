//! 卡记录 registry（`registry.json`）。对标 `docs/01 §6` 决策 #8 的最小实现。
//!
//! 原版**没有**这个文件——它是本重写在规格里新增的轻量功能，用于把读过的卡
//! 累积成一张可查表。因此无原版 JS 对照可依，语义完全按 docs/01 约定实现。
//!
//! 文件格式：`{"registry":[{uid,bcc,serial,key_a,key_b,status,date}]}`
//!
//! 偏离登记见 docs/02 §6。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::paths::Paths;

/// 一张卡的记录。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CardRecord {
    /// 7 或 10 字节 hex，原样保存（不做归一化）
    pub uid: String,
    /// BCC 校验字节，可为空
    #[serde(default)]
    pub bcc: String,
    /// 序列号，可为空
    #[serde(default)]
    pub serial: String,
    /// 已知 Key A，可为空
    #[serde(default)]
    pub key_a: String,
    /// 已知 Key B，可为空
    #[serde(default)]
    pub key_b: String,
    /// 自由文本状态
    #[serde(default)]
    pub status: String,
    /// ISO 日期或时间戳字符串
    #[serde(default)]
    pub date: String,
}

impl CardRecord {
    /// 以 UID 构造，日期留空由调用方填。
    pub fn new(uid: impl Into<String>) -> Self {
        Self {
            uid: uid.into(),
            ..Self::default()
        }
    }

    /// 该记录是否已有任何密钥信息。
    pub fn has_keys(&self) -> bool {
        !self.key_a.is_empty() || !self.key_b.is_empty()
    }
}

/// registry 文件的顶层结构。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Registry {
    #[serde(default)]
    pub registry: Vec<CardRecord>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    /// 读文件；不存在时返回空 registry（首次运行）。
    pub fn load(path: &Path) -> Result<Self, RegistryError> {
        match std::fs::read_to_string(path) {
            Ok(s) => match serde_json::from_str(&s) {
                Ok(r) => Ok(r),
                Err(e) => {
                    // 隔离损坏文件，否则用户无法自行恢复（docs/01 §6 决策 #8 的可查表直接失效）
                    let _ = std::fs::rename(path, PathBuf::from(format!("{}.bad", path.display())));
                    Err(RegistryError::Parse(e))
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::new()),
            Err(source) => Err(RegistryError::Io {
                path: path.into(),
                source,
            }),
        }
    }

    /// 原子写：先写临时文件再 rename，避免中途崩溃留下半个 JSON。
    pub fn save(&self, path: &Path) -> Result<(), RegistryError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| RegistryError::Io {
                path: parent.into(),
                source,
            })?;
        }
        let text = serde_json::to_string_pretty(self).map_err(RegistryError::Parse)?;
        // 显式追加后缀：with_extension 会把 .json 替换掉，registry_file()
        // 一旦改名就出错（与 model/keys.rs 的 save_keys 同一策略）
        let tmp = PathBuf::from(format!("{}.tmp", path.display()));
        std::fs::write(&tmp, text.as_bytes()).map_err(|source| RegistryError::Io {
            path: tmp.clone(),
            source,
        })?;
        // registry 记录含 key_a/key_b 明文，落盘前把 tmp 收紧为 0600——
        // rename 保留 mode，必须在 rename 之前收紧（与 model/keys.rs::save_keys
        // 同一理由：常见 umask 022 下的 0644 会同机其他用户可读跨卡密钥）。
        crate::paths::restrict_owner(&tmp);
        std::fs::rename(&tmp, path).map_err(|source| RegistryError::Io {
            path: path.into(),
            source,
        })
    }

    /// 按 UID 找记录，**ASCII 大小写不敏感**。
    ///
    /// UID 来源（`parse.rs` 的 `take_hex_after`）保留工具输出的原始大小写；
    /// mfoc 与 mfdetect 的输出大小写习惯不同，同一张卡会落两条记录。
    /// 沿用 `keys.rs::is_default_key` 已确立的大小写折叠先例。
    pub fn find(&self, uid: &str) -> Option<&CardRecord> {
        self.registry
            .iter()
            .find(|r| r.uid.eq_ignore_ascii_case(uid))
    }

    /// 已有的 UID 集合。
    pub fn uids(&self) -> Vec<&str> {
        self.registry.iter().map(|r| r.uid.as_str()).collect()
    }

    /// 追加新卡。UID 已存在则**不**重复追加，返回 `false`。
    pub fn append(&mut self, rec: CardRecord) -> bool {
        if self.find(&rec.uid).is_some() {
            return false;
        }
        self.registry.push(rec);
        true
    }

    /// 追加并落盘，返回是否真的新增。
    ///
    /// 落盘失败时回滚内存里的 push——否则调用方拿到 `Err` 时内存已多一条、
    /// 磁盘没有，重试语义含混。
    pub fn append_and_save(
        &mut self,
        rec: CardRecord,
        paths: &Paths,
    ) -> Result<bool, RegistryError> {
        if !self.append(rec) {
            return Ok(false);
        }
        if let Err(e) = self.save(&paths.registry_file()) {
            self.registry.pop();
            return Err(e);
        }
        Ok(true)
    }

    /// 记录总数。
    pub fn len(&self) -> usize {
        self.registry.len()
    }

    pub fn is_empty(&self) -> bool {
        self.registry.is_empty()
    }
}

/// registry 读写错误。
#[derive(Debug, Error)]
pub enum RegistryError {
    #[error("registry 文件 {path} 读写失败：{source}")]
    Io {
        path: std::path::PathBuf,
        source: std::io::Error,
    },
    #[error("registry JSON 解析失败：{0}")]
    Parse(#[from] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 只清理自己那一个目录。
    ///
    /// 早期版本是无字段的 `TempGuard`，drop 时遍历全局 `DIRS` 全删——但
    /// libtest 默认多线程并行，任一测试结束就会删掉**别的**正在跑的测试的
    /// 临时目录，表现为 `NotFound` 这类与被测逻辑毫无关系的随机失败。
    struct TempGuard(PathBuf);

    impl Drop for TempGuard {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn tempdir(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("nfc-s5-reg-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn missing_file_loads_empty() {
        let dir = tempdir("load-missing");
        let _guard = TempGuard(dir.clone());
        let r = Registry::load(&dir.join("registry.json")).unwrap();
        assert!(r.is_empty());
        assert_eq!(r.len(), 0);
    }

    #[test]
    fn save_then_load_roundtrip() {
        let dir = tempdir("roundtrip");
        let _guard = TempGuard(dir.clone());
        let p = dir.join("registry.json");
        let mut r = Registry::new();
        assert!(r.append(CardRecord {
            uid: "112233445566".into(),
            bcc: "77".into(),
            serial: "SN001".into(),
            key_a: "ffffffffffff".into(),
            key_b: "a0a1a2a3a4a5".into(),
            status: "已解密".into(),
            date: "2024-01-02_03_04_05".into(),
        }));
        r.save(&p).unwrap();
        let back = Registry::load(&p).unwrap();
        assert_eq!(back, r);
        assert_eq!(back.registry[0].key_a, "ffffffffffff");
    }

    #[test]
    fn append_dedups_by_uid() {
        let mut r = Registry::new();
        assert!(r.append(CardRecord::new("aabb")));
        assert!(!r.append(CardRecord::new("aabb")), "同 UID 不应重复");
        assert_eq!(r.len(), 1);
        assert_eq!(r.uids(), vec!["aabb"]);
    }

    #[test]
    fn find_locates_record() {
        let mut r = Registry::new();
        r.append(CardRecord::new("x1"));
        r.append(CardRecord::new("x2"));
        assert!(r.find("x2").is_some());
        assert!(r.find("nope").is_none());
    }

    #[test]
    fn has_keys_reflects_key_fields() {
        let mut c = CardRecord::new("u");
        assert!(!c.has_keys());
        c.key_b = "ffffffffffff".into();
        assert!(c.has_keys());
    }

    #[test]
    fn partial_json_is_tolerated() {
        // 只有 uid 的记录应能解析，缺失字段走 serde default
        let dir = tempdir("partial");
        let _guard = TempGuard(dir.clone());
        let p = dir.join("registry.json");
        std::fs::write(&p, r#"{"registry":[{"uid":"deadbeef"}]}"#).unwrap();
        let r = Registry::load(&p).unwrap();
        assert_eq!(r.len(), 1);
        assert_eq!(r.registry[0].uid, "deadbeef");
        assert_eq!(r.registry[0].bcc, "");
        assert!(!r.registry[0].has_keys());
    }

    #[test]
    fn empty_registry_object_parses() {
        let dir = tempdir("empty-obj");
        let _guard = TempGuard(dir.clone());
        let p = dir.join("registry.json");
        std::fs::write(&p, "{}").unwrap();
        let r = Registry::load(&p).unwrap();
        assert!(r.is_empty());
    }

    #[test]
    fn malformed_json_reports_parse_error() {
        let dir = tempdir("bad");
        let _guard = TempGuard(dir.clone());
        let p = dir.join("registry.json");
        std::fs::write(&p, "{ not json").unwrap();
        let e = Registry::load(&p).unwrap_err();
        assert!(matches!(e, RegistryError::Parse(_)), "实际: {e:?}");
    }

    #[test]
    fn save_creates_parent_dir() {
        let dir = tempdir("mkdir");
        let _guard = TempGuard(dir.clone());
        let p = dir.join("deep").join("nested").join("registry.json");
        let r = Registry::new();
        r.save(&p).unwrap();
        assert!(p.exists());
    }

    #[test]
    fn save_leaves_no_temp_file() {
        let dir = tempdir("notmp");
        let _guard = TempGuard(dir.clone());
        let p = dir.join("registry.json");
        Registry::new().save(&p).unwrap();
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "残留临时文件: {leftovers:?}");
        assert!(p.exists(), "目标文件应存在");
    }

    /// registry 记录含 key_a/key_b 明文，落盘后必须收紧为 0600。
    ///
    /// 常见 umask 022 下 `fs::write` 得 0644，同机其他用户可读跨卡可复用的
    /// 密钥——与 keys.txt（`model/keys.rs`）属同一防线，此处补齐 registry。
    #[cfg(unix)]
    #[test]
    fn save_restricts_registry_file_mode() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempdir("perm");
        let _guard = TempGuard(dir.clone());
        let p = dir.join("registry.json");
        let mut r = Registry::new();
        r.append(CardRecord::new("aabbccddeeff"));
        r.save(&p).unwrap();
        let mode = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "实际 mode: {mode:o}");
    }

    /// UID 大小写不敏感去重：mfoc 与 mfdetect 的输出大小写习惯不同，
    /// 同一张卡不应因此落两条记录。
    #[test]
    fn find_is_case_insensitive_on_uid() {
        let mut r = Registry::new();
        r.append(CardRecord::new("AABBCCDDEEFF"));
        assert!(r.find("aabbccddeeff").is_some());
        assert!(r.find("AABBCCDDEEFF").is_some());
        assert!(r.find("AaBbCcDdEeFf").is_some());
        // 去重同样按大小写折叠
        assert!(!r.append(CardRecord::new("aabbccddeeff")));
        assert_eq!(r.len(), 1);
    }

    /// 损坏的 registry.json 应被隔离成 .bad，让用户能自行恢复。
    #[test]
    fn malformed_json_is_quarantined_as_bad() {
        let dir = tempdir("quarantine");
        let p = dir.join("registry.json");
        std::fs::write(&p, "{ broken").unwrap();
        let e = Registry::load(&p).unwrap_err();
        assert!(matches!(e, RegistryError::Parse(_)));
        assert!(!p.exists(), "坏文件应被移走");
        assert!(
            dir.join("registry.json.bad").exists(),
            "应留下 .bad 供人工排查"
        );
    }

    /// 落盘失败时内存不应留下未持久化的记录。
    #[test]
    fn append_and_save_rolls_back_on_save_failure() {
        let dir = tempdir("rollback");
        let paths = Paths::new(&dir);
        let mut r = Registry::new();
        // 把 registry_file 指向一个已存在的目录，save 必然失败
        std::fs::create_dir_all(paths.registry_file()).unwrap();
        let e = r.append_and_save(CardRecord::new("u1"), &paths);
        assert!(e.is_err(), "应失败，实际: {e:?}");
        assert!(r.is_empty(), "失败后内存不应残留记录");
    }

    #[test]
    fn append_and_save_creates_then_dedups() {
        let dir = tempdir("append-save");
        let _guard = TempGuard(dir.clone());
        let paths = Paths::new(&dir);
        let mut r = Registry::new();
        assert!(r.append_and_save(CardRecord::new("u1"), &paths).unwrap());
        assert!(paths.registry_file().exists());
        // 第二次同 UID：不写盘但也不报错
        assert!(!r.append_and_save(CardRecord::new("u1"), &paths).unwrap());
        assert_eq!(r.len(), 1);
        let back = Registry::load(&paths.registry_file()).unwrap();
        assert_eq!(back.len(), 1);
    }

    #[test]
    fn save_then_reload_preserves_multiple_records() {
        let dir = tempdir("multi");
        let _guard = TempGuard(dir.clone());
        let p = dir.join("registry.json");
        let mut r = Registry::new();
        for i in 0..5 {
            r.append(CardRecord {
                uid: format!("uid{i:02}"),
                date: format!("2024-01-0{}", i + 1),
                ..CardRecord::default()
            });
        }
        r.save(&p).unwrap();
        let back = Registry::load(&p).unwrap();
        assert_eq!(back.len(), 5);
        assert_eq!(back.uids()[4], "uid04");
        assert_eq!(back.registry[3].date, "2024-01-04");
    }
}
