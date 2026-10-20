//! hex 编解码。
//!
//! 原版没有独立的 hex 函数，只有两处内联：
//! - bytes→hex：`command.js:425-427`，对 dump 编辑器与比较器各写了一遍
//! - hex→bytes：`dumpEditor.js:14` → `command.js:436` 的 `new Buffer.from(s, "hex")`
//!
//! 偏离登记见 docs/02 §6。

use thiserror::Error;

const DIGITS: &[u8; 16] = b"0123456789abcdef";

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum HexError {
    #[error("hex 字符数为奇数（{0}），无法按字节切分")]
    OddLength(usize),
    #[error("非法 hex 字符 {ch:?}，位于第 {index} 个字符")]
    InvalidChar { index: usize, ch: char },
}

/// bytes → 小写 hex，零填充、无分隔符。
///
/// 逐字对标 `command.js:426`：`('0' + (byte & 0xff).toString(16)).slice(-2)` 后 `join('')`。
pub fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(DIGITS[(b >> 4) as usize] as char);
        out.push(DIGITS[(b & 0x0f) as usize] as char);
    }
    out
}

/// bytes → 大写 hex。
///
/// 对标 `command.js:198`：写 UFUID 时读 dump 前 16 字节做 `.toUpperCase()`。
pub fn hex_encode_upper(bytes: &[u8]) -> String {
    hex_encode(bytes).to_ascii_uppercase()
}

/// hex → bytes。接受大小写；不接受 `0x` 前缀、空格与分隔符。
///
/// 偏离（原版 `command.js:436`）：`Buffer.from(s, "hex")` 遇非法字符**静默停止**、
/// 奇数尾**丢弃半字节**，实测 `"ffzz11"` → 1 字节、`"fff"` → 1 字节。畸形输入会
/// 写出**短 dump**，而该 dump 随后被 `nfc-mfclassic` 写进卡里，属数据损坏。
/// 本实现改为显式报错，由编辑器保存路径拦下。
pub fn hex_decode(text: &str) -> Result<Vec<u8>, HexError> {
    let chars: Vec<char> = text.chars().collect();
    if !chars.len().is_multiple_of(2) {
        return Err(HexError::OddLength(chars.len()));
    }
    let mut out = Vec::with_capacity(chars.len() / 2);
    for (i, pair) in chars.chunks(2).enumerate() {
        let hi = nibble(pair[0]).ok_or(HexError::InvalidChar {
            index: i * 2,
            ch: pair[0],
        })?;
        let lo = nibble(pair[1]).ok_or(HexError::InvalidChar {
            index: i * 2 + 1,
            ch: pair[1],
        })?;
        out.push(hi << 4 | lo);
    }
    Ok(out)
}

/// 只认 ASCII hex。不用 `char::to_digit`，因为它接受 Unicode 数字（如 `'٣'` → 3）。
fn nibble(c: char) -> Option<u8> {
    match c {
        '0'..='9' => Some(c as u8 - b'0'),
        'a'..='f' => Some(c as u8 - b'a' + 10),
        'A'..='F' => Some(c as u8 - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_matches_original_slice_minus_two_formula() {
        // 对标 ('0' + (b & 0xff).toString(16)).slice(-2)
        assert_eq!(hex_encode(&[0x00, 0x0f, 0xff]), "000fff");
        assert_eq!(hex_encode(&[]), "");
        assert_eq!(hex_encode(&[0xa0, 0xa1]), "a0a1");
    }

    #[test]
    fn encode_is_lowercase_and_zero_padded() {
        assert_eq!(hex_encode(&[0xde, 0xad, 0xbe, 0xef]), "deadbeef");
    }

    #[test]
    fn encode_upper_matches_write_ufuid_path() {
        assert_eq!(hex_encode_upper(&[0xa0, 0xa1, 0xa2, 0xa3]), "A0A1A2A3");
    }

    #[test]
    fn decode_roundtrip_all_byte_values() {
        let all: Vec<u8> = (0..=255u8).collect();
        assert_eq!(hex_decode(&hex_encode(&all)).unwrap(), all);
    }

    #[test]
    fn decode_accepts_both_cases() {
        assert_eq!(
            hex_decode("AaBbCcDdEeFf").unwrap(),
            vec![0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff]
        );
        assert_eq!(
            hex_decode("aabbccddeeff").unwrap(),
            vec![0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff]
        );
    }

    #[test]
    fn decode_empty_is_empty_not_error() {
        assert_eq!(hex_decode("").unwrap(), Vec::<u8>::new());
    }

    #[test]
    fn decode_rejects_odd_length() {
        // 原版 Buffer.from("fff", "hex") 静默丢弃半字节返回 [0xff]
        assert_eq!(hex_decode("fff"), Err(HexError::OddLength(3)));
    }

    #[test]
    fn decode_rejects_invalid_char_instead_of_truncating() {
        // 原版 Buffer.from("ffzz11", "hex") 静默返回 [0xff]
        assert!(matches!(
            hex_decode("ffzz11"),
            Err(HexError::InvalidChar { .. })
        ));
        assert!(matches!(
            hex_decode("zz"),
            Err(HexError::InvalidChar { .. })
        ));
    }

    #[test]
    fn decode_rejects_prefix_and_separators() {
        assert!(hex_decode("0xaa").is_err());
        assert!(hex_decode("aa bb").is_err());
    }
}
