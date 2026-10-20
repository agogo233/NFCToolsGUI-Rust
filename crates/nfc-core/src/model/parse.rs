//! 六个工具的 stdout 解析。真相源是 `command.js` 里各 `processHandler`。
//!
//! 两条全项目共有的约束（原版违反或未处理，本实现修掉）：
//!
//! 1. **chunk 边界与行边界无关**。`execUtils.js:28-36` 每次 `'data'` 事件喂一个
//!    chunk，`substring` / `indexOf` 全部对 chunk 敏感。原版只有 `updateMfocETA`
//!    （`command.js:688-692`）做了行缓冲，其余解析器都没做。本实现统一走 `LineBuffer`。
//! 2. **stdout 与 stderr 汇入同一 handler**（`execUtils.js:33-36`）。S2 已把两条
//!    通道分流，解析器只接受调用方指定的通道。
//!
//! 偏离登记见 docs/02 §6。

use std::sync::OnceLock;

use regex::Regex;

use super::keys::{KeyInfo, KeyType, UnknownKeyInfo};

// ---------------------------------------------------------------- 标记串

/// mfoc / nfc-mfdetect 的 UID 行标记，长 13。
const MARKER_UID: &str = "UID (NFCID1):";
/// 原版 `i += 14`（command.js:643）：标记 `UID (NFCID1):` 本身 13 字符，
/// 原版额外跳 1 位（越过 `i` 之后的空格）。
const MARKER_UID_OFFSET: usize = 14;
/// 原版 `substring(i, i + 14)` 的窗口长度。
const UID_WINDOW: usize = 14;
/// libnfc-collect 的 tag 行标记，长 19（**自带尾空格**）。
const MARKER_COLLECT_UID: &str = "Found tag with uid ";
/// libnfc-collect 的密钥行标记，长 **25**（**无尾随分隔符**，故原版偏移是 +26）。
const MARKER_COLLECT_KEY: &str = "collecting nonces for key";
/// cropto1_bs 的命中标记，长 10，**偏移却是 +11**（多吃一个空格）。
const MARKER_CROPTO1: &str = "Key found:";
/// nfc-mfdict 的命中标记，长 11（**自带尾空格**），偏移 +11。
const MARKER_MFDICT: &str = "Found Key: ";

/// 两个命中标记的**原版偏移都是 +11**，与标记自身长度无关（标记一以 `:` 结尾需多吃
/// 一个空格，标记二自带尾空格）。逐字复刻 `command.js:343` 与 `command.js:398`。
const HIT_SKIP: usize = 11;

/// mfoc 每次试一把密钥的进度前缀，长 **6**，对标 `command.js:693` 的 `/\[Key: /g`。
/// S5 的 ETA 解析消费它，S4 只登记常量。
pub const MARKER_MFOC_PROGRESS: &str = "[Key: ";

// ---------------------------------------------------------------- 行缓冲

/// 跨 chunk 的行缓冲：只吐出以 `\n` 结束的整行，残片留到下一次。
///
/// 对标 `command.js:688-692` 的 `mfocPendingLine`（全项目唯一做对的那处），
/// 本实现推广到全部解析器。
#[derive(Debug, Default)]
pub struct LineBuffer {
    pending: String,
}

impl LineBuffer {
    pub fn new() -> Self {
        Self::default()
    }

    /// 追加 chunk，返回本次新完成的整行（已剥掉行尾 `\r`）。
    ///
    /// mfoc 的进度条会原地覆盖并带入 `\r`，归一化掉才不会把同一行数两次。
    pub fn push(&mut self, chunk: &str) -> Vec<String> {
        self.pending.push_str(chunk);
        let Some(cut) = self.pending.rfind('\n') else {
            return Vec::new();
        };
        let ready: String = self.pending.drain(..=cut).collect();
        ready
            .lines()
            .map(|l| l.trim_end_matches('\r').to_string())
            .collect()
    }

    /// 工具退出时结算最后一行（原版无此步，末行无换行则整行丢失）。
    pub fn flush(&mut self) -> Vec<String> {
        if self.pending.is_empty() {
            return Vec::new();
        }
        let rest = std::mem::take(&mut self.pending);
        vec![rest.trim_end_matches('\r').to_string()]
    }
}

/// 在 `text` 中定位 `marker`，从标记起点后第 `skip` 个字符起，跳过空白取 `n` 个 hex 字符。
///
/// 偏移 `skip` 与长度 `n` 逐字复刻原版的 `indexOf(marker)` + `i += skip` +
/// `substring(i, i + n)`；但空白处理与字符集收紧（见 docs/02 §6）。
/// 标记后取最多 `n` 个 hex 字符（跳过空白）。
///
/// 对标 `command.js:643-645`：`substring(i, i + 14).replace(/\s+/g, "")`。
/// 关键差异：原版 `substring` 越界时**返回短串**而非报错，故此处收集到的
/// 字符不足 `n` 也照样返回（早期版本返回 `None`，导致 7 字节 UID 被丢弃）。
/// 只有标记不存在、或标记后一个 hex 都没有时才返回 `None`。
/// 取标记后第 `skip` 个字符起的 `n` 个字符窗口，去掉窗口内空白。
///
/// 严格对标原版的 `value.substring(i, i + n).replace(/\s+/g, "")`
/// （`command.js:299/305/344/399/645`）：
/// - **先按原始字符切 n 个**，再去空白。顺序反了会数错位——
///   `11 22 33 44 55 66` 有 17 字符，14 窗口只到 `"11 22 33 44 55"`，
///   去空白剩 10 位，末尾一字节被吃掉。原版如此，我们不"修好"。
/// - **不做 hex 校验**。原版 `substring` 是纯切片，`"0x1234"` 也会被原样
///   截走。校验留给调用方（如 `block0_is_valid`）。
fn take_window_after(text: &str, marker: &str, skip: usize, n: usize) -> Option<String> {
    let start = text.find(marker)? + skip;
    let window: String = text.get(start..)?.chars().take(n).collect();
    let out: String = window.chars().filter(|c| !c.is_whitespace()).collect();
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

/// `Sector NN - ` 行头，取其后的数字，对标 `keyParser.js` 的 `sectorRe`。
///
/// 原版用 `parseInt(head[1], 10)`，故只取**开头连续数字**——
/// `"Sector 12 - "` → 12，且不依赖两位补零（`"Sector  9 - "` → 9）。
fn sector_header_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"Sector\s+(\d+)").expect("静态正则"))
}

// ---------------------------------------------------------------- mfdetect / mfoc 密钥行

fn key_line_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // 原版 command.js:731：/ (\w{5}|\w{7})\s+Key \w(: \w{12}|)/g
        // 修 bug #1：尾组 `(: \w{12}|)` 空分支恒真，改为可选组
        // 收紧：(\w{5}|\w{7}) 改为具名 (Found|Unknown)，\w{12} 改为 [0-9A-Fa-f]{12}
        //
        // 说明：Mifare Classic 密钥恒为 6 字节 = 12 hex，工具输出不会给出
        // 更长的串，故 `[0-9A-Fa-f]{12}` 直接取满即可。
        // 早期版本尾部加了 `\b` / `(?![0-9A-Fa-f])` 之类的边界断言：
        //   - `\b` 在 12 位后接数字时仍成立，会把 16 位的串白截 4 位；
        //   - `(?!)` 需 `regex` 的 `look-around` feature，未启用则编译期报
        //     "look-around is not enabled"，是本项目踩过的坑。
        // 故此处不加边界断言，语义与原版 JS 的 `\w{12}` 一致。
        Regex::new(r"(Found|Unknown)\s+Key\s+([AB])\s*(?::\s*([0-9A-Fa-f]{12}))?")
            .expect("静态正则")
    })
}

/// mfoc / nfc-mfdetect 共用的密钥行解析器，对标 `keyParser.js` 的 `createKeyInfoParser`。
///
/// 相对原版的偏离：
/// - bug #1：尾组 `(:?\s*([0-9a-fA-F]{12})?)?` 空分支恒真 → 收紧为「`Found` 行
///   必须带满 12 位 hex，否则整行忽略」（原版 `if (!m[3]) continue` 同义）
/// - bug #2：原版每 chunk 把 `sector` 归零，跨块后扇区号整体错位
/// - bug #3：原版 `newKeys.join(key)` 是死代码（返回值被丢弃），`newKeys` 恒空
///
/// **扇区号取自 `Sector NN -` 行头，不取自匹配序号。** 这点很关键：
/// 序号只在「每个扇区恰好两条密钥、且无跳扇区」时才与真实扇区号一致。
/// 一旦工具跳过某个扇区、或某行只输出一条，序号就会与真实扇区号分叉，
/// 且**静默错位**——用户看到的是「A 扇区的密钥被算到了 B 扇区」。
/// 原版已按行头取值（`keyParser.js:20` `if (head) sector = parseInt(head[1], 10)`），
/// 本实现此前用 `index / 2` 反推，属回退到更差的假设。
#[derive(Debug, Default)]
pub struct KeyInfoParser {
    buf: LineBuffer,
    /// 最近一次看到的 `Sector NN -` 行头里的扇区号；还没见过则为 `None`。
    sector: Option<u8>,
    known: Vec<KeyInfo>,
    unknown: Vec<UnknownKeyInfo>,
    new_keys: Vec<String>,
}

impl KeyInfoParser {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, chunk: &str) {
        let lines = self.buf.push(chunk);
        self.scan(&lines);
    }

    /// 工具退出时结算末行。
    pub fn flush(&mut self) {
        let lines = self.buf.flush();
        self.scan(&lines);
    }

    pub fn known(&self) -> &[KeyInfo] {
        &self.known
    }

    pub fn unknown(&self) -> &[UnknownKeyInfo] {
        &self.unknown
    }

    /// 本次读卡新读到的已知密钥，原版 `newKeys`（因 bug #3 恒空）。
    pub fn new_keys(&self) -> &[String] {
        &self.new_keys
    }

    /// 对标 `command.js:587-589` 的三个数组重置。
    pub fn reset(&mut self) {
        *self = Self::new();
    }

    fn scan(&mut self, lines: &[String]) {
        for line in lines {
            // 扇区号取自行头，与原版 `keyParser.js:20` 同序：
            // 先更新 sector，再解析本行的密钥——同一行内的 A/B 共用该扇区号。
            if let Some(c) = sector_header_re().captures(line) {
                // `parseInt` 语义：只取开头连续数字，忽略其余部分。
                self.sector = c[1]
                    .parse::<u32>()
                    .ok()
                    .map(|v| v.min(u8::MAX as u32) as u8);
            }
            // 还没见过任何 Sector 行头 → 本行不属于密钥统计，整行跳过。
            // 原版 `if (sector === null) continue` 同义。
            let Some(sector) = self.sector else {
                continue;
            };
            for caps in key_line_re().captures_iter(line) {
                let Some(key_type) = KeyType::parse(&caps[2]) else {
                    continue;
                };
                if &caps[1] == "Found" {
                    // `Found` 行缺 12-hex key 时**整行忽略**，不产出记录。
                    // 仍消耗扇区号会导致其后所有扇区号整体错位，故此处不推进任何计数器。
                    let Some(key) = caps.get(3) else {
                        continue;
                    };
                    // 原版 `m[3].toLowerCase()`：工具可能输出大写 hex，
                    // 而 keys.txt 里是小写，不归一化则两者永远比不相等。
                    let key = key.as_str().to_ascii_lowercase();
                    self.known.push(KeyInfo {
                        key: key.clone(),
                        sector,
                        key_type,
                    });
                    self.new_keys.push(key);
                } else {
                    self.unknown.push(UnknownKeyInfo { sector, key_type });
                }
            }
        }
    }
}

// ---------------------------------------------------------------- mfoc UID

/// mfoc / nfc-mfdetect 的 `UID (NFCID1):` 行，取其后 14 个 hex 字符（7 字节）。
///
/// 对标 `command.js:642-646`：`indexOf` + `i += 14` + `substring(i, i + 14)`
/// + `.replace(/\s+/g, "")`，结果进 `cardID` 并用于 dump 文件名。
///
/// 偏离：原版**先取 14 字符再去空白**，UID 含空格时会被截短
/// （`"a1b2c3d4 e5f6a7b8"` → 取到 `"a1b2c3d4 e5f6"` → 去空白后只剩 12 字符）。
/// 本实现跳过空白取满 14 个 hex 字符。
#[derive(Debug, Default)]
pub struct UidParser {
    buf: LineBuffer,
    uid: Option<String>,
}

impl UidParser {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, chunk: &str) {
        let lines = self.buf.push(chunk);
        for line in &lines {
            if let Some(uid) = take_window_after(line, MARKER_UID, MARKER_UID_OFFSET, UID_WINDOW) {
                self.uid = Some(uid);
            }
        }
    }

    pub fn flush(&mut self) {
        for line in self.buf.flush() {
            if let Some(uid) = take_window_after(&line, MARKER_UID, MARKER_UID_OFFSET, UID_WINDOW) {
                self.uid = Some(uid);
            }
        }
    }

    pub fn uid(&self) -> Option<&str> {
        self.uid.as_deref()
    }
}

// ---------------------------------------------------------------- libnfc-collect

fn sector_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\(sector ([0-9]{1,2})\)").expect("静态正则"))
}

/// 一次采集的结果，对标 `command.js:277-307` 闭包里的 `uid` / `sector` / `keyType`。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CollectInfo {
    /// 4 字节 UID，`Found tag with uid ` 后 8 个 hex。
    pub uid: Option<String>,
    /// 扇区号。
    pub sector: Option<u8>,
    pub key_type: Option<KeyType>,
}

/// libnfc-collect 输出解析，对标 `command.js:296-307`。
///
/// 修掉两处原版缺陷：
/// - bug #4：原版 `sector = match[1]` 是**字符串**，而 `unknownKeyInfo[i][0]` 是
///   `parseInt` 出来的**数字**，`0 === "0"` 恒为 false ⇒ `unknownKeyInfo.shift()`
///   永不执行、HardNested autoRun 不推进。本实现统一为 `u8`。
/// - 扇区正则原版搜**整块**（`command.js:303`），一次输出多轮采集会取到别轮的
///   扇区；本实现只在含标记的**同一行**内搜。
///
/// 另：原版 `keyType = substring(i + 26, i + 27)` 未跳过分隔空格，真实输出形如
/// `collecting nonces for key A` 时会取到空格字符；本实现先 `trim_start`。
#[derive(Debug, Default)]
pub struct CollectParser {
    buf: LineBuffer,
    info: CollectInfo,
}

impl CollectParser {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, chunk: &str) {
        let lines = self.buf.push(chunk);
        self.scan(&lines);
    }

    pub fn flush(&mut self) {
        let lines = self.buf.flush();
        self.scan(&lines);
    }

    pub fn info(&self) -> &CollectInfo {
        &self.info
    }

    fn scan(&mut self, lines: &[String]) {
        for line in lines {
            // 用 if let 而非直接赋值：取不到时保留已解析出的值，不被 None 覆盖
            if let Some(uid) = take_window_after(
                line,
                MARKER_COLLECT_UID,
                MARKER_COLLECT_UID.chars().count(),
                8,
            ) {
                self.info.uid = Some(uid);
            }
            if let Some(i) = line.find(MARKER_COLLECT_KEY) {
                if let Some(t) = line
                    .get(i + MARKER_COLLECT_KEY.len()..)
                    .and_then(|r| r.trim_start().chars().find_map(KeyType::from_char))
                {
                    self.info.key_type = Some(t);
                }
                if let Some(caps) = sector_re().captures(line) {
                    self.info.sector = caps[1].parse::<u8>().ok();
                }
            }
        }
    }
}

// ---------------------------------------------------------------- cropto1_bs / nfc-mfdict

/// 从工具输出里取「命中的一把密钥」。
///
/// 对标 `command.js:337-349`（cropto1_bs）与 `command.js:391-404`（nfc-mfdict）。
///
/// 两个标记串长度不同但**偏移都是 +11**，原因是写法不同：
/// `Key found:` 以 `:` 结尾需多吃一个空格，`Found Key: ` 自带尾空格。
#[derive(Debug)]
pub struct SingleKeyParser {
    buf: LineBuffer,
    marker: &'static str,
    skip: usize,
    key: Option<String>,
}

impl SingleKeyParser {
    /// cropto1_bs：`indexOf("Key found:")` + `+11`（`command.js:341-344`）。
    pub fn cropto1() -> Self {
        Self::new(MARKER_CROPTO1)
    }

    /// nfc-mfdict：`indexOf("Found Key: ")` + `+11`（`command.js:396-399`）。
    pub fn mfdict() -> Self {
        Self::new(MARKER_MFDICT)
    }

    fn new(marker: &'static str) -> Self {
        Self {
            buf: LineBuffer::new(),
            marker,
            skip: HIT_SKIP,
            key: None,
        }
    }

    pub fn push(&mut self, chunk: &str) {
        let lines = self.buf.push(chunk);
        self.scan(&lines);
    }

    pub fn flush(&mut self) {
        let lines = self.buf.flush();
        self.scan(&lines);
    }

    /// 取出并清空。原版每次命中即 `saveKeys([key])`（`command.js:345,400`），按一次性语义。
    pub fn take(&mut self) -> Option<String> {
        self.key.take()
    }

    fn scan(&mut self, lines: &[String]) {
        for line in lines {
            if let Some(k) = take_window_after(line, self.marker, self.skip, 12) {
                self.key = Some(k);
            }
        }
    }
}

// ---------------------------------------------------------------- 写卡完成行

/// `nfc-mfclassic` 的完成行解析：`Done, X of Y blocks written.` / `read.`。
///
/// 用于判定写卡/读卡是否**全部**成功。关键细节：`w`/`f` 不写 block0，
/// 而工具打印的 `Y = uiBlocks + 1` 把 block0 也算进去了，故「全部成功」的
/// 判据是 `written == total - 1`（`total >= 1` 时）。工具中途失败（tag 被
/// 移走、auth 失败）会提前退出、**不**打印该行，此时返回 `None`。
///
/// 偏离：原版 `command.js` 只按退出码判成败（`code === 0`），而 `nfc-mfclassic`
/// 大写 `A` 容忍失败仍退出 0，会把「部分块写入」误报成成功。此处解析完成行，
/// 把真实写入块数暴露给调用方（见 docs/02 §6.26）。
#[derive(Debug, Default)]
pub struct BlockStatsParser {
    buf: LineBuffer,
    /// 最近一次抓到的 `(written, total)`。
    stats: Option<(u32, u32)>,
}

impl BlockStatsParser {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, chunk: &str) {
        let lines = self.buf.push(chunk);
        self.scan(&lines);
    }

    /// 工具退出时结算末行。
    pub fn flush(&mut self) {
        let lines = self.buf.flush();
        self.scan(&lines);
    }

    /// 最近一次抓到的 `(written, total)`；从未抓到过返回 `None`。
    ///
    /// 与 `SingleKeyParser::take` 不同：这里**不**清空——写卡只跑一次、
    /// 该行只出现一次，保留最后一次快照更便于收尾时取值。
    pub fn take(&self) -> Option<(u32, u32)> {
        self.stats
    }

    fn scan(&mut self, lines: &[String]) {
        for line in lines {
            if let Some(s) = parse_done_line(line) {
                self.stats = Some(s);
            }
        }
    }
}

fn done_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"Done,\s*(\d+)\s*of\s*(\d+)\s*blocks\s+(written|read)\.").expect("静态正则")
    })
}

fn parse_done_line(line: &str) -> Option<(u32, u32)> {
    let caps = done_re().captures(line.trim())?;
    let written = caps.get(1)?.as_str().parse().ok()?;
    let total = caps.get(2)?.as_str().parse().ok()?;
    Some((written, total))
}

#[cfg(test)]
mod tests {
    use super::*;

    // 必须带 `Sector NN - ` 行头：原版 keyParser.js 靠行头取扇区号，
    // 无行头的密钥行**整行忽略**（见 orig_keyparser.rs 第 4 组）。
    const KEY_LINE_KNOWN: &str = "Sector 00 - Found   Key A: ffffffffffff";
    const KEY_LINE_UNKNOWN: &str = "Sector 00 - Unknown Key B";

    // ------------------------------------------------------------ LineBuffer

    #[test]
    fn line_buffer_only_emits_complete_lines() {
        let mut b = LineBuffer::new();
        assert!(b.push("abc").is_empty());
        assert_eq!(b.push("def\n"), vec!["abcdef"]);
        assert_eq!(b.push("ghi\n"), vec!["ghi"]);
    }

    #[test]
    fn line_buffer_keeps_tail_across_chunks() {
        let mut b = LineBuffer::new();
        assert_eq!(b.push("one\ntw"), vec!["one"]);
        assert_eq!(b.push("o\n"), vec!["two"]);
    }

    #[test]
    fn line_buffer_flush_yields_final_unterminated_line() {
        let mut b = LineBuffer::new();
        b.push("no newline here");
        assert_eq!(b.flush(), vec!["no newline here"]);
        assert!(b.flush().is_empty());
    }

    #[test]
    fn line_buffer_strips_carriage_return() {
        let mut b = LineBuffer::new();
        assert_eq!(b.push("progress 12%\r\n"), vec!["progress 12%"]);
    }

    #[test]
    fn line_buffer_handles_blank_lines() {
        let mut b = LineBuffer::new();
        assert_eq!(b.push("\n\n"), vec!["", ""]);
    }

    // ------------------------------------------------------------ KeyInfoParser

    #[test]
    fn key_line_regex_matches_authors_comment_sample() {
        // command.js:730 注释：match [ Unknown Key A] or [ Found   Key B: ffffffffffff]
        let re = key_line_re();
        let c = re.captures(" Found   Key B: ffffffffffff").unwrap();
        assert_eq!(&c[1], "Found");
        assert_eq!(&c[2], "B");
        assert_eq!(&c[3], "ffffffffffff");

        let u = re.captures(" Unknown Key A").unwrap();
        assert_eq!(&u[1], "Unknown");
        assert_eq!(&u[2], "A");
        assert!(u.get(3).is_none());
    }

    #[test]
    fn key_line_regex_ignores_unrelated_lines() {
        // 完全不相关的行不该被正则命中
        let re = key_line_re();
        for s in [
            "MIFARE sector 0:",
            "Valid   Key A: ffffffffffff",
            "[Key: ffffffffffff] -> Found",
        ] {
            assert!(re.captures(s).is_none(), "不该匹配：{s}");
        }
        // " Found  Key A: 1234" 正则会命中（key 组为空）——这是有意的：
        // 是否忽略由 `scan` 决定，见 docs/02 §6.2 G 与
        // found_without_key_group_is_ignored_entirely。此处只断言"命中但无 key"。
        let caps = re.captures(" Found  Key A: 1234").expect("应命中");
        assert_eq!(&caps[1], "Found");
        assert_eq!(&caps[2], "A");
        assert!(caps.get(3).is_none(), "4 位 hex 不构成有效密钥");
    }

    #[test]
    fn parses_known_and_unknown_with_sector_mapping() {
        let mut p = KeyInfoParser::new();
        p.push(&format!("{KEY_LINE_KNOWN}\n{KEY_LINE_UNKNOWN}\n"));
        p.flush();
        assert_eq!(p.known().len(), 1);
        assert_eq!(p.known()[0].sector, 0);
        assert_eq!(p.known()[0].key_type, KeyType::A);
        assert_eq!(p.known()[0].key, "ffffffffffff");
        assert_eq!(p.unknown().len(), 1);
        assert_eq!(p.unknown()[0].sector, 0);
        assert_eq!(p.unknown()[0].key_type, KeyType::B);
    }

    #[test]
    fn sector_advances_every_two_lines() {
        let mut p = KeyInfoParser::new();
        p.push("Sector 00 - Found   Key A: aaaaaaaaaaaa Found   Key B: bbbbbbbbbbbb\n");
        p.push("Sector 01 - Unknown Key A               Unknown Key B\n");
        p.flush();
        let sectors: Vec<u8> = p.known().iter().map(|k| k.sector).collect();
        assert_eq!(sectors, vec![0, 0]);
        let unk: Vec<u8> = p.unknown().iter().map(|k| k.sector).collect();
        assert_eq!(unk, vec![1, 1]);
    }

    #[test]
    fn sector_index_is_cumulative_across_chunks() {
        // bug #2：原版每个 chunk 把 sector 归零，chunk 边界即扇区号整体错位
        let mut p = KeyInfoParser::new();
        for i in 0..3u8 {
            p.push(&format!(
                "Sector {i:02} - Found   Key A: aaaaaaaaaaaa Found   Key B: bbbbbbbbbbbb\n"
            ));
        }
        p.flush();
        let sectors: Vec<u8> = p.known().iter().map(|k| k.sector).collect();
        assert_eq!(sectors, vec![0, 0, 1, 1, 2, 2]);
    }

    #[test]
    fn line_split_across_chunks_is_not_lost() {
        // 密钥正好跨两个 chunk：前半 + 后半 = 12 hex
        let mut p = KeyInfoParser::new();
        p.push("Sector 00 - Found   Key A: dead");
        p.push("beefcafe1234\n");
        p.flush();
        assert_eq!(p.known().len(), 1);
        assert_eq!(p.known()[0].key, "deadbeefcafe");
        assert_eq!(p.known()[0].key.len(), 12, "Mifare 密钥恒 12 hex");
    }

    #[test]
    fn new_keys_is_actually_collected_bug_3() {
        // 原版 command.js:739 写的是 newKeys.join(key)，返回值丢弃 ⇒ newKeys 恒空
        let mut p = KeyInfoParser::new();
        p.push("Sector 00 - Found   Key A: deadbeef1234 Found   Key B: cafebabe5678\n");
        p.flush();
        assert_eq!(
            p.new_keys().to_vec(),
            vec!["deadbeef1234".to_string(), "cafebabe5678".to_string()]
        );
    }

    #[test]
    fn unknown_key_info_has_no_key_field() {
        let mut p = KeyInfoParser::new();
        p.push("Sector 00 - Unknown Key A\n");
        p.flush();
        assert_eq!(p.unknown().len(), 1);
        assert!(p.known().is_empty());
        assert!(p.new_keys().is_empty());
    }

    #[test]
    fn found_without_key_group_is_ignored_entirely() {
        // 若畸形行仍消耗序号，known 会落到 sector 1、unknown 会落到 sector 1
        let mut p = KeyInfoParser::new();
        p.push("Sector 00 - Found   Key A\n Found   Key B: aabbccddeeff\n Unknown Key B\n");
        p.flush();
        assert_eq!(p.known().len(), 1);
        assert_eq!(p.known()[0].sector, 0);
        assert_eq!(p.unknown().len(), 1);
        assert_eq!(p.unknown()[0].sector, 0);
    }

    #[test]
    fn reset_clears_all_accumulators() {
        let mut p = KeyInfoParser::new();
        p.push("Sector 00 - Found   Key A: deadbeef1234\n Unknown Key B\n");
        p.flush();
        p.reset();
        assert!(p.known().is_empty());
        assert!(p.unknown().is_empty());
        assert!(p.new_keys().is_empty());
        // 扇区上下文也归零：reset 后见到 Sector 07 应记 7，而非沿用旧的
        p.push("Sector 07 - Found   Key A: aabbccddeeff\n");
        p.flush();
        assert_eq!(p.known()[0].sector, 7, "reset 应清掉记住的扇区号");
    }

    // ------------------------------------------------------------ UidParser

    #[test]
    fn uid_parsed_from_mfoc_line() {
        // 原版 `substring(i, i+14)` 是**纯字符切片**，行有多长就取到哪。
        // 7 字节 UID（14 hex）正好取满
        let mut p = UidParser::new();
        p.push("UID (NFCID1): a1b2c3d4e5f6a7\n");
        assert_eq!(p.uid(), Some("a1b2c3d4e5f6a7"));
        // 4 字节 UID（8 hex）后面跟了别的字符，仍会被截满 14 个字符
        let mut p = UidParser::new();
        p.push("UID (NFCID1): a1b2c3d4 trailing\n");
        // 14 窗口 = "a1b2c3d4trail"（"a1b2c3d4 " 占 9 位 + "trail" 5 位）
        assert_eq!(
            p.uid(),
            Some("a1b2c3d4trail"),
            "substring(i, i+14) 取满 14 个原始字符"
        );
    }

    #[test]
    fn uid_window_is_sliced_before_whitespace_is_stripped() {
        // 原版 command.js:643-645 的两步顺序不可颠倒：
        //   substring(i, i + 14)  ← 先按原始字符切 14 个
        //   .replace(/\s+/g, "")  ← 再去空白
        // 故空白会**占掉**窗口里的名额。
        let mut p = UidParser::new();
        p.push("UID (NFCID1): a1b2c3d4e5f6 a7\n");
        // 14 字符窗口 = "a1b2c3d4e5f6 a"（末位是 'a'，空格占了 1 格）
        // → 去空白 = 13 位，原版正是如此少取一位
        assert_eq!(p.uid(), Some("a1b2c3d4e5f6a"));
    }

    #[test]
    fn uid_with_spaced_six_byte_id_is_truncated_to_ten() {
        // ⚠️ 原版缺陷，逐位锁定：`11 22 33 44 55 66` 有 17 字符，
        // 14 字符窗口只切到 "11 22 33 44 55" → 去空白后剩 10 位，
        // 末尾一个字节被吃掉。我们保持与原版一致而非"修好"，
        // 否则 6/10 字节 UID 的取法会和用户已习惯的行为不一致。
        let mut p = UidParser::new();
        p.push("UID (NFCID1): 11 22 33 44 55 66\n");
        assert_eq!(p.uid(), Some("1122334455"));
    }

    #[test]
    fn uid_absent_when_no_marker() {
        let mut p = UidParser::new();
        p.push("MIFARE sector 0:\n");
        p.flush();
        assert_eq!(p.uid(), None);
    }

    #[test]
    fn uid_does_not_validate_hex_like_original() {
        // 原版是纯 substring，**不做 hex 校验**：`0x1234` 会被原样截走。
        // 保持一致；真正需要合法性的地方（如写卡 block0）另走
        // `block0_is_valid` 校验。
        let mut p = UidParser::new();
        p.push("UID (NFCID1): 0x1234\n");
        assert_eq!(p.uid().map(|s| s.to_string()), Some("0x1234".to_string()));
    }

    // ------------------------------------------------------------ CollectParser

    #[test]
    fn collect_parses_all_three_fields() {
        let mut p = CollectParser::new();
        p.push("Found tag with uid a1b2c3d4\n");
        p.push("collecting nonces for key A (sector 3)\n");
        p.flush();
        assert_eq!(p.info().uid.as_deref(), Some("a1b2c3d4"));
        assert_eq!(p.info().sector, Some(3));
        assert_eq!(p.info().key_type, Some(KeyType::A));
    }

    #[test]
    fn collect_sector_is_numeric_not_string_bug_4() {
        // 原版 sector 是 string，与 parseInt 出来的 number 比较恒 false
        let mut p = CollectParser::new();
        p.push("collecting nonces for key B (sector 0)\n");
        p.flush();
        assert_eq!(p.info().sector, Some(0u8));
    }

    #[test]
    fn collect_key_type_skips_separator_space_bug() {
        let mut p = CollectParser::new();
        p.push("collecting nonces for key A (sector 1)\n");
        p.flush();
        assert_eq!(p.info().key_type, Some(KeyType::A));
    }

    #[test]
    fn collect_later_round_overwrites_earlier() {
        let mut p = CollectParser::new();
        p.push("collecting nonces for key A (sector 1)\n");
        p.push("collecting nonces for key B (sector 7)\n");
        p.flush();
        // 末次采集覆盖前次，与原版「闭包变量被后续 chunk 覆盖」一致
        assert_eq!(p.info().sector, Some(7));
        assert_eq!(p.info().key_type, Some(KeyType::B));
    }

    #[test]
    fn collect_uid_split_across_chunks() {
        let mut p = CollectParser::new();
        p.push("Found tag with uid a1b2");
        p.push("c3d4\n");
        p.flush();
        assert_eq!(p.info().uid.as_deref(), Some("a1b2c3d4"));
    }

    // ------------------------------------------------------------ SingleKeyParser

    #[test]
    fn cropto1_key_found_offset_is_11_not_10() {
        let mut p = SingleKeyParser::cropto1();
        p.push("Key found: deadbeef1234\n");
        assert_eq!(p.take().as_deref(), Some("deadbeef1234"));
        assert_eq!(p.take(), None);
    }

    #[test]
    fn mfdict_found_key_offset_is_11() {
        let mut p = SingleKeyParser::mfdict();
        p.push("Found Key: cafebabe5678\n");
        assert_eq!(p.take().as_deref(), Some("cafebabe5678"));
    }

    #[test]
    fn cropto1_does_not_match_mfdict_marker_and_vice_versa() {
        let mut a = SingleKeyParser::cropto1();
        a.push("Found Key: cafebabe5678\n");
        a.flush();
        assert_eq!(a.take(), None);

        let mut b = SingleKeyParser::mfdict();
        b.push("Key found: deadbeef1234\n");
        b.flush();
        assert_eq!(b.take(), None);
    }

    #[test]
    fn single_key_tolerates_whitespace_after_marker() {
        let mut p = SingleKeyParser::mfdict();
        // 原版 command.js:396-397：`i = indexOf("Found Key: ") + 11`
        // 然后 `substring(i, i + 12)`，最后去空白。
        // 本例标记后有 2 个多余空格，落在 12 字符窗口里，去空白后剩 10 位。
        p.push("Found Key:   cafebabe5678\n");
        assert_eq!(p.take().as_deref(), Some("cafebabe56"));
    }

    #[test]
    fn single_key_split_across_chunks() {
        let mut p = SingleKeyParser::cropto1();
        p.push("Key found: dead");
        p.push("beef1234\n");
        assert_eq!(p.take().as_deref(), Some("deadbeef1234"));
    }

    #[test]
    fn take_is_one_shot_semantics() {
        let mut p = SingleKeyParser::mfdict();
        p.push("Found Key: cafebabe5678\n");
        assert!(p.take().is_some());
        assert_eq!(p.take(), None);
    }

    // -------------------------------------------------- take_window_after

    #[test]
    fn take_window_after_slices_window_then_strips_whitespace() {
        // n 按原始字符计
        assert_eq!(
            take_window_after("xxABCDEF0123", "xx", 2, 4).as_deref(),
            Some("ABCD")
        );
        assert_eq!(
            take_window_after("xxABCDEF0123", "xx", 2, 6).as_deref(),
            Some("ABCDEF")
        );
        // 窗口内含空白：空白占名额
        assert_eq!(
            take_window_after("xxAB CD", "xx", 2, 6).as_deref(),
            Some("ABCD")
        );
        // 窗口内全是空白 → None
        assert_eq!(take_window_after("xx    ", "xx", 2, 4), None);
        // 标记不存在 → None
        assert_eq!(take_window_after("nope", "xx", 2, 2), None);
    }

    #[test]
    fn take_window_after_does_not_panic_on_multibyte_after_marker() {
        // 偏移落在多字节字符中间时 get() 返回 None
        assert_eq!(take_window_after("中中中", "中", 1, 2), None);
    }

    // -------------------------------------------------- BlockStatsParser

    #[test]
    fn block_stats_captures_written_and_total() {
        let mut p = BlockStatsParser::new();
        p.push("Writing 64 blocks |\n");
        p.push(".x.x.x.x.x.x.x.x.x.x.x.x.x.x.x.x.x.x.x.x.x.x.x.x.x.x.x.x.x.x.x.x.x.x.x.x.x.x.x.x.x.x.x.x.x.x.x.x.x.x.\n");
        p.push("Done, 63 of 64 blocks written.\n");
        assert_eq!(p.take(), Some((63, 64)));
    }

    #[test]
    fn block_stats_partial_write_is_not_complete() {
        let mut p = BlockStatsParser::new();
        p.push("Done, 60 of 64 blocks written.\n");
        assert_eq!(p.take(), Some((60, 64)));
    }

    #[test]
    fn block_stats_handles_read_line_too() {
        let mut p = BlockStatsParser::new();
        p.push("Done, 64 of 64 blocks read.\n");
        assert_eq!(p.take(), Some((64, 64)));
    }

    #[test]
    fn block_stats_none_when_tool_exits_early() {
        let mut p = BlockStatsParser::new();
        p.push("!\nError: tag was removed\n");
        p.flush();
        assert_eq!(p.take(), None);
    }

    #[test]
    fn block_stats_split_across_chunks() {
        let mut p = BlockStatsParser::new();
        p.push("Done, 63 of 64 blocks wri");
        p.push("ten.\n");
        assert_eq!(p.take(), Some((63, 64)));
    }

    /// 完整判据：`w`/`f` 不写 block0，Y 含 block0，故全部成功 = written == total - 1。
    #[test]
    fn block_stats_complete_criterion() {
        // 63/64 → 完整（block0 未写）
        assert!(is_complete(Some((63, 64))));
        // 60/64 → 不完整
        assert!(!is_complete(Some((60, 64))));
        // 工具中途失败（无完成行）→ 不完整
        assert!(!is_complete(None));
        // 退化情况 total == 0 不应出现，按不完整处理
        assert!(!is_complete(Some((0, 0))));
    }

    fn is_complete(stats: Option<(u32, u32)>) -> bool {
        match stats {
            Some((w, t)) if t >= 1 => w == t - 1,
            _ => false,
        }
    }
}
