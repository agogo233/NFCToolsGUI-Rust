//! dump 数据模型：裸二进制 Mifare Classic 内存镜像，**零字节头**。
//!
//! 对标 `command.js:423-433`（bytes→扇区文本）与 `dumpEditor.js:88-119`（结构校验）。
//! 1024 字节 = 64 块 × 16B = **16 扇区** × 4 块 × 16B。
//!
//! 偏离登记见 docs/02 §6。

use thiserror::Error;

use super::hex;

pub type Sector = Vec<String>;

/// 块大小 = 16 字节。`command.js:427` 的 `.match(/.{1,32}/g)` 每 32 个 hex 字符切一段。
pub const BLOCK_SIZE: usize = 16;
/// 每扇区 4 块（block 0/1/2 + 3 为尾块）。`command.js:429` 的 `i += 4`。
pub const BLOCKS_PER_SECTOR: usize = 4;
/// 扇区 = 64 字节。
pub const SECTOR_SIZE: usize = BLOCK_SIZE * BLOCKS_PER_SECTOR;
/// M1 1K = 16 扇区 = 1024 字节。
pub const M1_SECTORS: usize = 16;
/// M1 1K 总字节数。
pub const M1_SIZE: usize = SECTOR_SIZE * M1_SECTORS;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum DumpError {
    #[error("dump 为空：原版 `''.match(/.{{1,32}}/g)` 返回 null 后 `forEach` 抛 TypeError")]
    Empty,
    #[error("dump 长度 {len} 不是 {expected} 的整数倍，无法切分扇区")]
    NotSectorAligned { len: usize, expected: usize },
    #[error("扇区 {sector} 有 {got} 行，应为 {expected} 行")]
    LineCount {
        sector: usize,
        got: usize,
        expected: usize,
    },
    #[error("扇区 {sector} 第 {line} 行有 {got} 个字符，应为 {expected} 个")]
    LineLength {
        sector: usize,
        line: usize,
        got: usize,
        expected: usize,
    },
    #[error("扇区 {sector} 第 {line} 行含非 hex 字符")]
    NonHex { sector: usize, line: usize },
    #[error("尾块行有 {got} 个字符，应为 {expected} 个")]
    TrailerLength { got: usize, expected: usize },
    #[error("扇区 0 块 0 有 {got} 个字符，应为 {expected} 个")]
    Block0Length { got: usize, expected: usize },
    #[error("dump 为空扇区列表，取不到扇区 0 的块 0")]
    NoBlock0,
}

/// bytes → 扇区 × 行（规范形式）。
///
/// 每行 32 个**小写** hex 字符，每扇区 4 行。比较器按字符下标 diff，故不做分隔。
pub fn to_sector_lines(bytes: &[u8]) -> Result<Vec<Sector>, DumpError> {
    if bytes.is_empty() {
        return Err(DumpError::Empty);
    }
    if !bytes.len().is_multiple_of(SECTOR_SIZE) {
        return Err(DumpError::NotSectorAligned {
            len: bytes.len(),
            expected: SECTOR_SIZE,
        });
    }
    Ok(bytes
        .chunks(SECTOR_SIZE)
        .map(|sector| sector.chunks(BLOCK_SIZE).map(hex::hex_encode).collect())
        .collect())
}

/// 扇区 × 行 → bytes（`to_sector_lines` 的逆运算）。
pub fn from_sector_lines(sectors: &[Sector]) -> Result<Vec<u8>, DumpError> {
    if sectors.is_empty() {
        return Err(DumpError::Empty);
    }
    let mut out = Vec::with_capacity(sectors.len() * SECTOR_SIZE);
    for (si, sector) in sectors.iter().enumerate() {
        if sector.len() != BLOCKS_PER_SECTOR {
            return Err(DumpError::LineCount {
                sector: si,
                got: sector.len(),
                expected: BLOCKS_PER_SECTOR,
            });
        }
        for (li, line) in sector.iter().enumerate() {
            // 先查行长：否则非法字符会被误报成「行太短」，且短行会静默产出短 dump
            // （正是 bug #7 要堵的口子，调用方若只调本函数不调 validate 就漏了）
            let got = line.chars().count();
            if got != BLOCK_SIZE * 2 {
                return Err(DumpError::LineLength {
                    sector: si,
                    line: li,
                    got,
                    expected: BLOCK_SIZE * 2,
                });
            }
            // 已保证偶数长度，OddLength 分支不可达但需保留以穷尽匹配
            out.extend(hex::hex_decode(line).map_err(|e| match e {
                hex::HexError::OddLength(n) => DumpError::LineLength {
                    sector: si,
                    line: li,
                    got: n,
                    expected: BLOCK_SIZE * 2,
                },
                hex::HexError::InvalidChar { .. } => DumpError::NonHex {
                    sector: si,
                    line: li,
                },
            })?);
        }
    }
    Ok(out)
}

/// 扇区 → 编辑器文本（4 行以 `\n` 连接）。
///
/// 对标 `command.js:430` 的 `hexDataArray.slice(i, i + 4).join('\n')`。
pub fn to_editor_text(sector: &Sector) -> String {
    sector.join("\n")
}

/// 结构 + 内容校验，对标 `dumpEditor.js:112-118` 的 `checkData(isCheckContent)`。
///
/// 逐条对应原版判定：`splitData.length === 4`（行数）、`data.length === 32`（行长）、
/// `/^[0-9A-Fa-f]+$/`（字符集，仅 `check_content` 为真时查）。
///
/// 偏离：`data.length` 在 JS 里是 UTF-16 码元数，本实现用 `chars().count()`。
/// 非 ASCII 字符在 `check_content = true` 时被字符集判定拦下，结论一致。
///
/// 另一处偏离：原版对**空扇区列表**返回 `true`（`.each` 空数组的 vacuous truth），
/// 本实现在 `from_sector_lines` 与 `to_sector_lines` 阶段就以 `DumpError::Empty` 拦下。
pub fn validate(sectors: &[Sector], check_content: bool) -> Result<(), DumpError> {
    for (si, sector) in sectors.iter().enumerate() {
        if sector.len() != BLOCKS_PER_SECTOR {
            return Err(DumpError::LineCount {
                sector: si,
                got: sector.len(),
                expected: BLOCKS_PER_SECTOR,
            });
        }
        for (li, line) in sector.iter().enumerate() {
            let got = line.chars().count();
            if got != BLOCK_SIZE * 2 {
                return Err(DumpError::LineLength {
                    sector: si,
                    line: li,
                    got,
                    expected: BLOCK_SIZE * 2,
                });
            }
            if check_content && !line.chars().all(|c| c.is_ascii_hexdigit()) {
                return Err(DumpError::NonHex {
                    sector: si,
                    line: li,
                });
            }
        }
    }
    Ok(())
}

/// 尾块（扇区的第 4 块）字段切分，对标 `dumpEditor.js:91-94` 的四段 `substring`。
///
/// Key A = [0,12) / Access Control = [12,18) / GPD = [18,20) / Key B = [20,32)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trailer {
    pub key_a: String,
    pub access: String,
    pub gpd: String,
    pub key_b: String,
}

pub fn split_trailer(line: &str) -> Result<Trailer, DumpError> {
    let c: Vec<char> = line.chars().collect();
    let expected = BLOCK_SIZE * 2;
    if c.len() != expected {
        return Err(DumpError::TrailerLength {
            got: c.len(),
            expected,
        });
    }
    let seg = |a: usize, b: usize| c[a..b].iter().collect::<String>();
    Ok(Trailer {
        key_a: seg(0, 12),
        access: seg(12, 18),
        gpd: seg(18, 20),
        key_b: seg(20, 32),
    })
}

/// 扇区 0 的块 0（厂商块），**大写** 32 hex。
///
/// 对标 `command.js:198` 的 `readFileSync(p).subarray(0, 16).toString("hex").toUpperCase()`，
/// 用于 `nfc-mfsetuid -q`。
pub fn block0_hex(sectors: &[Sector]) -> Result<String, DumpError> {
    let line = sectors
        .first()
        .and_then(|s| s.first())
        .ok_or(DumpError::NoBlock0)?;
    if line.chars().count() != BLOCK_SIZE * 2 {
        return Err(DumpError::Block0Length {
            got: line.chars().count(),
            expected: BLOCK_SIZE * 2,
        });
    }
    Ok(line.to_ascii_uppercase())
}

/// 7 字节 UID（扇区 0 块 0 的前 14 个 hex），用于 dump 文件名。
///
/// 与 mfoc `UID (NFCID1):` 行取到的 `cardID`（`command.js:645`，14 hex）等长。
pub fn uid_hex(sectors: &[Sector]) -> Result<String, DumpError> {
    Ok(block0_hex(sectors)?.chars().take(14).collect())
}

// ─────────────────────── 比较器 diff ───────────────────────

/// 一行 hex 的逐字符 diff 段。
///
/// 原版用 `<mark>` 标签把**不同的字符**逐个包起来
/// （`dumpComparator.js:46-55`），故差异粒度是字符而非行。
/// egui 无 `<mark>`，故拆成「连续相同段 / 单个不同字符」的交替序列，
/// 由 app 层决定如何着色。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffSeg {
    /// 相同的一段。
    Same(String),
    /// 一个不同的字符（原版每个不同字符各包一个 `<mark>`）。
    Diff(char),
}

impl DiffSeg {
    /// 该段是否为差异。
    pub fn is_diff(&self) -> bool {
        matches!(self, DiffSeg::Diff(_))
    }

    /// 段内字符。
    pub fn text(&self) -> String {
        match self {
            DiffSeg::Same(s) => s.clone(),
            DiffSeg::Diff(c) => c.to_string(),
        }
    }
}

/// 一个扇区的 diff 结果：4 行，每行若干段。
pub type SectorDiff = Vec<Vec<DiffSeg>>;

/// 逐字符 diff 两份 dump，对标 `dumpComparator.js:32-89` 的
/// `markDifferentStrings(dumpA, dumpB, 4, 32)`。
///
/// **长度不等时的行为**（原版 `:65-86`）：
/// - 只在 `min(len_a, len_b)` 个扇区上做逐字符比较
/// - 多出来的那一侧，**所有字符**都算差异（整行全 `<mark>`）
/// - 短的那一侧对应位置补空串 `""`（`:74` / `:83`）
///
/// 故返回的扇区数恒为 `max(len_a, len_b)`。
///
/// 另有原版的一个隐性行为：内层循环固定跑 `subArrLength = 4` 轮，
/// 若某扇区实际只有 3 行，`dumpA[i][3]` 是 `undefined`，
/// `undefined[k] !== undefined[k]` 为假 → 该行被当作"无差异"输出 `undefined`，
/// 渲染出字面量 "undefined"。本实现按 `min(4, 实际行数)` 走并补空串，
/// 不复刻该 bug；合法 dump 恒为 4 行，两者结果一致。
pub fn diff_sectors(a: &[Sector], b: &[Sector]) -> Vec<(SectorDiff, SectorDiff)> {
    let min_len = a.len().min(b.len());
    let mut out: Vec<(SectorDiff, SectorDiff)> = Vec::with_capacity(a.len().max(b.len()));

    // 公共部分：逐字符 diff
    for i in 0..min_len {
        let (Some(sa), Some(sb)) = (a.get(i), b.get(i)) else {
            // 理论不可达（i < min_len），但避免 panic 优先于穷尽证明
            break;
        };
        out.push((diff_sector(sa, sb), diff_sector(sb, sa)));
    }

    // a 多出来的扇区：a 侧全差异，b 侧补空串
    for sa in &a[min_len..] {
        out.push((all_diff(sa), blank_like(sa)));
    }
    // b 多出来的扇区：反之
    for sb in &b[min_len..] {
        out.push((blank_like(sb), all_diff(sb)));
    }
    out
}

/// 单扇区 diff。返回 `(a 的分段, b 的分段)`。
fn diff_sector(a: &Sector, b: &Sector) -> SectorDiff {
    let rows = a.len().min(b.len()).min(BLOCKS_PER_SECTOR);
    let mut out: SectorDiff = Vec::with_capacity(rows);
    for r in 0..rows {
        let (Some(la), Some(lb)) = (a.get(r), b.get(r)) else {
            break;
        };
        out.push(diff_line(la, lb));
    }
    // 行数不等时，短的一侧补空行（保持扇区恒 4 行，与原版 forEach 4 轮对齐）
    while out.len() < BLOCKS_PER_SECTOR {
        out.push(Vec::new());
    }
    out
}

/// 单行 diff：把连续相同的字符合成一段，不同的字符各自成段。
fn diff_line(a: &str, b: &str) -> Vec<DiffSeg> {
    let mut out: Vec<DiffSeg> = Vec::new();
    let mut run = String::new();
    for (ca, cb) in a.chars().zip(b.chars()) {
        if ca == cb {
            run.push(ca);
        } else {
            if !run.is_empty() {
                out.push(DiffSeg::Same(std::mem::take(&mut run)));
            }
            out.push(DiffSeg::Diff(ca));
        }
    }
    if !run.is_empty() {
        out.push(DiffSeg::Same(run));
    }
    out
}

/// 整行全差异。
fn all_diff(sector: &Sector) -> SectorDiff {
    sector
        .iter()
        .map(|line| line.chars().map(DiffSeg::Diff).collect())
        .collect()
}

/// 整行空（原版对不上的一侧给 `""`）。
fn blank_like(sector: &Sector) -> SectorDiff {
    sector.iter().map(|_| Vec::new()).collect()
}

/// 把分段还原成纯文本（用于自测与"仅列出差异块"的场景）。
pub fn segs_to_text(segs: &[DiffSeg]) -> String {
    segs.iter().map(DiffSeg::text).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 造一个 n 扇区的合法 dump。
    fn make(n: usize) -> Vec<u8> {
        (0..n * SECTOR_SIZE).map(|i| (i % 251) as u8).collect()
    }

    #[test]
    fn size_constants_match_original_grouping() {
        assert_eq!(BLOCK_SIZE, 16);
        assert_eq!(SECTOR_SIZE, 64);
        assert_eq!(M1_SIZE, 1024);
        assert_eq!(M1_SECTORS, 16);
        // command.js:429 的 i += 4：每扇区 4 块
        assert_eq!(BLOCKS_PER_SECTOR, 4);
    }

    #[test]
    fn m1_dump_maps_to_16_sectors_of_4_lines() {
        let sectors = to_sector_lines(&make(M1_SECTORS)).unwrap();
        assert_eq!(sectors.len(), 16);
        assert!(sectors.iter().all(|s| s.len() == 4));
        assert!(sectors
            .iter()
            .all(|s| s.iter().all(|l| l.chars().count() == 32)));
    }

    #[test]
    fn sector_0_layout_matches_known_m1_dump() {
        // 合成的字节 ramp，逐块核对偏移用（非取自原版样本）
        let b0: Vec<u8> = (0..64u8).collect();
        let sectors = to_sector_lines(&b0).unwrap();
        assert_eq!(sectors[0][0], "000102030405060708090a0b0c0d0e0f");
        // 块 3 = 字节 48..64
        assert_eq!(sectors[0][3], "303132333435363738393a3b3c3d3e3f");
    }

    #[test]
    fn roundtrip_is_byte_identical() {
        let raw = make(M1_SECTORS);
        let sectors = to_sector_lines(&raw).unwrap();
        assert_eq!(from_sector_lines(&sectors).unwrap(), raw);
    }

    #[test]
    fn roundtrip_over_4k_card_still_works() {
        let raw = make(64); // 4K = 64 扇区
        let sectors = to_sector_lines(&raw).unwrap();
        assert_eq!(sectors.len(), 64);
        assert_eq!(from_sector_lines(&sectors).unwrap(), raw);
    }

    #[test]
    fn rejects_empty_dump() {
        assert_eq!(to_sector_lines(&[]), Err(DumpError::Empty));
        assert_eq!(from_sector_lines(&[]), Err(DumpError::Empty));
    }

    #[test]
    fn rejects_length_not_multiple_of_64() {
        assert_eq!(
            to_sector_lines(&make(1)[..63]),
            Err(DumpError::NotSectorAligned {
                len: 63,
                expected: 64
            })
        );
    }

    #[test]
    fn validate_rejects_wrong_line_count() {
        let mut s = to_sector_lines(&make(1)).unwrap();
        s[0].pop();
        assert_eq!(
            validate(&s, true),
            Err(DumpError::LineCount {
                sector: 0,
                got: 3,
                expected: 4
            })
        );
    }

    #[test]
    fn validate_rejects_wrong_line_length() {
        let mut s = to_sector_lines(&make(1)).unwrap();
        s[0][1].push('0');
        assert!(matches!(
            validate(&s, true),
            Err(DumpError::LineLength {
                sector: 0,
                line: 1,
                got: 33,
                expected: 32
            })
        ));
    }

    #[test]
    fn validate_gates_non_hex_on_check_content_only() {
        let mut s = to_sector_lines(&make(1)).unwrap();
        s[0][2].replace_range(0..1, "z");
        // 原版 checkData() 不传参时只查结构
        assert_eq!(validate(&s, false), Ok(()));
        assert_eq!(
            validate(&s, true),
            Err(DumpError::NonHex { sector: 0, line: 2 })
        );
    }

    #[test]
    fn validate_accepts_real_m1_sector_zero_from_original_harness() {
        let sector = vec![
            "0000000000000000000000000000FFFF".to_string(),
            "4807E000000000000000000000000000".to_string(),
            "A0000000000000000000000000000000".to_string(),
            "FF078000000000000000000000000000".to_string(),
        ];
        assert_eq!(validate(std::slice::from_ref(&sector), true), Ok(()));
    }

    #[test]
    fn from_sector_lines_rejects_short_line() {
        // 不调 validate 时也要拦住，否则 4 行 × 2 字符会产出 4 字节「扇区」
        let bad = vec![vec!["00".to_string(); 4]];
        assert!(matches!(
            from_sector_lines(&bad),
            Err(DumpError::LineLength {
                sector: 0,
                line: 0,
                ..
            })
        ));
    }

    #[test]
    fn from_sector_lines_reports_non_hex_separately_from_length() {
        let bad = vec![vec![
            "0".repeat(32),
            "0".repeat(32),
            "0".repeat(32),
            format!("{}Z", "0".repeat(31)),
        ]];
        assert_eq!(
            from_sector_lines(&bad),
            Err(DumpError::NonHex { sector: 0, line: 3 })
        );
    }

    #[test]
    fn trailer_split_matches_substring_offsets() {
        let t = split_trailer("FF078000000000000000000000000000").unwrap();
        assert_eq!(t.key_a, "FF0780000000");
        assert_eq!(t.access, "000000");
        assert_eq!(t.gpd, "00");
        assert_eq!(t.key_b, "000000000000");
    }

    #[test]
    fn trailer_rejects_short_line() {
        assert!(matches!(
            split_trailer("FF0780"),
            Err(DumpError::TrailerLength {
                got: 6,
                expected: 32
            })
        ));
    }

    #[test]
    fn block0_is_uppercase_32_hex() {
        let s = to_sector_lines(&[0xab; SECTOR_SIZE]).unwrap();
        assert_eq!(block0_hex(&s).unwrap(), "ABABABABABABABABABABABABABABABAB");
    }

    #[test]
    fn uid_hex_is_14_chars_like_mfoc_nfcid1() {
        let s = to_sector_lines(&[0x11; SECTOR_SIZE]).unwrap();
        assert_eq!(uid_hex(&s).unwrap(), "11111111111111");
    }

    #[test]
    fn block0_on_empty_sector_list_is_error() {
        assert_eq!(block0_hex(&[]), Err(DumpError::NoBlock0));
    }

    #[test]
    fn editor_text_joins_four_lines_with_newline() {
        let s = to_sector_lines(&make(1)).unwrap();
        let text = to_editor_text(&s[0]);
        assert_eq!(text.lines().count(), 4);
        assert!(text.contains('\n'));
    }

    // ─────────────────── 比较器 diff ───────────────────

    /// 造一个指定长度的行。
    fn line(s: &str) -> String {
        s.to_owned()
    }

    /// 造一个 4 行的扇区，全用同一个 32 字符模板。
    fn sector(fill: &str) -> Sector {
        (0..BLOCKS_PER_SECTOR)
            .map(|_| line(&fill.repeat(BLOCK_SIZE * 2 / fill.len())))
            .collect()
    }

    #[test]
    fn diff_identical_dumps_have_no_diff_segments() {
        let a = to_sector_lines(&make(4)).unwrap();
        let b = a.clone();
        let d = diff_sectors(&a, &b);
        assert_eq!(d.len(), 4);
        for (da, db) in &d {
            for (ra, rb) in da.iter().zip(db) {
                assert!(!ra.iter().any(DiffSeg::is_diff), "a 侧出现差异段");
                assert!(!rb.iter().any(DiffSeg::is_diff), "b 侧出现差异段");
            }
        }
    }

    #[test]
    fn diff_marks_only_the_characters_that_differ() {
        // 32 字符行，仅最后一个字符不同 → 只有 1 个 Diff 段
        let a = vec![vec![line(&"a".repeat(32))]];
        let b = vec![vec![line(&("a".repeat(31) + "b"))]];
        let d = diff_sectors(&a, &b);
        let (da, db) = &d[0];
        let diffs_a: Vec<&DiffSeg> = da[0].iter().filter(|s| s.is_diff()).collect();
        assert_eq!(diffs_a.len(), 1, "应只有 1 个差异字符");
        assert_eq!(diffs_a[0].text(), "a");
        assert_eq!(db[0].iter().filter(|s| s.is_diff()).count(), 1);
        assert_eq!(db[0].iter().find(|s| s.is_diff()).unwrap().text(), "b");
        // 还原后应与原行一致
        assert_eq!(segs_to_text(&da[0]), "a".repeat(32));
        assert_eq!(segs_to_text(&db[0]), "a".repeat(31) + "b");
    }

    #[test]
    fn diff_marks_each_differing_char_separately_but_merges_same_runs() {
        // 前 16 字符不同、后 16 相同。
        //
        // 原版 `dumpComparator.js:46-58` 对每个不同字符各包一个 `<mark>`，
        // 相同字符则原样拼接（即不合并成段）。故**差异粒度 = 字符**，
        // 这里逐条对齐；而本实现把连续的相同字符合并成一个 `Same` 段，
        // 这是纯内部优化——渲染结果（哪些字符高亮）与原版逐字相同。
        let a = vec![vec![line(&"a".repeat(32))]];
        let b = vec![vec![line(&("b".repeat(16) + &"a".repeat(16)))]];
        let d = diff_sectors(&a, &b);
        let segs = &d[0].0[0];

        // 16 个不同字符 → 16 个 Diff 段（与原版 16 个 <mark> 一一对应）
        let diffs: Vec<&DiffSeg> = segs.iter().filter(|s| s.is_diff()).collect();
        assert_eq!(diffs.len(), 16, "差异字符数不符: {diffs:?}");
        assert!(diffs.iter().all(|s| s.text() == "a"));

        // 相同的 16 个字符合并成 1 个 Same 段
        let sames: Vec<&DiffSeg> = segs.iter().filter(|s| !s.is_diff()).collect();
        assert_eq!(sames.len(), 1, "相同字符应合并成 1 段");
        assert_eq!(sames[0].text(), "a".repeat(16));

        // 还原后必须与原行逐字相同
        assert_eq!(segs_to_text(segs), "a".repeat(32));
    }

    #[test]
    fn diff_shorter_dump_pads_longer_side_with_blank() {
        // 原版 dumpComparator.js:65-86：a 多出的扇区在 b 侧补空串
        let a = vec![sector("a"), sector("a"), sector("a")];
        let b = vec![sector("b")];
        let d = diff_sectors(&a, &b);
        assert_eq!(d.len(), 3, "扇区数应为 max(3,1) = 3");
        // 第 2、3 个扇区：a 侧全差异，b 侧空
        for (da, db) in &d[1..] {
            for r in 0..BLOCKS_PER_SECTOR {
                assert!(da[r].iter().all(DiffSeg::is_diff), "多出侧应全差异");
                assert!(db[r].is_empty(), "短侧应补空串");
            }
        }
    }

    #[test]
    fn diff_longer_b_side_marks_b() {
        let a = vec![sector("a")];
        let b = vec![sector("b"), sector("b")];
        let d = diff_sectors(&a, &b);
        assert_eq!(d.len(), 2);
        for r in 0..BLOCKS_PER_SECTOR {
            assert!(d[1].0[r].is_empty(), "a 侧应补空串");
            assert!(d[1].1[r].iter().all(DiffSeg::is_diff));
        }
    }

    #[test]
    fn diff_always_yields_four_rows_per_sector() {
        // 原版内层循环固定 4 轮，本实现对短扇区补空行以保持 4 行
        let a = vec![vec![line("aa")]];
        let b = vec![vec![line("bb")]];
        let d = diff_sectors(&a, &b);
        assert_eq!(d[0].0.len(), BLOCKS_PER_SECTOR);
        assert_eq!(d[0].1.len(), BLOCKS_PER_SECTOR);
    }

    #[test]
    fn diff_of_two_empty_dumps_is_empty() {
        assert!(diff_sectors(&[], &[]).is_empty());
    }
}
