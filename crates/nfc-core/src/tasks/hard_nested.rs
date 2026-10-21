//! HardNested 破解闭环。对标 `command.js:236-357`（`hard-nested-config-done` /
//! `run-hard-nested` / `sendHardNestedProgress`）。
//!
//! 偏离登记见 docs/02 §6。
//!
//! 流程：探测（autoRun 时）→ 采集 nonce → [collectOnly 则保存退出] → cropto1_bs
//! 破解 → 命中则移除对应 unknown（任意位置，S18 F1）→ autoRun 则递归下一轮。
//!
//! 设计：`HardNestedRunner` 持显式状态机 [`HardNestedPhase`]，把「本轮该跑哪个
//! 工具、跑完该做什么」表达为纯函数，便于对原版那个 120 行的嵌套闭包做矩阵测试。

use std::path::Path;

use crate::i18n;
use crate::model::keys::{KeyInfo, KeyType, UnknownKeyInfo};
use crate::paths::Paths;
use crate::state::{AppState, Event, HardNestedConfig, HardNestedPhase};

/// 采集 / 破解的工具名，对标 `command.js:281` / `command.js:319`。
pub const TOOL_COLLECT: &str = "libnfc-collect";
pub const TOOL_CRYPTO1: &str = "cropto1_bs";

/// 一轮破解所需的上下文。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RoundContext {
    pub config: HardNestedConfig,
    /// 本轮采集到的 UID（用于 collectOnly 的默认文件名），`None` 表示未读到
    pub uid: Option<String>,
    /// 本轮采集到的扇区号
    pub sector: Option<u8>,
    /// 本轮采集到的密钥类型
    pub key_type: Option<KeyType>,
}

impl RoundContext {
    pub fn new(config: HardNestedConfig) -> Self {
        Self {
            config,
            ..Self::default()
        }
    }

    /// `libnfc-collect` 的参数，对标 `command.js:282-290`。
    ///
    /// 7 个位置参数：knownKey、knownSector(块号)、knownKeyType、targetSector(块号)、
    /// targetKeyType、"bin"、nonces 路径。
    ///
    /// 注意原版在此处就把**扇区号就地换成块号**（`command.js:272-273`），
    /// `HardNestedConfig` 里的 `known_sector` / `target_sector` 存的是扇区号，
    /// 换算只发生在此处。
    pub fn collect_args(&self, nonces: &Path) -> Vec<String> {
        vec![
            self.config.known_key.clone(),
            HardNestedConfig::trailer_block(self.config.known_sector).to_string(),
            self.config.known_key_type.as_char().to_string(),
            HardNestedConfig::trailer_block(self.config.target_sector).to_string(),
            self.config.target_key_type.as_char().to_string(),
            "bin".to_string(),
            nonces.display().to_string(),
        ]
    }

    /// `cropto1_bs` 的参数，对标 `command.js:320`。
    pub fn crypto1_args(&self, nonces: &Path) -> Vec<String> {
        vec![nonces.display().to_string()]
    }

    /// collectOnly 的默认文件名，对标 `command.js:299`。
    ///
    /// 原版 `` `${uid}_${sector}${keyType}` ``；三者任一缺失时整体回落 `"nonces"`。
    pub fn nonces_default_name(&self) -> String {
        match (&self.uid, self.sector, self.key_type) {
            (Some(u), Some(s), Some(k)) => format!("{u}_{s}{}", k.as_char()),
            _ => "nonces".to_string(),
        }
    }
}

/// autoRun 的一轮该做什么。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    /// 无已知密钥，无法采集（`command.js:259-262`）
    StopNoKnownKey,
    /// unknown 已空，无事可做（`command.js:263-267`）
    StopAllDecrypted,
    /// 用户已取消
    StopCancelled,
    /// 先跑 `nfc-mfdetect` 探测（本轮前置）
    Detect,
    /// 跑 `libnfc-collect` 采集
    Collect,
    /// 跑 `cropto1_bs` 破解
    Crack,
    /// 采集与破解都完成，收尾
    Done,
    /// collectOnly 等待用户保存 nonce 文件
    SaveNonces,
    /// autoRun 本轮无进展，主动终止（偏离，见 `after_crack_guarded`）
    StopNoProgress,
}

/// 破解状态机。
#[derive(Clone, Debug, Default)]
pub struct HardNestedRunner {
    phase: HardNestedPhase,
    /// autoRun 时 unknown 的总数，用于进度 `已解/总数`
    total_unknown: usize,
    /// autoRun 模式下本轮要解的扇区（`unknown[0]`）
    target: Option<UnknownKeyInfo>,
}

impl HardNestedRunner {
    pub fn new() -> Self {
        Self::default()
    }

    /// 当前阶段。
    pub fn phase(&self) -> &HardNestedPhase {
        &self.phase
    }

    /// unknown 总数（autoRun 时初始化）。
    pub fn total_unknown(&self) -> usize {
        self.total_unknown
    }

    /// autoRun 的进度文案，`command.js:250` 的 `${total - remaining + 1}/${total}`。
    ///
    /// `remaining` 为当前 `unknown` 剩余数。原版 `Math.max(1, total - len + 1)`。
    pub fn progress_text(remaining: usize, total: usize) -> String {
        if total == 0 {
            return String::new();
        }
        let index = total.saturating_sub(remaining).saturating_add(1).max(1);
        format!("{index}/{total}")
    }

    /// autoRun 的进度百分比，对标 `command.js:712-717`。
    ///
    /// 原版 `Math.min(100, Math.round(index / total * 100))`。
    pub fn progress_percent(remaining: usize, total: usize) -> Option<f32> {
        if total == 0 {
            return None;
        }
        let index = total.saturating_sub(remaining).saturating_add(1).max(1);
        Some((index as f32 / total as f32 * 100.0).round().min(100.0))
    }

    /// `hard-nested-config-done` 的入口判定，对标 `command.js:248-272`。
    ///
    /// - `auto_run` 为真 → 先 `Detect`（先跑一遍 mfdetect 更新密钥池）
    /// - 否则 → `Collect`
    ///
    /// 注意原版在 `Detect` 之后的**回调**里才查 `knownKeyInfo.length == 0` 与
    /// `unknownKeyInfo.length == 0`（`command.js:256-268`），因为那时要等探测结果。
    pub fn begin(&mut self, config: &HardNestedConfig, unknown: &[UnknownKeyInfo]) -> Step {
        self.total_unknown = unknown.len();
        if config.auto_run {
            self.phase = HardNestedPhase::AutoDetecting;
            Step::Detect
        } else {
            self.total_unknown = unknown.len();
            self.phase = HardNestedPhase::Collecting;
            Step::Collect
        }
    }

    /// 探测结束后的判定，对标 `command.js:256-268`。
    pub fn after_detect(&mut self, known: &[KeyInfo], unknown: &[UnknownKeyInfo]) -> Step {
        if known.is_empty() {
            self.phase = HardNestedPhase::Idle;
            return Step::StopNoKnownKey;
        }
        if unknown.is_empty() {
            self.phase = HardNestedPhase::Idle;
            return Step::StopAllDecrypted;
        }
        self.target = unknown.first().copied();
        self.phase = HardNestedPhase::Collecting;
        Step::Collect
    }

    /// 采集结束后的判定，对标 `command.js:297-331`。
    ///
    /// - `collect_only` → `SavingNonces`，由调用方弹保存框后调 [`Self::after_save`]
    /// - 否则 → `Cracking`
    pub fn after_collect(&mut self, config: &HardNestedConfig) -> Step {
        if config.collect_only {
            self.phase = HardNestedPhase::SavingNonces;
            Step::SaveNonces
        } else {
            self.phase = HardNestedPhase::Cracking;
            Step::Crack
        }
    }

    /// collectOnly 保存结束。
    pub fn after_save(&mut self) -> Step {
        self.phase = HardNestedPhase::Idle;
        Step::Done
    }

    /// 破解结束后的判定，对标 `command.js:341-350`。
    ///
    /// - 命中且移除了队首 unknown → autoRun 则 `Detect` 递归下一轮
    /// - 否则 autoRun 也 `Detect`（原版无条件递归，靠 `unknown` 空来终止）
    /// - 非 autoRun → `Done`
    pub fn after_crack(&mut self, config: &HardNestedConfig, unknown_remaining: usize) -> Step {
        if config.auto_run {
            // 原版 command.js:341 无条件递归，终止靠 after_detect 的 unknown 空判定
            self.phase = HardNestedPhase::AutoDetecting;
            Step::Detect
        } else {
            self.phase = HardNestedPhase::Idle;
            let _ = unknown_remaining;
            Step::Done
        }
    }

    /// autoRun 的无进展兜底：破解后 `unknown` 数量未减少即停。
    ///
    /// 偏离（原版 `command.js:341-350` **无此判据**）：原版只要 autoRun 就无条件
    /// 递归下一轮，若 `cropto1_bs` 始终找不出密钥会**无限递归**。原版靠
    /// `totalUnknownKeys` 固定而 `unknownKeyInfo` 不变来暴露问题（进度条永远停在
    /// 同一格），实为缺陷。此处给出显式终止条件。
    pub fn after_crack_guarded(
        &mut self,
        config: &HardNestedConfig,
        unknown_before: usize,
        unknown_after: usize,
    ) -> Step {
        if config.auto_run && unknown_after >= unknown_before {
            // 本轮没解出任何密钥，再跑下去结果不会变
            self.phase = HardNestedPhase::Idle;
            return Step::StopNoProgress;
        }
        self.after_crack(config, unknown_after)
    }

    /// 手动中止（等价原版 `killProcess`），修掉 `Step::StopCancelled` 不可达的问题。
    pub fn cancel(&mut self) -> Step {
        self.phase = HardNestedPhase::Idle;
        Step::StopCancelled
    }

    /// 本轮瞄准的 unknown（队首），`after_detect` 后才有值。
    pub fn target(&self) -> Option<UnknownKeyInfo> {
        self.target
    }

    /// 落进 `AppState` 的阶段，供 UI 观察。
    pub fn sync(&self, state: &mut AppState) {
        state.hard_nested_task.phase = self.phase;
    }
}

/// 从 `unknown` 队列里移除命中的那条（任意位置），对标 `command.js:333-335`
/// 的 `shift()` 并修正其已知缺陷。
///
/// 原版只在「命中的扇区与类型与**队首**一致」时 `shift()`，命中非队首时结果
/// 被静默丢弃（§6.2 #27 登记的「已知功能限制」：该 unknown 永远留在队列，
/// 队首一直挡着它）。S18 改为按 `(sector, key_type)` 在**任意位置**移除命中项：
/// 乱序命中不再丢密钥，autoRun 的下一轮队列也随之推进。返回是否移除成功。
pub fn remove_unknown(unknown: &mut Vec<UnknownKeyInfo>, sector: u8, key_type: KeyType) -> bool {
    let pos = unknown
        .iter()
        .position(|u| u.sector == sector && u.key_type == key_type);
    match pos {
        Some(i) => {
            unknown.remove(i);
            true
        }
        None => false,
    }
}

/// autoRun 的状态条文案，对标 `command.js:250` 与 `command.js:317`。
pub fn auto_status_text(remaining: usize, total: usize) -> String {
    if total == 0 {
        return i18n::t("indicator_doing_hard_nested");
    }
    format!(
        "{} - {}",
        i18n::t("indicator_doing_hard_nested"),
        HardNestedRunner::progress_text(remaining, total)
    )
}

/// 采集阶段的状态条文案，对标 `command.js:277`。
pub fn collect_status_text(auto_run: bool, remaining: usize, total: usize) -> String {
    if auto_run {
        format!(
            "{} - {}",
            i18n::t("indicator_collecting_nonces"),
            HardNestedRunner::progress_text(remaining, total)
        )
    } else {
        i18n::t("indicator_collecting_nonces")
    }
}

/// no known key 的收尾，对标 `command.js:259-262`。
pub fn report_no_known_key(state: &mut AppState) {
    state.apply(Event::Log(format!(
        "\n{}",
        i18n::t("log_msg_not_found_known_key")
    )));
}

/// unknown 全解完的收尾，对标 `command.js:263-267`。
pub fn report_all_decrypted(state: &mut AppState) {
    state.apply(Event::Log(format!(
        "\n{}\n",
        i18n::t("log_msg_tried_decrypt_all_unknown_keys")
    )));
}

/// autoRun 起始时清空 nonces 文件，对标 `command.js:249`。
pub fn clear_nonces(paths: &Paths) -> std::io::Result<()> {
    if let Some(parent) = paths.nonces_file().parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(paths.nonces_file(), b"")
}

/// 采集到的 nonce 文件是否为空（collectOnly 的判定依据，`command.js:297`）。
pub fn nonces_is_empty(paths: &Paths) -> bool {
    match std::fs::metadata(paths.nonces_file()) {
        Ok(m) => m.len() == 0,
        Err(_) => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(auto_run: bool, collect_only: bool) -> HardNestedConfig {
        HardNestedConfig {
            known_key: "ffffffffffff".into(),
            known_sector: 0,
            known_key_type: KeyType::A,
            target_sector: 1,
            target_key_type: KeyType::B,
            collect_only,
            auto_run,
        }
    }

    fn known() -> Vec<KeyInfo> {
        vec![KeyInfo {
            key: "ffffffffffff".into(),
            sector: 0,
            key_type: KeyType::A,
        }]
    }

    fn unknown(n: u8) -> Vec<UnknownKeyInfo> {
        (0..n)
            .map(|i| UnknownKeyInfo {
                sector: i + 1,
                key_type: KeyType::B,
            })
            .collect()
    }

    // ---------------------------------------------------------- 参数拼装

    #[test]
    fn collect_args_convert_sector_to_trailer_block() {
        let a = RoundContext::new(cfg(false, false)).collect_args(Path::new("/d/nonces.bin"));
        assert_eq!(a.len(), 7);
        // knownSector 0 → block 3；targetSector 1 → block 7
        assert_eq!(a[0], "ffffffffffff");
        assert_eq!(a[1], "3");
        assert_eq!(a[2], "A");
        assert_eq!(a[3], "7");
        assert_eq!(a[4], "B");
        assert_eq!(a[5], "bin");
        assert_eq!(a[6], "/d/nonces.bin");
    }

    #[test]
    fn collect_args_block_numbers_for_high_sectors() {
        let mut c = cfg(false, false);
        c.known_sector = 15;
        c.target_sector = 39;
        let a = RoundContext::new(c).collect_args(Path::new("/n"));
        assert_eq!(a[1], "63", "扇区 15 → 块 63");
        assert_eq!(a[3], "159", "扇区 39 → 块 159");
    }

    #[test]
    fn crypto1_args_is_just_nonces_path() {
        let a = RoundContext::new(cfg(false, false)).crypto1_args(Path::new("/d/n.bin"));
        assert_eq!(a, vec!["/d/n.bin"]);
    }

    #[test]
    fn nonces_default_name_needs_all_three_parts() {
        let c = cfg(false, true);
        let mut r = RoundContext::new(c);
        // 三者齐全
        r.uid = Some("112233445566".into());
        r.sector = Some(3);
        r.key_type = Some(KeyType::B);
        assert_eq!(r.nonces_default_name(), "112233445566_3B");
        // 缺 uid
        r.uid = None;
        assert_eq!(r.nonces_default_name(), "nonces");
        // 缺 sector
        r.uid = Some("112233445566".into());
        r.sector = None;
        assert_eq!(r.nonces_default_name(), "nonces");
        // 缺 key_type
        r.sector = Some(3);
        r.key_type = None;
        assert_eq!(r.nonces_default_name(), "nonces");
    }

    // ---------------------------------------------------------- 状态机矩阵

    #[test]
    fn begin_auto_run_goes_to_detect() {
        let mut r = HardNestedRunner::new();
        assert_eq!(r.begin(&cfg(true, false), &unknown(3)), Step::Detect);
        assert_eq!(*r.phase(), HardNestedPhase::AutoDetecting);
        assert_eq!(r.total_unknown(), 3);
    }

    #[test]
    fn begin_manual_goes_to_collect() {
        let mut r = HardNestedRunner::new();
        assert_eq!(r.begin(&cfg(false, false), &unknown(2)), Step::Collect);
        assert_eq!(*r.phase(), HardNestedPhase::Collecting);
        assert_eq!(r.total_unknown(), 2);
    }

    #[test]
    fn after_detect_stops_without_known_key() {
        let mut r = HardNestedRunner::new();
        r.begin(&cfg(true, false), &unknown(3));
        assert_eq!(r.after_detect(&[], &unknown(3)), Step::StopNoKnownKey);
        assert_eq!(*r.phase(), HardNestedPhase::Idle);
    }

    #[test]
    fn after_detect_stops_when_all_decrypted() {
        let mut r = HardNestedRunner::new();
        r.begin(&cfg(true, false), &unknown(3));
        assert_eq!(r.after_detect(&known(), &[]), Step::StopAllDecrypted);
        assert_eq!(*r.phase(), HardNestedPhase::Idle);
    }

    #[test]
    fn after_detect_collects_when_both_present() {
        let mut r = HardNestedRunner::new();
        r.begin(&cfg(true, false), &unknown(3));
        assert_eq!(r.after_detect(&known(), &unknown(3)), Step::Collect);
        assert_eq!(*r.phase(), HardNestedPhase::Collecting);
    }

    #[test]
    fn after_collect_branches_on_collect_only() {
        let mut r = HardNestedRunner::new();
        r.begin(&cfg(false, false), &unknown(1));
        assert_eq!(r.after_collect(&cfg(false, false)), Step::Crack);
        assert_eq!(*r.phase(), HardNestedPhase::Cracking);

        let mut r2 = HardNestedRunner::new();
        r2.begin(&cfg(false, true), &unknown(1));
        assert_eq!(r2.after_collect(&cfg(false, true)), Step::SaveNonces);
        assert_eq!(*r2.phase(), HardNestedPhase::SavingNonces);
    }

    #[test]
    fn after_crack_recurses_on_auto_run() {
        // autoRun：破解完必须回到探测阶段继续下一轮（phase = AutoDetecting）
        let mut r = HardNestedRunner::new();
        r.begin(&cfg(true, false), &unknown(3));
        assert_eq!(r.after_crack(&cfg(true, false), 2), Step::Detect);
        assert_eq!(*r.phase(), HardNestedPhase::AutoDetecting);

        // 手动模式：破解完即结束（phase = Idle）
        let mut m = HardNestedRunner::new();
        m.begin(&cfg(false, false), &unknown(3));
        assert_eq!(m.after_crack(&cfg(false, false), 2), Step::Done);
        assert_eq!(*m.phase(), HardNestedPhase::Idle);
    }

    #[test]
    fn after_crack_done_on_manual() {
        let mut r = HardNestedRunner::new();
        r.begin(&cfg(false, false), &unknown(1));
        assert_eq!(r.after_crack(&cfg(false, false), 1), Step::Done);
    }

    #[test]
    fn after_save_returns_to_idle() {
        let mut r = HardNestedRunner::new();
        r.begin(&cfg(false, true), &unknown(1));
        r.after_collect(&cfg(false, true));
        assert_eq!(r.after_save(), Step::Done);
        assert_eq!(*r.phase(), HardNestedPhase::Idle);
    }

    /// autoRun 全链：Detect → Collect → Crack → Detect，直到 unknown 空。
    /// 这是「递归终止靠 unknown 空」的核心回归。
    #[test]
    fn auto_run_loop_terminates_when_unknown_empty() {
        let mut r = HardNestedRunner::new();
        let mut u = unknown(2);
        let c = cfg(true, false);
        assert_eq!(r.begin(&c, &u), Step::Detect);
        let mut rounds = 0;
        let mut step = Step::Detect;
        while step != Step::StopAllDecrypted && rounds < 20 {
            match step {
                Step::Detect => {
                    // 不做短路：终止判定必须真的走 after_detect，
                    // 否则这条测试验的是测试自己而不是状态机
                    step = r.after_detect(&known(), &u);
                }
                Step::Collect => {
                    let _ = r.after_collect(&c);
                    step = Step::Crack;
                }
                Step::Crack => {
                    // 每轮解掉一条匹配项（移除与队首同扇区/同类型的条目）
                    let first = u[0];
                    assert!(remove_unknown(&mut u, first.sector, first.key_type));
                    step = r.after_crack(&c, u.len());
                }
                _ => break,
            }
            rounds += 1;
        }
        assert_eq!(step, Step::StopAllDecrypted, "应因 unknown 清空而终止");
        assert!(u.is_empty());
        // 7 次迭代 = 3 次 Detect + 2 次 Collect + 2 次 Crack（末次 Detect 即终止判定）
        assert_eq!(rounds, 7, "含终止那次的 Detect");
    }

    /// autoRun 无进展兜底：破解后数量没减少就必须停，否则会无限递归。
    #[test]
    fn auto_run_stops_when_no_progress() {
        let mut r = HardNestedRunner::new();
        let c = cfg(true, false);
        r.begin(&c, &unknown(3));
        // unknown 一直是 3
        assert_eq!(r.after_crack_guarded(&c, 3, 3), Step::StopNoProgress);
        assert_eq!(*r.phase(), HardNestedPhase::Idle);
        // 有进展则继续
        assert_eq!(r.after_crack_guarded(&c, 3, 2), Step::Detect);
    }

    #[test]
    fn cancel_returns_to_idle() {
        let mut r = HardNestedRunner::new();
        r.begin(&cfg(false, false), &unknown(1));
        assert_eq!(r.cancel(), Step::StopCancelled);
        assert_eq!(*r.phase(), HardNestedPhase::Idle);
    }

    #[test]
    fn after_detect_records_target() {
        let mut r = HardNestedRunner::new();
        r.begin(&cfg(true, false), &unknown(3));
        assert!(r.target().is_none(), "探测前无目标");
        r.after_detect(&known(), &unknown(3));
        assert_eq!(r.target().map(|t| t.sector), Some(1), "应记下队首 unknown");
    }

    // ---------------------------------------------------------- 进度

    #[test]
    fn progress_text_counts_down_remaining() {
        assert_eq!(HardNestedRunner::progress_text(3, 3), "1/3");
        assert_eq!(HardNestedRunner::progress_text(2, 3), "2/3");
        assert_eq!(HardNestedRunner::progress_text(1, 3), "3/3");
        assert_eq!(HardNestedRunner::progress_text(0, 3), "4/3");
    }

    #[test]
    fn progress_percent_caps_at_100() {
        assert_eq!(HardNestedRunner::progress_percent(3, 3), Some(33.0));
        assert_eq!(HardNestedRunner::progress_percent(2, 3), Some(67.0));
        assert_eq!(HardNestedRunner::progress_percent(1, 3), Some(100.0));
        // 原版 Math.min(100, ...) 兜住越界
        assert_eq!(HardNestedRunner::progress_percent(0, 3), Some(100.0));
    }

    #[test]
    fn progress_of_zero_total_is_none() {
        assert_eq!(HardNestedRunner::progress_percent(0, 0), None);
        assert_eq!(HardNestedRunner::progress_text(0, 0), "");
    }

    // ---------------------------------------------------------- unknown 队列

    #[test]
    fn remove_unknown_removes_head() {
        let mut u = unknown(3);
        assert!(remove_unknown(&mut u, 1, KeyType::B));
        assert_eq!(u.len(), 2);
        assert_eq!(u[0].sector, 2);
    }

    #[test]
    fn remove_unknown_removes_non_head_match() {
        // S18（F1）：命中第 2 条即移除该条，不再要求它是队首
        let mut u = unknown(3);
        assert!(remove_unknown(&mut u, 2, KeyType::B));
        assert_eq!(u.len(), 2);
        assert_eq!(u[0].sector, 1, "队首保持不变");
        assert_eq!(u[1].sector, 3, "命中项被移除");
    }

    #[test]
    fn remove_unknown_respects_key_type() {
        let mut u = unknown(2);
        assert!(!remove_unknown(&mut u, 1, KeyType::A));
        assert_eq!(u.len(), 2);
    }

    #[test]
    fn remove_unknown_on_empty_is_safe() {
        let mut u: Vec<UnknownKeyInfo> = Vec::new();
        assert!(!remove_unknown(&mut u, 0, KeyType::A));
    }

    // ---------------------------------------------------------- 文案

    #[test]
    fn auto_status_text_includes_progress() {
        let t = auto_status_text(2, 3);
        assert!(t.contains("2/3"), "实际: {t}");
        assert!(t.contains("HardNested"), "实际: {t}");
    }

    #[test]
    fn auto_status_text_without_total_has_no_slash() {
        let t = auto_status_text(0, 0);
        assert!(!t.contains('/'), "实际: {t}");
    }

    #[test]
    fn collect_status_text_appends_progress_only_on_auto_run() {
        let manual = collect_status_text(false, 2, 3);
        assert!(!manual.contains('/'), "手动模式无进度: {manual}");
        let auto = collect_status_text(true, 2, 3);
        assert!(auto.contains("2/3"), "实际: {auto}");
    }

    #[test]
    fn reports_use_original_wording() {
        let mut s = AppState::new();
        report_no_known_key(&mut s);
        assert!(
            s.log_buffer.contains("未发现已知密钥"),
            "实际: {}",
            s.log_buffer
        );
        let mut s2 = AppState::new();
        report_all_decrypted(&mut s2);
        assert!(s2.log_buffer.contains("已尝试解密全部未知密钥"));
    }

    // ---------------------------------------------------------- nonces 文件

    #[test]
    fn clear_nonces_creates_and_truncates() {
        let dir = std::env::temp_dir().join(format!("nfc-s7-hn-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let paths = Paths::new(&dir);
        clear_nonces(&paths).unwrap();
        assert!(paths.nonces_file().exists());
        assert!(nonces_is_empty(&paths));
        std::fs::write(paths.nonces_file(), b"nonce-data").unwrap();
        assert!(!nonces_is_empty(&paths));
        clear_nonces(&paths).unwrap();
        assert!(nonces_is_empty(&paths));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn nonces_is_empty_when_file_absent() {
        let dir = std::env::temp_dir().join(format!("nfc-s7-hn2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let paths = Paths::new(&dir);
        assert!(nonces_is_empty(&paths), "文件不存在视作空");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sync_writes_phase_into_state() {
        let mut r = HardNestedRunner::new();
        r.begin(&cfg(true, false), &unknown(2));
        let mut s = AppState::new();
        r.sync(&mut s);
        assert_eq!(s.hard_nested_task.phase, HardNestedPhase::AutoDetecting);
    }
}
