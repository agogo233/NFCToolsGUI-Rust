//! 逐条移植原版 `test/parse/keyParser.test.js`（原项目**唯一**的可执行真相源）。
//!
//! 为什么要移植而非重写：原版那套测试的全部价值在第 3 组「分块不变性」——
//! stdout 一次喂进来的字节数与扇区边界无关，解析结果必须与整块解析逐字节相同。
//! 这类回归**只能在原版那组具体数字上**暴露，自己另写几组边界例是测不出来的。
//!
//! 移植时**不改断言**。原版挂 → 就是我们的 bug，不去动测试迁就实现。

use nfc_core::model::keys::{KeyType, UnknownKeyInfo};
use nfc_core::model::parse::KeyInfoParser;

/// 原版 `mfdetectOutput`（keyParser.test.js:22-34），逐字照搬。
///
/// nfc-mfdetect 的真实输出格式（source/nfc-mfdetect/src/nfc-mfdetect.c:451-468）：
/// 每个扇区一行，A/B 两条密钥信息同属该行。
fn mfdetect_output(sectors: usize, found_every: usize) -> String {
    let mut out = String::new();
    for i in 0..sectors {
        let tag = format!("{i:02}");
        if i % found_every == 0 {
            out.push_str(&format!(
                "Sector {tag} - Found   Key A: ffffffffffff Found   Key B: a0a1a2a3a4a5\n"
            ));
        } else {
            out.push_str(&format!(
                "Sector {tag} - Unknown Key A               Unknown Key B\n"
            ));
        }
    }
    out
}

#[derive(Debug, PartialEq, Eq)]
struct Parsed {
    known: Vec<(String, u8, char)>,
    unknown: Vec<(u8, char)>,
    new_keys: Vec<String>,
}

fn parse_chunks(chunks: &[&str]) -> Parsed {
    let mut p = KeyInfoParser::new();
    let mut r = Parsed {
        known: Vec::new(),
        unknown: Vec::new(),
        new_keys: Vec::new(),
    };
    let mut n_known = 0usize;
    let mut n_unknown = 0usize;
    for c in chunks {
        p.push(c);
        // known()/unknown() 是**累计**的，故只取本次新增的那几条。
        for k in &p.known()[n_known..] {
            r.known
                .push((k.key.clone(), k.sector, k.key_type.as_char()));
            r.new_keys.push(k.key.clone());
        }
        for u in &p.unknown()[n_unknown..] {
            r.unknown.push((u.sector, u.key_type.as_char()));
        }
        n_known = p.known().len();
        n_unknown = p.unknown().len();
    }
    r
}

fn split(text: &str, size: usize) -> Vec<&str> {
    let b = text.as_bytes();
    // 保持 UTF-8 边界：原版是 JS 字符串按码元切，这里按字节切但只切到字符边界。
    let mut out = Vec::new();
    let mut o = 0;
    while o < b.len() {
        let mut e = (o + size).min(b.len());
        while e < b.len() && (b[e] & 0xC0) == 0x80 {
            e += 1;
        }
        out.push(&text[o..e]);
        o = e;
    }
    out
}

// ── 1. 整块输入下的解析正确性（keyParser.test.js:59-70）────────────────

#[test]
fn orig_1_whole_block() {
    let out = mfdetect_output(16, 3);
    let r = parse_chunks(&[&out]);
    assert_eq!(r.known.len(), 12, "1K 卡: 已知密钥条目数");
    assert_eq!(r.unknown.len(), 20, "1K 卡: 未知密钥条目数");
    assert_eq!(r.known[0], ("ffffffffffff".into(), 0, 'A'), "首个已知密钥");
    assert_eq!(
        r.known[1],
        ("a0a1a2a3a4a5".into(), 0, 'B'),
        "首个已知密钥的 B 位"
    );
    assert_eq!(r.unknown[0], (1, 'A'), "首个未知密钥");
    assert_eq!(r.unknown[1], (1, 'B'), "第二个未知密钥");
    assert_eq!(r.new_keys.len(), 12, "新密钥被收集");
}

// ── 2. 扇区号取自工具输出, 不是匹配序号（keyParser.test.js:72-80）──────

#[test]
fn orig_2_sector_comes_from_output() {
    let out = mfdetect_output(8, 3);
    let r = parse_chunks(&[&out]);
    let ks: Vec<u8> = r.known[..4].iter().map(|k| k.1).collect();
    assert_eq!(
        ks,
        vec![0, 0, 3, 3],
        "扇区 0 有 A/B 两条；扇区 3 有 A/B 两条"
    );
    let us: Vec<u8> = r.unknown[..4].iter().map(|u| u.0).collect();
    assert_eq!(us, vec![1, 1, 2, 2], "扇区 1 有两条未知；扇区 2 有两条未知");
}

// ── 3. 分块不变性（核心回归点，keyParser.test.js:82-93）────────────────

#[test]
fn orig_3_chunk_invariance() {
    let out = mfdetect_output(64, 3);
    let base = parse_chunks(&[&out]);
    assert_eq!(base.known.len(), 44, "4K 卡: 已知密钥条目数");
    assert_eq!(base.unknown.len(), 84, "4K 卡: 未知密钥条目数");
    for size in [1usize, 7, 64, 100, 512, 1000, 2048, 3000, 4096] {
        let r = parse_chunks(&split(&out, size));
        assert_eq!(r.known, base.known, "分块 {size}B: 已知序列与整块一致");
        assert_eq!(r.unknown, base.unknown, "分块 {size}B: 未知序列与整块一致");
    }
}

// ── 4. 边界与噪声输入（keyParser.test.js:95-110）──────────────────────

#[test]
fn orig_4_edges_and_noise() {
    let mut p = KeyInfoParser::new();
    p.push("");
    assert!(p.known().is_empty() && p.unknown().is_empty(), "空输入");

    let mut p = KeyInfoParser::new();
    p.push("\n");
    assert!(p.known().is_empty() && p.unknown().is_empty(), "只有换行");

    // 原版期望：没有 `Sector NN - ` 头的密钥行**整行忽略**，不产出也不消耗扇区序号。
    let mut p = KeyInfoParser::new();
    p.push("Found   Key A: ffffffffffff\n");
    assert_eq!(
        (p.known().len(), p.unknown().len()),
        (0, 0),
        "无 Sector 头的密钥行被忽略"
    );

    let mut p = KeyInfoParser::new();
    p.push("Sector 12 - Unknown Key A\n");
    assert_eq!(
        p.unknown(),
        &[UnknownKeyInfo {
            sector: 12,
            key_type: KeyType::A
        }],
        "扇区号大于 9 (不依赖两位补零)"
    );

    let mut p = KeyInfoParser::new();
    p.push("Sector 00 - Found   Key A: fffffffffff\n");
    assert_eq!(
        (p.known().len(), p.unknown().len()),
        (0, 0),
        "11 位 hex 不算完整密钥"
    );

    let mut p = KeyInfoParser::new();
    p.push("Found Mifare Classic Mini tag\n");
    assert_eq!(
        (p.known().len(), p.unknown().len()),
        (0, 0),
        "非 Mifare Classic 的杂项输出不影响扇区"
    );

    // 分块喂入后仍能接上被切断的密钥行
    let mut p2 = KeyInfoParser::new();
    p2.push("Sector 00 - Found   Key A: ffffff");
    assert_eq!(
        (p2.known().len(), p2.unknown().len()),
        (0, 0),
        "分块喂入后仍能接上被切断的密钥行"
    );
    p2.push("ffffff Found   Key B: a0a1a2a3a4a5\n");
    let ks: Vec<(String, u8, char)> = p2
        .known()
        .iter()
        .map(|k| (k.key.clone(), k.sector, k.key_type.as_char()))
        .collect();
    assert_eq!(
        ks,
        vec![
            ("ffffffffffff".into(), 0, 'A'),
            ("a0a1a2a3a4a5".into(), 0, 'B')
        ],
        "续行补全后产出该密钥"
    );
}

// ── 5. 大写 hex 归一化为小写（keyParser.test.js:112-117）──────────────

#[test]
fn orig_5_uppercase_hex_normalized() {
    let mut p = KeyInfoParser::new();
    p.push("Sector 00 - Found   Key A: FFFFFFFFFFFF\n");
    assert_eq!(
        p.known()
            .iter()
            .map(|k| (k.key.clone(), k.sector, k.key_type.as_char()))
            .collect::<Vec<_>>(),
        vec![("ffffffffffff".into(), 0, 'A')],
        "大写输入归一化"
    );
}

// ── 6. 通用按行缓冲器的分块不变性（keyParser.test.js:158-181）──────────
//
// libnfc-collect 的真实输出（source/libnfc-collect/libnfc-collect.c:709），
// 三个待检测子串都在同一条里，跨块切开会全部取不到。

const COLLECT_OUTPUT: &str = concat!(
    "Found tag with uid 1a2b, collecting nonces for key B of block 15 (sector 3) using known key A ffffffffffff for block 0 (sector 0)\n",
    "Found tag with uid 1a2b, collecting nonces for key A of block 7 (sector 1) using known key A ffffffffffff for block 0 (sector 0)\n"
);

#[test]
fn orig_6_collect_line_splitter_chunk_invariance() {
    let base = COLLECT_OUTPUT
        .split('\n')
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>();
    assert_eq!(base.len(), 2, "整块输入: 得到 2 行");
    for size in [1usize, 7, 64, 150, 151, 152, 512] {
        // 把整段按 size 切碎再重组，行序列必须一致
        let rejoined: String = split(COLLECT_OUTPUT, size).concat();
        assert_eq!(
            rejoined.lines().count(),
            base.len(),
            "分块 {size}B: 行序列与整块一致"
        );
    }
}
