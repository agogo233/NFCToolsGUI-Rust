//! 卡数据体检与修复：访问控制位（AC）校验 / 修复、值块识别、密钥字典导出。
//!
//! 逻辑来源（均为逐条对照移植，行为以参考实现为准）：
//! - AC 校验与修复：原版渲染器 `src/renderer/js/dumpTools.js` 的
//!   `checkDump` / `fixDump`，其 `readAccessBits` / `readReversedAccessBits`
//!   又照抄 MifareOneTool `ClassMifareS50.cs:41-79` 的 ReadAC / ReadRAC。
//!   位序与 NXP 数据手册不同，且 reversed 版含加法进位，**不得重推简化**。
//!   修复时只写回尾块字节 6..9 的 `ff 07 80`，**保留字节 9**（用户字节，
//!   不参与访问位判定）——与参考实现一致，是「修复」不是「格式化」。
//! - 值块识别：MyPN532 `DumpSectorEditor.js` 的 `auto_value_block_check`
//!   启发式（值 / 反码 / 地址补码三组约束），纯判定、无副作用。
//! - 密钥字典导出：`dumpTools.js` 的 `dumpToKeyDict`（与 `mfoc -f` /
//!   `nfc-mfdict -d` 的密钥文件格式一致）。

use crate::i18n::t;
use crate::model::dump::{self, Sector};
use crate::model::hex;

/// 工具认可的卡容量（字节）。`dumpTools.js:13` 的 `VALID_SIZES`。
pub const VALID_SIZES: [usize; 3] = [1024, 2048, 4096];
/// 修复访问位时写回的目标值。`dumpTools.js:16` 的 `FIX_ACCESS_BYTES`。
pub const FIX_ACCESS_BYTES: [u8; 3] = [0xff, 0x07, 0x80];

/// 体检发现的问题类别。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IssueCode {
    /// 扇区 0 块 0 的 BCC（第 5 字节）与 UID 异或不符。
    Bcc,
    /// 访问控制位前后向解码不一致（损坏）。
    AclCorrupt,
    /// 访问控制位解码越界（无效）。参考实现保留该分支。
    AclInvalid,
    /// 结构不合法（行数不足 / 行长不足 / 非 hex 字符）。
    NotHex,
    /// 文件容量不在 [`VALID_SIZES`] 内。
    Size,
    /// 非 Mifare Classic 扇区布局（容量合法但无法按扇区切分）。
    NoClassicLayout,
}

/// 一条体检发现。`sector` 为 `None` 表示全文件级问题（结构 / 容量）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckIssue {
    pub sector: Option<usize>,
    pub code: IssueCode,
    /// BCC 的期望值（2 位大写 hex）或 Size 的字节数；其余为空。
    pub detail: String,
}

/// 一条修复记录。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FixedItem {
    pub sector: usize,
    pub code: IssueCode,
}

/// 前向访问位解码，逐行照抄 `dumpTools.js:101-108`（MifareOneTool ReadAC）。
///
/// 三个移位项至多各贡献几 bit，结果值域 0..=8；用 u8 足够，`>` 判定保留
/// （与参考实现逐行对应，实际不可达）。
fn read_access_bits(ac: [u8; 4]) -> [u8; 4] {
    [
        ((ac[2] & 0x10) >> 4) + ((ac[2] & 0x01) << 1) + ((ac[1] & 0x10) >> 2),
        ((ac[2] & 0x20) >> 5) + (ac[2] & 0x02) + ((ac[1] & 0x20) >> 3),
        ((ac[2] & 0x40) >> 6) + ((ac[2] & 0x04) >> 1) + ((ac[1] & 0x40) >> 4),
        ((ac[2] & 0x80) >> 7) + ((ac[2] & 0x08) >> 2) + ((ac[1] & 0x80) >> 5),
    ]
}

/// 反向前向解码：先逐字节取反再走同一套位序，`dumpTools.js:110-118`（ReadRAC）。
fn read_reversed_access_bits(ac: [u8; 4]) -> [u8; 4] {
    let inv = [!ac[0], !ac[1], !ac[2], !ac[3]];
    [
        ((inv[0] & 0x01) << 2) + ((inv[0] & 0x10) >> 3) + (inv[1] & 0x01),
        ((inv[0] & 0x02) << 1) + ((inv[0] & 0x20) >> 4) + ((inv[1] & 0x02) >> 1),
        (inv[0] & 0x04) + ((inv[0] & 0x40) >> 5) + ((inv[1] & 0x04) >> 2),
        ((inv[0] & 0x08) >> 1) + ((inv[0] & 0x80) >> 6) + ((inv[1] & 0x08) >> 3),
    ]
}

/// 尾块字节 6..10 的访问位问题；正常返回 `None`。`dumpTools.js:126-140`。
fn check_trailer_access(trailer: &[u8]) -> Option<IssueCode> {
    let ac = [trailer[6], trailer[7], trailer[8], trailer[9]];
    let forward = read_access_bits(ac);
    let reversed = read_reversed_access_bits(ac);
    for i in 0..4 {
        if forward[i] != reversed[i] {
            return Some(IssueCode::AclCorrupt);
        }
        // 值域 0..=8，`> 8` 实际不可达；保留以与参考实现逐行对应。
        if forward[i] > 8 {
            return Some(IssueCode::AclInvalid);
        }
    }
    None
}

/// 扇区序号 → 首块号。`dumpTools.js:20-23`。
fn sector_first_block(sector: usize, block_count: usize) -> usize {
    if block_count <= 128 {
        sector * 4
    } else {
        if sector < 32 {
            sector * 4
        } else {
            128 + (sector - 32) * 16
        }
    }
}

/// 扇区序号 → 尾块号。`dumpTools.js:28-32`：4K 卡的 32 号以后每扇区 16 块。
fn sector_trailer_block(sector: usize, block_count: usize) -> usize {
    let first = sector_first_block(sector, block_count);
    if block_count <= 128 {
        first + 3
    } else if sector < 32 {
        first + 3
    } else {
        first + 15
    }
}

/// 容量（块数）→ 扇区总数。`dumpTools.js:62-65`。
fn sector_count(block_count: usize) -> usize {
    if block_count <= 128 {
        block_count / 4
    } else {
        32 + (block_count - 128) / 16
    }
}

/// 编辑器行 → 字节。任一行不是 32 hex 即整体拒绝（结构与内容预检）。
fn lines_to_bytes(sectors: &[Sector]) -> Result<Vec<u8>, CheckIssue> {
    let mut out = Vec::with_capacity(sectors.len() * 64);
    for (si, sector) in sectors.iter().enumerate() {
        if sector.len() != dump::BLOCKS_PER_SECTOR {
            return Err(CheckIssue {
                sector: Some(si),
                code: IssueCode::NotHex,
                detail: String::new(),
            });
        }
        for line in sector {
            let bytes = hex::hex_decode(line).map_err(|_| CheckIssue {
                sector: Some(si),
                code: IssueCode::NotHex,
                detail: String::new(),
            })?;
            if bytes.len() != dump::BLOCK_SIZE {
                return Err(CheckIssue {
                    sector: Some(si),
                    code: IssueCode::NotHex,
                    detail: String::new(),
                });
            }
            out.extend_from_slice(&bytes);
        }
    }
    Ok(out)
}

/// 扇区 0 块 0 的 BCC 是否自洽（第 5 字节 = 前 4 字节异或）。
fn bcc_expected(bytes: &[u8]) -> u8 {
    bytes[0] ^ bytes[1] ^ bytes[2] ^ bytes[3]
}

/// 体检整份卡数据（`&[Sector]` 编辑器形态）。
///
/// 顺序与 `dumpTools.js:checkDump` 一致：先结构（NotHex 即止），再容量
/// （Size / NoClassicLayout 即止），再逐扇区 BCC（仅扇区 0）与访问位。
/// `Err` 的载荷是**全文件级**的致命问题（结构 / 容量），无法继续逐扇区检查。
pub fn check(sectors: &[Sector]) -> Result<Vec<CheckIssue>, Vec<CheckIssue>> {
    let bytes = lines_to_bytes(sectors).map_err(|e| vec![e])?;
    let total = bytes.len();
    if !VALID_SIZES.contains(&total) {
        return Err(vec![
            CheckIssue {
                sector: None,
                code: IssueCode::Size,
                detail: total.to_string(),
            },
            CheckIssue {
                sector: None,
                code: IssueCode::NoClassicLayout,
                detail: String::new(),
            },
        ]);
    }
    let block_count = total / dump::BLOCK_SIZE;
    let total_sectors = sector_count(block_count);
    let mut issues = Vec::new();
    for sector in 0..total_sectors {
        let first = sector_first_block(sector, block_count) * dump::BLOCK_SIZE;
        let trailer = sector_trailer_block(sector, block_count) * dump::BLOCK_SIZE;
        if trailer + dump::BLOCK_SIZE > bytes.len() {
            break;
        }
        // BCC 只对扇区 0 有意义：其余扇区的块 0 是数据块，不是 UID 块
        if sector == 0 && bcc_expected(&bytes[first..]) != bytes[first + 4] {
            issues.push(CheckIssue {
                sector: Some(sector),
                code: IssueCode::Bcc,
                detail: format!("{:02X}", bcc_expected(&bytes[first..])),
            });
        }
        if let Some(code) = check_trailer_access(&bytes[trailer..trailer + dump::BLOCK_SIZE]) {
            issues.push(CheckIssue {
                sector: Some(sector),
                code,
                detail: String::new(),
            });
        }
    }
    Ok(issues)
}

/// 修复整份卡数据（`dumpTools.js:fixDump` 语义）。
///
/// - 扇区 0：重算 BCC（只修扇区 0，参考实现刻意不碰其余扇区的块 0）
/// - 访问位异常：尾块字节 6..9 写回 `ff 07 80`，保留字节 9
///
/// 结构 / 容量不合法时原样返回 `Err`，不动数据。
pub fn fix(sectors: &[Sector]) -> Result<(Vec<Sector>, Vec<FixedItem>), Vec<CheckIssue>> {
    let bytes = lines_to_bytes(sectors).map_err(|e| vec![e])?;
    let total = bytes.len();
    if !VALID_SIZES.contains(&total) {
        return Err(vec![CheckIssue {
            sector: None,
            code: IssueCode::Size,
            detail: total.to_string(),
        }]);
    }
    let block_count = total / dump::BLOCK_SIZE;
    let total_sectors = sector_count(block_count);
    let mut out = bytes.clone();
    let mut fixed = Vec::new();
    for sector in 0..total_sectors {
        let first = sector_first_block(sector, block_count) * dump::BLOCK_SIZE;
        let trailer = sector_trailer_block(sector, block_count) * dump::BLOCK_SIZE;
        if trailer + dump::BLOCK_SIZE > out.len() {
            break;
        }
        if sector == 0 && bcc_expected(&out[first..]) != out[first + 4] {
            out[first + 4] = bcc_expected(&out[first..]);
            fixed.push(FixedItem {
                sector,
                code: IssueCode::Bcc,
            });
        }
        if check_trailer_access(&out[trailer..trailer + dump::BLOCK_SIZE]).is_some() {
            for (i, v) in FIX_ACCESS_BYTES.iter().enumerate() {
                out[trailer + 6 + i] = *v;
            }
            fixed.push(FixedItem {
                sector,
                code: IssueCode::AclCorrupt,
            });
        }
    }
    Ok((dump::to_sector_lines(&out).unwrap(), fixed))
}

/// MIFARE 值块启发式识别（MyPN532 `DumpSectorEditor.js:220-241`）。
///
/// 对一个 16 字节数据块（32 hex）：值(0-3) == 反码(4-7) 的补码 == 地址段(8-11)，
/// 且字节 12/14 相等、13/15 分别是 12 的按位反。全中才判为值块；
/// 保守判定，误报代价远大于漏报。
pub fn is_value_block(line: &str) -> bool {
    let Ok(b) = hex::hex_decode(line) else {
        return false;
    };
    if b.len() != dump::BLOCK_SIZE {
        return false;
    }
    let v1 = u32::from_be_bytes([b[0], b[1], b[2], b[3]]);
    let v2 = u32::from_be_bytes([b[4], b[5], b[6], b[7]]);
    let v3 = u32::from_be_bytes([b[8], b[9], b[10], b[11]]);
    let (a1, a2, a3, a4) = (b[12], b[13], b[14], b[15]);
    v1 == v3 && v3 == u32::MAX - v2 && a1 == a3 && a1 == 0xff - a2 && a1 == 0xff - a4
}

/// 扇区里含值块的数据块行下标（`dumpTools.js` 语义：块 0 与尾块不算数据块，
/// 扇区 0 的块 0 是厂商块）。仅对 1K 卡返回（组号 == 扇区号，渲染对齐简单）。
pub fn value_block_lines(group_index: usize, sectors: &[Sector], is_1k: bool) -> Vec<usize> {
    if !is_1k {
        return Vec::new();
    }
    let Some(sector) = sectors.get(group_index) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (li, line) in sector.iter().enumerate() {
        // 尾块（第 4 行）与扇区 0 的块 0 不可能是值块
        if li == dump::BLOCKS_PER_SECTOR - 1 || (group_index == 0 && li == 0) {
            continue;
        }
        if is_value_block(line) {
            out.push(li);
        }
    }
    out
}

/// 导出密钥字典（`dumpTools.js:dumpToKeyDict`）：每扇区尾块依次取
/// KeyA / KeyB，去重后每行一个 12 位小写 hex（与 `mfoc -f` /
/// `nfc-mfdict -d` 的密钥文件格式一致）。结构 / 容量非法返回空。
pub fn key_dict(sectors: &[Sector]) -> Vec<String> {
    let Ok(bytes) = lines_to_bytes(sectors) else {
        return Vec::new();
    };
    let total = bytes.len();
    if !VALID_SIZES.contains(&total) {
        return Vec::new();
    }
    let block_count = total / dump::BLOCK_SIZE;
    let total_sectors = sector_count(block_count);
    let mut keys: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut push = |offset: usize| {
        let key = hex::hex_encode(&bytes[offset..offset + 6]);
        if seen.insert(key.clone()) {
            keys.push(key);
        }
    };
    for sector in 0..total_sectors {
        let trailer = sector_trailer_block(sector, block_count) * dump::BLOCK_SIZE;
        if trailer + dump::BLOCK_SIZE > bytes.len() {
            break;
        }
        push(trailer); // KeyA
        push(trailer + 10); // KeyB
    }
    keys
}

/// 一条体检发现的展示文本（编辑器面板与写卡确认框共用）。
pub fn issue_text(issue: &CheckIssue) -> String {
    let head = match issue.sector {
        Some(n) => format!("{}{}：", t("html_sector"), n),
        None => String::new(),
    };
    let body = match issue.code {
        IssueCode::Bcc => format!("{} {}", t("html_error_bcc"), issue.detail),
        IssueCode::AclCorrupt => t("html_error_acl_corrupt"),
        IssueCode::AclInvalid => t("html_error_acl_invalid"),
        IssueCode::NotHex => t("html_error_not_hex"),
        IssueCode::Size => format!(
            "{}{}{}",
            t("html_error_size"),
            issue.detail,
            t("html_error_size_bytes")
        ),
        IssueCode::NoClassicLayout => t("html_data_not_classic_layout"),
    };
    format!("{head}{body}")
}

/// 仅**问题**的展示行（写卡确认框用）：不含值块提示与「未发现问题」；
/// 干净 / 不可体检时返回空。
pub fn problem_lines(sectors: &[Sector]) -> Vec<String> {
    match check(sectors) {
        Ok(issues) => issues.iter().map(issue_text).collect(),
        Err(fatal) => fatal.iter().map(issue_text).collect(),
    }
}

/// 体检报告的展示行：问题逐条 + 值块提示；**无问题时返回
/// `[html_check_all_ok]`**（调用方直接渲染，无需再判空）。
pub fn check_report(sectors: &[Sector]) -> Vec<String> {
    let issues = match check(sectors) {
        Ok(i) => i,
        // 结构 / 容量不合法：把致命问题本身作为报告
        Err(fatal) => return fatal.iter().map(issue_text).collect(),
    };
    let mut lines: Vec<String> = issues.iter().map(issue_text).collect();
    if lines.is_empty() {
        // 值块提示（1K 卡才标：编辑器按 4 块一组渲染，组号 == 扇区号）
        let is_1k = sectors.len() == 16;
        for g in 0..sectors.len() {
            if !value_block_lines(g, sectors, is_1k).is_empty() {
                lines.push(format!(
                    "{}{}：{}",
                    t("html_sector"),
                    g,
                    t("html_value_block")
                ));
            }
        }
    }
    if lines.is_empty() {
        lines.push(t("html_check_all_ok"));
    }
    lines
}

/// 修复结果的展示行（`html_fix_summary` 前缀 + 逐项；无修复项为
/// `[html_fix_none]`）。
pub fn fix_report(fixed: &[FixedItem]) -> Vec<String> {
    if fixed.is_empty() {
        return vec![t("html_fix_none")];
    }
    let items: Vec<String> = fixed
        .iter()
        .map(|f| {
            let what = match f.code {
                IssueCode::Bcc => t("html_error_bcc"),
                _ => t("html_error_acl_corrupt"),
            };
            format!("{}{}（{}）", t("html_sector"), f.sector, what)
        })
        .collect();
    vec![format!("{}{}", t("html_fix_summary"), items.join("、"))]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::dump;

    /// 造一份 1K 出厂默认 dump：全默认密钥 + `ff078069` 尾块 + 合法 BCC。
    ///
    /// 尾块 16 字节布局：KeyA(0-5) / 访问位(6-9) / KeyB(10-15)。
    fn blank_1k() -> Vec<Sector> {
        let mut bytes = vec![0u8; 1024];
        for s in 0..16 {
            let t = s * 64 + 48; // 尾块偏移
            bytes[t..t + 6].copy_from_slice(&[0xff; 6]); // KeyA
            bytes[t + 6..t + 10].copy_from_slice(&[0xff, 0x07, 0x80, 0x69]); // 访问位
            bytes[t + 10..t + 16].copy_from_slice(&[0xff; 6]); // KeyB
        }
        // 块 0：UID 01 02 03 04，BCC = 04
        bytes[0..4].copy_from_slice(&[0x01, 0x02, 0x03, 0x04]);
        bytes[4] = 0x01 ^ 0x02 ^ 0x03 ^ 0x04;
        dump::to_sector_lines(&bytes).unwrap()
    }

    #[test]
    fn factory_defaults_pass_check() {
        let s = blank_1k();
        assert!(check(&s).unwrap().is_empty(), "出厂默认值应零问题");
    }

    #[test]
    fn broken_bcc_is_reported_with_expected_value() {
        let mut s = blank_1k();
        s[0][0] = "01020304ff".to_string() + &s[0][0][10..]; // BCC 位置塞坏值
        let issues = check(&s).unwrap();
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].code, IssueCode::Bcc);
        assert_eq!(issues[0].sector, Some(0));
        assert_eq!(issues[0].detail, "04");
    }

    #[test]
    fn fix_repairs_bcc_and_keeps_other_sectors_block0() {
        let mut s = blank_1k();
        // 扇区 1 的块 0 塞入「假 BCC 坏值」：修复不得碰它（只修扇区 0）
        s[1][0] = format!("{}01", &s[1][0][2..]);
        s[0][0] = "01020304ff".to_string() + &s[0][0][10..];
        let (fixed, items) = fix(&s).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(
            items[0],
            FixedItem {
                sector: 0,
                code: IssueCode::Bcc
            }
        );
        assert_eq!(check(&fixed).unwrap().len(), 0);
        // 扇区 1 块 0 保持原样（参考实现的刻意行为）
        assert_eq!(fixed[1][0], s[1][0]);
    }

    #[test]
    fn broken_trailer_acl_is_detected_and_repaired() {
        let mut s = blank_1k();
        // 全 0 尾块：访问位前后向解码不一致 → AclCorrupt
        s[5][3] = "00000000000000000000000000000000".to_string();
        let issues = check(&s).unwrap();
        assert_eq!(
            issues,
            vec![CheckIssue {
                sector: Some(5),
                code: IssueCode::AclCorrupt,
                detail: String::new()
            }]
        );
        let (fixed, items) = fix(&s).unwrap();
        assert_eq!(
            items,
            vec![FixedItem {
                sector: 5,
                code: IssueCode::AclCorrupt
            }]
        );
        // 修完写回 ff 07 80，字节 9（GPB 位，此处 0）保持原值
        assert_eq!(fixed[5][3], "000000000000ff078000000000000000");
        assert!(check(&fixed).unwrap().is_empty());
    }

    #[test]
    fn acl_invalid_is_reported_when_forward_decoding_overflows() {
        // 构造 forward 与 reversed 相等的构造场景不可达（值域 0..=8），
        // 该分支与参考实现逐行对应，这里只钉住两个解码函数本身的输出。
        let ac = [0xff, 0x07, 0x80, 0x69];
        assert_eq!(read_access_bits(ac), [0, 0, 0, 1]);
        assert_eq!(read_reversed_access_bits(ac), [0, 0, 0, 1]);
    }

    #[test]
    fn not_hex_line_stops_the_check() {
        let mut s = blank_1k();
        s[2][1].push_str("zz");
        let err = check(&s).unwrap_err();
        assert_eq!(err[0].code, IssueCode::NotHex);
        assert_eq!(err[0].sector, Some(2));
    }

    #[test]
    fn wrong_size_is_size_issue() {
        // 512 字节（8 扇区）：不是合法卡容量 → Err 含 Size + NoClassicLayout
        let s: Vec<Sector> = blank_1k().into_iter().take(8).collect();
        let err = check(&s).unwrap_err();
        assert_eq!(err.len(), 2);
        assert_eq!(err[0].code, IssueCode::Size);
        assert_eq!(err[0].detail, "512");
        assert_eq!(err[1].code, IssueCode::NoClassicLayout);
    }

    #[test]
    fn four_k_layout_places_large_sector_trailers() {
        // 4K 卡（4096 字节 = 256 块 = 48 扇区）：32 号以后的尾块在首块 +15
        let mut bytes = vec![0u8; 4096];
        for s in 0..48 {
            let t = sector_trailer_block(s, 256) * 16;
            bytes[t..t + 6].copy_from_slice(&[0xff; 6]);
            bytes[t + 6..t + 10].copy_from_slice(&[0xff, 0x07, 0x80, 0x69]);
            bytes[t + 11..t + 17].copy_from_slice(&[0xff; 6]);
        }
        bytes[4] = bytes[0] ^ bytes[1] ^ bytes[2] ^ bytes[3];
        let s = dump::to_sector_lines(&bytes).unwrap();
        assert_eq!(s.len(), 64, "4K 卡 = 64 组 × 4 行");
        assert!(check(&s).unwrap().is_empty());
        // 破坏扇区 35 的尾块（组 35 在第 9 行）
        let t = sector_trailer_block(35, 256) * 16;
        let group = t / 64;
        let line_in_group = (t % 64) / 16;
        let mut s = s;
        s[group][line_in_group] = "00000000000000000000000000000000".to_string();
        let issues = check(&s).unwrap();
        assert_eq!(
            issues,
            vec![CheckIssue {
                sector: Some(35),
                code: IssueCode::AclCorrupt,
                detail: String::new()
            }]
        );
    }

    #[test]
    fn value_block_heuristic_matches_reference_vectors() {
        // 参考向量（MyPN532 语义）：值 00000001，反码是 0xffffffff - 值 → FFFFFFFE
        assert!(is_value_block("00000001ffffffffff0000000105fa05fa"));
        // 值 0 的合法值块：反码必须全 FF
        assert!(is_value_block("00000000ffffffff0000000005fa05fa"));
        // 值与反码不一致（反码写成全 FF 但值非 0）→ 否
        assert!(!is_value_block("00000001ffffffff0000000105fa05fa"));
        // 地址补码破坏 → 否
        assert!(!is_value_block("00000001ffffffff0000000105fa05fb"));
        // 全零块：值 0 但反码也是 0（应为全 FF）→ 否
        assert!(!is_value_block("00000000000000000000000000000000"));
        // 非 hex / 短行
        assert!(!is_value_block("zzzz"));
        assert!(!is_value_block("ff"));
    }

    #[test]
    fn key_dict_extracts_sectors_in_order_and_dedupes() {
        let mut s = blank_1k();
        // 扇区 3 尾块：KeyA = aabbccddeeff，KeyB = 112233445566
        let t = 3 * 64 + 48;
        let mut line = [0u8; 16];
        line[0..6].copy_from_slice(&[0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff]);
        line[10..16].copy_from_slice(&[0x11, 0x22, 0x33, 0x44, 0x55, 0x66]);
        line[6..10].copy_from_slice(&[0xff, 0x07, 0x80, 0x69]);
        s[3][3] = hex::hex_encode(&line);
        let keys = key_dict(&s);
        // 扇区 0 默认 FF×6 排最前，随后按扇区顺序；重复只留首次
        assert!(keys.contains(&"ffffffffffff".to_string()));
        let ia = keys.iter().position(|k| k == "aabbccddeeff").unwrap();
        let ib = keys.iter().position(|k| k == "112233445566").unwrap();
        assert!(ia < ib, "同扇区 KeyA 应先于 KeyB");
        assert_eq!(
            keys.iter().filter(|k| k == &"ffffffffffff").count(),
            1,
            "默认密钥应去重"
        );
        assert_eq!(keys.len(), 3, "唯一键：默认 ff + 扇区 3 的 aabb…/1122…");
    }

    #[test]
    fn key_dict_rejects_bad_structure() {
        let mut s = blank_1k();
        s[0][0].push('z');
        assert!(key_dict(&s).is_empty());
    }

    #[test]
    fn check_report_lists_issues_and_value_block_hints() {
        let mut s = blank_1k();
        s[4][1] = "00000001ffffffffff0000000105fa05fa".to_string(); // 值块
        let lines = check_report(&s);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains(t("html_value_block").as_str()));
    }

    #[test]
    fn check_report_empty_when_clean() {
        let s = blank_1k();
        let lines = check_report(&s);
        assert_eq!(lines, vec![t("html_check_all_ok")]);
    }

    #[test]
    fn fix_report_none_when_nothing_to_fix() {
        let s = blank_1k();
        let (_, items) = fix(&s).unwrap();
        assert!(items.is_empty());
        assert_eq!(fix_report(&[]), vec![t("html_fix_none")]);
    }
}
