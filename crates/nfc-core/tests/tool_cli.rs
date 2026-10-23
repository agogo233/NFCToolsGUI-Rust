//! 把 engine 真正会 spawn 的每条命令行，喂给**真实二进制**看它认不认（S12-2b）。
//!
//! 这批测试的由来：S0–S12 全部跑在 `MockToolRunner` 上，参数拼接「对拍原版」
//! 只证明了**字符串**与 `command.js` 一致，没证明**二进制**接受这些字符串。
//! 编出真工具后一跑就露馅——见 `mfoc_binary_rejects_our_flags`。
//!
//! 无读卡器时这些工具会打印错误并非零退出，故断言只能落在
//! 「它抱怨的是**预期**错误」而非「退出 0」。

use std::path::PathBuf;

use nfc_core::paths::Paths;
use nfc_core::runner::{GateState, NfcToolRunner, ToolRunner};

fn find_bin_dir() -> Option<PathBuf> {
    if let Ok(v) = std::env::var("NFC_FRAMEWORK_BIN") {
        let p = PathBuf::from(v);
        if p.is_dir() {
            return Some(p);
        }
    }
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for a in manifest.ancestors().take(5) {
        let c = a.join("framework").join("bin");
        if c.is_dir() {
            return Some(c);
        }
    }
    for a in manifest.ancestors() {
        let nested = a.join("NFCToolsGUI").join("framework").join("bin");
        if nested.is_dir() {
            return Some(nested);
        }
        if a.file_name().is_some_and(|n| n == "NFCToolsGUI") {
            let own = a.join("framework").join("bin");
            if own.is_dir() {
                return Some(own);
            }
        }
    }
    None
}

macro_rules! bin_or_skip {
    () => {
        match find_bin_dir() {
            Some(d) => d,
            None => {
                eprintln!("跳过：未找到 framework/bin");
                return;
            }
        }
    };
}

fn gate_ok() -> GateState {
    GateState {
        has_current_device: true,
        is_device_connected: true,
        is_running_task: false,
        deep_scan: false,
    }
}

/// 跑一个工具到退出，返回合并输出。
fn run(bin: &std::path::Path, tool: &str, args: &[String]) -> String {
    let conf = std::env::temp_dir().join(format!("nfc-cli-{}-{}", std::process::id(), tool));
    let _ = std::fs::create_dir_all(&conf);
    let mut r = NfcToolRunner::new(bin.to_path_buf(), conf.clone());
    r.start(tool, args, &gate_ok())
        .unwrap_or_else(|e| panic!("spawn {tool} 失败: {e:?}"));
    let mut out = String::new();
    for _ in 0..2000 {
        r.pump(&mut |c: &nfc_core::runner::Chunk| out.push_str(&c.text));
        if r.exit().is_some() {
            let _ = std::fs::remove_dir_all(&conf);
            return out;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let _ = std::fs::remove_dir_all(&conf);
    panic!("{tool} 10 秒内没退出");
}

/// 「参数没被接受」的判据。
///
/// `mfoc` 系列用 getopt，参数不认识会打 usage；`-O` 缺失会打
/// `parameter -O is mandatory`——后者说明它**认得** `-O` 只是缺值。
fn looks_like_argv_rejection(out: &str) -> bool {
    let lower = out.to_lowercase();
    lower.contains("usage:")
        || lower.contains("invalid option")
        || lower.contains("unrecognized option")
}

fn tmp_paths(tag: &str) -> (Paths, PathBuf) {
    let d = std::env::temp_dir().join(format!("nfc-cli-{}-{}", std::process::id(), tag));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    // 造一个合法 keyfile，否则 mfoc 家族会先抱怨 keyfile 而轮不到看参数
    std::fs::write(d.join("keys.txt"), "FFFFFFFFFFFF\n").unwrap();
    (Paths::new(d.clone()), d)
}

// ─────────────────────────────────────────────────────────────
// 关键回归：read-IC 的 mfoc 参数
// ─────────────────────────────────────────────────────────────

/// **`read-IC` 实际 spawn 的那个二进制，必须接受 `-O/-f`。**
///
/// 这条曾**真的红过**：engine 读卡 spawn 的是 `nfc-mfclassic`，而该二进制是
/// mfclassic 风格（`f|r|R|w|W a|b u|U<uid> <dump>` 位置参数），收到 `-O…/-f…`
/// 直接打 usage 退出。原版 `command.js:104` spawn 的是 **`mfoc`**。
///
/// 即：单测里「参数与原版字符串一致」成立，但**二进制层面这条主流程跑不起来**。
/// 现已改回 `mfoc`（S12-2b），本测试守住不再改回去。
#[test]
fn read_ic_flags_are_accepted_by_the_real_binary() {
    let bin = bin_or_skip!();
    let (paths, dir) = tmp_paths("readic");
    let args = nfc_core::tasks::mfoc::mfoc_args(&paths, &[], 0);
    // 引擎读卡实际 spawn 的工具（engine.rs::read_ic）
    let out = run(&bin, "mfoc", &args);
    assert!(
        !looks_like_argv_rejection(&out),
        "读卡用的 mfoc 拒绝了 read-IC 的参数 {args:?}:\n{out}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// 写卡/格式化用的 `nfc-mfclassic` 走**位置参数**，与读卡不是一套。
///
/// 一并钉住，防止有人「统一」成一种风格而弄坏另一条链路。
#[test]
fn write_stage_flags_are_accepted_by_mfclassic() {
    let bin = bin_or_skip!();
    let (paths, dir) = tmp_paths("write");
    let args =
        nfc_core::tasks::card_write::mfclassic_write_args(&dir.join("src.mfd"), &paths.temp_mfd());
    assert!(
        args[0] == "w" && args[1] == "A" && args[2] == "u",
        "写卡参数应是 mfclassic 的位置参数风格，实为 {args:?}"
    );
    let out = run(&bin, "nfc-mfclassic", &args);
    assert!(
        !looks_like_argv_rejection(&out),
        "nfc-mfclassic 拒绝了写卡参数 {args:?}:\n{out}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// 反向确认：原版那个 `mfoc` 二进制**确实**吃 `-O/-f`。
///
/// 这条不是重复——它证明上一条的失败是「工具选错了」，
/// 而不是「flag 拼错了」。若哪天 mfoc 也变了，这条会先红。
#[test]
fn original_mfoc_binary_accepts_read_ic_flags() {
    let bin = bin_or_skip!();
    if !bin
        .join(if cfg!(windows) { "mfoc.exe" } else { "mfoc" })
        .is_file()
    {
        eprintln!("跳过：mfoc 二进制不在（compile.sh 产出名可能不同）");
        return;
    }
    let (paths, dir) = tmp_paths("origmfoc");
    let args = nfc_core::tasks::mfoc::mfoc_args(&paths, &[], 0);
    let out = run(&bin, "mfoc", &args);
    assert!(
        !looks_like_argv_rejection(&out),
        "原版 mfoc 也拒绝了这些参数:\n{out}"
    );
    // 且它应真的读到了 keyfile（证明 -f 生效），再抱怨没卡
    assert!(
        out.contains("custom key") || out.contains("No NFC device"),
        "mfoc 既没抱怨参数也没读 keyfile，输出异常:\n{out}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// `detect-card-type` 的 `-N -f<keys>`。
#[test]
fn detect_flags_are_accepted_by_the_real_binary() {
    let bin = bin_or_skip!();
    let (paths, dir) = tmp_paths("detect");
    let args = nfc_core::tasks::detect::detect_args(&paths, false, false);
    let out = run(&bin, "nfc-mfdetect", &args);
    assert!(
        !looks_like_argv_rejection(&out),
        "nfc-mfdetect 拒绝了 detect 的参数 {args:?}:\n{out}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// `write-ufuid-uid` 的 `nfc-mfsetuid -q <block0hex>`。
#[test]
fn mfsetuid_flags_are_accepted() {
    let bin = bin_or_skip!();
    let args = nfc_core::tasks::card_write::mfsetuid_args(&"11".repeat(16));
    let out = run(&bin, "nfc-mfsetuid", &args);
    assert!(
        !looks_like_argv_rejection(&out),
        "nfc-mfsetuid 拒绝了 {args:?}:\n{out}"
    );
}

/// `lock-ufuid` 的 `nfc-mflock -q`。
#[test]
fn mflock_flags_are_accepted() {
    let bin = bin_or_skip!();
    let args = nfc_core::tasks::card_write::mflock_args();
    let out = run(&bin, "nfc-mflock", &args);
    assert!(
        !looks_like_argv_rejection(&out),
        "nfc-mflock 拒绝了 {args:?}:\n{out}"
    );
}
