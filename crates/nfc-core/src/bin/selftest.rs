//! `--selftest` 自检模式：用 Mock 驱动跑通全流程，不碰硬件、不弹真对话框。
//!
//! 对标 docs/04 §5 的 12 项清单。当前实现覆盖其中**纯 core 可达**的 10 项；
//! 第 9-11 项（dump editor / comparator / history）属 S10，届时补齐。
//!
//! 运行：`cargo run -p nfc-core --bin selftest`（CI 必跑）
//! 退出码：0 = 全绿，非 0 = 有 FAIL。

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use nfc_core::data::Registry;
use nfc_core::dialogs::MockDialog;
use nfc_core::model::dump::{from_sector_lines, to_sector_lines, validate, BLOCKS_PER_SECTOR};
use nfc_core::model::keys::{save_keys, KeyType, UnknownKeyInfo, DEFAULT_KEYS};
use nfc_core::model::parse::{KeyInfoParser, UidParser};
use nfc_core::paths::Paths;
use nfc_core::runner::{Chunk, GateState, MockToolRunner, ToolRunner};
use nfc_core::state::{
    AppState, DeviceState, DictTestConfig, Event, HardNestedConfig, HardNestedPhase,
};
use nfc_core::tasks::card_write::{
    mfclassic_format_args, mfclassic_write_args, mflock_args, mfsetuid_args, prepare_lock,
    prepare_write, prepare_write_uid, PrepareOutcome,
};
use nfc_core::tasks::detect::{after_detect, detect_args, guidance_for_unknown};
use nfc_core::tasks::dict_test::{apply_dict_choice, dict_args, report_found_key};
use nfc_core::tasks::hard_nested::{remove_unknown, HardNestedRunner, RoundContext, Step};
use nfc_core::tasks::mfoc::{
    apply_eta, dump_save, format_duration, mfoc_args, mfoc_keys_total, EtaTracker, SaveOutcome,
};

static SEQ: AtomicUsize = AtomicUsize::new(0);

struct Env {
    dir: PathBuf,
    paths: Paths,
}

impl Env {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "nfc-selftest-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::SeqCst)
        ));
        fs::create_dir_all(&dir).expect("建临时目录");
        Self {
            paths: Paths::new(&dir),
            dir,
        }
    }

    fn state(&self) -> AppState {
        AppState::new()
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn gate() -> GateState {
    GateState {
        has_current_device: true,
        is_device_connected: true,
        is_running_task: false,
        deep_scan: false,
    }
}

/// 一张 1K Mifare Classic 的合成 dump（16 扇区 × 4 块）。
fn synth_dump() -> Vec<u8> {
    let mut v = vec![0u8; 1024];
    for (i, b) in v.iter_mut().enumerate() {
        // 块 0 放 UID + BCC，尾块放访问位与 key，形态上像真卡
        if i == 0 {
            *b = 0x11;
        } else if i % BLOCKS_PER_SECTOR == 3 && (i / BLOCKS_PER_SECTOR).is_multiple_of(2) {
            *b = 0x07;
        } else {
            *b = (i as u8).wrapping_mul(7).wrapping_add(3);
        }
    }
    v
}

/// 退出码分流检查。
fn check_exit_codes() {
    // 0 / 1 / 2 / kill 四分支
    assert_eq!(nfc_core::runner::Exit::new(Some(0)).result(), Ok(()));
    assert!(nfc_core::runner::Exit::new(Some(1)).result().is_err());
    assert!(nfc_core::runner::Exit::new(Some(2)).result().is_err());
    assert_eq!(
        nfc_core::runner::Exit::new(None).result(),
        Err(nfc_core::runner::ToolError::Killed)
    );
}

/// 1. connect → detect → guidance。
fn case_connect_detect() {
    let e = Env::new();
    let mut s = e.state();
    s.current_device = Some("/dev/ttyACM0".into());
    s.apply(Event::DeviceState(DeviceState::Connected));
    assert!(s.is_device_connected);

    // detect：nfc-mfdetect 输出含 3 个 unknown
    let mut runner = MockToolRunner::new(vec![Chunk::stdout(
        // 照抄 nfc-mfdetect.c:448-470：每扇区一行，A/B 印在同一行，扇区号带在行头。
        "MIFARE Classic 1K detected\n\
         Sector 00 - Found   Key A: ffffffffffff Found   Key B: ffffffffffff\n\
         Sector 01 - Unknown Key A               Unknown Key B\n",
    )]);
    runner
        .start(
            "nfc-mfdetect",
            &detect_args(&e.paths, false, false),
            &gate(),
        )
        .unwrap();
    let mut parser = KeyInfoParser::new();
    let mut out = Vec::new();
    runner.pump(&mut |c| {
        out.push(c.clone());
        parser.push(&c.text);
    });
    parser.flush();
    assert!(runner.exit().is_some(), "mfdetect 应已退出");

    // 真实 mfdetect 每个扇区都出 A/B 两条，故 1 known + 2 unknown 不可能出现
    assert_eq!(parser.known().len(), 2, "扇区 0 的 A/B");
    assert_eq!(parser.unknown().len(), 2, "扇区 1 的 A/B");
    let outcome = after_detect(&mut s, &parser, &e.paths, false).unwrap();
    assert_eq!(outcome, nfc_core::tasks::detect::DetectOutcome::Proceed);
    let (_, text, buttons) = guidance_for_unknown(&parser).expect("有 unknown 应出引导");
    assert!(text.contains('2'), "引导应含数量: {text}");
    assert_eq!(buttons.len(), 2);
}

/// 2. read-IC → dump 自动保存 → dump_history。
fn case_read_ic_saves_dump() {
    let e = Env::new();
    fs::write(e.paths.temp_mfd(), synth_dump()).unwrap();
    let mut s = e.state();
    let parser = KeyInfoParser::new();
    assert_eq!(
        after_detect(&mut s, &parser, &e.paths, true).unwrap(),
        nfc_core::tasks::detect::DetectOutcome::Proceed
    );

    // ETA 全链
    let total = mfoc_keys_total(0, &mfoc_args(&e.paths, &[]));
    let mut eta = EtaTracker::new(total, 0);
    for i in 0..total {
        if let Some(u) = eta.push("[Key: aabbccddeeff]\n", (i as u64) * 1000) {
            apply_eta(&mut s, &u);
        }
    }
    assert!(eta.is_finished());
    assert_eq!(s.progress, Some(100.0));
    assert_eq!(s.status.text, "正在解卡");

    // 用户取消保存 → 落默认名
    let mut dlg = MockDialog::new().with_save(None);
    let out = dump_save(&mut s, &e.paths, Some("112233445566"), &mut dlg);
    let SaveOutcome::Saved { target, chosen } = out else {
        panic!("应自动保存");
    };
    assert!(!chosen, "取消应回落默认名");
    assert!(target.exists());
    assert_eq!(fs::metadata(&target).unwrap().len(), 1024);
    assert_eq!(s.dump_history.len(), 1);
    s.apply(Event::TaskDone {
        code: Some(0),
        killed: false,
    });
    assert!(!s.is_running_task);
}

/// 3. write-IC（确认接受）。
fn case_write_ic() {
    let e = Env::new();
    let src = e.paths.dumpfiles_dir().join("in.mfd");
    fs::create_dir_all(e.paths.dumpfiles_dir()).unwrap();
    fs::write(&src, synth_dump()).unwrap();
    let mut dlg = MockDialog::new()
        .with_open(Some(src.clone()))
        .with_confirm(true);
    assert_eq!(
        prepare_write(&e.paths, None, &mut dlg),
        PrepareOutcome::Ready(src.clone()),
        "write-IC 必须把选中的 dump 路径带回，否则第 4 个参数无从构造"
    );
    let args = mfclassic_write_args(&src, &e.paths.temp_mfd());
    assert_eq!(args.len(), 6);
    assert_eq!(&args[..3], &["w", "A", "u"]);
    assert!(args[4].ends_with("temp.mfd"));
}

/// 4. format-card。
fn case_format_card() {
    let e = Env::new();
    let args = mfclassic_format_args(&e.paths.temp_mfd());
    assert_eq!(args[0], "f");
    assert_eq!(args[3], args[4], "源与目标同为 temp.mfd");
    assert_eq!(args[5], "f");
}

/// 5. write-uid / lock。
fn case_write_uid_and_lock() {
    let e = Env::new();
    let dump = e.dir.join("u.mfd");
    fs::write(&dump, synth_dump()).unwrap();
    let mut s = e.state();
    let mut dlg = MockDialog::new().with_confirm(true).with_confirm(true);
    let hex = prepare_write_uid(&mut s, &e.paths, &mut dlg, Some(dump)).expect("应通过");
    assert_eq!(hex.len(), 32);
    assert_eq!(mfsetuid_args(&hex), vec!["-q".to_string(), hex.clone()]);

    let mut dlg2 = MockDialog::new().with_confirm(true);
    assert_eq!(prepare_lock(&mut dlg2), PrepareOutcome::ReadyNoFile);
    assert_eq!(mflock_args(), vec!["-q"]);

    // 取消分支
    let mut dlg3 = MockDialog::new().with_confirm(false);
    assert_eq!(prepare_lock(&mut dlg3), PrepareOutcome::Cancelled);
}

/// 6. hard_nested 手动全链。
fn case_hard_nested_manual() {
    let e = Env::new();
    let config = HardNestedConfig {
        known_key: "ffffffffffff".into(),
        known_sector: 0,
        known_key_type: KeyType::A,
        target_sector: 1,
        target_key_type: KeyType::B,
        collect_only: false,
        auto_run: false,
    };
    let ctx = RoundContext::new(config.clone());
    let collect = ctx.collect_args(&e.paths.nonces_file());
    assert_eq!(collect.len(), 7);
    assert_eq!(collect[1], "3", "扇区 0 → 块 3");
    assert_eq!(collect[3], "7", "扇区 1 → 块 7");
    assert_eq!(ctx.crypto1_args(&e.paths.nonces_file()).len(), 1);

    let mut r = HardNestedRunner::new();
    let unknown = vec![UnknownKeyInfo {
        sector: 1,
        key_type: KeyType::B,
    }];
    let known = [nfc_core::model::keys::KeyInfo {
        key: "ffffffffffff".into(),
        sector: 0,
        key_type: KeyType::A,
    }];
    assert_eq!(r.begin(&config, &unknown), Step::Collect);
    assert_eq!(r.after_collect(&config), Step::Crack);
    assert_eq!(r.after_crack(&config, 0), Step::Done);
    r.sync(&mut AppState::new());
    assert!(!known.is_empty());
}

/// 7. hard_nested autoRun 递归。
fn case_hard_nested_autorun() {
    let config = HardNestedConfig {
        known_key: "ffffffffffff".into(),
        known_sector: 0,
        known_key_type: KeyType::A,
        target_sector: 1,
        target_key_type: KeyType::B,
        collect_only: false,
        auto_run: true,
    };
    let known = vec![nfc_core::model::keys::KeyInfo {
        key: "ffffffffffff".into(),
        sector: 0,
        key_type: KeyType::A,
    }];
    let mut unknown: Vec<UnknownKeyInfo> = (1..=3)
        .map(|i| UnknownKeyInfo {
            sector: i,
            key_type: KeyType::B,
        })
        .collect();
    let total = unknown.len();
    let mut r = HardNestedRunner::new();
    let mut s = AppState::new();
    assert_eq!(r.begin(&config, &unknown), Step::Detect);
    let mut guard = 0;
    loop {
        guard += 1;
        assert!(guard < 50, "autoRun 不该无限递归");
        match r.after_detect(&known, &unknown) {
            Step::StopAllDecrypted => break,
            Step::Collect => {
                r.sync(&mut s);
                assert_eq!(*r.phase(), HardNestedPhase::Collecting);
                assert_eq!(r.after_collect(&config), Step::Crack);
                let head = unknown[0];
                assert!(remove_unknown(&mut unknown, head.sector, head.key_type));
                let before = unknown.len() + 1;
                assert_eq!(
                    r.after_crack_guarded(&config, before, unknown.len()),
                    Step::Detect
                );
            }
            other => panic!("意外分支: {other:?}"),
        }
    }
    assert!(unknown.is_empty());
    assert_eq!(r.total_unknown(), total);
}

/// 8. dict_test。
fn case_dict_test() {
    let e = Env::new();
    let mut s = e.state();
    let dict = e.dir.join("rockyou.dic");
    let name = apply_dict_choice(&mut s, Some(dict.clone())).expect("应返回文件名");
    assert_eq!(name, "rockyou.dic");
    let config = DictTestConfig {
        sector: 3,
        key_type: KeyType::B,
        start_position: 0,
    };
    let a = dict_args(&config, &dict);
    assert_eq!(a[0], "-s3");
    assert!(a[3].ends_with("rockyou.dic"));
    assert!(report_found_key(&mut s, "deadbeef1234", &e.paths).unwrap());
    let saved = fs::read_to_string(e.paths.keys_file()).unwrap();
    assert!(saved.contains("deadbeef1234"));
}

/// 12. cancel-task（中途 kill）。
fn case_cancel() {
    let mut runner =
        MockToolRunner::new(vec![Chunk::stdout("line 1\n"), Chunk::stdout("line 2\n")]);
    runner
        .start("mfoc", &mfoc_args(&Paths::new("/tmp"), &[]), &gate())
        .unwrap();
    assert!(runner.is_running());
    let mut got = Vec::new();
    runner.pump(&mut |c| got.push(c.clone()));
    assert_eq!(got.len(), 2);
    runner.kill();
    let mut after = Vec::new();
    runner.pump(&mut |c| after.push(c.clone()));
    assert!(after.is_empty(), "kill 后不再有输出");
    let ex = runner.exit().expect("kill 后应能取退出信息");
    assert!(ex.signal);
    assert_eq!(ex.result(), Err(nfc_core::runner::ToolError::Killed));
    assert!(!runner.is_running());
}

/// 附加：UID 解析与 save_keys 剔除默认密钥。
fn case_uid_and_keys() {
    let mut p = UidParser::new();
    p.push("UID (NFCID1): 11 22 33 44 55 66\n");
    p.flush();
    // 原版 command.js:643-645 固定 `substring(i, i + 14)`，且**先切窗口、
    // 后去空白**。本例整段 17 字符，14 字符窗口只到 "11 22 33 44 55"，
    // 去空白后剩 10 位 —— 末尾一字节被吃掉（见 docs/02 §6 登记）。
    assert_eq!(p.uid(), Some("1122334455"));

    let e = Env::new();
    // 默认密钥不应写入
    let fresh = save_keys(&e.paths, &["ffffffffffff".to_string()]).unwrap();
    assert!(fresh.is_empty());
    // 非默认密钥应写入
    let fresh = save_keys(&e.paths, &["deadbeef1234".to_string()]).unwrap();
    assert_eq!(fresh, vec!["deadbeef1234".to_string()]);
    let text = fs::read_to_string(e.paths.keys_file()).unwrap();
    assert!(text.contains("deadbeef1234"));
    assert!(!text.contains("ffffffffffff"));
    assert_eq!(DEFAULT_KEYS.len(), 13);
}

/// 附加：dump 往返与校验。
fn case_dump_roundtrip() {
    let data = synth_dump();
    let lines = to_sector_lines(&data).expect("1024B 应可转扇区行");
    assert_eq!(lines.len(), 16, "1024B = 16 扇区");
    assert_eq!(validate(&lines, true), Ok(()));
    let back = from_sector_lines(&lines).expect("应可还原");
    assert_eq!(back, data, "往返应一致");
    // 非 64 倍数应被拒
    assert!(to_sector_lines(&data[..1000]).is_err());
    // 空应被拒
    assert!(to_sector_lines(&[]).is_err());
    // trailer 切分
    let trailer = &lines[0][3];
    assert_eq!(trailer.len(), 32);
}

/// 附加：registry 往返。
fn case_registry() {
    let e = Env::new();
    let mut r = Registry::new();
    assert!(r
        .append_and_save(nfc_core::data::CardRecord::new("AABBCCDDEEFF"), &e.paths)
        .unwrap());
    // 大小写折叠去重
    assert!(!r
        .append_and_save(nfc_core::data::CardRecord::new("aabbccddeeff"), &e.paths)
        .unwrap());
    let back = Registry::load(&e.paths.registry_file()).unwrap();
    assert_eq!(back.len(), 1);
    assert!(back.find("AaBbCcDdEeFf").is_some());
}

/// 附加：时长格式化边界。
fn case_format_duration() {
    assert_eq!(format_duration(0), "00:00");
    assert_eq!(format_duration(3599), "59:59");
    assert_eq!(format_duration(3600), "01:00:00");
}

fn main() {
    println!("NFCToolsGUI selftest");
    println!("========================================");

    let cases: Vec<(&str, fn())> = vec![
        ("1  connect → detect → guidance", case_connect_detect),
        ("2  read-IC → dump 自动保存", case_read_ic_saves_dump),
        ("3  write-IC 确认写卡", case_write_ic),
        ("4  format-card", case_format_card),
        ("5  write-uid / lock-ufuid", case_write_uid_and_lock),
        ("6  hard_nested 手动全链", case_hard_nested_manual),
        ("7  hard_nested autoRun 递归", case_hard_nested_autorun),
        ("8  dict_test", case_dict_test),
        ("9  dump 往返与校验", case_dump_roundtrip),
        ("10 registry 往返与去重", case_registry),
        ("11 UID 解析 / save_keys 剔除默认", case_uid_and_keys),
        ("12 cancel-task（中途 kill）", case_cancel),
        ("13 format_duration 边界", case_format_duration),
        ("14 退出码四分支", check_exit_codes),
    ];

    let mut pass = 0usize;
    let mut fail = 0usize;
    for (name, f) in cases {
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
            Ok(()) => {
                println!("PASS  {name}");
                pass += 1;
            }
            Err(_) => {
                println!("FAIL  {name}");
                fail += 1;
            }
        }
    }

    println!("========================================");
    println!("合计 {pass} 项，通过 {pass}，失败 {fail}");
    if fail > 0 {
        std::process::exit(1);
    }
}
