//! 读卡闭环编排。对标 `command.js:583-616`（`readICThenExec`）与
//! `command.js:162-182`（`detect-card-type`）。
//!
//! 偏离登记见 docs/02 §6。
//!
//! 与 mfoc.rs 的分工：mfoc.rs 负责「解卡 + ETA + 落盘」，本模块负责
//! 「先 nfc-mfdetect 探测 → 再执行主命令」的两段式串联。
//!
//! **两条路径必须分开**，这是原版的真实语义（易错点）：
//! - [`after_detect`]：对应 `readICThenExec`，供 write-IC / format-card /
//!   auto hard-nested 使用。无论探测出多少 unknown 密钥，**都继续跑主命令**。
//! - [`guidance_for_unknown`]：对应 `detect-card-type`，**只有**这条路径在
//!   unknown 非空时弹引导。把它合进 `after_detect` 会让写卡/格式化永远无法执行。

use std::path::Path;

use crate::i18n;
use crate::model::keys::save_keys;
use crate::model::parse::KeyInfoParser;
use crate::paths::Paths;
use crate::state::{AppState, Event};

/// 探测阶段的参数，对标 `command.js:590`。
///
/// `is_save_dump_file` 为真 → `["-O<temp.mfd>", "-f<keys>"]`（读出 dump）
/// 为假 → `["-N", "-f<keys>"]`（只探测不落盘）
pub fn detect_args(paths: &Paths, is_save_dump_file: bool) -> Vec<String> {
    if is_save_dump_file {
        vec![
            format!("-O{}", paths.temp_mfd().display()),
            format!("-f{}", paths.keys_file().display()),
        ]
    } else {
        vec![
            "-N".to_string(),
            format!("-f{}", paths.keys_file().display()),
        ]
    }
}

/// unknown 密钥的引导文案，对标 `command.js:175-177`。
///
/// `检测到 N 个未知扇区密钥, 可尝试 HardNested 或字典测试`
pub fn unknown_guidance_text(unknown_count: usize) -> String {
    format!(
        "{}{}{}",
        i18n::t("guidance_unknown_pre"),
        unknown_count,
        i18n::t("guidance_unknown_post")
    )
}

/// 引导弹窗的两个按钮，对标 `command.js:177-180`。
pub fn unknown_guidance_buttons() -> Vec<(&'static str, String)> {
    vec![
        ("hard-nested", i18n::t("html_hard_nested")),
        ("dict-test", i18n::t("html_test_dictionary")),
    ]
}

/// 探测完成后的处置，对标 `command.js:600-608`。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DetectOutcome {
    /// 读卡成功，可以继续主命令。
    Proceed,
    /// 读卡失败（temp dump 缺失或为空），**不**跑主命令。
    ReadFailed { reason: ReadFailure },
}

/// 读卡失败的具体原因。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadFailure {
    /// temp dump 不存在——工具没写出文件
    Missing,
    /// temp dump 长度为 0
    Empty,
}

impl ReadFailure {
    /// 对应 i18n key。
    ///
    /// 注意：原版这两条路径**都**落到 `log_msg_card_read_failed`
    /// （`command.js:603-607`）。`log_msg_dump_empty_not_saved` 是 mfoc 路径的文案，
    /// 在此处出现属新增分支，登记见 docs/02 §6.2。
    pub fn i18n_key(self) -> &'static str {
        match self {
            ReadFailure::Missing | ReadFailure::Empty => "log_msg_card_read_failed",
        }
    }
}

/// `readICThenExec` 的收尾，对标 `command.js:594-610`。
///
/// 顺序严格对齐原版：
/// 1. `saveKeys(newKeys)`（`command.js:596`） —— 探测读到的**新密钥**落进 keys.txt（否则下次重跑同一把）
/// 2. 落盘模式下 temp 不存在 → 报读卡失败，不跑主命令
/// 3. 落盘模式下 temp 长度 0 → 删掉，报读卡失败
/// 4. 以上都过 → `Proceed`（`command.js:609-610`）
///
/// **本函数不弹引导**（原版 `readICThenExec` 也没有），见模块文档。
///
/// **会把解析出的 known / unknown 回填进 `AppState`**（对标
/// `command.js:596` 的 `saveKeys(newKeys)` 之后那批全局变量赋值）。
/// 这一步是检测链路的**必需环节**——`hard-nested` 面板的预填、
/// 未知密钥引导、HardNested autoRun 的每轮目标全都读这两个列表。
/// 此前只落盘密钥、不回填列表，导致 `detect-card-type` 之后
/// `unknown_key_info` 恒空，进而引导永不出现、autoRun 必走
/// `StopNoKnownKey`。见 docs/02 §6.14。
pub fn after_detect(
    state: &mut AppState,
    parser: &KeyInfoParser,
    paths: &Paths,
    is_save_dump_file: bool,
) -> Result<DetectOutcome, String> {
    // 0) 先回填密钥信息列表：后续的引导与 HardNested 都读它，
    //    必须在 temp 校验之前完成（校验失败也要保留已解析出的信息，
    //    否则用户会丢掉刚探测到的密钥列表）。
    state.known_key_info = parser
        .known()
        .iter()
        .map(|k| (k.key.clone(), k.sector, k.key_type))
        .collect();
    state.unknown_key_info = parser
        .unknown()
        .iter()
        .map(|u| (u.sector, u.key_type))
        .collect();

    // 1) 再把新密钥落盘，对标 command.js:596
    let fresh = save_keys(paths, parser.new_keys()).map_err(|e| e.to_string())?;
    if !fresh.is_empty() {
        state.apply(Event::EventItem {
            kind: crate::state::EventKind::Key,
            text: format!("{}{}", i18n::t("event_keys_saved"), fresh.join(", ")),
        });
    }

    if is_save_dump_file {
        let temp = paths.temp_mfd();
        // 单次 stat 区分 NotFound 与 len==0，避免 exists() + metadata() 的 TOCTOU
        let failure = match std::fs::metadata(&temp) {
            Err(_) => Some(ReadFailure::Missing),
            Ok(m) if m.len() == 0 => {
                let _ = std::fs::remove_file(&temp);
                Some(ReadFailure::Empty)
            }
            Ok(_) => None,
        };
        if let Some(reason) = failure {
            let msg = i18n::t(reason.i18n_key());
            state.apply(Event::Log(format!("\n{msg}\n")));
            return Err(msg);
        }
    }

    Ok(DetectOutcome::Proceed)
}

/// `detect-card-type` 的引导，对标 `command.js:171-180`。
///
/// 语义：**不**拦后续动作（原版这条路径本来就没有主命令），只负责在 unknown
/// 非空时产出一个引导事件。`None` 表示无需引导。
///
/// 刻意不复用 `Event::Guidance` 作为返回值载体——`AppState::apply` 把它归为
/// no-op（与原版一致：引导走 IPC 直达 UI，不落 status），返回值必须自带内容，
/// 否则整条路径不可观测。
/// 引导条内容：`(数量文案, 拼好的引导文案, [(action 名, 按钮文案)])`。
/// 单独起别名是为了让 `guidance_for_unknown` / `after_detect` 的签名可读。
pub type Guidance = (&'static str, String, Vec<(&'static str, String)>);

pub fn guidance_for_unknown(parser: &KeyInfoParser) -> Option<Guidance> {
    let n = parser.unknown().len();
    if n == 0 {
        return None;
    }
    Some((
        "detect-card-type",
        unknown_guidance_text(n),
        unknown_guidance_buttons(),
    ))
}

/// 探测阶段的起始状态条 i18n key，对标 `command.js:590`。
pub fn detect_status_key(is_save_dump_file: bool) -> &'static str {
    if is_save_dump_file {
        "indicator_backing_up_current_card"
    } else {
        "indicator_detecting_ic_card"
    }
}

/// 探测阶段的起始状态条文案（已过 i18n），对标 `command.js:590`。
pub fn detect_status(is_save_dump_file: bool) -> String {
    i18n::t(detect_status_key(is_save_dump_file))
}

/// 确保密钥文件存在，不存在则建空文件，对标 `command.js:747-750`。
pub fn check_key_file_exists(paths: &Paths) -> std::io::Result<()> {
    let p = paths.keys_file();
    if p.exists() {
        return Ok(());
    }
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&p, b"")
}

/// 读卡失败时是否应删除残留的 temp dump，对标 `command.js:595-597`。
///
/// 原版只在「存在且长度为 0」时 `unlinkSync`。此处保留该语义。
pub fn purge_empty_temp(path: &Path) -> bool {
    match std::fs::metadata(path) {
        Ok(m) if m.len() == 0 => std::fs::remove_file(path).is_ok(),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn tempdir(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("nfc-s5-det-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    /// 构造一个带 unknown 密钥的 parser（跳过 mfdetect 进程）
    fn parser_with_unknown(n: usize) -> KeyInfoParser {
        let mut p = KeyInfoParser::new();
        // 格式取自 tests/fixtures/mfdetect-keys.txt：` Unknown Key A`
        p.push(&"Sector 00 - Unknown Key A\n".repeat(n));
        p.flush();
        p
    }

    /// 构造一个带已知密钥（因而 new_keys 非空）的 parser
    fn parser_with_new_keys(keys: &[&str]) -> KeyInfoParser {
        let mut p = KeyInfoParser::new();
        for k in keys {
            p.push(&format!("Sector 00 - Found   Key A: {k}\n"));
        }
        p.flush();
        p
    }

    // ---------------------------------------------------------- detect_args

    #[test]
    fn detect_args_with_dump_uses_output_flag() {
        let dir = tempdir("args-dump");
        let paths = Paths::new(&dir);
        let a = detect_args(&paths, true);
        assert_eq!(a.len(), 2);
        assert!(a[0].starts_with("-O"), "实际: {}", a[0]);
        assert!(a[0].contains("temp.mfd"));
        assert!(a[1].starts_with("-f"), "实际: {}", a[1]);
    }

    #[test]
    fn detect_args_without_dump_uses_dash_n() {
        let dir = tempdir("args-n");
        let paths = Paths::new(&dir);
        let a = detect_args(&paths, false);
        assert_eq!(a[0], "-N");
        assert!(a[1].starts_with("-f"), "实际: {}", a[1]);
        assert!(
            !a.iter().any(|x| x.contains("temp.mfd")),
            "不带 dump 时不应出现 -O"
        );
    }

    // ---------------------------------------------------------- guidance

    #[test]
    fn unknown_guidance_text_matches_original_wording() {
        assert_eq!(
            unknown_guidance_text(3),
            "检测到 3 个未知扇区密钥, 可尝试 HardNested 或字典测试"
        );
    }

    #[test]
    fn unknown_guidance_text_zero_is_still_renderable() {
        // 0 时不应被调用，但函数不应 panic
        assert!(unknown_guidance_text(0).contains('0'));
    }

    #[test]
    fn guidance_buttons_are_hard_nested_and_dict_test() {
        let b = unknown_guidance_buttons();
        assert_eq!(b.len(), 2);
        assert_eq!(b[0].0, "hard-nested");
        assert_eq!(b[0].1, i18n::t("html_hard_nested"));
        assert!(b[0].1.contains("HardNested"), "实际: {}", b[0].1);
        assert_eq!(b[1].0, "dict-test");
        assert_eq!(b[1].1, "字典测试");
    }

    #[test]
    fn guidance_is_none_when_no_unknown() {
        assert!(guidance_for_unknown(&KeyInfoParser::new()).is_none());
        assert!(guidance_for_unknown(&parser_with_unknown(0)).is_none());
    }

    #[test]
    fn guidance_carries_text_and_buttons() {
        let (_, text, buttons) = guidance_for_unknown(&parser_with_unknown(4)).expect("应有引导");
        assert!(text.contains('4'), "实际: {text}");
        assert!(text.contains("未知扇区密钥"), "实际: {text}");
        assert_eq!(buttons.len(), 2);
        assert_eq!(buttons[0].0, "hard-nested");
    }

    // ---------------------------------------------------------- after_detect

    #[test]
    fn after_detect_proceeds_when_temp_valid() {
        let dir = tempdir("proc");
        let paths = Paths::new(&dir);
        std::fs::write(paths.temp_mfd(), b"1024 bytes...").unwrap();
        let mut s = AppState::new();
        let p = parser_with_unknown(0);
        assert_eq!(
            after_detect(&mut s, &p, &paths, true).unwrap(),
            DetectOutcome::Proceed
        );
        assert!(paths.temp_mfd().exists(), "有效 dump 不应被删");
    }

    #[test]
    fn after_detect_with_dump_missing_fails() {
        let dir = tempdir("miss");
        let paths = Paths::new(&dir);
        let mut s = AppState::new();
        let p = parser_with_unknown(0);
        let err = after_detect(&mut s, &p, &paths, true).unwrap_err();
        assert_eq!(err, i18n::t("log_msg_card_read_failed"));
        assert_eq!(ReadFailure::Missing.i18n_key(), "log_msg_card_read_failed");
        assert!(
            s.log_buffer.contains("读卡失败，未执行操作"),
            "实际: {}",
            s.log_buffer
        );
    }

    #[test]
    fn after_detect_with_empty_dump_deletes_and_fails() {
        let dir = tempdir("empty");
        let paths = Paths::new(&dir);
        std::fs::write(paths.temp_mfd(), b"").unwrap();
        let mut s = AppState::new();
        let p = parser_with_unknown(0);
        let err = after_detect(&mut s, &p, &paths, true).unwrap_err();
        // 原版此路径也报「读卡失败」，不是 mfoc 那句「未读到卡内容」
        assert_eq!(err, i18n::t("log_msg_card_read_failed"));
        assert!(!paths.temp_mfd().exists(), "0 字节 temp 应被删除");
    }

    #[test]
    fn after_detect_ignores_temp_when_not_saving() {
        let dir = tempdir("nosave");
        let paths = Paths::new(&dir);
        // 不落盘模式下 temp 不存在也无所谓
        let mut s = AppState::new();
        let p = parser_with_unknown(0);
        assert_eq!(
            after_detect(&mut s, &p, &paths, false).unwrap(),
            DetectOutcome::Proceed
        );
    }

    /// 核心语义锁：readICThenExec 路径**不因 unknown 密钥而拦下主命令**。
    /// 若把 guidance 合进 after_detect，write-IC / format-card / auto hard-nested
    /// 会全部无法执行。
    #[test]
    fn after_detect_still_proceeds_with_many_unknown_keys() {
        let dir = tempdir("unknown-proceed");
        let paths = Paths::new(&dir);
        let mut s = AppState::new();
        let p = parser_with_unknown(9);
        assert_eq!(p.unknown().len(), 9);
        assert_eq!(
            after_detect(&mut s, &p, &paths, false).unwrap(),
            DetectOutcome::Proceed
        );
    }

    // ---------------------------------------------------------- save_keys 联动

    #[test]
    fn after_detect_saves_new_keys_to_keys_file() {
        let dir = tempdir("savekeys");
        let paths = Paths::new(&dir);
        let mut s = AppState::new();
        // 两个都是 DEFAULT_KEYS 成员（见 model::keys::DEFAULT_KEYS），
        // save_keys 应当全部剔除，keys.txt 保持为空。
        let p = parser_with_new_keys(&["ffffffffffff", "a0a1a2a3a4a5"]);
        assert_eq!(p.new_keys().len(), 2);
        after_detect(&mut s, &p, &paths, false).unwrap();
        let saved = std::fs::read_to_string(paths.keys_file()).unwrap_or_default();
        assert_eq!(saved.trim(), "", "默认密钥应被剔除: {saved:?}");

        // 混入非默认密钥时才应落盘
        let p2 = parser_with_new_keys(&["ffffffffffff", "112233445566"]);
        let mut s2 = AppState::new();
        after_detect(&mut s2, &p2, &paths, false).unwrap();
        let saved2 = std::fs::read_to_string(paths.keys_file()).unwrap_or_default();
        assert!(saved2.contains("112233445566"), "实际: {saved2:?}");
        assert!(
            !saved2.contains("ffffffffffff"),
            "默认密钥应被剔除: {saved2:?}"
        );
        // 应推一条 Key 类事件（第二个 parser 才有非默认密钥）
        assert!(
            s2.events
                .iter()
                .any(|e| e.kind == crate::state::EventKind::Key),
            "应有 Key 事件，实际: {:?}",
            s2.events
        );
        // 全部为默认密钥时不应有 Key 事件
        assert!(
            !s.events
                .iter()
                .any(|e| e.kind == crate::state::EventKind::Key),
            "无新密钥时不该有 Key 事件，实际: {:?}",
            s.events
        );
    }

    #[test]
    fn after_detect_no_new_keys_pushes_no_event() {
        let dir = tempdir("nokeys");
        let paths = Paths::new(&dir);
        let mut s = AppState::new();
        let p = KeyInfoParser::new();
        after_detect(&mut s, &p, &paths, false).unwrap();
        assert!(s.events.is_empty(), "无新密钥不应有事件");
    }

    // ---------------------------------------------------------- 杂项

    #[test]
    fn detect_status_picks_backup_or_detect() {
        assert_eq!(detect_status_key(true), "indicator_backing_up_current_card");
        assert_eq!(detect_status_key(false), "indicator_detecting_ic_card");
        assert_eq!(detect_status(true), "正在备份当前卡");
        assert_eq!(detect_status(false), "正在检测卡片");
    }

    #[test]
    fn check_key_file_creates_missing_file() {
        let dir = tempdir("keyfile");
        let paths = Paths::new(&dir);
        let p = paths.keys_file();
        assert!(!p.exists());
        check_key_file_exists(&paths).unwrap();
        assert!(p.exists());
        assert_eq!(std::fs::read(&p).unwrap(), b"");
        // 二次调用不覆盖已有内容
        std::fs::write(&p, b"aabbccddeeff").unwrap();
        check_key_file_exists(&paths).unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"aabbccddeeff");
    }

    #[test]
    fn purge_empty_temp_only_removes_zero_len() {
        let dir = tempdir("purge");
        let zero = dir.join("z");
        std::fs::write(&zero, b"").unwrap();
        assert!(purge_empty_temp(&zero));
        assert!(!zero.exists());

        let full = dir.join("f");
        std::fs::write(&full, b"data").unwrap();
        assert!(!purge_empty_temp(&full));
        assert!(full.exists());

        assert!(!purge_empty_temp(&dir.join("nonexistent")));
    }
}
