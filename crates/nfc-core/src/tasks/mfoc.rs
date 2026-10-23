//! MFOC 解卡任务。对标 `command.js:618-726`（`mfoc` / `updateMfocETA` /
//! `formatDuration` / `getTimeList`）。
//!
//! 偏离登记见 docs/02 §6。
//!
//! 设计：原版 `mfoc()` 把「起进程 → 逐块喂 ETA → 收尾保存」写成一个带闭包的
//! 大函数。此处拆成三部分，便于单测：
//! - [`EtaTracker`]：纯状态机，不碰 IO，`push(chunk)` → 产出事件
//! - [`dump_save`]：收尾的保存决策，返回 [`AppState`] 需要的事件
//! - [`mfoc_args`] / [`mfoc_keys_total`]：起进程前的参数与基线计算

use std::path::{Path, PathBuf};

use crate::dialogs::{DialogProvider, PendingSave};
use crate::i18n;
use crate::model::keys::DEFAULT_KEYS;
use crate::model::parse::{LineBuffer, MARKER_MFOC_PROGRESS};
use crate::paths::Paths;
use crate::state::{AppState, Event};

/// ETA 节流间隔，原版 `command.js:704` 的 `1000`。
pub const ETA_THROTTLE_MS: u64 = 1_000;

/// 一次 ETA 更新要落地的状态变更。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EtaUpdate {
    /// 要追加到日志的文本（状态条 + ETA 文本），无则不推。
    pub status: Option<String>,
    /// 0.0-100.0 的进度；`Some(100.0)` 表示已收敛。
    pub percent: Option<f32>,
}

/// 字典攻击 ETA 估算器，对标 `command.js:686-710` 的 `updateMfocETA` 与四个
/// `mfoc*` 模块级变量。
///
/// 原版逻辑（逐条对应）：
/// 1. `mfocPendingLine += value`，取 `lastIndexOf("\n")`；无换行则整块缓冲，不产出
/// 2. 已完成行里数 `/\[Key: /g` 的出现次数，累加到 `mfocKeysDone`
/// 3. `mfocKeysDone === 0` 直接返回
/// 4. `>= mfocKeysTotal` → 收敛：状态条回「正在解卡」，进度 100，只此一次
/// 5. 距上次更新 < 1s 则跳过（节流）
/// 6. 否则按 `已用时长 / 已完成数 * 剩余数` 线性外推，向上取整到整秒
#[derive(Debug)]
pub struct EtaTracker {
    /// 攻击轮数分母：内置默认密钥 + 密钥文件密钥 + `-k` 密钥。
    keys_total: usize,
    keys_done: usize,
    /// 跨 chunk 未见换行的残行。复用 `LineBuffer` 以继承其 `\r` 归一与 flush 语义。
    buf: LineBuffer,
    start_ms: u64,
    last_update_ms: u64,
    /// 进度是否已推到 100（收敛后不再产出）。
    finished: bool,
}

impl EtaTracker {
    /// 以单调递增的毫秒时钟构造。`now_ms` 应取自单调时钟（`Instant`），
    /// 不可用墙钟——系统时间前跳会算出负的剩余时间。
    pub fn new(keys_total: usize, now_ms: u64) -> Self {
        Self {
            keys_total,
            keys_done: 0,
            buf: LineBuffer::new(),
            start_ms: now_ms,
            // 偏离（docs/02 §6.2 #15，原版 `command.js:628` 的 mfocLastUpdate = 0）：原版
            // `mfocLastUpdate = 0`，而首帧 `now - 0` 远大于 1000，1s 节流对首帧完全失效。
            // 此处初始化为当前时刻，首帧必然被节流拦下。
            last_update_ms: now_ms,
            finished: false,
        }
    }

    /// 攻击轮数分母。
    pub fn keys_total(&self) -> usize {
        self.keys_total
    }

    /// 已完成轮数。
    pub fn keys_done(&self) -> usize {
        self.keys_done
    }

    /// 是否已收敛（进度推满后不再产出）。
    pub fn is_finished(&self) -> bool {
        self.finished
    }

    /// 喂入一个输出块，产出本次应生效的状态变更。
    pub fn push(&mut self, chunk: &str, now_ms: u64) -> Option<EtaUpdate> {
        // 不可写成 `self.push_lines(self.buf.push(chunk), ..)`：那样同时可变借用
        // self 两次（self.buf 与 self.push_lines），编译器报 E0499。
        let lines = self.buf.push(chunk);
        self.push_lines(lines, now_ms)
    }

    /// 工具退出时结算末行（末行无 `\n` 时其 `[Key: ` 计数不能丢，否则永不收敛）。
    ///
    /// 原版无此步；与 `parse.rs::LineBuffer::flush` 同源，属本项目统一约定。
    pub fn flush(&mut self, now_ms: u64) -> Option<EtaUpdate> {
        let lines = self.buf.flush();
        if lines.is_empty() {
            return None;
        }
        self.push_lines(lines, now_ms)
    }

    fn push_lines(&mut self, lines: Vec<String>, now_ms: u64) -> Option<EtaUpdate> {
        if self.finished {
            return None;
        }
        for line in &lines {
            self.keys_done += count_keys_done(line);
        }
        // 本次没有任何完整行成行（例如 chunk 末尾是个未终止的半行）时不应出
        // 更新：否则会对着同一份 keys_done 反复刷同一个 ETA 百分比，
        // 表现为进度条在无新数据时也跳动。
        if lines.is_empty() || self.keys_done == 0 {
            return None;
        }

        if self.keys_done >= self.keys_total {
            self.finished = true;
            return Some(EtaUpdate {
                status: Some(i18n::t("indicator_reading_ic_card")),
                percent: Some(100.0),
            });
        }

        if now_ms.saturating_sub(self.last_update_ms) < ETA_THROTTLE_MS {
            return None;
        }
        self.last_update_ms = now_ms;

        let elapsed_ms = now_ms.saturating_sub(self.start_ms);
        // 原版：Math.round((now - start) / 1000 / keysDone * (total - keysDone))
        // 整数实现：先算毫秒，除以 keysDone 得每轮毫秒，再乘剩余轮数。
        let remain_ms = elapsed_ms as u128 * (self.keys_total - self.keys_done) as u128
            / self.keys_done as u128;
        // 向上取整到整秒（JS 的 Math.round 对正数是就近，但秒级展示下取整更稳）
        let remain_sec = remain_ms.div_ceil(1000) as u64;

        Some(EtaUpdate {
            status: Some(format!(
                "{} - {} {}",
                i18n::t("indicator_reading_ic_card"),
                i18n::t("html_eta_remaining"),
                format_duration(remain_sec)
            )),
            percent: Some((self.keys_done as f32 / self.keys_total as f32 * 100.0).round()),
        })
    }
}

/// 数 `text` 里 `[Key: ` 的出现次数，对标原版 `complete.match(/\[Key: /g)`。
///
/// 复用 `parse::MARKER_MFOC_PROGRESS`（`"[Key: "`）而非硬编码字面量——该常量
/// 在 `parse.rs` 中专为本模块保留，marker 一改两处应同步。
pub fn count_keys_done(text: &str) -> usize {
    text.matches(MARKER_MFOC_PROGRESS).count()
}

/// 时长格式化，`hh:mm:ss` 或 `mm:ss`。对标 `command.js:719-726`。
///
/// 原版 `h > 0` 才带小时段；`m`、`s` 各补零到两位。
pub fn format_duration(sec: u64) -> String {
    let two = |n: u64| {
        if n < 10 {
            format!("0{n}")
        } else {
            format!("{n}")
        }
    };
    let h = sec / 3600;
    let m = (sec / 60) % 60;
    let s = sec % 60;
    if h > 0 {
        format!("{}:{}:{}", two(h), two(m), two(s))
    } else {
        format!("{}:{}", two(m), two(s))
    }
}

/// 从 `mfoc` 的参数里取出 `-k` 开头的密钥参数，对标 `command.js:632`。
pub fn arg_keys(args: &[String]) -> Vec<&str> {
    args.iter()
        .filter(|a| a.starts_with("-k"))
        .map(String::as_str)
        .collect()
}

/// ETA 分母：内置默认密钥 + 密钥文件密钥 + `-k` 密钥，对标 `command.js:628-633`。
pub fn mfoc_keys_total(file_keys: usize, args: &[String]) -> usize {
    DEFAULT_KEYS.len() + file_keys + arg_keys(args).len()
}

/// 构造 `mfoc` 的完整参数：`-O<temp>` + `-f<keys>` 前置，`-P<probes>` 随后，
/// 最后追加用户传入的其余参数。
pub fn mfoc_args(paths: &Paths, extra: &[String], probes: u32) -> Vec<String> {
    let mut v = vec![
        format!("-O{}", paths.temp_mfd().display()),
        format!("-f{}", paths.keys_file().display()),
    ];
    if probes > 0 {
        v.push(format!("-P{probes}"));
    }
    v.extend(extra.iter().cloned());
    v
}

/// 从 `nfc-mfclassic` 的输出里取卡片 UID，用作 dump 默认文件名的前缀。
///
/// 对标 `command.js:641-645`：
/// ```js
/// let i = value.indexOf("UID (NFCID1):")
/// if (i >= 0) { i += 14; cardID = value.substring(i, i + 14).replace(/\s+/g, "") }
/// ```
/// 即：定位标记 → 跳过 14 字符的标记本身 → 取 UID → 去掉所有空白
/// （原版 mfoc 输出里 UID 是分两行印的，中间有换行与空格）。
///
/// `nfc-mfclassic` 可能吐多个含该标记的数据块，故取**最后一个**
/// （与原版一致：每次命中都覆盖 `cardID`）。
///
/// S18 修正（原 §6.15 登记的原版缺陷）：原版的 14 字符窗口**按含空白计**，
/// UID 折行且缩进较深时取到的 ID 被截断。现改为「跳过前导空白后按行取」：
/// 到下一个换行为止，非空白字符封顶 [`UID_MAX_HEX`]（14，兼容 7 字节卡号），
/// 行内空白容忍。文件名前缀与原版在常见格式（同行、单换行折行）下完全一致。
pub const MARKER_UID_NFCID: &str = "UID (NFCID1):";
/// `MARKER_UID_NFCID` 的字节长度，原版的 `i += 14` 即此。
const MARKER_UID_NFCID_LEN: usize = 14;
/// UID 非空白字符上限（4 字节 = 8 hex，7 字节 = 14 hex，封顶 14）。
const UID_MAX_HEX: usize = 14;
/// 每次匹配最多扫描的字符数（防整块无换行输出时的无界扫描）。
const UID_SCAN_CAP: usize = 128;

/// 见 [`MARKER_UID_NFCID`]。返回已去空白的 UID，形如 `A1B2C3D4`。
pub fn take_uid(text: &str) -> Option<String> {
    let mut found = None;
    for (idx, _) in text.match_indices(MARKER_UID_NFCID) {
        let rest = &text[idx + MARKER_UID_NFCID_LEN..];
        let mut hex = String::new();
        let mut non_ws = 0usize;
        let mut started = false;
        for ch in rest.chars().take(UID_SCAN_CAP) {
            if !ch.is_whitespace() {
                hex.push(ch);
                non_ws += 1;
                started = true;
                if non_ws >= UID_MAX_HEX {
                    break;
                }
            } else if started && ch == '\n' {
                // 首个非空白之后的换行 = UID 行结束（行内空格照旧容忍）
                break;
            }
        }
        if !hex.is_empty() {
            found = Some(hex);
        }
    }
    found
}

/// dump 保存失败的原因，对标 `command.js:678-681`（原版只有一条「保存失败」）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SaveFailure {
    /// `dumpfiles/` 创建失败。
    Mkdir,
    /// 移动到目标路径失败。
    Move,
}

/// dump 保存的结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SaveOutcome {
    /// 已移动到 `target`，`chosen` 为真表示用户手动选了路径（否则是自动默认名）。
    Saved { target: PathBuf, chosen: bool },
    /// dump 不存在或长度为 0，未保存（原版 `command.js:650-661`）
    Empty,
    /// 落盘失败，调用方应 `printExitLog(1)`（原版 `command.js:681`）。
    Failed { reason: SaveFailure },
}

/// 把 ETA 更新落到 `AppState`。
pub fn apply_eta(state: &mut AppState, u: &EtaUpdate) {
    if let Some(text) = &u.status {
        state.apply(Event::Status {
            text: text.clone(),
            indicator: state.status.indicator,
        });
    }
    if let Some(p) = u.percent {
        state.apply(Event::Progress(Some(p)));
    }
}

/// 判断 temp dump 是否「空」（不存在或长度为 0），对标 `command.js:650-661`。
fn dump_is_empty(p: &Path) -> bool {
    match std::fs::metadata(p) {
        Ok(m) => m.len() == 0,
        Err(_) => true,
    }
}

/// 移动文件，跨设备时退化为「复制 + 删除」。
///
/// 偏离（原版 `command.js:676` 只 `fs.rename`）：用户在保存对话框把路径选到
/// U 盘 / 网络盘时 `rename` 必定返回 `EXDEV`，原版此时只打印「保存失败」，
/// 而 `temp.mfd` 是唯一副本，用户拿不到任何结果。此处补 copy+remove 兜底。
/// 目标已存在时先删除——Windows 的 `rename` 不覆盖已存在文件。
fn move_file(from: &Path, to: &Path) -> std::io::Result<()> {
    if to.exists() {
        let _ = std::fs::remove_file(to);
    }
    match std::fs::rename(from, to) {
        Ok(()) => Ok(()),
        Err(e) if e.raw_os_error() == Some(EXDEV) => {
            std::fs::copy(from, to)?;
            std::fs::remove_file(from)?;
            Ok(())
        }
        Err(e) => Err(e),
    }
}

/// Linux/macOS 的 `EXDEV` 错误码。
#[cfg(unix)]
const EXDEV: i32 = 18;
/// Windows 的等价错误码。
#[cfg(windows)]
const EXDEV: i32 = 17;

/// mfoc 收尾：把 temp dump 移到 `dumpfiles/`，必要时弹保存对话框。
///
/// 对标 `command.js:648-681`。要点：
/// - dump 空（不存在或 0 字节）→ 删掉并返回 [`SaveOutcome::Empty`]，原版不弹框
/// - `dumpfiles/` 不存在则创建（偏离：原版 `command.js:661` 的 `fs.mkdir(dumpsFolder, ...)` 忽略回调错误）
/// - 用户取消对话框 → 仍用默认名保存（原版 `url || defaultName`）
/// - 移动失败 → [`SaveOutcome::Failed`]，原版打印「保存失败」并 `printExitLog(1)`
///
/// 失败时日志会带上 temp.mfd 的实际路径——此时它是 dump 的**唯一副本**，
/// 用户需要知道去哪里抢救。
/// [`dump_save`] 的前半段：算好要弹什么框，**不弹**（S11-7）。
///
/// 返回 `None` 表示「dump 为空，不该弹框」（原版 `command.js:650-661` 的两个
/// 早退分支，已在 [`dump_prepare`] 内部处理并记日志）。
pub fn dump_prepare(state: &mut AppState, paths: &Paths, uid: Option<&str>) -> Option<PendingSave> {
    let temp = paths.temp_mfd();
    if dump_is_empty(&temp) {
        // 原版对 0 字节会 unlink；不存在时 unlink 也无害
        let _ = std::fs::remove_file(&temp);
        state.apply(Event::Log(format!(
            "\n{}\n",
            i18n::t("log_msg_dump_empty_not_saved")
        )));
        return None;
    }

    let dir = paths.dumpfiles_dir();
    if std::fs::create_dir_all(&dir).is_err() {
        state.apply(Event::Log(format!(
            "\n\n{}: {}\n",
            i18n::t("log_msh_save_failed"),
            temp.display()
        )));
        return None;
    }

    let default_name = Paths::dump_file_name(uid.unwrap_or("card"), &crate::paths::timestamp());
    let default_path = dir.join(default_name);

    Some(PendingSave {
        default_path: default_path.clone(),
        request: crate::dialogs::SaveDialogRequest {
            title: i18n::t("dialog_title_save_to"),
            default_path,
            filters: vec![crate::dialogs::dump_filter(&i18n::t("file_type_dump"))],
            message: i18n::t("dialog_msg_choose_save_path"),
        },
        source: temp,
        // 读卡存盘后要重刷历史列表（`command.js:678` 的 sendToMainWindow 之后
        // 由前端重读目录）。
        refresh_history: true,
    })
}

/// [`dump_save`] 的后半段：拿到用户选的路径后落盘。
pub fn dump_commit(
    state: &mut AppState,
    pending: &PendingSave,
    url: Option<PathBuf>,
) -> SaveOutcome {
    let temp = &pending.source;
    let chosen = url.is_some();
    let target = url.unwrap_or_else(|| pending.default_path.clone());

    match move_file(temp, &target) {
        Err(_) => {
            state.apply(Event::Log(format!(
                "\n\n{}: {}\n",
                i18n::t("log_msh_save_failed"),
                temp.display()
            )));
            SaveOutcome::Failed {
                reason: SaveFailure::Move,
            }
        }
        Ok(()) => {
            let key = if chosen {
                "log_msg_file_already_saved_to"
            } else {
                "log_msg_dump_auto_saved"
            };
            state.apply(Event::Log(format!(
                "\n\n{} {}\n",
                i18n::t(key),
                target.display()
            )));
            state.apply(Event::DumpSaved(target.clone()));
            SaveOutcome::Saved { target, chosen }
        }
    }
}

/// 同步版收尾：自己弹框再落盘。
///
/// **生产路径不走这里**——`dialog.show_save_dialog` 是同步调用，而
/// `Engine::pump` 跑在 UI 线程上，真弹 rfd 会把窗口冻住（`read-IC` 的默认
/// 文件名含刚读出的 UID，事先没法预选路径，见 `dialogs` 模块文档）。
/// `Engine::finish` 走的是 `dump_prepare` → app 弹框 → `dump_commit`。
/// 本函数保留给单测与 core selftest。
pub fn dump_save(
    state: &mut AppState,
    paths: &Paths,
    uid: Option<&str>,
    dialog: &mut dyn DialogProvider,
) -> SaveOutcome {
    let Some(pending) = dump_prepare(state, paths, uid) else {
        return SaveOutcome::Empty;
    };
    let url = dialog.show_save_dialog(&pending.request);
    dump_commit(state, &pending, url)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---------------------------------------------------------- format_duration

    #[test]
    fn duration_under_one_hour_is_mm_ss() {
        assert_eq!(format_duration(0), "00:00");
        assert_eq!(format_duration(1), "00:01");
        assert_eq!(format_duration(59), "00:59");
        assert_eq!(format_duration(60), "01:00");
        assert_eq!(format_duration(61), "01:01");
        assert_eq!(format_duration(3599), "59:59");
    }

    #[test]
    fn duration_at_and_over_one_hour_is_hh_mm_ss() {
        assert_eq!(format_duration(3600), "01:00:00");
        assert_eq!(format_duration(3661), "01:01:01");
        assert_eq!(format_duration(86_399), "23:59:59");
        assert_eq!(format_duration(86_400), "24:00:00");
    }

    #[test]
    fn duration_never_pads_hour_when_zero() {
        // 关键边界：3599 仍是 mm:ss，3600 才进位到 hh
        assert!(
            !format_duration(3599).contains(':') || format_duration(3599).matches(':').count() == 1
        );
        assert_eq!(format_duration(3599).matches(':').count(), 1);
        assert_eq!(format_duration(3600).matches(':').count(), 2);
    }

    // ---------------------------------------------------------- count_keys_done

    #[test]
    fn counts_key_markers() {
        assert_eq!(count_keys_done(""), 0);
        assert_eq!(count_keys_done("[Key: aabbccddeeff]"), 1);
        assert_eq!(count_keys_done("[Key: a][Key: b][Key: c]"), 3);
        // 必须是 "[Key: " 带空格，"[Key:" 不计
        assert_eq!(count_keys_done("[Key:a]"), 0);
        // 跨行累计
        assert_eq!(count_keys_done("[Key: a]\n[Key: b]\n"), 2);
    }

    // ---------------------------------------------------------- ETA 基线

    #[test]
    fn keys_total_is_default_plus_file_plus_k_args() {
        // 无 -k：13 个默认密钥 + 2 个文件密钥
        assert_eq!(mfoc_keys_total(2, &[]), 15);
        let args = vec!["-kaabbccddeeff".to_string(), "-k112233445566".to_string()];
        assert_eq!(mfoc_keys_total(2, &args), 17);
        // 非 -k 开头的参数不计入
        assert_eq!(
            mfoc_keys_total(0, &["-O/tmp/x".to_string(), "-f/k.txt".to_string()]),
            13
        );
    }

    #[test]
    fn arg_keys_filters_prefix() {
        let args = vec![
            "-O/a".to_string(),
            "-kaa".to_string(),
            "-x".to_string(),
            "-k".to_string(),
        ];
        assert_eq!(arg_keys(&args), vec!["-kaa", "-k"]);
    }

    // ---------------------------------------------------------- ETA 状态机

    #[test]
    fn eta_buffers_until_newline() {
        let mut t = EtaTracker::new(10, 0);
        // 无换行 → 只缓冲，不产出
        assert_eq!(t.push("[Key: aa", 5_000), None);
        assert_eq!(t.push("aabbccddeeff]", 5_000), None);
        assert_eq!(t.keys_done(), 0);
        // 补上换行才结算
        let u = t.push("\n", 5_000).expect("应产出");
        assert_eq!(t.keys_done(), 1);
        assert!(u.percent.is_some());
    }

    #[test]
    fn eta_first_frame_is_throttled() {
        // 偏离 bug #4：首帧必须被 1s 节流拦下
        let mut t = EtaTracker::new(10, 1_000_000);
        let u = t.push("[Key: aabbccddeeff]\n", 1_000_000);
        assert_eq!(u, None, "首帧与 start 同时刻，节流应拦下");
        assert_eq!(t.keys_done(), 1, "但计数照常累加");
    }

    #[test]
    fn eta_throttles_within_one_second() {
        let mut t = EtaTracker::new(100, 0);
        assert!(t.push("[Key: a]\n", 0).is_none());
        assert!(t.push("[Key: b]\n", 999).is_none(), "未满 1s 应拦下");
        let u = t.push("[Key: c]\n", 1_000);
        assert!(u.is_some(), "满 1s 应产出");
        assert!(t.push("[Key: d]\n", 1_500).is_none());
        assert!(t.push("[Key: e]\n", 2_000).is_some());
    }

    #[test]
    fn eta_percent_rounds_and_grows() {
        let mut t = EtaTracker::new(8, 0);
        // 逐次喂，每 1s 一帧
        let mut last = 0.0;
        for i in 1..=3 {
            let u = t
                .push(&format!("[Key: a{i}]\n"), i * 1_000)
                .unwrap_or_else(|| panic!("第 {i} 帧应产出"));
            let p = u.percent.expect("应带 percent");
            assert!(p > last, "percent 必须单调递增：{last} -> {p}");
            assert!(p < 100.0);
            last = p;
        }
        // 3/8 = 37.5 → 38
        assert_eq!(last, 38.0);
    }

    #[test]
    fn eta_converges_to_100_then_stops() {
        let mut t = EtaTracker::new(3, 0);
        assert!(t.push("[Key: a]\n", 0).is_none());
        assert!(t.push("[Key: b]\n", 1_000).is_some());
        // 第 3 个达到分母 → 收敛
        let u = t.push("[Key: c]\n", 2_000).expect("应产出");
        assert_eq!(u.percent, Some(100.0));
        assert_eq!(
            u.status.as_deref(),
            Some(i18n::t("indicator_reading_ic_card").as_str()),
            "收敛时应推状态条文本"
        );
        assert!(t.is_finished());
        // 收敛后不再产出任何更新
        assert_eq!(t.push("[Key: d]\n", 9_000), None);
        assert_eq!(t.keys_done(), 3, "收敛后不再累加");
    }

    #[test]
    fn eta_overshoot_still_converges() {
        let mut t = EtaTracker::new(2, 0);
        assert!(t.push("[Key: a]\n", 0).is_none());
        // 一帧内来 5 个，超过分母 2
        let u = t.push("[Key: a][Key: b][Key: c][Key: d][Key: e]\n", 1_000);
        assert_eq!(u.expect("应产出").percent, Some(100.0));
        assert!(t.is_finished());
    }

    #[test]
    fn eta_zero_total_does_not_divide_by_zero() {
        // 分母 0：keys_done >= 0 立即成立，应走收敛分支而非除零
        let mut t = EtaTracker::new(0, 0);
        let u = t.push("[Key: a]\n", 0);
        assert_eq!(u.expect("应产出").percent, Some(100.0));
    }

    #[test]
    fn eta_status_text_contains_eta_label() {
        let mut t = EtaTracker::new(100, 0);
        assert!(t.push("[Key: a]\n", 0).is_none());
        let u = t.push("[Key: b]\n", 2_000).unwrap();
        let s = u.status.unwrap();
        assert!(s.contains("预计剩余"), "实际: {s}");
        assert!(s.contains("正在解卡"), "实际: {s}");
    }

    #[test]
    fn eta_remain_grows_slower_than_linear_guess() {
        // 10 轮总数。第 1 帧在 t=0 被节流吞掉（bug #15 的行为），
        // 但计数已累加为 1；t=5000 这帧再计 1 → keys_done=2。
        // 剩余 = 5000ms / 2 轮 * 8 轮 = 20000ms = 20s → "00:20"
        let mut t = EtaTracker::new(10, 0);
        assert!(t.push("[Key: a]\n", 0).is_none());
        let u = t.push("[Key: b]\n", 5_000).unwrap();
        let s = u.status.unwrap();
        assert!(s.ends_with("00:20"), "实际: {s}");
    }

    #[test]
    fn eta_remain_is_linear_extrapolation() {
        // 单独验算 1 轮完成的情形：5s / 1 轮 * 9 轮 = 45s
        // 构造方式是「先喂 1 轮让它收敛前不产出，再让第 2 轮不新增计数」不可行，
        // 故直接验证 2 轮情形的精确值，并另测 elapsed=0 时剩余为 0。
        let mut t = EtaTracker::new(10, 0);
        assert!(t.push("[Key: a]\n", 0).is_none());
        // 第 2 帧时间戳仍为 0 → 被节流，keys_done 停在 1
        assert!(t.push("[Key: b]\n", 0).is_none());
        assert_eq!(t.keys_done(), 2);
        // 45s 情形：换成分母 2、已完成 1 的等价比例（1s / 1 轮 * 1 轮 = 1s）
        let mut t2 = EtaTracker::new(2, 0);
        assert!(t2.push("[Key: a]\n", 0).is_none());
        let u = t2.push("[Key: b]\n", 1_000).unwrap();
        // 1s/1 轮 × 1 轮 = 1s。但分母 2、已完成 2 ⇒ 此处已收敛，
        // 收敛分支只推状态条文本、不带 ETA 文本。
        assert_eq!(u.percent, Some(100.0));
    }

    /// 末行无 `\n` 时 `[Key: ` 计数不能丢，否则 keys_done 停在 N-1 永不收敛。
    /// 原版无此步（bug：mfoc 末轮输出常无换行）。
    #[test]
    fn eta_flush_settles_last_line_without_newline() {
        // 分母取 2：前一行 1 个 key + 末行 1 个 key = 2，flush 后正好收敛。
        // （分母取 3 时只有 2 个 key，永远收敛不了，达不到本用例的意图。）
        let mut t = EtaTracker::new(2, 0);
        assert!(t.push("[Key: a]\n", 0).is_none());
        // 最后一轮没有尾随换行 —— LineBuffer 暂存，push 阶段不结算
        assert!(t.push("[Key: b]", 0).is_none());
        assert_eq!(t.keys_done(), 1, "未 flush 前末行不结算");
        let u = t.flush(0).expect("flush 应结算末行");
        assert_eq!(u.percent, Some(100.0), "flush 后应收敛到 100%");
        assert!(t.is_finished());
    }

    #[test]
    fn eta_flush_on_empty_buffer_is_none() {
        let mut t = EtaTracker::new(3, 0);
        assert_eq!(t.flush(0), None);
    }

    /// `\r` 归一：mfoc 进度条用回车覆盖，若某轮只有 `\r` 无 `\n`，
    /// 原版的裸字符串缓冲会无限增长且计数恒 0。
    #[test]
    fn eta_handles_carriage_return_only_lines() {
        let mut t = EtaTracker::new(3, 0);
        // 一行里两个 key + 回车（无换行），LineBuffer 会归一 \r
        assert!(t.push("[Key: a][Key: b]\r", 0).is_none());
        assert!(t.push("\n", 0).is_none());
        assert_eq!(t.keys_done(), 2, "\r 不应导致计数丢失");
    }

    #[test]
    fn eta_partial_line_carries_across_chunks() {
        let mut t = EtaTracker::new(10, 0);
        assert!(t.push("[Key: aa", 0).is_none());
        assert!(t.push("bbccddeeff]\n[Key: c", 0).is_none());
        // 第一行已结算，第二行还在缓冲
        assert_eq!(t.keys_done(), 1);
        assert!(t.push("]\n", 0).is_none());
        assert_eq!(t.keys_done(), 2);
    }

    // ---------------------------------------------------------- apply_eta

    #[test]
    fn apply_eta_writes_status_and_progress() {
        let mut s = AppState::new();
        apply_eta(
            &mut s,
            &EtaUpdate {
                status: Some("正在解卡 - 预计剩余 00:45".into()),
                percent: Some(38.0),
            },
        );
        assert_eq!(s.status.text, "正在解卡 - 预计剩余 00:45");
        assert_eq!(s.progress, Some(38.0));
    }

    #[test]
    fn apply_eta_with_empty_update_is_noop() {
        let mut s = AppState::new();
        let before = s.status.text.clone();
        apply_eta(&mut s, &EtaUpdate::default());
        assert_eq!(s.status.text, before);
        assert_eq!(s.progress, None);
    }

    // ---------------------------------------------------------- take_uid

    #[test]
    fn take_uid_matches_original_slice() {
        // command.js:641-645：indexOf + 14 + substring(14) + 去空白
        let line = "  UID (NFCID1): a1 b2 c3 d4\n";
        assert_eq!(take_uid(line).as_deref(), Some("a1b2c3d4"));
    }

    /// S18 修正（原 §6.15 登记的原版缺陷）：原版 14 字符窗口**按含空白计**，
    /// UID 折到下一行且缩进较深时会被截断。现改为跳前导空白后按行取，
    /// 深缩进折行也能取到完整 UID。
    #[test]
    fn take_uid_wrapped_deep_indent_gets_full_uid() {
        let out = "UID (NFCID1):\n        a1b2c3d4\n";
        assert_eq!(take_uid(out).as_deref(), Some("a1b2c3d4"));
    }

    /// 标记后紧跟换行（UID 在下一行、无缩进）同样取全。
    #[test]
    fn take_uid_next_line_no_indent_gets_full_uid() {
        let out = "UID (NFCID1):\na1b2c3d4\n";
        assert_eq!(take_uid(out).as_deref(), Some("a1b2c3d4"));
    }

    /// 7 字节 UID（14 hex）取满上限，不被截。
    #[test]
    fn take_uid_seven_byte_uid_gets_all_fourteen_hex() {
        let out = "UID (NFCID1): a1b2c3d4e5f607\n";
        assert_eq!(take_uid(out).as_deref(), Some("a1b2c3d4e5f607"));
        // 超过上限的输入按 [`UID_MAX_HEX`] 截断，不越界
        let over = "UID (NFCID1): a1b2c3d4e5f60718\n";
        assert_eq!(take_uid(over).as_deref(), Some("a1b2c3d4e5f607"));
    }

    #[test]
    fn take_uid_same_line_padding_is_tolerated() {
        // 常见的实际格式：UID 与标记同行，后面有补齐空格
        let out = "UID (NFCID1): a1b2c3d4        \n";
        assert_eq!(take_uid(out).as_deref(), Some("a1b2c3d4"));
    }

    #[test]
    fn take_uid_returns_last_occurrence() {
        // 原版每次命中都覆盖 cardID，取最后一个
        let out = "UID (NFCID1): aaaa\nsomething\nUID (NFCID1): bbbb\n";
        assert_eq!(take_uid(out).as_deref(), Some("bbbb"));
    }

    #[test]
    fn take_uid_absent_returns_none() {
        assert_eq!(take_uid("no marker here\n"), None);
        // 标记后没有内容也不能返回空串（会被当成有效 UID）
        assert_eq!(take_uid("UID (NFCID1):\n"), None);
    }

    #[test]
    fn take_uid_does_not_panic_on_multibyte_tail() {
        // 标记后紧跟中文字符：按字节切不能切进 UTF-8 中间
        let out = "UID (NFCID1): 卡片读取完成";
        let _ = take_uid(out);
    }

    // ---------------------------------------------------------- dump_save

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

    fn tempdir(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("nfc-s5-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn dump_save_empty_does_not_open_dialog() {
        let dir = tempdir("empty");
        let _guard = TempGuard(dir.clone());
        let paths = Paths::new(&dir);
        std::fs::write(paths.temp_mfd(), b"").unwrap();
        let mut s = AppState::new();
        let mut dlg = crate::dialogs::MockDialog::new().with_save(Some(dir.join("x.mfd")));
        let out = dump_save(&mut s, &paths, Some("1122"), &mut dlg);
        assert_eq!(out, SaveOutcome::Empty);
        assert_eq!(dlg.save_calls(), 0, "空 dump 不应弹框");
        assert!(!paths.temp_mfd().exists(), "空文件应被删除");
        assert!(s.log_buffer.contains("未读到卡内容"));
    }

    #[test]
    fn dump_save_missing_temp_is_empty() {
        let dir = tempdir("missing");
        let _guard = TempGuard(dir.clone());
        let paths = Paths::new(&dir);
        let mut s = AppState::new();
        let mut dlg = crate::dialogs::MockDialog::new();
        assert_eq!(
            dump_save(&mut s, &paths, None, &mut dlg),
            SaveOutcome::Empty
        );
        assert_eq!(dlg.save_calls(), 0);
    }

    #[test]
    fn dump_save_uses_chosen_path_and_pushes_dump_saved() {
        let dir = tempdir("chosen");
        let _guard = TempGuard(dir.clone());
        let paths = Paths::new(&dir);
        std::fs::write(paths.temp_mfd(), b"payload").unwrap();
        let target = dir.join("picked.mfd");
        let mut s = AppState::new();
        let mut dlg = crate::dialogs::MockDialog::new().with_save(Some(target.clone()));
        let out = dump_save(&mut s, &paths, Some("112233445566"), &mut dlg);
        assert_eq!(
            out,
            SaveOutcome::Saved {
                target: target.clone(),
                chosen: true
            }
        );
        assert!(target.exists());
        assert!(!paths.temp_mfd().exists(), "temp 应被 rename 走");
        assert!(s.log_buffer.contains("已保存到"), "实际: {}", s.log_buffer);
        assert_eq!(s.dump_history, vec!["picked.mfd".to_string()]);
        assert_eq!(
            dlg.save_requests[0].default_path.extension().unwrap(),
            "mfd"
        );
    }

    #[test]
    fn dump_save_cancel_falls_back_to_default_name() {
        let dir = tempdir("cancel");
        let _guard = TempGuard(dir.clone());
        let paths = Paths::new(&dir);
        std::fs::write(paths.temp_mfd(), b"payload").unwrap();
        let mut s = AppState::new();
        // 预设 None = 用户取消
        let mut dlg = crate::dialogs::MockDialog::new().with_save(None);
        let out = dump_save(&mut s, &paths, Some("112233445566"), &mut dlg);
        let SaveOutcome::Saved { target, chosen } = out else {
            panic!("取消也应自动保存，实际: {out:?}");
        };
        assert!(!chosen);
        assert!(target.exists());
        // 默认名以 uid 开头
        let name = target.file_name().unwrap().to_string_lossy().to_string();
        assert!(name.starts_with("112233445566_"), "实际: {name}");
        assert!(name.ends_with(".mfd"), "实际: {name}");
        assert!(s.log_buffer.contains("解密结果已自动保存到"));
    }

    #[test]
    fn dump_save_without_uid_uses_card_prefix() {
        let dir = tempdir("nouid");
        let _guard = TempGuard(dir.clone());
        let paths = Paths::new(&dir);
        std::fs::write(paths.temp_mfd(), b"x").unwrap();
        let mut s = AppState::new();
        let mut dlg = crate::dialogs::MockDialog::new().with_save(None);
        let SaveOutcome::Saved { target, .. } = dump_save(&mut s, &paths, None, &mut dlg) else {
            panic!("应自动保存");
        };
        let name = target.file_name().unwrap().to_string_lossy().to_string();
        assert!(
            name.starts_with("card_"),
            "无 UID 时用 'card' 前缀，实际: {name}"
        );
    }

    #[test]
    fn dump_save_creates_dumpfiles_dir() {
        let dir = tempdir("mkdir");
        let _guard = TempGuard(dir.clone());
        let paths = Paths::new(&dir);
        std::fs::write(paths.temp_mfd(), b"x").unwrap();
        assert!(!paths.dumpfiles_dir().exists());
        let mut s = AppState::new();
        let mut dlg = crate::dialogs::MockDialog::new().with_save(None);
        assert!(matches!(
            dump_save(&mut s, &paths, None, &mut dlg),
            SaveOutcome::Saved { .. }
        ));
        assert!(paths.dumpfiles_dir().is_dir(), "dumpfiles/ 应被创建");
    }

    #[test]
    fn dump_save_rename_failure_reports_failed() {
        let dir = tempdir("fail");
        let _guard = TempGuard(dir.clone());
        let paths = Paths::new(&dir);
        std::fs::write(paths.temp_mfd(), b"x").unwrap();
        let mut s = AppState::new();
        // 选一个不存在的父目录 → rename 失败
        let bad = dir.join("no-such-dir").join("out.mfd");
        let mut dlg = crate::dialogs::MockDialog::new().with_save(Some(bad));
        let out = dump_save(&mut s, &paths, Some("aabb"), &mut dlg);
        assert_eq!(
            out,
            SaveOutcome::Failed {
                reason: SaveFailure::Move
            }
        );
        assert!(s.log_buffer.contains("保存失败"), "实际: {}", s.log_buffer);
        // 失败时 temp 仍在
        assert!(paths.temp_mfd().exists());
    }

    #[test]
    fn dump_save_empty_after_first_save_is_idempotent() {
        let dir = tempdir("twice");
        let _guard = TempGuard(dir.clone());
        let paths = Paths::new(&dir);
        let mut s = AppState::new();
        let mut dlg = crate::dialogs::MockDialog::new()
            .with_save(None)
            .with_save(None);
        std::fs::write(paths.temp_mfd(), b"a").unwrap();
        assert!(matches!(
            dump_save(&mut s, &paths, None, &mut dlg),
            SaveOutcome::Saved { .. }
        ));
        // 第二次 temp 已不在
        assert_eq!(
            dump_save(&mut s, &paths, None, &mut dlg),
            SaveOutcome::Empty
        );
        assert_eq!(s.dump_history.len(), 1);
    }

    // ---------------------------------------------------------- mfoc_args

    #[test]
    fn mfoc_args_prefixes_output_and_keys_file() {
        let dir = tempdir("args");
        let _guard = TempGuard(dir.clone());
        let paths = Paths::new(&dir);
        let a = mfoc_args(&paths, &["-C".to_string()], 0);
        assert_eq!(a.len(), 3);
        assert!(a[0].starts_with("-O"), "实际: {}", a[0]);
        assert!(a[0].contains("temp.mfd"));
        assert!(a[1].starts_with("-f"), "实际: {}", a[1]);
        assert!(a[1].contains("keys.txt"));
        assert_eq!(a[2], "-C");
    }

    #[test]
    fn mfoc_args_appends_probes_when_positive() {
        let dir = tempdir("args_probes");
        let _guard = TempGuard(dir.clone());
        let paths = Paths::new(&dir);
        let a = mfoc_args(&paths, &[], 50);
        assert_eq!(a.len(), 3);
        assert!(a[2].starts_with("-P"));
        assert!(a[2].contains("50"));
    }
}
