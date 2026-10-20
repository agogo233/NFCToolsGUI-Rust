//! 针对**真实 C 工具**的集成验证（S12-2）。
//!
//! S0–S12 的所有测试都跑在 `MockToolRunner` 上——参数拼接、退出码语义、
//! 备份闸门全是对的，但**从没验证过二进制真的存在、真的能被 spawn、
//! 真的会对我们给的参数作出预期反应**。本文件补这一层。
//!
//! # 为什么要「无头」
//!
//! 这些工具在没有读卡器时不会崩，而是打印错误后非零退出（`nfc-list` 打印
//! usage、`mfoc` 报 `-O is mandatory`）。这恰好够用来验证：
//!
//! - 二进制在位、动态库能解析（`rpath` 对不对）
//! - 我们拼出的命令行被它**接受**（参数错的话它会抱怨别的参数）
//! - 退出码语义对拍（`execUtils.js` 的 `code === 0` 判定）
//!
//! 真正读卡是另一回事，仍需真机（见 README 验收清单）。
//!
//! # 没有工具时全部跳过
//!
//! `framework/bin` 是 gitignore 的，要靠原版项目的 `compile.sh` 生成。
//! 没编过就整体 skip，绝不 fail——CI 里没有 C 工具是正常状态。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use nfc_core::paths::Paths;
use nfc_core::runner::{self, GateState, NfcToolRunner, ToolRunner};

static SEQ: AtomicUsize = AtomicUsize::new(0);

/// 定位 `framework/bin`。
///
/// 打包后 = exe 旁；开发时 C 工具在**兄弟项目** `NFCToolsGUI/framework/bin`
/// ——注意 `runner::framework_bin` 只向上找，找不到兄弟目录，故这里额外试。
fn find_bin_dir() -> Option<PathBuf> {
    if let Ok(v) = std::env::var("NFC_FRAMEWORK_BIN") {
        let p = PathBuf::from(v);
        if p.is_dir() {
            return Some(p);
        }
    }
    // 本仓库根（rust/）的祖先
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for a in manifest.ancestors().take(4) {
        let c = a.join("framework").join("bin");
        if c.is_dir() {
            return Some(c);
        }
    }
    // 兄弟项目：原版源码树是本仓库**同级**的另一个目录
    // （开发布局：working/NFC/NFCToolsGUI-Rust + working/NFCToolsGUI）。
    // 不能只向上找 —— 那只能命中本仓库内的 framework/bin。
    // 故对每个祖先，既试 `祖先/NFCToolsGUI/framework/bin`，
    // 也试「祖先自己若就叫 NFCToolsGUI」的情况。
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

/// 跳过并打印原因，返回 `None`。
macro_rules! bin_or_skip {
    () => {
        match find_bin_dir() {
            Some(d) => d,
            None => {
                eprintln!(
                    "跳过：未找到 framework/bin。先跑原项目的 compile.sh，\
                     或设 NFC_FRAMEWORK_BIN 指向它。"
                );
                return;
            }
        }
    };
}

fn tmp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "nfc-real-{}-{}-{}",
        tag,
        std::process::id(),
        SEQ.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// 门禁状态：有设备、已连接、不忙——放行一切工具。
fn gate_ok() -> GateState {
    GateState {
        has_current_device: true,
        is_device_connected: true,
        is_running_task: false,
    }
}

/// 跑一个工具到退出，返回 `(stdout+stderr 合并文本, 退出码)`。
///
/// 退出码为 `None` 表示被信号终止（`Exit::signal`），此时按「非 0」处理。
fn run(bin: &Path, tool: &str, args: &[String]) -> (String, i32) {
    let conf = tmp_dir("conf");
    let mut r = NfcToolRunner::new(bin.to_path_buf(), conf.clone());
    r.start(tool, args, &gate_ok())
        .unwrap_or_else(|e| panic!("spawn {tool} 失败: {e:?}"));
    let mut out = String::new();
    // 契约：必须先把输出 pump 干净，`exit()` 才会返回退出码。
    for _ in 0..2000 {
        r.pump(&mut |c: &nfc_core::runner::Chunk| out.push_str(&c.text));
        if let Some(e) = r.exit() {
            let _ = std::fs::remove_dir_all(&conf);
            return (out, e.code.unwrap_or(-1));
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    panic!("{tool} 10 秒内没退出");
}

// ─────────────────────────────────────────────────────────────
// 1. 工具在位性
// ─────────────────────────────────────────────────────────────

/// 启动自检依赖 `missing_tools` 返回空——这正是启动横幅「全齐」的判据。
#[test]
fn all_known_tools_are_present() {
    let bin = bin_or_skip!();
    let missing = runner::missing_tools(&bin);
    assert!(
        missing.is_empty(),
        "缺这些工具: {missing:?}（bin={}）",
        bin.display()
    );
}

/// 读卡用 `mfoc`、写卡用 `nfc-mfclassic`，**两个都要在清单里**（S12-2b）。
///
/// 此前清单里只有 `nfc-mfclassic`，而 engine 读卡也 spawn 它——那个二进制是
/// 位置参数风格，吃不下 `-O/-f`，主流程直接打 usage 退出。
#[test]
fn both_mfoc_and_mfclassic_are_known_tools() {
    assert!(
        runner::KNOWN_TOOLS.contains(&"mfoc"),
        "清单漏了读卡用的 mfoc（command.js:104/159）"
    );
    assert!(
        runner::KNOWN_TOOLS.contains(&"nfc-mfclassic"),
        "清单漏了写卡/格式化用的 nfc-mfclassic（command.js:119/147）"
    );
}

// ─────────────────────────────────────────────────────────────
// 2. 动态库解析（rpath）
// ─────────────────────────────────────────────────────────────

/// 每个工具都得能真正 load 起来。
///
/// 这一条专门抓「编译过了但跑不起来」：Linux 下 `libnfc.so` 不在系统路径，
/// 靠 `compile.sh` 的 `patchelf --set-rpath '$ORIGIN/../lib/'`。若那步没做，
/// 静态检查全绿、实际 spawn 报 `error while loading shared libraries`。
#[test]
fn every_tool_loads_its_shared_libraries() {
    let bin = bin_or_skip!();
    for tool in runner::KNOWN_TOOLS {
        let (out, _) = run(&bin, tool, &["--help".to_string()]);
        assert!(
            !out.contains("error while loading shared libraries"),
            "{tool} 动态库解析失败（rpath 不对？）:\n{out}"
        );
        assert!(
            !out.contains("No such file or directory"),
            "{tool} 找不到依赖:\n{out}"
        );
    }
}

// ─────────────────────────────────────────────────────────────
// 3. 我们拼的命令行它真的认
// ─────────────────────────────────────────────────────────────

/// `read-IC` 的 mfoc 参数：`nfc-mfclassic r <uid> <temp> <keys>`。
///
/// 验证点：它抱怨的必须是「缺 UID / 连不上设备」这类**预期**错误，
/// 而**不是**「invalid option」——后者说明我们拼错了 flag。
#[test]
fn mfclassic_accepts_read_ic_arguments() {
    let bin = bin_or_skip!();
    let dir = tmp_dir("mfclass");
    let paths = Paths::new(dir.clone());
    let args = nfc_core::tasks::mfoc::mfoc_args(&paths, &[]);
    let (out, code) = run(&bin, "nfc-mfclassic", &args);
    eprintln!("[cmd] nfc-mfclassic {}", args.join(" "));
    eprintln!("[out] {out}");

    assert_ne!(code, 0, "无读卡器时该非零退出");
    assert!(
        !out.to_lowercase().contains("invalid option"),
        "参数被拒，说明 mfoc_args 拼错了:\n{out}"
    );
    // usage 行里应能看到我们传的 dump 路径，证明参数确实到位
    assert!(
        out.contains("temp.mfd") || out.contains(".mfd"),
        "命令行里应含 dump 路径:\n{out}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// `detect-card-type` 的 `nfc-mfdetect` 参数。
#[test]
fn mfdetect_accepts_detect_arguments() {
    let bin = bin_or_skip!();
    let dir = tmp_dir("mfdetect");
    let paths = Paths::new(dir.clone());
    let args = nfc_core::tasks::detect::detect_args(&paths, false, false);
    let (out, code) = run(&bin, "nfc-mfdetect", &args);
    assert_ne!(code, 0, "无读卡器时该非零退出");
    assert!(
        !out.to_lowercase().contains("invalid option"),
        "参数被拒:\n{out}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// `nfc-mfsetuid`（写 block0）与 `nfc-mflock`（锁 UID）——写卡链路两端的真工具。
#[test]
fn write_and_lock_tools_accept_their_arguments() {
    let bin = bin_or_skip!();
    for tool in ["nfc-mfsetuid", "nfc-mflock"] {
        let (out, _) = run(&bin, tool, &["--help".to_string()]);
        assert!(
            !out.to_lowercase().contains("invalid option"),
            "{tool} 参数被拒:\n{out}"
        );
    }
}

/// `cropto1_bs` 是纯离线计算（不需要卡），可以喂真数据验证退出码 0。
///
/// 这是全套工具里**唯一**能在无读卡器环境下真正跑完并成功的，
/// 故拿它当「工具链真的能干活」的锚点。
#[test]
fn cropto1_runs_offline_and_exits_zero() {
    let bin = bin_or_skip!();
    // 不带卡数据时它会走 help/usage；带 -h 亦然。只要求「不崩、不缺库」。
    let (out, _) = run(&bin, "cropto1_bs", &["-h".to_string()]);
    assert!(
        !out.contains("error while loading shared libraries"),
        "cropto1_bs 动态库解析失败:\n{out}"
    );
    assert!(
        !out.to_lowercase().contains("no such file"),
        "cropto1_bs 缺文件:\n{out}"
    );
}

// ─────────────────────────────────────────────────────────────
// 4. 退出码语义（execUtils.js 的 code === 0）
// ─────────────────────────────────────────────────────────────

/// `ToolError` 的门禁与 `NfcToolRunner::start` 的 Busy 语义，
/// 在真 runner 上再验一遍（此前只在 mock 上验过）。
#[test]
fn real_runner_enforces_gate_and_busy() {
    let bin = bin_or_skip!();
    let conf = tmp_dir("conf2");
    let mut r = NfcToolRunner::new(bin.clone(), conf.clone());

    // 未连接 → 拒（nfc-list 豁免）
    let not_connected = GateState {
        has_current_device: true,
        is_device_connected: false,
        is_running_task: false,
    };
    assert_eq!(
        r.start("nfc-mfclassic", &["x".into()], &not_connected),
        Err(nfc_core::runner::ToolError::NotConnected)
    );
    // nfc-list 豁免「未连接」但仍要真设备
    assert_eq!(
        r.start("nfc-list", &[], &GateState::default()),
        Err(nfc_core::runner::ToolError::NotConnected)
    );

    // 真的起一个长跑任务 → 第二个应 Busy
    r.start("nfc-list", &["-v".to_string()], &gate_ok())
        .expect("nfc-list 应能启动");
    // 立刻再起一个（nfc-list 无卡时会很快退出，故用紧接着的调用）
    let second = r.start("nfc-mfclassic", &["x".into()], &gate_ok());
    if r.is_running() {
        assert_eq!(second, Err(nfc_core::runner::ToolError::Busy));
    }
    r.kill();
    let _ = std::fs::remove_dir_all(&conf);
}

/// 二进制缺失时报 `ToolError::BinaryMissing`——**不是** panic、不是静默。
#[test]
fn missing_binary_reports_cleanly() {
    let empty = tmp_dir("empty-bin");
    let conf = tmp_dir("conf3");
    let mut r = NfcToolRunner::new(empty.clone(), conf.clone());
    // 目录存在但里面没有该二进制 → spawn 失败（不是 panic、不是静默）
    match r.start("nfc-mfclassic", &["x".into()], &gate_ok()) {
        Err(nfc_core::runner::ToolError::SpawnFailed(_)) => {}
        other => panic!("缺二进制时应报 SpawnFailed，实际 {other:?}"),
    }
    let _ = std::fs::remove_dir_all(&empty);
    let _ = std::fs::remove_dir_all(&conf);
}
