//! 跨模块集成测试：串口枚举 → libnfc.conf → nfc-list → AppState。
//!
//! 全部用 `MockToolRunner` + `MockSerial` 驱动，零硬件、零二进制依赖。

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use nfc_core::model::dump::{
    block0_hex, from_sector_lines, split_trailer, to_sector_lines, uid_hex, validate, M1_SECTORS,
    M1_SIZE,
};
use nfc_core::model::keys::{is_default_key, save_keys, KeyType};
use nfc_core::model::parse::{
    CollectParser, KeyInfoParser, SingleKeyParser, UidParser, MARKER_MFOC_PROGRESS,
};
use nfc_core::paths::{Paths, DEVICE_SPEED};
use nfc_core::runner::{Chunk, GateState, MockToolRunner, ToolError, ToolRunner};
use nfc_core::serial::{
    disconnect, is_device_list_equal, judge_connect, write_nfc_config, ConnectFlow, DeviceError,
    MockSerial, MutableSerial, PortScanner, DEVICE_NONE, DEVICE_PREFIX,
};
use nfc_core::state::{AppState, DeviceState, Event, Indicator};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/");

static SEQ: AtomicUsize = AtomicUsize::new(0);

/// 建临时目录。返回 `Paths` 与配套的守卫。
///
/// 守卫持有本次创建的目录列表，测试结束（含 panic）时逐个删除。
fn tmp_guarded() -> (Paths, TempGuard) {
    let dir = std::env::temp_dir().join(format!(
        "nfc-core-int-{}-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::SeqCst),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&dir).unwrap();
    (Paths::new(dir.clone()), TempGuard(vec![dir]))
}

/// 无论断言通过还是 panic 都清理**本测试自己**创建的临时目录。
///
/// 不能用「共享 Vec + drop 时 drain 全部」的写法：libtest 默认多线程并行，
/// 任一测试结束就会连带删掉其他正在使用中的目录，导致 `NotFound` 这类
/// 与被测逻辑毫无关系的随机失败。
struct TempGuard(Vec<PathBuf>);

impl Drop for TempGuard {
    fn drop(&mut self) {
        for d in std::mem::take(&mut self.0) {
            let _ = fs::remove_dir_all(d);
        }
    }
}

fn read_fixture(name: &str) -> String {
    // `FIXTURES` 是运行时 `&str`，`concat!` 只收字面量，故此处用 `format!` 拼路径。
    fs::read_to_string(format!("{FIXTURES}{name}")).unwrap()
}

/// nfc-list 的门禁前置条件：必须有当前设备，且未连接（原版对 nfc-list 的放行条件）。
fn gs_connected(has_device: bool) -> GateState {
    GateState {
        has_current_device: has_device,
        is_device_connected: false,
        is_running_task: false,
        deep_scan: false,
    }
}

/// 跑完一次连接流程，返回 (判定结果, 最终状态, 全部事件)。
fn run_connect(script: Vec<Chunk>, device: &str) -> (Option<bool>, AppState, Vec<Event>) {
    let (paths, _guard) = tmp_guarded();
    let mut runner = MockToolRunner::new(script);
    let mut flow = ConnectFlow::new(device);
    let mut events = Vec::new();
    flow.begin(&paths, &mut runner, &gs_connected(true), &mut |e| {
        events.push(e)
    })
    .unwrap();
    flow.pump(&mut runner, &mut |e| events.push(e));
    let verdict = flow.finish(&mut runner, &mut |e| events.push(e));
    let mut s = AppState::new();
    for e in &events {
        s.apply(e.clone());
    }
    (verdict, s, events)
}

#[test]
fn connect_opened_reaches_connected_state() {
    let (verdict, s, events) = run_connect(
        vec![Chunk::stdout(read_fixture("nfc-list-opened.txt"))],
        "/dev/ttyACM0",
    );
    assert_eq!(verdict, Some(true));
    assert!(s.is_device_connected);
    assert!(matches!(s.device_state, DeviceState::Connected));
    assert!(!s.is_running_task);
    let logs: String = events
        .iter()
        .filter_map(|e| match e {
            Event::Log(t) => Some(t.as_str()),
            _ => None,
        })
        .collect();
    assert!(logs.contains("开始连接设备"));
    assert!(logs.contains("发现设备"));
    assert!(events.iter().any(|e| matches!(
        e,
        Event::Status {
            indicator: Indicator::Running,
            ..
        }
    )));
}

#[test]
fn connect_unable_reaches_failed_state() {
    let (verdict, s, events) = run_connect(
        vec![Chunk::stdout(read_fixture("nfc-list-unable.txt"))],
        "/dev/ttyACM0",
    );
    assert_eq!(verdict, Some(false));
    assert!(!s.is_device_connected);
    assert!(matches!(s.device_state, DeviceState::Failed));
    assert!(!s.is_running_task);
    assert!(events
        .iter()
        .any(|e| matches!(e, Event::Log(t) if t.contains("未发现设备"))));
}

#[test]
fn connect_with_empty_output_reaches_failed_state() {
    // 未命中任何判定标记 → 沿用原版语义：isDeviceConnected 保持 false
    let (verdict, s, _events) = run_connect(vec![], "/dev/ttyACM0");
    assert_eq!(verdict, Some(false));
    assert!(!s.is_device_connected);
    assert!(matches!(s.device_state, DeviceState::Failed));
}

#[test]
fn connect_rejected_without_current_device() {
    // 原版门禁：currentDevice === null 时无条件拒绝，含 nfc-list
    let (paths, _guard) = tmp_guarded();
    let mut runner = MockToolRunner::new(vec![]);
    let mut flow = ConnectFlow::new("/dev/ttyACM0");
    let mut events = Vec::new();
    let r = flow.begin(&paths, &mut runner, &gs_connected(false), &mut |e| {
        events.push(e)
    });
    assert!(matches!(r, Err(DeviceError::Tool(_))));
    assert!(runner.started.is_empty());
    assert!(events
        .iter()
        .any(|e| matches!(e, Event::DeviceState(DeviceState::Failed))));
}

#[test]
fn connect_busy_when_a_task_is_running() {
    let (paths, _guard) = tmp_guarded();
    let mut runner = MockToolRunner::new(vec![]);
    let mut flow = ConnectFlow::new("/dev/ttyACM0");
    let mut events = Vec::new();
    let mut busy = gs_connected(true);
    busy.is_running_task = true;
    let r = flow.begin(&paths, &mut runner, &busy, &mut |e| events.push(e));
    assert!(matches!(r, Err(DeviceError::Tool(ToolError::Busy))));
    assert!(runner.started.is_empty());
}

#[test]
fn connect_writes_libnfc_conf_before_spawning() {
    let (paths, _guard) = tmp_guarded();
    let mut runner = MockToolRunner::new(vec![]);
    let mut flow = ConnectFlow::new("/dev/ttyACM0");
    assert!(!paths.nfc_config().exists(), "begin 之前不应有配置文件");
    flow.begin(&paths, &mut runner, &gs_connected(true), &mut |_| {})
        .unwrap();
    let conf = fs::read_to_string(paths.nfc_config()).unwrap();
    assert!(conf.starts_with("device.name = \"NFC_Device\""));
    assert!(conf.contains(&format!("pn532_uart:/dev/ttyACM0:{DEVICE_SPEED}")));
    assert_eq!(
        runner.started.first().map(|s| s.0.as_str()),
        Some("nfc-list")
    );
    assert!(runner.started.first().unwrap().1.is_empty());
}

#[test]
fn kill_during_connect_truncates_pending_and_reports_killed() {
    let (paths, _guard) = tmp_guarded();
    // 脚本分两块，第二块才含成功标记；kill 之后它必须被丢弃，
    // 否则 nfc-list 被杀却仍被判为已连接。
    let mut runner = MockToolRunner::new(vec![
        Chunk::stdout("pn532_uart:/dev/ttyACM0:115200\n"),
        Chunk::stdout(read_fixture("nfc-list-opened.txt")),
    ]);
    let mut flow = ConnectFlow::new("/dev/ttyACM0");
    let mut events = Vec::new();
    flow.begin(&paths, &mut runner, &gs_connected(true), &mut |e| {
        events.push(e)
    })
    .unwrap();
    runner.kill();
    flow.pump(&mut runner, &mut |e| events.push(e));
    let mut s = AppState::new();
    s.is_running_task = true;
    let verdict = flow.finish(&mut runner, &mut |e| {
        let clone = e.clone();
        s.apply(clone);
        events.push(e)
    });
    assert_eq!(verdict, Some(false), "被 kill 且未观察到成功标记时应判失败");
    assert!(!s.is_running_task);
    assert!(matches!(s.device_state, DeviceState::Failed));
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, Event::Log(t) if t.contains("发现设备"))),
        "被截断的块不应产生结论日志"
    );
    assert!(events.iter().any(|e| matches!(
        e,
        Event::TaskDone {
            code: None,
            killed: true
        }
    )));
    // Error 事件由 AppState::apply 内部合成（`state.events`），不回流到 emit
    // 回调——事件流只承载"外部推给 core 的输入"，core 自行派生的状态不进流。
    assert!(
        s.events
            .iter()
            .any(|e| e.kind == nfc_core::state::EventKind::Error),
        "被 kill 应在 state.events 留一条 Error，实际: {:?}",
        s.events
    );
}

#[test]
fn connect_then_disconnect_resets_all_flags() {
    let (mut _verdict, mut s, _events) = run_connect(
        vec![Chunk::stdout(read_fixture("nfc-list-opened.txt"))],
        "/dev/ttyACM0",
    );
    s.current_device = Some("/dev/ttyACM0".into());
    let mut events = Vec::new();
    disconnect(&mut s, &mut |e| events.push(e));
    assert!(s.current_device.is_none());
    assert!(!s.is_device_connected);
    assert!(matches!(s.device_state, DeviceState::None));
    assert_eq!(events.len(), 1);
}

#[test]
fn scanner_pushes_device_list_only_on_change() {
    let mut scanner =
        PortScanner::new(Box::new(MockSerial::from(["/dev/ttyACM0", "/dev/ttyACM1"])));
    let mut s = AppState::new();
    let mut frames = Vec::new();
    for _ in 0..3 {
        let changed = scanner.scan_if_changed().unwrap().is_some();
        if changed {
            let list = scanner.ports().to_vec();
            s.apply(Event::DeviceList(list));
        }
        frames.push(changed);
    }
    assert_eq!(frames, vec![true, false, false]);
    assert_eq!(s.device_list.len(), 2);
    assert_eq!(
        s.device_list,
        vec!["/dev/ttyACM0".to_string(), "/dev/ttyACM1".to_string()]
    );
}

#[test]
fn scanner_detects_unplug_then_replug() {
    // 必须用同一个 scanner + 可变端口列表：换 scanner 会重置 `last`，
    // 把"拔线"误判成首次扫描，测不到变化检测本身。
    let mock = MutableSerial::new(&["/dev/ttyACM0"]);
    let mut scanner = PortScanner::new(Box::new(mock.clone()));
    let mut s = AppState::new();
    let apply_if_changed = |scanner: &mut PortScanner, s: &mut AppState| {
        if let Some(list) = scanner.scan_if_changed().unwrap() {
            s.apply(Event::DeviceList(list));
        }
    };
    apply_if_changed(&mut scanner, &mut s);
    assert_eq!(s.device_list, vec!["/dev/ttyACM0".to_string()]);
    // 重复扫描：列表没变 → 不推送（原版 index.js:66 的 isListEqual 短路）
    apply_if_changed(&mut scanner, &mut s);
    assert_eq!(s.device_list.len(), 1, "列表未变不应重复推送");
    // 拔线
    mock.set(&[]);
    apply_if_changed(&mut scanner, &mut s);
    assert!(s.device_list.is_empty());
    // 换口热插入
    mock.set(&["/dev/ttyUSB0"]);
    apply_if_changed(&mut scanner, &mut s);
    assert_eq!(s.device_list, vec!["/dev/ttyUSB0".to_string()]);
}

#[test]
fn scanner_prefix_stripping_contract_matches_renderer() {
    // 对标 preload.js:7 的 devicePrefix 与 index.js:79 的
    // `v.replace(mainProcess.devicePrefix, "")`。
    //
    // 关键：原版是 `String.replace`（**子串**替换，非前缀剥离）。
    // macOS 上 nfc-list 给的是 /dev/tty.usbserial-XXXX，没有那个点，
    // 于是前缀原样显示。本测试锁定这一"缺陷但属原版行为"的契约。
    assert_eq!(DEVICE_PREFIX, "/dev/tty.");
    assert_eq!(DEVICE_NONE, " ");

    // ⚠️ 关键：DEVICE_PREFIX 带尾点（`/dev/tty.`），而 Linux 的 libnfc 设备名
    // 形如 `/dev/ttyACM0`——**没有那个点**，所以 `replace` 一无所获，界面上
    // 显示的仍是完整路径。这不是我们改错，是原版就如此（preload.js:7）。
    // 真正会被剥掉的只有 macOS 的 `/dev/tty.usbserial-XXXX`（点后的部分）。
    assert_eq!(
        "/dev/ttyACM0".replace(DEVICE_PREFIX, ""),
        "/dev/ttyACM0",
        "Linux 设备名不含尾点，原版不剥离"
    );
    assert_eq!(
        "/dev/tty.usbserial-1234".replace(DEVICE_PREFIX, ""),
        "usbserial-1234",
        "macOS 设备名含尾点，会被剥掉 /dev/tty."
    );
    assert_eq!(
        "/dev/bus/usb/001".replace(DEVICE_PREFIX, ""),
        "/dev/bus/usb/001"
    );
}

#[test]
fn list_equality_is_order_sensitive_like_renderer_is_list_equal() {
    let a = vec!["a".to_string(), "b".to_string()];
    assert!(is_device_list_equal(&a, &["a".into(), "b".into()]));
    assert!(!is_device_list_equal(&a, &["b".into(), "a".into()]));
    assert!(is_device_list_equal(&[], &[]));
    assert!(!is_device_list_equal(&a, &["a".into()]));
}

#[test]
fn judge_connect_is_fixture_driven_for_all_three_cases() {
    assert_eq!(
        judge_connect(&read_fixture("nfc-list-opened.txt")),
        Some(true)
    );
    assert_eq!(
        judge_connect(&read_fixture("nfc-list-unable.txt")),
        Some(false)
    );
    // nfc-list 找不到设备时 stdout 为空
    assert_eq!(judge_connect(&read_fixture("nfc-list-nofound.txt")), None);
}

#[test]
fn write_nfc_config_round_trips_through_paths() {
    let (paths, _guard) = tmp_guarded();
    write_nfc_config(&paths, "/dev/ttyUSB0", DEVICE_SPEED).unwrap();
    assert_eq!(
        fs::read_to_string(paths.nfc_config()).unwrap(),
        Paths::libnfc_conf("/dev/ttyUSB0", DEVICE_SPEED)
    );
}

// ==================================================================== S4 数据模型

/// 把 fixture 按固定宽度切碎，模拟真实子进程的 chunk 边界与行边界无关。
///
/// 宽度 1 最严苛：任何依赖「标记与取值同处一个 chunk」的实现都会漏判。
fn chunks_of(text: &str, width: usize) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    chars
        .chunks(width)
        .map(|c| c.iter().collect::<String>())
        .collect()
}

fn feed_key_info(width: usize) -> KeyInfoParser {
    let mut p = KeyInfoParser::new();
    for c in chunks_of(&read_fixture("mfdetect-keys.txt"), width) {
        p.push(&c);
    }
    p.flush();
    p
}

#[test]
fn key_info_parses_full_1k_card_from_fixture() {
    let p = feed_key_info(usize::MAX);
    // 扇区 0-3 的 A/B 全已知，扇区 4-15 只有 A 已知
    assert_eq!(p.known().len(), 20);
    assert_eq!(p.unknown().len(), 12);
    assert_eq!(p.known()[0].sector, 0);
    assert_eq!(p.known()[0].key_type, KeyType::A);
    assert_eq!(p.known()[0].key, "ffffffffffff");
    assert_eq!(p.unknown()[0].sector, 4);
    assert_eq!(p.unknown()[0].key_type, KeyType::B);
    // 扇区号覆盖满 1K 的 16 个扇区
    let all: std::collections::BTreeSet<u8> = p
        .known()
        .iter()
        .map(|k| k.sector)
        .chain(p.unknown().iter().map(|k| k.sector))
        .collect();
    assert_eq!(all.len(), 16);
    assert_eq!(*all.iter().next().unwrap(), 0);
    assert_eq!(*all.iter().next_back().unwrap(), 15);
}

#[test]
fn key_info_result_is_identical_for_every_chunk_width() {
    // 原版 bug #2：扇区号取 chunk 内序号，chunk 边界即整体错位
    let baseline = feed_key_info(usize::MAX);
    for width in [1usize, 2, 3, 7, 13, 31, 64, 200] {
        let p = feed_key_info(width);
        assert_eq!(
            p.known(),
            baseline.known(),
            "chunk 宽度 {width} 下 known 不一致"
        );
        assert_eq!(
            p.unknown(),
            baseline.unknown(),
            "chunk 宽度 {width} 下 unknown 不一致"
        );
        assert_eq!(
            p.new_keys(),
            baseline.new_keys(),
            "chunk 宽度 {width} 下 new_keys 不一致"
        );
    }
}

#[test]
fn key_info_width_one_matches_expected_sectors() {
    // 上一条测试只做「各宽度互比」，若解析器在所有宽度下同样错（如 fixture 缺前导
    // 空格）就会全绿。这里给绝对锚点。
    let p = feed_key_info(1);
    assert_eq!(p.known().len(), 20);
    assert_eq!(p.unknown().len(), 12);
    assert_eq!(p.known()[0].sector, 0);
    assert_eq!(p.known()[0].key_type, KeyType::A);
    assert_eq!(p.known()[0].key, "ffffffffffff");
    assert_eq!(p.unknown()[0].sector, 4);
    assert_eq!(p.unknown()[0].key_type, KeyType::B);
}

#[test]
fn new_keys_from_detect_are_all_usable_keys() {
    let p = feed_key_info(usize::MAX);
    // 原版 newKeys.join(key) 死代码 ⇒ 恒空，读卡读到的已知密钥从不落盘
    assert!(!p.new_keys().is_empty());
    assert!(p.new_keys().iter().all(|k| k.len() == 12));
    assert!(p
        .new_keys()
        .iter()
        .all(|k| k.chars().all(|c| c.is_ascii_hexdigit())));
}

#[test]
fn mfoc_fixture_yields_uid_and_progress_and_keys() {
    let text = read_fixture("mfoc-output.txt");
    // 该常量 S4 只登记、S5 的 ETA 解析才消费；此处仅锁定 fixture 里的出现次数
    assert_eq!(text.matches(MARKER_MFOC_PROGRESS).count(), 3);

    let mut uid = UidParser::new();
    let mut keys = KeyInfoParser::new();
    for c in chunks_of(&text, 5) {
        uid.push(&c);
        keys.push(&c);
    }
    uid.flush();
    keys.flush();

    // 原版 command.js:643-645 固定 `substring(i, i + 14)`：fixture 里是
    // 16 位（8 字节）UID，原版同样只取前 14 位。此处锁定该行为。
    assert_eq!(uid.uid(), Some("a1b2c3d4e5f6a7"));
    assert_eq!(
        uid.uid().unwrap().len(),
        14,
        "原版 substring(i, i+14) 固定 14 字符窗口"
    );
    assert_eq!(keys.known().len(), 20);
    assert_eq!(keys.unknown().len(), 12);
}

#[test]
fn mfoc_uid_is_stable_across_chunk_widths() {
    let text = read_fixture("mfoc-output.txt");
    for width in [1usize, 4, 17, 64, usize::MAX] {
        let mut uid = UidParser::new();
        for c in chunks_of(&text, width) {
            uid.push(&c);
        }
        uid.flush();
        assert_eq!(uid.uid(), Some("a1b2c3d4e5f6a7"), "宽度 {width}");
    }
}

#[test]
fn collect_fixture_yields_uid_sector_and_keytype() {
    let text = read_fixture("collect-output.txt");
    for width in [1usize, 6, 40, usize::MAX] {
        let mut p = CollectParser::new();
        for c in chunks_of(&text, width) {
            p.push(&c);
        }
        p.flush();
        assert_eq!(p.info().uid.as_deref(), Some("a1b2c3d4"), "宽度 {width}");
        assert_eq!(p.info().sector, Some(11), "宽度 {width}");
        assert_eq!(p.info().key_type, Some(KeyType::A), "宽度 {width}");
    }
}

#[test]
fn cropto1_fixture_yields_the_found_key() {
    for width in [1usize, 9, 33, usize::MAX] {
        let mut p = SingleKeyParser::cropto1();
        for c in chunks_of(&read_fixture("cropto1-output.txt"), width) {
            p.push(&c);
        }
        p.flush();
        assert_eq!(p.take().as_deref(), Some("8fd0a4f256e9"), "宽度 {width}");
    }
}

#[test]
fn mfdict_fixture_yields_the_found_key() {
    for width in [1usize, 9, 33, usize::MAX] {
        let mut p = SingleKeyParser::mfdict();
        for c in chunks_of(&read_fixture("mfdict-output.txt"), width) {
            p.push(&c);
        }
        p.flush();
        assert_eq!(p.take().as_deref(), Some("714c5c886e97"), "宽度 {width}");
    }
}

#[test]
fn detect_keys_that_are_all_defaults_leave_keys_file_empty() {
    // 本 fixture 的 new_keys 只有 ffffffffffff 与 a0a1a2a3a4a5，全是默认密钥。
    // 这正是 bug #6 的意义：一张出厂卡读出来的密钥不该污染 keys.txt。
    let (paths, _guard) = tmp_guarded();

    let p = feed_key_info(usize::MAX);
    assert!(!p.new_keys().is_empty());
    assert!(
        p.new_keys().iter().all(|k| is_default_key(k)),
        "fixture 里应当全是默认密钥"
    );

    let fresh = save_keys(&paths, p.new_keys()).unwrap();
    assert!(fresh.is_empty(), "默认密钥不应作为新增上报");
    assert_eq!(fs::read_to_string(paths.keys_file()).unwrap(), "");
}

#[test]
fn save_keys_merges_dedupes_and_strips_defaults_against_existing() {
    let (paths, _guard) = tmp_guarded();

    // 先落一把非默认密钥
    save_keys(&paths, &["111111111111".to_string()]).unwrap();
    // 再来：重复的、大写的默认密钥、全新的
    let fresh = save_keys(
        &paths,
        &[
            "111111111111".to_string(), // 重复，不算新增
            "FFFFFFFFFFFF".to_string(), // 大写默认密钥，bug #6 必须剔掉
            "deadbeef1234".to_string(), // 全新
        ],
    )
    .unwrap();

    assert_eq!(fresh, vec!["deadbeef1234"]);
    assert_eq!(
        fs::read_to_string(paths.keys_file()).unwrap(),
        "111111111111\ndeadbeef1234"
    );

    // 二次保存无新增 ⇒ 不重复上报
    let again = save_keys(&paths, &["deadbeef1234".to_string()]).unwrap();
    assert!(again.is_empty());
}

#[test]
fn save_keys_treats_missing_file_as_empty_set() {
    let (paths, _guard) = tmp_guarded();
    assert!(!paths.keys_file().exists());
    let fresh = save_keys(&paths, &["deadbeef1234".to_string()]).unwrap();
    assert_eq!(fresh, vec!["deadbeef1234"]);
}

#[test]
fn save_keys_keeps_user_supplied_key_and_joins_without_trailing_newline() {
    let (paths, _guard) = tmp_guarded();
    let incoming = vec!["cafebabe5678".to_string(), "deadbeef1234".to_string()];
    save_keys(&paths, &incoming).unwrap();
    let stored = fs::read_to_string(paths.keys_file()).unwrap();
    assert_eq!(stored, "cafebabe5678\ndeadbeef1234");
    assert!(!stored.ends_with('\n'));
}

#[test]
fn save_keys_leaves_no_temp_file_behind() {
    let (paths, _guard) = tmp_guarded();
    save_keys(&paths, &["deadbeef1234".to_string()]).unwrap();
    let siblings: Vec<String> = fs::read_dir(paths.base())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        siblings.iter().all(|n| !n.ends_with(".tmp")),
        "残留：{siblings:?}"
    );
}

#[test]
fn dump_roundtrip_through_sector_lines_is_lossless() {
    // 1K M1 全扇区，用可预期的字节模式
    let raw: Vec<u8> = (0..M1_SIZE).map(|i| (i % 253) as u8).collect();
    let sectors = to_sector_lines(&raw).unwrap();
    assert_eq!(sectors.len(), M1_SECTORS);
    assert_eq!(validate(&sectors, true), Ok(()));
    assert_eq!(from_sector_lines(&sectors).unwrap(), raw);
}

#[test]
fn dump_sector_zero_roundtrips_to_block0_and_uid() {
    let raw: Vec<u8> = (0..M1_SIZE).map(|i| i as u8).collect();
    let sectors = to_sector_lines(&raw).unwrap();
    assert_eq!(
        block0_hex(&sectors).unwrap(),
        "000102030405060708090A0B0C0D0E0F"
    );
    assert_eq!(uid_hex(&sectors).unwrap(), "00010203040506");
}

#[test]
fn dump_hex_text_rejects_malformed_input_instead_of_silently_shortening() {
    // 原版 Buffer.from 静默截断 ⇒ 写出短 dump ⇒ 被写进卡里
    let sectors = vec![vec![
        "0".repeat(32),
        "0".repeat(32),
        "0".repeat(32),
        // 只有第 3 行含非法字符：前 3 行合法，才能确保走到字符集判定而非长度判定
        format!("{}Z", "0".repeat(31)),
    ]];
    assert!(validate(&sectors, true).is_err());
    assert!(from_sector_lines(&sectors).is_err());
}

#[test]
fn real_m1_sector_zero_from_original_harness_validates() {
    let sectors = vec![vec![
        "0000000000000000000000000000FFFF".to_string(),
        "4807E000000000000000000000000000".to_string(),
        "A0000000000000000000000000000000".to_string(),
        "FF078000000000000000000000000000".to_string(),
    ]];
    assert_eq!(validate(&sectors, true), Ok(()));
    let t = split_trailer(&sectors[0][3]).unwrap();
    assert_eq!(t.key_a, "FF0780000000");
    assert_eq!(t.access, "000000");
    assert_eq!(t.gpd, "00");
    assert_eq!(t.key_b, "000000000000");
}

// ============================================================ S5 读卡闭环

use nfc_core::data::{CardRecord, Registry};
use nfc_core::dialogs::MockDialog;
use nfc_core::tasks::detect::{
    after_detect, check_key_file_exists, detect_args, guidance_for_unknown, DetectOutcome,
};
use nfc_core::tasks::mfoc::{
    apply_eta, count_keys_done, dump_save, format_duration, mfoc_args, mfoc_keys_total, EtaTracker,
    SaveFailure, SaveOutcome,
};

/// 用真实 fixture 跑一遍 nfc-mfdetect（刻意按 17 字节切碎，模拟 chunk 边界），
/// 返回处置结果与最终状态。
fn run_detect(fixture: &str, is_save_dump: bool) -> (DetectOutcome, AppState, KeyInfoParser) {
    let (paths, _guard) = tmp_guarded();
    if is_save_dump {
        fs::write(paths.temp_mfd(), b"x".repeat(1024)).unwrap();
    }
    let mut parser = KeyInfoParser::new();
    for chunk in read_fixture(fixture).as_bytes().chunks(17) {
        parser.push(std::str::from_utf8(chunk).unwrap());
    }
    parser.flush();
    let mut s = AppState::new();
    check_key_file_exists(&paths).unwrap();
    s.apply(Event::Status {
        text: nfc_core::i18n::t(&nfc_core::tasks::detect::detect_status(is_save_dump)),
        indicator: Indicator::Running,
    });
    let out = after_detect(&mut s, &parser, &paths, is_save_dump).unwrap();
    (out, s, parser)
}

/// mfdetect-keys.txt 混有 Unknown 行，故走 NeedGuidance 之外的 readICThenExec 语义：
/// **仍应 Proceed**（原版 write-IC / format-card 不因 unknown 被拦）。
#[test]
fn s5_detect_real_fixture_proceeds_despite_unknown_keys() {
    let (out, s, parser) = run_detect("mfdetect-keys.txt", false);
    assert!(
        !parser.unknown().is_empty(),
        "fixture 应含 Unknown 行，否则本测试无意义"
    );
    assert_eq!(
        out,
        DetectOutcome::Proceed,
        "readICThenExec 路径不因 unknown 密钥拦下主命令"
    );
    assert_eq!(s.status.text, "正在检测卡片");
}

#[test]
fn s5_detect_unknown_keys_raise_guidance_with_two_buttons() {
    let (_, _, parser) = run_detect("mfdetect-keys.txt", false);
    let (_, text, buttons) = guidance_for_unknown(&parser).expect("应产出引导");
    assert!(text.contains("未知扇区密钥"), "实际: {text}");
    assert!(text.contains(&parser.unknown().len().to_string()));
    assert_eq!(buttons.len(), 2);
    assert_eq!(buttons[0].0, "hard-nested");
    assert_eq!(buttons[1].0, "dict-test");
}

#[test]
fn s5_detect_no_guidance_when_all_keys_known() {
    let (_paths, _guard) = tmp_guarded();
    let mut parser = KeyInfoParser::new();
    parser.push(" Found   Key A: ffffffffffff\n");
    parser.flush();
    assert!(parser.unknown().is_empty());
    assert!(guidance_for_unknown(&parser).is_none());
}

#[test]
fn s5_detect_args_shape_differs_by_mode() {
    let (paths, _guard) = tmp_guarded();
    let with = detect_args(&paths, true, false);
    let without = detect_args(&paths, false, false);
    assert!(with[0].starts_with("-O"));
    assert_eq!(without[0], "-N");
    assert_eq!(with[1], without[1], "两种模式的 -f 应相同");
}

/// 完整 mfoc 链：喂 ETA 输出 → 落盘 → 断言 AppState 全字段。
#[test]
fn s5_mfoc_full_chain_saves_dump_and_updates_state() {
    let (paths, _guard) = tmp_guarded();
    fs::write(paths.temp_mfd(), b"DUMP".repeat(256)).unwrap();

    // 基线：13 默认 + 1 文件密钥 = 14 轮
    let args = mfoc_args(&paths, &[]);
    let total = mfoc_keys_total(1, &args);
    assert_eq!(total, 14);
    assert_eq!(count_keys_done("[Key: a][Key: b]"), 2);

    let mut s = AppState::new();
    s.apply(Event::Status {
        text: "正在解卡 - 预计剩余 00:30".into(),
        indicator: Indicator::Running,
    });

    // 逐秒喂 14 轮。t=0 那帧被节流吞掉（bug #15），末轮收敛。
    let mut eta = EtaTracker::new(total, 0);
    let mut frames = 0;
    for i in 0..total {
        let u = eta.push("[Key: aabbccddeeff]\n", (i as u64) * 1_000);
        if let Some(u) = u {
            apply_eta(&mut s, &u);
            frames += 1;
        }
    }
    assert!(eta.is_finished());
    assert!(frames >= 2, "节流后仍应产出多帧，实际 {frames}");
    assert_eq!(s.progress, Some(100.0));
    // 收敛必须把状态条回落为纯文案（这条断言依赖起始值带 ETA 串，
    // 否则"什么都没做"也会通过）
    assert_eq!(s.status.text, "正在解卡");

    // 落盘：用户选了路径
    let target = paths.base().join("chosen.mfd");
    let mut dlg = MockDialog::new().with_save(Some(target.clone()));
    let out = dump_save(&mut s, &paths, Some("112233445566"), &mut dlg);
    assert_eq!(
        out,
        SaveOutcome::Saved {
            target: target.clone(),
            chosen: true
        }
    );
    assert!(target.exists());
    assert!(!paths.temp_mfd().exists());
    assert_eq!(s.dump_history, vec!["chosen.mfd".to_string()]);

    // 收尾事件
    s.apply(Event::TaskDone {
        code: Some(0),
        killed: false,
    });
    assert!(!s.is_running_task);
}

/// mfoc 输出末行无换行时，flush 必须结算，否则进度卡在 99% 永不收敛。
#[test]
fn s5_mfoc_eta_flush_converges_on_last_line_without_newline() {
    let mut eta = EtaTracker::new(2, 0);
    assert!(eta.push("[Key: aabbccddeeff]\n", 0).is_none());
    // 末行无 `\n` → LineBuffer 暂存，push 阶段 keys_done 仍为 1，2 >= 2 不成立。
    // t=1000 恰好等于节流窗口（ETA_THROTTLE_MS），若此刻结算得 percent=50，
    // 但 bytes 尚未成行，故 push 走的是"节流"分支返回 None。
    assert_eq!(eta.push("[Key: aabbccddeeff]", 1_000), None);
    assert_eq!(eta.keys_done(), 1, "末行未 flush 前不计数");
    assert!(!eta.is_finished());
    // flush 结算末行 → 2 >= 2 收敛
    let u = eta.flush(1_000).expect("flush 应结算");
    assert_eq!(u.percent, Some(100.0));
    assert!(eta.is_finished());
}

#[test]
fn s5_mfoc_cancel_save_uses_default_name_with_uid() {
    let (paths, _guard) = tmp_guarded();
    fs::write(paths.temp_mfd(), b"DUMP").unwrap();
    let mut s = AppState::new();
    let mut dlg = MockDialog::new().with_save(None);
    let out = dump_save(&mut s, &paths, Some("aabbccddeeff"), &mut dlg);
    let SaveOutcome::Saved { target, chosen } = out else {
        panic!("取消也应自动保存");
    };
    assert!(!chosen);
    let name = target.file_name().unwrap().to_string_lossy().to_string();
    assert!(name.starts_with("aabbccddeeff_"), "实际: {name}");
    assert!(target.exists());
    assert_eq!(s.dump_history.len(), 1);
}

#[test]
fn s5_mfoc_empty_dump_never_opens_dialog() {
    let (paths, _guard) = tmp_guarded();
    fs::write(paths.temp_mfd(), b"").unwrap();
    let mut s = AppState::new();
    // 预设一个「若被弹框就会用到」的目标，用来证明它根本没被弹
    let unused = paths.base().join("never.mfd");
    let mut dlg = MockDialog::new().with_save(Some(unused.clone()));
    let out = dump_save(&mut s, &paths, Some("aabb"), &mut dlg);
    assert_eq!(out, SaveOutcome::Empty);
    assert_eq!(dlg.save_calls(), 0, "空 dump 不应弹框");
    assert!(!unused.exists(), "未弹框则目标不应存在");
    assert!(s.log_buffer.contains("未读到卡内容"));
}

/// 移动失败时必须是 `Failed { reason: Move }`，调用方据此 printExitLog(1)；
/// 且 temp.mfd 保留（它是唯一副本），日志带上它的路径供用户抢救。
#[test]
fn s5_mfoc_move_failure_reports_reason_and_keeps_temp() {
    let (paths, _guard) = tmp_guarded();
    fs::write(paths.temp_mfd(), b"KEEPME").unwrap();
    let mut s = AppState::new();
    // 目标父目录不存在 → 移动必然失败
    let bad = paths.base().join("missing-dir").join("out.mfd");
    let mut dlg = MockDialog::new().with_save(Some(bad));
    let out = dump_save(&mut s, &paths, Some("aabb"), &mut dlg);
    assert_eq!(
        out,
        SaveOutcome::Failed {
            reason: SaveFailure::Move
        }
    );
    assert!(
        paths.temp_mfd().exists(),
        "失败时 temp.mfd 是唯一副本，不应被删"
    );
    assert!(
        s.log_buffer
            .contains(&paths.temp_mfd().display().to_string()),
        "日志应带 temp 路径，实际: {}",
        s.log_buffer
    );
}

#[test]
fn s5_eta_boundaries_match_original_format() {
    // docs/03 要求的 0 / 59 / 3599 / 3600 四个边界
    assert_eq!(format_duration(0), "00:00");
    assert_eq!(format_duration(59), "00:59");
    assert_eq!(format_duration(3599), "59:59");
    assert_eq!(format_duration(3600), "01:00:00");
}

/// 用 MockToolRunner 喂 mfoc 输出，验证 ETA 在真实 chunk 边界下的收敛。
#[test]
fn s5_eta_converges_over_mock_runner_chunks() {
    let (paths, _guard) = tmp_guarded();
    let script: Vec<Chunk> = (0..5)
        .map(|i| Chunk::stdout(format!("[Key: key{i:02}]\n")))
        .collect();
    let mut runner = MockToolRunner::new(script);
    runner
        .start(
            "mfoc",
            &mfoc_args(&paths, &[]),
            &GateState {
                has_current_device: true,
                is_device_connected: true,
                is_running_task: false,
                deep_scan: false,
            },
        )
        .unwrap();

    // MockToolRunner::pump 一次排空全部 pending，故逐块手工分帧：
    // 每次只喂一块，模拟真实进程的分次输出，并让时钟按 1s 步进。
    let mut eta = EtaTracker::new(5, 0);
    let mut s = AppState::new();
    let mut clock = 0u64;
    let mut fed = 0usize;
    let script_text: Vec<String> = (0..5).map(|i| format!("[Key: key{i:02}]\n")).collect();
    for (i, text) in script_text.iter().enumerate() {
        if let Some(u) = eta.push(text, clock) {
            apply_eta(&mut s, &u);
        }
        clock += 1_000;
        fed += 1;
        let _ = i;
    }
    // 5/5 轮后应收敛（末帧直接判收敛，不受节流影响）
    assert_eq!(fed, 5);
    assert!(eta.is_finished());
    assert_eq!(s.progress, Some(100.0));
    // runner 侧也能正常收尾
    runner.pump(&mut |_| {});
    assert!(runner.exit().is_some());
}

#[test]
fn s5_registry_appends_and_persists_across_reload() {
    let (paths, _guard) = tmp_guarded();
    let mut r = Registry::new();
    for i in 0..3 {
        assert!(r
            .append_and_save(
                CardRecord {
                    uid: format!("uid{i}"),
                    date: "2024-01-01".into(),
                    ..CardRecord::default()
                },
                &paths
            )
            .unwrap());
    }
    assert!(!r.append_and_save(CardRecord::new("uid0"), &paths).unwrap());
    let back = Registry::load(&paths.registry_file()).unwrap();
    assert_eq!(back.len(), 3);
    assert_eq!(back.uids(), vec!["uid0", "uid1", "uid2"]);
    let tmp = PathBuf::from(format!("{}.tmp", paths.registry_file().display()));
    assert!(!tmp.exists(), "原子写不应留临时文件");
}

#[test]
fn s5_registry_records_new_keys_from_detect() {
    let (paths, _guard) = tmp_guarded();
    let mut parser = KeyInfoParser::new();
    parser.push(&read_fixture("mfdetect-keys.txt"));
    parser.flush();
    let mut r = Registry::new();
    let known = parser.known();
    assert!(!known.is_empty(), "fixture 应含已知密钥");
    let uid = "112233445566";
    assert!(r.append(CardRecord {
        uid: uid.into(),
        key_a: known[0].key.clone(),
        status: "detected".into(),
        date: "2024-01-01".into(),
        ..CardRecord::default()
    }));
    r.save(&paths.registry_file()).unwrap();
    let back = Registry::load(&paths.registry_file()).unwrap();
    assert!(back.find(uid).unwrap().has_keys());
}

#[test]
fn s5_detect_failure_paths_do_not_persist_keys() {
    let (paths, _guard) = tmp_guarded();
    let mut parser = KeyInfoParser::new();
    // 用**非默认**密钥：a0a1a2a3a4a5 是 DEFAULT_KEYS 第 2 项，会被剔除，
    // 用它断言"应落盘"会得到空文件。
    parser.push("Sector 00 - Found   Key A: 112233445566\n");
    parser.flush();
    let mut s = AppState::new();
    // 落盘模式下 temp 不存在 → 读卡失败
    let e = after_detect(&mut s, &parser, &paths, true).unwrap_err();
    assert!(!e.is_empty());
    // 但新密钥仍应已落盘（原版 command.js:596 的 saveKeys 在读卡失败判定之前）
    let saved = fs::read_to_string(paths.keys_file()).unwrap_or_default();
    assert!(saved.contains("112233445566"), "实际: {saved:?}");

    // 对照：全是默认密钥时不应有任何落盘
    let (paths2, _g2) = tmp_guarded();
    let mut p2 = KeyInfoParser::new();
    p2.push("Sector 00 - Found   Key A: a0a1a2a3a4a5\n");
    p2.flush();
    let mut s2 = AppState::new();
    let _ = after_detect(&mut s2, &p2, &paths2, true);
    assert_eq!(
        fs::read_to_string(paths2.keys_file())
            .unwrap_or_default()
            .trim(),
        "",
        "默认密钥应被剔除"
    );
}

// ============================================================ S6/S7 写卡与破解

use nfc_core::state::DictTestConfig;
use nfc_core::tasks::card_write::{
    block0_is_valid, mfclassic_format_args, mfclassic_write_args, mflock_args, mfsetuid_args,
    prepare_lock, prepare_write, prepare_write_uid, read_block0_hex, resolve_dump_id,
    PrepareOutcome, WriteFailure,
};
use nfc_core::tasks::dict_test::{apply_dict_choice, dict_args, report_found_key};
use nfc_core::tasks::hard_nested::{
    remove_unknown, HardNestedRunner, RoundContext, Step, TOOL_COLLECT, TOOL_CRYPTO1,
};
use nfc_core::tasks::phw::{block0_bcc_ok, patch_data_block, SectorBlock};

#[test]
fn s6_write_chain_builds_mfclassic_and_confirms() {
    let (paths, _guard) = tmp_guarded();
    let _s = AppState::new();
    let src = paths.base().join("src.mfd");
    let mut dlg = MockDialog::new()
        .with_open(Some(src.clone()))
        .with_confirm(true);
    assert_eq!(
        prepare_write(&paths, None, &mut dlg),
        PrepareOutcome::Ready(src.clone()),
        "write-IC 选中的文件必须随结果返回，否则调用方无从构造 nfc-mfclassic 第 4 参"
    );
    // 参数逐字断言
    let args = mfclassic_write_args(&src, &paths.temp_mfd());
    assert_eq!(
        args,
        vec![
            "w".to_string(),
            "A".to_string(),
            "u".to_string(),
            args[3].clone(),
            args[4].clone(),
            "f".to_string()
        ]
    );
    assert!(args[3].ends_with("src.mfd"));
    assert!(args[4].ends_with("temp.mfd"));
}

#[test]
fn s6_write_cancelled_never_builds_args() {
    let (paths, _guard) = tmp_guarded();
    let s = AppState::new();
    let mut dlg = MockDialog::new()
        .with_open(Some(paths.base().join("src.mfd")))
        .with_confirm(false);
    assert_eq!(
        prepare_write(&paths, None, &mut dlg),
        PrepareOutcome::Cancelled
    );
    assert!(s.events.is_empty());
    assert!(s.log_buffer.is_empty());
}

#[test]
fn s6_format_chain_uses_temp_twice() {
    let (paths, _guard) = tmp_guarded();
    let args = mfclassic_format_args(&paths.temp_mfd());
    assert_eq!(args[0], "f");
    assert_eq!(args[3], args[4]);
    assert!(args[3].ends_with("temp.mfd"));
    assert_eq!(args[5], "f");
}

#[test]
fn s6_write_uid_chain_reads_block0_and_confirms() {
    let (paths, _guard) = tmp_guarded();
    let dump = paths.base().join("uid.mfd");
    fs::write(&dump, vec![0xab; 32]).unwrap();
    let mut s = AppState::new();
    let mut dlg = MockDialog::new().with_confirm(true);
    let hex = prepare_write_uid(&mut s, &paths, &mut dlg, Some(dump)).expect("应通过");
    assert!(block0_is_valid(&hex));
    assert_eq!(mfsetuid_args(&hex), vec!["-q".to_string(), hex.clone()]);
    // 确认框正文必须含即将写入的 block0
    assert!(dlg.confirm_requests[0].message.contains(&hex));
}

#[test]
fn s6_write_uid_short_dump_is_rejected_before_confirm() {
    let (paths, _guard) = tmp_guarded();
    let dump = paths.base().join("short.mfd");
    fs::write(&dump, vec![0xab; 4]).unwrap();
    let mut s = AppState::new();
    let mut dlg = MockDialog::new().with_confirm(true);
    let e = prepare_write_uid(&mut s, &paths, &mut dlg, Some(dump)).unwrap_err();
    assert_eq!(
        e,
        PrepareOutcome::Failed {
            reason: WriteFailure::TooShort
        }
    );
    assert_eq!(dlg.confirm_calls(), 0, "过短不该弹确认框");
}

#[test]
fn s6_lock_chain_requires_confirm() {
    let mut ok = MockDialog::new().with_confirm(true);
    assert_eq!(prepare_lock(&mut ok), PrepareOutcome::ReadyNoFile);
    assert_eq!(mflock_args(), vec!["-q"]);

    let mut no = MockDialog::new().with_confirm(false);
    assert_eq!(prepare_lock(&mut no), PrepareOutcome::Cancelled);
}

#[test]
fn s6_block0_and_dump_id_helpers() {
    let (paths, _guard) = tmp_guarded();
    let p = paths.base().join("a.mfd");
    fs::write(&p, vec![0xcd; 16]).unwrap();
    assert_eq!(read_block0_hex(&p).unwrap(), "CD".repeat(16));
    assert_eq!(
        resolve_dump_id("x.mfd", &paths.dumpfiles_dir()),
        paths.dumpfiles_dir().join("x.mfd")
    );
    assert_eq!(
        resolve_dump_id("/abs/y.mfd", &paths.dumpfiles_dir()),
        PathBuf::from("/abs/y.mfd")
    );
}

#[test]
fn s7_hard_nested_collect_then_crack_chain() {
    let (paths, _guard) = tmp_guarded();
    let config = nfc_core::state::HardNestedConfig {
        known_key: "ffffffffffff".into(),
        known_sector: 0,
        known_key_type: nfc_core::model::keys::KeyType::A,
        target_sector: 1,
        target_key_type: nfc_core::model::keys::KeyType::B,
        collect_only: false,
        auto_run: false,
    };
    let ctx = RoundContext::new(config);
    // 采集
    let c = ctx.collect_args(&paths.nonces_file());
    assert_eq!(c.len(), 7);
    assert_eq!(c[0], "ffffffffffff");
    assert_eq!(c[1], "3", "扇区 0 → 块 3");
    assert_eq!(c[3], "7", "扇区 1 → 块 7");
    assert!(c[6].ends_with("nonces.bin"));
    assert_eq!(TOOL_COLLECT, "libnfc-collect");
    // 破解
    assert_eq!(ctx.crypto1_args(&paths.nonces_file()).len(), 1);
    assert_eq!(TOOL_CRYPTO1, "cropto1_bs");
}

#[test]
fn s7_auto_run_loop_drains_unknown() {
    let mut unknown = vec![
        nfc_core::model::keys::UnknownKeyInfo {
            sector: 1,
            key_type: nfc_core::model::keys::KeyType::B,
        },
        nfc_core::model::keys::UnknownKeyInfo {
            sector: 2,
            key_type: nfc_core::model::keys::KeyType::A,
        },
    ];
    let total = unknown.len();
    let config = nfc_core::state::HardNestedConfig {
        known_key: "ffffffffffff".into(),
        known_sector: 0,
        known_key_type: nfc_core::model::keys::KeyType::A,
        target_sector: 1,
        target_key_type: nfc_core::model::keys::KeyType::B,
        collect_only: false,
        auto_run: true,
    };
    let known = vec![nfc_core::model::keys::KeyInfo {
        key: "ffffffffffff".into(),
        sector: 0,
        key_type: nfc_core::model::keys::KeyType::A,
    }];
    let mut r = HardNestedRunner::new();
    assert_eq!(r.begin(&config, &unknown), Step::Detect);
    // 逐轮解掉队首
    let mut guard = 0;
    while !unknown.is_empty() && guard < 10 {
        assert_eq!(r.after_detect(&known, &unknown), Step::Collect);
        assert_eq!(r.after_collect(&config), Step::Crack);
        let head = unknown[0];
        assert!(remove_unknown(&mut unknown, head.sector, head.key_type));
        assert_eq!(r.after_crack(&config, unknown.len()), Step::Detect);
        guard += 1;
    }
    assert!(unknown.is_empty(), "autoRun 应解完所有 unknown");
    assert_eq!(r.total_unknown(), total);
    // 现在探测会判定为「已全部解出」
    assert_eq!(r.after_detect(&known, &unknown), Step::StopAllDecrypted);
}

#[test]
fn s7_hard_nested_no_known_key_stops() {
    let mut r = HardNestedRunner::new();
    let config = nfc_core::state::HardNestedConfig {
        auto_run: true,
        collect_only: false,
        ..Default::default()
    };
    let unknown = vec![nfc_core::model::keys::UnknownKeyInfo {
        sector: 1,
        key_type: nfc_core::model::keys::KeyType::A,
    }];
    r.begin(&config, &unknown);
    assert_eq!(r.after_detect(&[], &unknown), Step::StopNoKnownKey);
}

#[test]
fn s7_dict_test_chain_saves_key_and_updates_path() {
    let (paths, _guard) = tmp_guarded();
    let mut s = AppState::new();
    // 选字典
    let dict = paths.base().join("rockyou.dic");
    let name = apply_dict_choice(&mut s, Some(dict.clone())).expect("应返回文件名");
    assert_eq!(name, "rockyou.dic");
    assert_eq!(s.dict_path, dict);
    // 参数
    let cfg = DictTestConfig {
        sector: 3,
        key_type: nfc_core::model::keys::KeyType::B,
        start_position: 0,
    };
    let a = dict_args(&cfg, &dict);
    assert_eq!(a[0], "-s3");
    assert_eq!(a[1], "-tB");
    assert_eq!(a[2], "-l0");
    assert!(a[3].ends_with("rockyou.dic"));
    // 命中落盘
    assert!(report_found_key(&mut s, "deadbeef1234", &paths).unwrap());
    let saved = fs::read_to_string(paths.keys_file()).unwrap();
    assert!(saved.contains("deadbeef1234"));
}

/// S14：写数据扇区「读回→改块→写回」的数据层契约（对标 `commands.rs:1176`）。
///
/// 模拟 `nfc-mfdetect -O` 读回一个 1K 卡到 temp.mfd，把 block0 写进扇区 0 块 1，
/// 验证只改目标块、整卡写回用 temp 自身当 keys.mfd（第 4/5 参同一路径）。
#[test]
fn s14_phw_write_data_read_back_patch_and_self_keys() {
    let (paths, _guard) = tmp_guarded();
    // 扇区 0 块 1 之外预置非零，验证改块不误伤
    let mut dump = vec![0u8; 1024];
    dump[32..48].copy_from_slice(&[0xEE; 16]); // 扇0块2
    fs::write(paths.temp_mfd(), &dump).unwrap();

    let block0 = "04A8A68A800400010000000000000000"; // BCC 自洽
    assert!(block0_bcc_ok(block0));
    let target = SectorBlock {
        sector: 0,
        block: 1,
    };
    assert!(target.is_valid());

    // 读回 → 改块 → 写回（引擎 patch_phw_block 的等价数据操作）
    let mut bytes = fs::read(paths.temp_mfd()).unwrap();
    assert!(patch_data_block(&mut bytes, block0, target));
    fs::write(paths.temp_mfd(), &bytes).unwrap();

    let check = fs::read(paths.temp_mfd()).unwrap();
    assert_eq!(&check[16..32], hex_bytes(block0), "目标块应被改写");
    assert_eq!(check[32..48], [0xEE; 16], "块 2 不应被动");

    // 整卡写回：nfc-mfclassic w A u <temp> <temp> f（temp 自作 keys）
    let args = mfclassic_write_args(&paths.temp_mfd(), &paths.temp_mfd());
    assert_eq!(&args[..3], &["w", "A", "u"]);
    assert_eq!(args[3], args[4], "源与 keys 必须同为读回 dump");
    assert_eq!(args[5], "f");
}

/// 32 位 hex → 16 字节。
fn hex_bytes(hex: &str) -> [u8; 16] {
    let mut out = [0u8; 16];
    for i in 0..16 {
        out[i] = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).unwrap();
    }
    out
}
