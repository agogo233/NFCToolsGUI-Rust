//! 手机/手环操作区数据层。
//!
//! 对齐原版 `src-tauri/src/commands.rs` 的 `phw_read_uid` / `phw_write_uid` /
//! `phw_write_data`：块 0 是只读 UID 块、块 3 是扇区尾块（密钥/ACL），二者
//! 都不能覆盖，故写数据扇区只允许块 1-2；写 UID 位时按 block0 的 BCC 是否
//! 自洽决定是否给 `nfc-mfsetuid` 追加 `-f` 强制写入。

/// 扇区号上限（合法范围 0-15）。
pub const SECTOR_MAX: u32 = 15;
/// 可写块下限。块 0 是只读 UID 块，禁止覆盖。
pub const BLOCK_MIN: u32 = 1;
/// 可写块上限。块 3 是扇区尾块（密钥/ACL），禁止覆盖。
pub const BLOCK_MAX: u32 = 2;
/// 一个扇区包含的块数。
pub const BLOCKS_PER_SECTOR: u32 = 4;
/// 一个块的字节数。
pub const BLOCK_SIZE: usize = 16;

/// block0 的 BCC 是否自洽。
///
/// 对标 `commands.rs:1033`：前 4 字节按位异或应等于第 5 字节。不自洽说明卡号
/// 被手工输入过或块数据被改动，`nfc-mfsetuid` 需要 `-f` 才写得进去。
/// 入参越界或非 hex 时按 0 计，不 panic——拼串出错的代价是「BCC 不自洽」，
/// 而不是崩在工具参数上。
pub fn block0_bcc_ok(block0: &str) -> bool {
    let xor = [0usize, 1, 2, 3]
        .iter()
        .copied()
        .fold(0u8, |acc, i| acc ^ byte(block0, i));
    byte(block0, 4) == xor
}

/// 取 block0 hex 第 `i` 个字节的值；越界或非 hex 返回 0。
fn byte(block0: &str, i: usize) -> u8 {
    let start = i * 2;
    block0
        .get(start..start + 2)
        .and_then(|s| u8::from_str_radix(s, 16).ok())
        .unwrap_or(0)
}

/// `nfc-mfsetuid -q <block0>` 的参数；BCC 不自洽时追加 `-f`。
///
/// 对标 `commands.rs:1056-1059` / `1069-1071`。
pub fn mfsetuid_args(block0: &str) -> Vec<String> {
    let mut args = vec!["-q".to_string(), block0.to_string()];
    if !block0_bcc_ok(block0) {
        args.push("-f".to_string());
    }
    args
}

/// 写数据扇区的目标块定位。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct SectorBlock {
    pub sector: u32,
    pub block: u32,
}

impl SectorBlock {
    /// 块号是否在可写范围（扇区 0-15、块 1-2）。
    ///
    /// 对标 `commands.rs:1191`。越界直接报错而非交给工具，是因为工具侧没有
    /// 这层校验，越界写会把只读 UID 块或扇区尾块改掉。
    pub fn is_valid(self) -> bool {
        self.sector <= SECTOR_MAX && (BLOCK_MIN..=BLOCK_MAX).contains(&self.block)
    }

    /// 该块在整卡 dump 中的字节偏移 `(sector * 4 + block) * 16`。
    ///
    /// 对标 `commands.rs:1225`。
    pub fn offset(self) -> usize {
        ((self.sector * BLOCKS_PER_SECTOR + self.block) as usize) * BLOCK_SIZE
    }
}

/// 把 block0 数据写进 dump 的目标块，成功返回 `true`。
///
/// 对标 `commands.rs:1225-1238`。block0 不是 32 位 hex、块号越界或 dump 太
/// 短时都**不改动**原内容并返回 `false`——调用方据此终止写回，绝不拿残缺数据
/// 去整卡写入。
pub fn patch_data_block(dump: &mut [u8], block0: &str, target: SectorBlock) -> bool {
    if block0.len() != BLOCK_SIZE * 2 {
        return false;
    }
    let mut data = [0u8; BLOCK_SIZE];
    for (i, slot) in data.iter_mut().enumerate() {
        let Some(hex) = block0.get(i * 2..i * 2 + 2) else {
            return false;
        };
        let Ok(byte) = u8::from_str_radix(hex, 16) else {
            return false;
        };
        *slot = byte;
    }
    let off = target.offset();
    let Some(end) = off.checked_add(BLOCK_SIZE) else {
        return false;
    };
    if end > dump.len() {
        return false;
    }
    dump[off..end].copy_from_slice(&data);
    true
}

/// `phw-write-uid` 的危险确认框，正文拼接卡号（对标 `commands.rs:1144`）。
pub fn write_uid_confirm(block0: &str) -> crate::dialogs::ConfirmRequest {
    crate::dialogs::ConfirmRequest {
        title: crate::i18n::t("dialog_title_danger_operation"),
        message: format!(
            "{}{}",
            crate::i18n::t("dialog_msg_confirm_phw_write_uid"),
            block0
        ),
    }
}

/// `phw-write-data` 的危险确认框，正文含卡号与目标 扇区/块（对标 `commands.rs:1203`）。
pub fn write_data_confirm(block0: &str, target: SectorBlock) -> crate::dialogs::ConfirmRequest {
    crate::dialogs::ConfirmRequest {
        title: crate::i18n::t("dialog_title_danger_operation"),
        message: format!(
            "{}{}{}{}/{}",
            crate::i18n::t("dialog_msg_confirm_phw_write_data"),
            block0,
            crate::i18n::t("dialog_msg_phw_write_to"),
            target.sector,
            target.block,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 扇区 0 块 1 的 block0：UID `04A8A68A` + BCC `80`（04^A8^A6^8A = 80，自洽）。
    const VALID_BCC: &str = "04A8A68A800400010000000000000000";
    /// 同上，但把 BCC 位（第 5 字节）改成 00，制造不自洽。
    const BROKEN_BCC: &str = "04A8A68A000400010000000000000000";

    #[test]
    fn bcc_ok_accepts_self_consistent_block0() {
        assert!(block0_bcc_ok(VALID_BCC));
    }

    #[test]
    fn bcc_ok_rejects_broken_checksum() {
        assert!(!block0_bcc_ok(BROKEN_BCC));
    }

    #[test]
    fn bcc_ok_handles_short_or_garbage_without_panicking() {
        // 非 hex / 过短不应 panic（越界字节按 0 计，等价「BCC 恰好自洽」）。
        let _ = block0_bcc_ok("");
        let _ = block0_bcc_ok("04A8");
        let _ = block0_bcc_ok("GHIJKLMNOPQRSTUVWXYZ0123456789");
    }

    #[test]
    fn bcc_edge_all_zero_valid_all_one_invalid() {
        // 全 0：异或 0 == 第 5 字节 0 → 自洽
        assert!(block0_bcc_ok("00000000000000000000000000000000"));
        // 全 F：异或四个 F 得 0 ≠ 第 5 字节 F → 不自洽
        assert!(!block0_bcc_ok("FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF"));
    }

    #[test]
    fn mfsetuid_args_is_quiet_when_bcc_holds() {
        assert_eq!(mfsetuid_args(VALID_BCC), vec!["-q", VALID_BCC]);
    }

    #[test]
    fn mfsetuid_args_forces_when_bcc_is_broken() {
        assert_eq!(mfsetuid_args(BROKEN_BCC), vec!["-q", BROKEN_BCC, "-f"]);
    }

    #[test]
    fn sector_block_bounds() {
        assert!(SectorBlock {
            sector: 0,
            block: 1
        }
        .is_valid());
        assert!(SectorBlock {
            sector: 15,
            block: 2
        }
        .is_valid());
        assert!(!SectorBlock {
            sector: 0,
            block: 0
        }
        .is_valid());
        assert!(!SectorBlock {
            sector: 0,
            block: 3
        }
        .is_valid());
        assert!(!SectorBlock {
            sector: 16,
            block: 1
        }
        .is_valid());
    }

    #[test]
    fn offset_formula_matches_upstream() {
        assert_eq!(
            SectorBlock {
                sector: 0,
                block: 1
            }
            .offset(),
            16
        );
        assert_eq!(
            SectorBlock {
                sector: 0,
                block: 2
            }
            .offset(),
            32
        );
        assert_eq!(
            SectorBlock {
                sector: 1,
                block: 1
            }
            .offset(),
            80
        );
        assert_eq!(
            SectorBlock {
                sector: 15,
                block: 2
            }
            .offset(),
            992
        );
    }

    #[test]
    fn patch_writes_target_block_only() {
        let mut dump = vec![0u8; 256];
        dump[0..16].copy_from_slice(&[0xAA; 16]);
        dump[32..48].copy_from_slice(&[0xBB; 16]);
        let target = SectorBlock {
            sector: 0,
            block: 1,
        };
        assert!(patch_data_block(&mut dump, VALID_BCC, target));
        assert_eq!(dump[0..16], [0xAA; 16], "块 0 不应被改动");
        assert_eq!(
            &dump[16..32],
            &[
                0x04, 0xA8, 0xA6, 0x8A, 0x80, 0x04, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
                0x00, 0x00
            ]
        );
        assert_eq!(dump[32..48], [0xBB; 16], "块 2 不应被改动");
    }

    #[test]
    fn patch_rejects_short_or_non_hex_block0() {
        let mut dump = vec![0u8; 64];
        assert!(!patch_data_block(
            &mut dump,
            "04A8A68AEB",
            SectorBlock::default()
        ));
        assert!(!patch_data_block(
            &mut dump,
            "ZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZ",
            SectorBlock::default()
        ));
    }

    #[test]
    fn patch_rejects_when_dump_is_too_short() {
        let mut dump = vec![0u8; 8];
        assert!(!patch_data_block(
            &mut dump,
            VALID_BCC,
            SectorBlock {
                sector: 0,
                block: 1
            }
        ));
        assert_eq!(dump, [0u8; 8], "失败时不得改动原内容");
    }
}
