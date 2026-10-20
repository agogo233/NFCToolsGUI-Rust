//! 串口枚举与设备连接。对标原版 `SerialPort.list()` / `conn-usb-devices` / `setNFCConfig`。
//!
//! 生产实现走 `serialport` crate（`real-serial` feature），单测用 `MockSerial`，
//! 因此本模块默认编译不引入系统库依赖。

use std::fs;

use crate::i18n;
use crate::paths::{Paths, DEVICE_SPEED};
use crate::runner::{Chunk, GateState, ToolError, ToolRunner};
use crate::state::{AppState, DeviceState, Event, Indicator};

/// 下拉列表展示时剥离的串口前缀，对标 preload.js `devicePrefix`。
pub const DEVICE_PREFIX: &str = "/dev/tty.";

/// 「断开设备」的动作参数。原版用单个空格表示。
pub const DEVICE_NONE: &str = " ";

/// 连接成功标记，对标原版 `value.indexOf("NFC device: NFC_Device opened") >= 0`。
pub const MARKER_OPENED: &str = "NFC device: NFC_Device opened";

/// 连接失败标记。
pub const MARKER_UNABLE: &str = "Unable to open NFC device";

/// 串口枚举与连接流程的错误。
#[derive(thiserror::Error, Debug, Clone, PartialEq, Eq)]
pub enum DeviceError {
    /// 枚举失败（权限 / 系统调用失败）。
    #[error("serial enumeration failed: {0}")]
    Enumerate(String),
    /// 写 `libnfc.conf` 失败。
    #[error("write libnfc.conf failed: {0}")]
    WriteConfig(String),
    /// `nfc-list` 启动失败。保留 `ToolError` 原类型，调用方可区分
    /// NotConnected（门禁拒绝）/ Busy（任务运行中）/ SpawnFailed。
    #[error(transparent)]
    Tool(#[from] ToolError),
}

/// 串口枚举抽象。生产实现 `RealSerial`，单测与演示用 `MockSerial`。
/// `Send + Sync`：允许持 `Box<dyn SerialTrait>` 的 `PortScanner` 跨线程移动。
pub trait SerialTrait: Send + Sync {
    /// 枚举全部串口短名，对标 `SerialPort.list()` 后取 `port["path"]`。
    fn available_ports(&self) -> Result<Vec<String>, DeviceError>;
}

/// 固定列表实现，供单测与零硬件演示使用。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MockSerial {
    pub ports: Vec<String>,
}

/// 可在测试运行中替换端口列表的 mock，用 `Arc<Mutex<..>>` 共享。
///
/// 热插拔的真实形态是**同一个** scanner 每 500ms 轮询一次、列表发生变化。
/// 固定列表的 `MockSerial` 做不到这点——换 scanner 会把 `last` 一并重置，
/// 于是"拔线"（列表变空）会被误判成"首次扫描"，测不出变化检测逻辑。
#[derive(Debug, Clone, Default)]
pub struct MutableSerial {
    inner: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

impl MutableSerial {
    pub fn new(ports: &[&str]) -> Self {
        Self {
            inner: std::sync::Arc::new(std::sync::Mutex::new(
                ports.iter().map(|s| (*s).to_string()).collect(),
            )),
        }
    }

    /// 替换端口列表（模拟热插拔）。
    pub fn set(&self, ports: &[&str]) {
        *self.inner.lock().unwrap() = ports.iter().map(|s| (*s).to_string()).collect();
    }
}

impl SerialTrait for MutableSerial {
    fn available_ports(&self) -> Result<Vec<String>, DeviceError> {
        Ok(self.inner.lock().unwrap().clone())
    }
}

impl MockSerial {
    pub fn from<I, S>(ports: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            ports: ports.into_iter().map(Into::into).collect(),
        }
    }
}

impl SerialTrait for MockSerial {
    fn available_ports(&self) -> Result<Vec<String>, DeviceError> {
        Ok(self.ports.clone())
    }
}

/// 生产实现，对标 `SerialPort.list()`。
#[cfg(feature = "real-serial")]
#[derive(Debug, Default, Clone, Copy)]
pub struct RealSerial;

#[cfg(feature = "real-serial")]
impl SerialTrait for RealSerial {
    fn available_ports(&self) -> Result<Vec<String>, DeviceError> {
        // serialport 4.x 的 `SerialPortInfo` 没有 `path` 字段，设备路径为 `port_name`。
        serialport::available_ports()
            .map(|ports| ports.into_iter().map(|p| p.port_name).collect())
            .map_err(|e| DeviceError::Enumerate(e.to_string()))
    }
}

/// 对标 renderer `isListEqual`：长度相同且逐元素相等（顺序敏感）。
pub fn is_device_list_equal(a: &[String], b: &[String]) -> bool {
    a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| x == y)
}

/// 热插拔扫描器：结果与上次推送比对，变化才返回 `Some`。
///
/// 轮询周期（500ms）由 app 侧定时器负责，本结构只管比对。
pub struct PortScanner {
    serial: Box<dyn SerialTrait>,
    last: Vec<String>,
}

impl PortScanner {
    pub fn new(serial: Box<dyn SerialTrait>) -> Self {
        Self {
            serial,
            last: Vec::new(),
        }
    }

    /// 上次推送的列表（从未推送过时为空）。
    pub fn ports(&self) -> &[String] {
        &self.last
    }

    pub fn scan_if_changed(&mut self) -> Result<Option<Vec<String>>, DeviceError> {
        let ports = self.serial.available_ports()?;
        if is_device_list_equal(&ports, &self.last) {
            Ok(None)
        } else {
            self.last = ports.clone();
            Ok(Some(ports))
        }
    }
}

/// 写 `libnfc.conf`。内容见 `Paths::libnfc_conf`，波特率恒 115200。
pub fn write_nfc_config(paths: &Paths, device: &str) -> Result<(), DeviceError> {
    if let Some(parent) = paths.nfc_config().parent() {
        fs::create_dir_all(parent)
            .map_err(|e| DeviceError::WriteConfig(e.to_string()))?;
    }
    fs::write(paths.nfc_config(), Paths::libnfc_conf(device, DEVICE_SPEED))
        .map_err(|e| DeviceError::WriteConfig(e.to_string()))
}

/// 判定累计的 `nfc-list` 输出：`Some(true)` 已连接、`Some(false)` 未连接、`None` 未知。
///
/// 两个标记同现时以成功优先（实际输出不会同时出现）。
pub fn judge_connect(text: &str) -> Option<bool> {
    if text.contains(MARKER_OPENED) {
        Some(true)
    } else if text.contains(MARKER_UNABLE) {
        Some(false)
    } else {
        None
    }
}

/// `nfc-list` 连接流程。对标原版 `conn-usb-devices` + `setNFCConfig`：
/// `begin` 写配置并启动，`pump` 逐块判定，`finish` 收尾。
///
/// 调用方须先设置 `state.current_device` 再算 `GateState`，否则 `gate` 会把
/// `nfc-list` 判为 `NotConnected`（原版门禁在 `currentDevice === null` 时无条件拒绝）。
pub struct ConnectFlow {
    device: String,
    output: String,
    connected: Option<bool>,
}

impl ConnectFlow {
    pub fn new(device: impl Into<String>) -> Self {
        Self {
            device: device.into(),
            output: String::new(),
            connected: None,
        }
    }

    pub fn device(&self) -> &str {
        &self.device
    }

    /// 写 `libnfc.conf` 并启动 `nfc-list`。任何一步失败都置 `DeviceState::Failed`。
    pub fn begin(
        &mut self,
        paths: &Paths,
        runner: &mut dyn ToolRunner,
        gs: &GateState,
        emit: &mut dyn FnMut(Event),
    ) -> Result<(), DeviceError> {
        emit(Event::Status {
            text: i18n::t("indicator_connecting_device"),
            indicator: Indicator::Running,
        });
        if let Err(e) = write_nfc_config(paths, &self.device) {
            emit(Event::DeviceState(DeviceState::Failed));
            return Err(e);
        }
        emit(Event::DeviceState(DeviceState::Connecting));
        emit(Event::Log(format!(
            "\n\n### {} ###\n",
            i18n::t("log_msg_start_connect_device")
        )));
        runner.start("nfc-list", &[], gs).map_err(|e| {
            emit(Event::DeviceState(DeviceState::Failed));
            DeviceError::Tool(e)
        })
    }

    /// 排空已到达的输出块。先推原始输出块，命中判定标记后再追加结论日志
    /// （对标原版 `printLog(data)` 之后才调 `processHandler`）。
    ///
    /// 判定分两条独立通道，对标原版 `command.js:793-802`：
    /// - stdout 里的 `NFC device: … opened` → 连上了（`processHandler`）
    /// - stderr 里的 `Unable to open NFC device` → 打不开（`stderrHandler`）
    ///
    /// 原版这两条**互不覆盖**：stderr 那条在进程退出时才跑，且会把先前的
    /// true 纠正成 false。我们早先把两个流混进一个缓冲区、用 `judge_connect`
    /// 一次判定，导致「先 opened 后 unable」永远停在 true——UI 显示已连接，
    /// 而卡实际读不出来，且无任何错误提示。pn532 打开成功但握手失败即此情形。
    pub fn pump(&mut self, runner: &mut dyn ToolRunner, emit: &mut dyn FnMut(Event)) {
        runner.pump(&mut |c: &Chunk| {
            emit(Event::Log(c.text.clone()));
            self.output.push_str(&c.text);
            // 两个标记各自独立判定、与到达顺序无关：任一命中即可定论，
            // **后到的那个仍会覆盖**——这正是原版两个回调的语义。
            // 只看本块文本，不重扫历史，避免重复打结论日志。
            if let Some(v) = judge_connect(&c.text) {
                if self.connected != Some(v) {
                    emit(Event::Log(format!(
                        "\n*** {} ***\n",
                        i18n::t(if v {
                            "log_msg_discover_device"
                        } else {
                            "log_msg_not_found_device"
                        })
                    )));
                }
                self.connected = Some(v);
            }
        });
    }

    /// 进程退出后收尾。返回判定结果，`None` 表示尚未退出。
    ///
    /// 顺序对标原版 `setNFCConfig` 的 close 回调：先 `update-device-status`，
    /// 再走 `printExitLog`（即 `TaskDone`）。
    pub fn finish(
        &mut self,
        runner: &mut dyn ToolRunner,
        emit: &mut dyn FnMut(Event),
    ) -> Option<bool> {
        let exit = runner.exit()?;
        let connected = self.connected.unwrap_or(false);
        emit(Event::DeviceState(if connected {
            DeviceState::Connected
        } else {
            DeviceState::Failed
        }));
        emit(Event::TaskDone {
            code: exit.code,
            killed: exit.signal,
        });
        Some(connected)
    }
}

/// 断开设备：对标原版 `conn-usb-devices` 的 `device === " "` 分支
/// （清 `currentDevice` + 回推 `device` 状态）。
///
/// 走 `state.apply` 而非只改字段：`AppState::apply(Event::DeviceState)` 才会
/// 同步 `is_device_connected` 与 `device_state`。早期版本只把 `current_device`
/// 置 None 就返回，导致 `is_device_connected` 仍为 true —— UI 上表现为拔线后
/// 状态条还挂着"已连接"，且 `gate()` 继续放行需要连接的 action。
pub fn disconnect(state: &mut AppState, emit: &mut dyn FnMut(Event)) {
    state.current_device = None;
    let e = Event::DeviceState(DeviceState::None);
    state.apply(e.clone());
    emit(e);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::MockToolRunner;
    use std::fs;
    use std::path::PathBuf;

    const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/");

    /// 临时目录序号，避免同一进程内并行测试撞名。
    static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    /// 父目录不存在的 base，用于触发 `fs::write` 失败。
    const NO_PARENT: &str = "/nonexistent-parent-for-s3-test/nfc";

    fn read_fixture(name: &str) -> String {
        fs::read_to_string(format!("{FIXTURES}{name}")).unwrap()
    }

    /// 建临时目录并返回配套守卫：守卫只删自己那一个目录，
    /// 避免 libtest 多线程下互相删目录。
    fn tmp_guarded() -> (Paths, TempDir) {
        let p = std::env::temp_dir().join(format!(
            "nfc-s3-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        (Paths::new(p.clone()), TempDir(p))
    }

    struct TempDir(PathBuf);

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn tmp_paths(tag: &str) -> Paths {
        let dir = std::env::temp_dir().join(format!(
            "nfc-core-s3-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        DIRS.with(|d| d.borrow_mut().push(dir.clone()));
        Paths::new(dir)
    }

    /// 收集本测试创建的临时目录，末尾统一清理（测试并行，不可用同一目录名）。
    fn drop_dirs(dirs: &[std::path::PathBuf]) {
        for d in dirs {
            let _ = fs::remove_dir_all(d);
        }
    }

    thread_local! {
        static DIRS: std::cell::RefCell<Vec<std::path::PathBuf>> =
            const { std::cell::RefCell::new(Vec::new()) };
    }

    fn kind(e: &Event) -> &'static str {
        match e {
            Event::Log(_) => "Log",
            Event::Status { .. } => "Status",
            Event::DeviceList(_) => "DeviceList",
            Event::DeviceState(DeviceState::None) => "DeviceState:None",
            Event::DeviceState(DeviceState::Connecting) => "DeviceState:Connecting",
            Event::DeviceState(DeviceState::Connected) => "DeviceState:Connected",
            Event::DeviceState(DeviceState::Failed) => "DeviceState:Failed",
            Event::Progress(_) => "Progress",
            Event::EventItem { .. } => "EventItem",
            Event::Guidance { .. } => "Guidance",
            Event::DumpSaved(_) => "DumpSaved",
            Event::BinaryData { .. } => "BinaryData",
            Event::HardNestedConfig(_) => "HardNestedConfig",
            Event::DictTestConfig(_) => "DictTestConfig",
            Event::DictHistory(_) => "DictHistory",
            Event::DictFileName(_) => "DictFileName",
            Event::SavedBinary { .. } => "SavedBinary",
            Event::TaskDone { .. } => "TaskDone",
        }
    }

    fn gs() -> GateState {
        GateState {
            has_current_device: true,
            is_device_connected: false,
            is_running_task: false,
        }
    }

    // ---------- is_device_list_equal ----------

    #[test]
    fn is_list_equal_matrix() {
        let a = vec!["a".to_string(), "b".to_string()];
        assert!(is_device_list_equal(
            &a,
            &["a".to_string(), "b".to_string()]
        ));
        assert!(is_device_list_equal(&[], &[]));
        assert!(!is_device_list_equal(&a, &["b".into(), "a".into()]));
        assert!(!is_device_list_equal(&a, &["a".into()]));
        assert!(!is_device_list_equal(&[], &["a".into()]));
        assert!(!is_device_list_equal(&a, &[]));
    }

    // ---------- PortScanner ----------

    #[test]
    fn port_scanner_drops_duplicate_lists() {
        let mut s = PortScanner::new(Box::new(MockSerial::from(["a", "b"])));
        assert!(s.scan_if_changed().unwrap().is_some());
        assert!(s.scan_if_changed().unwrap().is_none());
        assert_eq!(s.ports(), &["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn port_scanner_reports_hotplug_both_ways() {
        let mut s = PortScanner::new(Box::new(MockSerial::from(["a"])));
        assert!(s.scan_if_changed().unwrap().is_some());
        // 拔线 → 空列表也算变化
        s.serial = Box::new(MockSerial::from([] as [&str; 0]));
        assert!(s.scan_if_changed().unwrap().unwrap().is_empty());
        // 热插入两个 → 顺序变化也算变化
        let mut s = PortScanner::new(Box::new(MockSerial::from(["b", "a"])));
        assert_eq!(
            s.scan_if_changed().unwrap().unwrap(),
            vec!["b".to_string(), "a".to_string()]
        );
    }

    #[test]
    fn port_scanner_propagates_enumeration_error() {
        struct Boom;
        impl SerialTrait for Boom {
            fn available_ports(&self) -> Result<Vec<String>, DeviceError> {
                Err(DeviceError::Enumerate("no permission".into()))
            }
        }
        let mut s = PortScanner::new(Box::new(Boom));
        assert_eq!(
            s.scan_if_changed(),
            Err(DeviceError::Enumerate("no permission".into()))
        );
        assert!(s.ports().is_empty());
    }

    // ---------- judge_connect ----------

    /// libnfc 的 `ERR()`（nfc-list.c:155）走 **stderr**，而
    /// `NFC device: … opened` 走 **stdout**。原版 `command.js:794-802` 因此把它们
    /// 分给两个回调：`processHandler` 读 stdout（连上 → true），
    /// `stderrHandler` 读 stderr（打不开 → false）——后者在**进程退出时**才跑。
    ///
    /// 我们把两个流混进一个缓冲区、用 `judge_connect` 判，且判到一次就
    /// `connected.is_none()` 短路不再改。若 stderr 在 opened 之后才到unable，
    /// 最终状态会永远停在 true。
    ///
    /// 这不是臆想的场景：pn532 打开成功但握手失败时，libnfc 会先 ERR 出一条
    /// `Unable to open NFC device` 再非零退出。此时 UI 显示「已连接」，
    /// 用户点读卡却什么都读不出来，且没有任何错误提示。
    #[test]
    fn opened_then_unable_must_not_stay_reported_as_connected() {
        let paths = tmp_paths("mix");
        let mut runner = MockToolRunner::new(vec![]).with_scripts(vec![vec![
            Chunk::stdout("NFC device: NFC_Device opened\n"),
            // 握手失败：libnfc ERR() 写 stderr
            Chunk::stderr_chunk("Unable to open NFC device\n"),
        ]]);
        let mut flow = ConnectFlow::new("/dev/ttyACM0");
        let mut events = Vec::new();
        flow.begin(&paths, &mut runner, &gs(), &mut |e| events.push(e))
            .unwrap();
        for _ in 0..8 {
            flow.pump(&mut runner, &mut |e| events.push(e));
        }
        assert_eq!(
            flow.finish(&mut runner, &mut |e| events.push(e)),
            Some(false),
            "最终必须报未连接：原版 close 回调会把 isDeviceConnected 纠正成 false"
        );
        drop_dirs(&DIRS.with(|d| d.borrow().clone()));
    }

    #[test]
    fn judge_connect_opened_fixture_is_true() {
        assert_eq!(
            judge_connect(&read_fixture("nfc-list-opened.txt")),
            Some(true)
        );
    }

    #[test]
    fn judge_connect_unable_fixture_is_false() {
        assert_eq!(
            judge_connect(&read_fixture("nfc-list-unable.txt")),
            Some(false)
        );
    }

    #[test]
    fn judge_connect_quiet_output_is_unknown() {
        // 只含噪声、不含任一标记 → 判不出（等更多输出）
        assert_eq!(judge_connect(""), None);
        assert_eq!(
            judge_connect("0 ISO14443A passive target(s) found.\n"),
            None
        );
        // 半截标记不算命中：真实 `ERR("Unable to open NFC device: %s", …)`
        // （nfc-list.c:155）带设备串，但仅凭前缀也不足以定论——此处锁死
        // 「标记必须完整出现」，防止将来把 `Unable` 单独当判定依据。
        assert_eq!(judge_connect("Unable to open NFC"), None);
        assert_eq!(judge_connect("NFC device: NFC_Device open"), None);
    }

    #[test]
    fn judge_connect_survives_marker_split_across_chunks() {
        // 原版按 data 块判定，标记被拆两块会漏判；累计判定可命中
        assert_eq!(judge_connect("NFC device: NFC_"), None);
        assert_eq!(judge_connect("NFC device: NFC_Device opened"), Some(true));
    }

    #[test]
    fn judge_connect_both_markers_prefers_opened() {
        assert_eq!(
            judge_connect(&format!("{MARKER_UNABLE}\n{MARKER_OPENED}")),
            Some(true)
        );
    }

    // ---------- write_nfc_config ----------

    #[test]
    fn write_nfc_config_writes_expected_content() {
        let paths = tmp_paths("write");
        write_nfc_config(&paths, "/dev/ttyACM0").unwrap();
        assert!(paths.nfc_config().is_file());
        assert_eq!(
            fs::read_to_string(paths.nfc_config()).unwrap(),
            "device.name = \"NFC_Device\"\ndevice.connstring = \"pn532_uart:/dev/ttyACM0:115200\""
        );
        let dirs: Vec<_> = DIRS.with(|d| d.borrow().clone());
        drop_dirs(&dirs);
    }

    #[test]
    fn write_nfc_config_reports_error_when_parent_missing() {
        let paths = Paths::new(NO_PARENT);
        let err = write_nfc_config(&paths, "/dev/ttyACM0").unwrap_err();
        assert!(matches!(err, DeviceError::WriteConfig(_)));
        assert!(err.to_string().contains("libnfc.conf"));
    }

    // ---------- ConnectFlow ----------

    #[test]
    fn begin_writes_conf_and_starts_nfc_list() {
        let paths = tmp_paths("begin");
        let mut runner = MockToolRunner::new(vec![]);
        let mut flow = ConnectFlow::new("/dev/ttyACM0");
        let mut events = Vec::new();
        let r = flow.begin(&paths, &mut runner, &gs(), &mut |e| events.push(e));
        assert!(r.is_ok());
        assert_eq!(
            fs::read_to_string(paths.nfc_config()).unwrap(),
            Paths::libnfc_conf("/dev/ttyACM0", DEVICE_SPEED)
        );
        let st = runner.started.first().expect("已 start nfc-list");
        assert_eq!(st.0, "nfc-list");
        assert!(st.1.is_empty());
        let kinds: Vec<_> = events.iter().map(kind).collect();
        // 顺序锁定，对标原版 conn-usb-devices：状态条 → 连接中 → 日志横幅
        assert_eq!(kinds, vec!["Status", "DeviceState:Connecting", "Log"]);
        assert!(events
            .iter()
            .any(|e| matches!(e, Event::Log(t) if t.contains("开始连接设备"))));
        assert!(events
            .iter()
            .all(|e| !matches!(e, Event::DeviceState(DeviceState::Failed))));
        let dirs: Vec<_> = DIRS.with(|d| d.borrow().clone());
        drop_dirs(&dirs);
    }

    #[test]
    fn begin_write_failure_marks_failed_and_does_not_start() {
        let mut runner = MockToolRunner::new(vec![]);
        let mut flow = ConnectFlow::new("/dev/ttyACM0");
        let mut events = Vec::new();
        let r = flow.begin(&Paths::new(NO_PARENT), &mut runner, &gs(), &mut |e| {
            events.push(e)
        });
        assert!(matches!(r, Err(DeviceError::WriteConfig(_))));
        assert!(events
            .iter()
            .any(|e| matches!(e, Event::DeviceState(DeviceState::Failed))));
        assert!(runner.started.is_empty());
    }

    #[test]
    fn finish_emits_device_state_before_task_done() {
        let (paths, _guard) = tmp_guarded();
        let mut runner =
            MockToolRunner::new(vec![Chunk::stdout(read_fixture("nfc-list-opened.txt"))]);
        let mut flow = ConnectFlow::new("/dev/ttyACM0");
        let mut events = Vec::new();
        // 必须先 begin：pump 只在 runner 已 start 后才有输出可读
        // （MockToolRunner 在 job 为 None 时直接返回）。
        flow.begin(
            &paths,
            &mut runner,
            &GateState {
                has_current_device: true,
                is_device_connected: false,
                is_running_task: false,
            },
            &mut |e| events.push(e),
        )
        .unwrap();
        flow.pump(&mut runner, &mut |e| events.push(e));
        flow.finish(&mut runner, &mut |e| events.push(e));
        let dev = events
            .iter()
            .position(|e| matches!(e, Event::DeviceState(DeviceState::Connected)))
            .unwrap_or_else(|| panic!("未见 Connected 事件，实际: {events:?}"));
        let done = events
            .iter()
            .position(|e| matches!(e, Event::TaskDone { .. }))
            .unwrap_or_else(|| panic!("未见 TaskDone 事件，实际: {events:?}"));
        assert!(dev < done, "update-device-status 应早于 printExitLog");
    }

    #[test]
    fn disconnect_clears_device_and_state() {
        let mut s = AppState::new();
        s.current_device = Some("/dev/ttyACM0".into());
        s.apply(Event::DeviceState(DeviceState::Connected));
        let mut events = Vec::new();
        disconnect(&mut s, &mut |e| events.push(e));
        assert!(s.current_device.is_none());
        assert!(!s.is_device_connected);
        assert!(matches!(s.device_state, DeviceState::None));
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], Event::DeviceState(DeviceState::None)));
    }

    #[test]
    fn sentinels_match_original() {
        assert_eq!(DEVICE_NONE, " ");
        assert_eq!(DEVICE_PREFIX, "/dev/tty.");
        assert!(MARKER_OPENED.starts_with("NFC device: NFC_"));
        assert_eq!(MARKER_UNABLE, "Unable to open NFC device");
    }
}
