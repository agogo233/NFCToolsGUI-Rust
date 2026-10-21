//! 子进程执行框架，对标原版 `src/execUtils.js::exec`。
//!
//! 原版用 `child_process.spawn(binPath + cmd, args)` 启动 `framework/bin` 下的 C 工具，
//! 把 stdout/stderr 的每个 data 块同时回灌日志并交给 `processHandler`，进程 `close` 时按
//! `(code, signal)` 分流：被信号终止 → 事件条 error + 复位空闲；非零退出 → 打印
//! `exit code: N` + 事件条 error + 置出错；`code === 0` 不做收尾（由调用方触发）。
//!
//! 这里改成「启动 / 排空 / 取退出」三步的 pull 模型，便于 egui 每帧非阻塞驱动：
//! `start` 一次、`pump` 每帧、`exit` 拿到退出码后再由调用方决定收尾。

use std::collections::VecDeque;
use std::fmt;
use std::io::{ErrorKind, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;

/// 唯一豁免「未连接」门禁的工具名，对标 execUtils.js 的 `cmd !== "nfc-list"`。
pub const ALWAYS_ALLOWED: &str = "nfc-list";

/// 向上查找 `framework/bin` 时最多上溯几层。
///
/// 无界上溯有风险：开发机上从 `target/debug` 一路走到 `/`，
/// 中途任何一层若恰好有 `framework/bin`（比如用户在别处解开的源码包），
/// 就会被误命中并 spawn 无关的同名程序。故设上限。
const MAX_LOOKUP_LEVELS: usize = 5;

/// 环境变量：显式指定工具目录，优先级最高。
///
/// 为什么需要：向上查找只能覆盖「工具与本仓库同处一棵祖先树」的情况。
/// 但本项目把 C 工具**完全外包**给了原版 NFCToolsGUI（它们是 git 子模块），
/// 开发者完全可能把它 clone 在**任意**位置——比如
/// `~/work/NFCToolsGUI` 与 `~/work/NFC/NFCToolsGUI-Rust` 平级。此时两者是**兄弟**目录，
/// 向上查找永远看不到，程序启动就是一条红条。
///
/// 集成测试（`tests/real_tools.rs`、`tests/tool_cli.rs`）此前各自实现了
/// 「先看环境变量」的查找逻辑，而生产路径没有——两处行为不一致。
/// 现统一到 [`framework_bin`]，测试与程序用同一套规则。
pub const FRAMEWORK_BIN_ENV: &str = "NFC_FRAMEWORK_BIN";

/// 定位 `framework/bin`。
///
/// 查找顺序：
/// 1. 环境变量 [`FRAMEWORK_BIN_ENV`]（显式指定，最高优先级）
/// 2. `root/framework/bin`（打包后：exe 旁就是它，一步命中）
/// 3. 从 `root` 向上逐级查找，最多 [`MAX_LOOKUP_LEVELS`] 层
///    （开发时：exe 在 `target/debug/`，而 `framework/` 在仓库根）
///
/// 都没找到时返回 `root/framework/bin`——让后续的「二进制缺失」报错指向
/// 预期位置，而不是 `None` 或空路径。
pub fn framework_bin(root: &Path) -> PathBuf {
    let direct = root.join("framework").join("bin");

    if let Some(dir) = std::env::var_os(FRAMEWORK_BIN_ENV).map(PathBuf::from) {
        if !dir.as_os_str().is_empty() {
            return dir;
        }
    }

    if direct.is_dir() {
        return direct;
    }
    root.ancestors()
        .take(MAX_LOOKUP_LEVELS)
        .map(|a| a.join("framework").join("bin"))
        .find(|c| c.is_dir())
        .unwrap_or(direct)
}

/// `framework/bin` 下工具的二进制路径。原版直接拼 `binPath + cmd`，不带扩展名。
pub fn tool_path(bin_dir: &Path, tool: &str) -> PathBuf {
    bin_dir.join(tool)
}

/// execUtils.js 里门禁读到的 `status` 三个字段 + S18 的深层扫描开关。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GateState {
    /// `status.currentDevice !== null`（原版门禁在此为 null 时无条件拒绝，含 nfc-list）。
    pub has_current_device: bool,
    /// `status.isDeviceConnected`
    pub is_device_connected: bool,
    /// `status.isRunningTask`
    pub is_running_task: bool,
    /// S18：深层扫描（`LIBNFC_INTRUSIVE_SCAN=1`）。对标 MyPN532 的
    /// `scan-device deep`：对响应慢 / 不被常规探测发现的读卡器做侵入式扫描。
    pub deep_scan: bool,
}

impl GateState {
    pub fn from_state(s: &crate::state::AppState) -> Self {
        Self {
            has_current_device: s.current_device.is_some(),
            is_device_connected: s.is_device_connected,
            is_running_task: s.is_running_task,
            deep_scan: s.deep_scan,
        }
    }
}

/// 对标原版 exec 的两个拒绝分支与退出码分流。
#[derive(thiserror::Error, Debug, Clone, PartialEq, Eq)]
pub enum ToolError {
    #[error("device not connected")]
    NotConnected,
    #[error("task already running")]
    Busy,
    #[error("task killed")]
    Killed,
    #[error("tool exited with code {0}")]
    Failed(i32),
    #[error("failed to start tool: {0}")]
    SpawnFailed(String),
    /// 动作已识别但尚未接线（S11 分两步走，未接线的走这条）。
    ///
    /// 存在的意义：让"点了没反应"变成一条**可见**的错误，而不是静默失败。
    #[error("action not wired: {0}")]
    NotWired(&'static str),
}

impl ToolError {
    /// 两个拒绝分支对应原版弹窗提示的 i18n key；退出码类由调用方按 `TaskDone` 处理。
    pub fn i18n_key(&self) -> Option<&'static str> {
        match self {
            ToolError::NotConnected => Some("dialog_msg_not_connected_device"),
            ToolError::Busy => Some("dialog_msg_running_task"),
            ToolError::Killed => Some("event_task_killed"),
            ToolError::Failed(_) | ToolError::SpawnFailed(_) | ToolError::NotWired(_) => None,
        }
    }
}

/// 对标原版 `close(code, signal)`：被信号终止时 `code` 为 `None`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Exit {
    pub code: Option<i32>,
    pub signal: bool,
}

impl Exit {
    /// 便捷构造：正常退出 `Exit::new(Some(0))`；被信号终止 `Exit::new(None)`。
    pub fn new(code: Option<i32>) -> Self {
        Self {
            code,
            signal: code.is_none(),
        }
    }

    /// `code == 0` → Ok；非 0 且被信号终止 → Killed；否则 Failed。
    pub fn result(&self) -> Result<(), ToolError> {
        if self.code == Some(0) {
            Ok(())
        } else if self.signal {
            Err(ToolError::Killed)
        } else {
            Err(ToolError::Failed(self.code.unwrap_or(-1)))
        }
    }
}

/// 一个输出块。`stderr` 为 true 对应原版 `task.stderr.on('data')`。
///
/// 保持块粒度而非行粒度：原版的 `processHandler` 收到的就是 data 块，按行拆会改变行为。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    pub text: String,
    pub stderr: bool,
}

impl Chunk {
    pub fn stdout<S: Into<String>>(text: S) -> Self {
        Self {
            text: text.into(),
            stderr: false,
        }
    }

    pub fn stderr_chunk<S: Into<String>>(text: S) -> Self {
        Self {
            text: text.into(),
            stderr: true,
        }
    }
}

/// 对标 execUtils.js::exec 的 spawn 前置门禁。
///
/// `currentDevice === null` 一律拒绝；未连接时只有 `ALWAYS_ALLOWED` 可放行；忙碌一律拒绝
/// （含 `ALWAYS_ALLOWED`，原版对 `isRunningTask` 的检查不受该豁免影响）。
pub fn gate(tool: &str, st: GateState) -> Result<(), ToolError> {
    if !st.has_current_device || (!st.is_device_connected && tool != ALWAYS_ALLOWED) {
        return Err(ToolError::NotConnected);
    }
    if st.is_running_task {
        return Err(ToolError::Busy);
    }
    Ok(())
}

/// 已知工具名清单。
///
/// 这是**权威清单**：与 `Engine` 实际会 spawn 的名字一一对应，
/// `missing_tools` 按它逐个探测，`package.sh` 也照它挑文件。
///
/// 关于 `mfoc` vs `nfc-mfclassic`：**两个都要在**（S12-2b 更正）。
///
/// 原版用的是两个不同的二进制，参数风格完全不同：
/// - `mfoc` —— 读卡（`command.js:104`、`:159`），开关式 `-O<dump> -f<keys>`
/// - `nfc-mfclassic` —— 写卡 / 格式化（`command.js:119`、`:147`），
///   位置参数 `f|r|R|w|W a|b u|U<uid> <dump> [keys] [f]`
///
/// 此前本重写读卡也用 `nfc-mfclassic`，被它打 usage 拒绝——单测里参数
/// 「与原版一致」故未暴露。详见 docs/02 §6.17。
pub const KNOWN_TOOLS: &[&str] = &[
    "nfc-list",
    "mfoc",
    "nfc-mfclassic",
    "nfc-mfdetect",
    "nfc-mfdict",
    "nfc-mfsetuid",
    "nfc-mflock",
    "libnfc-collect",
    "cropto1_bs",
    // S18：全加密爆破（compile.sh 第 8 段已产出该二进制，此前只缺接线）
    "mfcuk",
];

/// 工具在 `bin_dir` 下是否就位。
///
/// **必须同时试带扩展名的形式**：Windows 上 `CreateProcess` 会自动补
/// `.exe`（所以 `NfcToolRunner::start` 用不带扩展名的路径也能跑），
/// 但 `Path::is_file()` 是纯文件系统检查、**不补扩展名**。
/// 若只查裸名，Windows 上永远不会判为就位，红条会永远显示。
fn tool_present(bin_dir: &Path, tool: &str) -> bool {
    let p = tool_path(bin_dir, tool);
    p.is_file() || p.with_extension("exe").is_file()
}

/// 启动自检：`framework/bin` 下缺哪些工具。
///
/// S12 交付要求「启动自检 framework/bin 缺失引导」。原版没有这一步——
/// `framework/bin` 是 gitignore 的（不进仓库），缺失时 `spawn` 会在用户
/// 点下按钮之后才报错，而用户看到的是一条看不懂的 IO 错误。
/// 此处让 app 在**启动时**就能发现，并列出缺了什么。
pub fn missing_tools(bin_dir: &Path) -> Vec<&'static str> {
    KNOWN_TOOLS
        .iter()
        .copied()
        .filter(|t| !tool_present(bin_dir, t))
        .collect()
}

/// 执行一个 C 工具的抽象。生产用 `NfcToolRunner`，单测与 CI 用 `MockToolRunner`。
pub trait ToolRunner {
    /// 门禁通过后启动工具。二进制缺失等 spawn 失败同样返回 `Err`。
    fn start(&mut self, tool: &str, args: &[String], gs: &GateState) -> Result<(), ToolError>;

    /// 排空已到达的输出块，不阻塞。
    fn pump(&mut self, emit: &mut dyn FnMut(&Chunk));

    /// 进程已结束则返回退出信息。
    fn exit(&mut self) -> Option<Exit>;

    /// 终止进程，对标原版 `task.kill()`。
    fn kill(&mut self);

    /// 是否有任务在跑，对标 `status.isRunningTask`。
    fn is_running(&self) -> bool;

    /// 最近一次 `start` 的内容，供测试断言"跑了什么命令、什么参数"。
    ///
    /// 默认 `None`（生产实现无从得知）；`MockToolRunner` 覆写。
    fn started_last(&self) -> Option<(String, Vec<String>, GateState)> {
        None
    }
}

struct Job {
    child: Arc<Mutex<Option<Child>>>,
    chunks: Receiver<Chunk>,
    ready: Arc<AtomicBool>,
    pending: Arc<AtomicUsize>,
}

/// 真 spawn `framework/bin/<tool>`，注入 `LIBNFC_SYSCONFDIR`。
pub struct NfcToolRunner {
    bin_dir: PathBuf,
    conf_dir: PathBuf,
    job: Option<Job>,
}

impl fmt::Debug for NfcToolRunner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NfcToolRunner")
            .field("bin_dir", &self.bin_dir)
            .field("conf_dir", &self.conf_dir)
            .field("running", &self.job.is_some())
            .finish()
    }
}

impl NfcToolRunner {
    pub fn new(bin_dir: PathBuf, conf_dir: PathBuf) -> Self {
        Self {
            bin_dir,
            conf_dir,
            job: None,
        }
    }

    pub fn bin_dir(&self) -> &Path {
        &self.bin_dir
    }

    pub fn conf_dir(&self) -> &Path {
        &self.conf_dir
    }

    fn spawn(&mut self, tool: &str, args: &[String], deep_scan: bool) -> Result<(), ToolError> {
        let (tx, rx) = channel();
        let ready = Arc::new(AtomicBool::new(false));
        let pending = Arc::new(AtomicUsize::new(0));
        let mut cmd = Command::new(tool_path(&self.bin_dir, tool));
        cmd.current_dir(&self.bin_dir)
            .args(args)
            .env("LIBNFC_SYSCONFDIR", &self.conf_dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // S18 深层扫描：对响应慢 / 不被常规探测发现的读卡器，
        // 让 libnfc 在 nfc_open 时做侵入式设备扫描。
        if deep_scan {
            cmd.env("LIBNFC_INTRUSIVE_SCAN", "1");
        }

        // Windows：release 主程序是 GUI 子系统，自身没有控制台；framework/bin 下的
        // 工具是控制台程序。不指定该标志时，Windows 会为每个子进程新分配一个控制台
        // 窗口，表现为点按钮时闪黑框。stdout/stderr 已重定向到管道，输出不受影响。
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }

        let child = cmd
            .spawn()
            .map_err(|e| ToolError::SpawnFailed(e.to_string()))?;
        let handle = Arc::new(Mutex::new(Some(child)));
        let (mut stdout, mut stderr) = {
            let mut g = handle.lock().unwrap();
            let c = g.as_mut().expect("child just moved in");
            (
                c.stdout.take().expect("stdout piped"),
                c.stderr.take().expect("stderr piped"),
            )
        };

        let tx_err = tx.clone();
        let p_out = pending.clone();
        let t_out = thread::spawn(move || drain(&mut stdout, &tx, &p_out, false));
        let p_err = pending.clone();
        let t_err = thread::spawn(move || drain(&mut stderr, &tx_err, &p_err, true));
        let ready_flag = ready.clone();
        // 协调线程只负责在两个 reader 结束后置位 ready，等价 Node「全部 data 早于 close」。
        // 用 AtomicBool 而非 mpsc<()>：信号需可重复读取。若用一次性 channel，一旦 exit 抢先
        // 消费掉 ready 而 try_wait 尚未 reap 到子进程，下一帧就再也取不到信号，exit 永久
        // 返回 None、is_running_task 永不清零，后续所有工具都会被 gate 判 Busy。
        // 退出状态改由 `exit` 用 `try_wait` 轮询，因此这里绝不在持锁状态下 wait——否则
        // `kill` 会被持锁的 wait 永久挡住，SIGKILL 发不出去，egui 单线程 UI 直接假死。
        thread::spawn(move || {
            drop(t_out.join());
            drop(t_err.join());
            ready_flag.store(true, Ordering::SeqCst);
        });

        self.job = Some(Job {
            child: handle,
            chunks: rx,
            ready,
            pending,
        });
        Ok(())
    }

    fn pump_impl(&mut self, emit: &mut dyn FnMut(&Chunk)) {
        let Some(j) = self.job.as_ref() else {
            return;
        };
        // Receiver 不可 Clone，但 `try_recv(&self)` 只借用，故直接借 job 的引用。
        // 借 `j`（而非 clone 字段）让借用期覆盖整个循环，避免与 `pending` 的读冲突。
        let mut buf = Vec::new();
        while let Ok(c) = j.chunks.try_recv() {
            j.pending.fetch_sub(1, Ordering::SeqCst);
            buf.push(c);
        }
        for c in &buf {
            emit(c);
        }
    }

    fn exit_impl(&mut self) -> Option<Exit> {
        let j = self.job.as_ref()?;
        // 与 MockToolRunner 同一契约：所有已发送的输出必须先被 pump 走、两个 reader 线程
        // 也都结束，才允许取退出码。否则尾部块会随 Receiver 析构一起丢。
        if j.pending.load(Ordering::SeqCst) != 0 || !j.ready.load(Ordering::SeqCst) {
            return None;
        }
        let mut g = match j.child.lock() {
            Ok(g) => g,
            Err(_) => return None,
        };
        let c = g.as_mut()?;
        match c.try_wait().ok().flatten() {
            Some(status) => {
                let code = status.code();
                let signal = code.is_none();
                drop(g);
                self.job = None;
                Some(Exit { code, signal })
            }
            None => None,
        }
    }
}

impl ToolRunner for NfcToolRunner {
    fn start(&mut self, tool: &str, args: &[String], gs: &GateState) -> Result<(), ToolError> {
        gate(tool, *gs)?;
        if self.job.is_some() {
            return Err(ToolError::Busy);
        }
        self.spawn(tool, args, gs.deep_scan)
    }

    fn pump(&mut self, emit: &mut dyn FnMut(&Chunk)) {
        self.pump_impl(emit)
    }

    fn exit(&mut self) -> Option<Exit> {
        self.exit_impl()
    }

    fn kill(&mut self) {
        let Some(job) = self.job.as_mut() else {
            return;
        };
        // 锁只被 `exit` 在微秒级持有，故这里直接 lock 不会长时间等待；用 `if let` 而非
        // `unwrap` 以容忍 poison，绝不让一次加锁失败把 UI 线程拖死。
        if let Ok(mut g) = job.child.lock() {
            if let Some(c) = g.as_mut() {
                let _ = c.kill();
            }
        }
    }

    fn is_running(&self) -> bool {
        self.job.is_some()
    }
}

fn drain<R: Read>(stream: &mut R, tx: &Sender<Chunk>, pending: &AtomicUsize, stderr: bool) {
    let mut buf = [0u8; 4096];
    loop {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                let text = String::from_utf8_lossy(&buf[..n]).into_owned();
                // 先计数再入队：保证「pending 非 0 ⟺ 还有块在路上或已入队」。
                // 反过来会在最后一块被 pump 走后先归零、再被加回 1，exit 可能在该帧
                // 返回 Some，而尾部块已随 Receiver 析构丢失。
                pending.fetch_add(1, Ordering::SeqCst);
                let _ = tx.send(Chunk { text, stderr });
            }
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
}

#[derive(Debug, Default)]
struct MockJob {
    pending: VecDeque<Chunk>,
    drained: bool,
}

/// 脚本化实现：按预设 chunk 序列回放输出，供单测与 CI 使用，零硬件、零二进制依赖。
#[derive(Debug, Default)]
pub struct MockToolRunner {
    script: Vec<Chunk>,
    /// 按次弹出的脚本队列（`with_scripts`）。`None` 表示用尽后回落到 `script`。
    ///
    /// **为什么需要它**：HardNested autoRun 会多轮拉起同一个工具，而
    /// 「每轮输出不同」正是该闭环的语义。只靠 `script` 的话每次 `start`
    /// 都完整回放同一份输出，第 1 轮与第 2 轮无从区分，
    /// 结构上就**写不出**能跑满多轮的测试。
    per_start: VecDeque<Vec<Chunk>>,
    /// 每次 `start` 各自的退出码；`None` 表示用 `exit_code`。
    exit_per_start: VecDeque<Option<i32>>,
    exit_code: Option<i32>,
    killed: bool,
    job: Option<MockJob>,
    /// 已接受的 `start` 调用，供断言门禁与参数是否正确。
    pub started: Vec<(String, Vec<String>, GateState)>,
    /// 本次 `start` 生效的退出码（`with_exits` 可逐次改变）。
    current_exit: Option<i32>,
}

impl MockToolRunner {
    pub fn new(script: Vec<Chunk>) -> Self {
        Self {
            script,
            exit_code: Some(0),
            ..Default::default()
        }
    }

    /// 指定退出码；传 `None` 模拟「被信号终止」。
    pub fn with_exit(mut self, code: Option<i32>) -> Self {
        self.exit_code = code;
        self
    }

    /// 逐次给出不同的脚本：`scripts[0]` 给第 1 次 `start`，`scripts[1]` 给第 2 次……
    ///
    /// 用尽后回落到 [`Self::new`] 给的 `script`（通常是空）。
    pub fn with_scripts(mut self, scripts: Vec<Vec<Chunk>>) -> Self {
        self.per_start = scripts.into();
        self
    }

    /// 逐次给出不同的退出码。
    pub fn with_exits(mut self, codes: Vec<Option<i32>>) -> Self {
        self.exit_per_start = codes.into();
        self
    }
}

impl ToolRunner for MockToolRunner {
    fn start(&mut self, tool: &str, args: &[String], gs: &GateState) -> Result<(), ToolError> {
        gate(tool, *gs)?;
        if self.job.is_some() {
            return Err(ToolError::Busy);
        }
        self.killed = false;
        let script = match self.per_start.pop_front() {
            Some(s) => s,
            None => self.script.clone(),
        };
        self.current_exit = self.exit_per_start.pop_front().unwrap_or(self.exit_code);
        // 与 NfcToolRunner 同一契约：exit 要求先 pump 一次（观察到「不再有输出」）。
        // 脚本为空也不短路——真实现里协调线程要跑完才有 ready，空输出也不例外。
        self.job = Some(MockJob {
            pending: script.into(),
            drained: false,
        });
        self.started.push((tool.to_string(), args.to_vec(), *gs));
        Ok(())
    }

    fn pump(&mut self, emit: &mut dyn FnMut(&Chunk)) {
        let Some(job) = self.job.as_mut() else {
            return;
        };
        while let Some(c) = job.pending.pop_front() {
            emit(&c);
        }
        job.drained = true;
    }

    fn exit(&mut self) -> Option<Exit> {
        if !self.job.as_ref().is_some_and(|j| j.drained) {
            return None;
        }
        self.job = None;
        Some(Exit {
            code: if self.killed { None } else { self.current_exit },
            signal: self.killed,
        })
    }

    fn kill(&mut self) {
        self.killed = true;
        if let Some(j) = self.job.as_mut() {
            j.pending.clear();
            j.drained = true;
        }
    }

    fn is_running(&self) -> bool {
        self.job.is_some()
    }

    fn started_last(&self) -> Option<(String, Vec<String>, GateState)> {
        self.started.last().cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{AppState, Event};

    /// 进程级环境变量的 RAII 守卫：`Drop` 时还原。
    ///
    /// `set_var` 在 Rust 2024 起是 `unsafe`，且环境变量是**进程全局**的——
    /// 测试默认多线程并发，别的测试会在本测试 `set` 与 `drop` 之间读到它，
    /// 导致「本该向上查找的用例」被环境变量劫持（实测踩过）。
    ///
    /// 故凡碰环境变量的测试都必须先 `let _s = env_lock();` 把本文件里
    /// 所有相关用例串起来。
    struct EnvGuard {
        key: &'static str,
        prev: Option<std::ffi::OsString>,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    /// 串行化所有会改环境变量的测试。
    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    impl EnvGuard {
        fn set(key: &'static str, val: &std::path::Path) -> Self {
            let lock = env_lock();
            // SAFETY: 已持锁，本进程内无其他测试同时读写该 key。
            let prev = std::env::var_os(key);
            unsafe { std::env::set_var(key, val) };
            Self {
                key,
                prev,
                _lock: lock,
            }
        }
        fn unset(key: &'static str) -> Self {
            let lock = env_lock();
            let prev = std::env::var_os(key);
            unsafe { std::env::remove_var(key) };
            Self {
                key,
                prev,
                _lock: lock,
            }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            // SAFETY: 同 set（仍持锁，字段声明顺序保证 lock 在 prev 之后 drop）。
            unsafe {
                match self.prev.take() {
                    Some(v) => std::env::set_var(self.key, v),
                    None => std::env::remove_var(self.key),
                }
            }
        }
    }

    const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/");

    fn chunks_from_file(path: &str) -> Vec<Chunk> {
        let raw = std::fs::read_to_string(path).expect("fixture readable");
        raw.lines()
            .map(|l| Chunk::stdout(format!("{l}\n")))
            .collect()
    }

    /// 把 fixture 重新拼成完整文本，用于校验标记是否存在。
    fn fixture_text(path: &str) -> String {
        chunks_from_file(path)
            .iter()
            .map(|c| c.text.clone())
            .collect()
    }

    fn pass_gate() -> GateState {
        GateState {
            has_current_device: true,
            is_device_connected: true,
            is_running_task: false,
        }
    }

    // ---------- 路径 ----------

    #[test]
    fn framework_bin_falls_back_to_root_join_when_absent() {
        let _s = env_lock();
        // 都没找到时必须回落到 root/framework/bin，让报错指向预期位置
        assert_eq!(
            framework_bin(Path::new("/nonexistent-opt/app")).as_path(),
            Path::new("/nonexistent-opt/app/framework/bin")
        );
    }

    #[test]
    fn framework_bin_prefers_nearest_when_several_exist() {
        let _s = env_lock();
        // 上级与同级都有 framework/bin 时必须命中**近**的那个，
        // 否则开发机上会拿错工具目录。
        let root = std::env::temp_dir().join(format!("nfc-fb-pkg-{}", std::process::id()));
        let near = root.join("framework").join("bin");
        let far = root
            .parent()
            .unwrap()
            .join(format!("nfc-fb-far-{}", std::process::id()))
            .join("framework")
            .join("bin");
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(far.parent().unwrap().parent().unwrap());
        std::fs::create_dir_all(&near).unwrap();
        std::fs::create_dir_all(&far).unwrap();
        assert_eq!(framework_bin(&root), near, "应优先命中同级");
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(far.parent().unwrap().parent().unwrap());
    }

    /// 上溯必须有上限：否则可能命中无关目录下的 `framework/bin` 并 spawn
    /// 其中来历不明的同名程序。
    #[test]
    fn framework_bin_lookup_is_bounded() {
        let _s = env_lock();
        let root = std::env::temp_dir().join(format!("nfc-fb-deep-{}", std::process::id()));
        // 从 a 到 g 共 7 层，`deep` = a/b/c/d/e/f/g
        let deep = root.join("a/b/c/d/e/f/g");
        std::fs::create_dir_all(&deep).unwrap();
        // framework/bin 放在 a 下 —— 距 deep 7 层，超过 5 层上限
        let bin = root.join("a").join("framework").join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let got = framework_bin(&deep);
        assert_ne!(got, bin, "上溯超过 {MAX_LOOKUP_LEVELS} 层仍被命中");
        // 找不到时回落到 root 下的预期路径
        assert_eq!(got, deep.join("framework").join("bin"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn framework_bin_walks_up_in_dev_layout() {
        let _s = env_lock();
        // 开发形态：exe 在 target/debug，framework/ 在仓库根
        let repo = std::env::temp_dir().join(format!("nfc-fb-dev-{}", std::process::id()));
        let exe_dir = repo.join("target").join("debug");
        let bin = repo.join("framework").join("bin");
        let _ = std::fs::remove_dir_all(&repo);
        std::fs::create_dir_all(&exe_dir).unwrap();
        std::fs::create_dir_all(&bin).unwrap();
        assert_eq!(
            framework_bin(&exe_dir),
            bin,
            "应向上找到仓库根的 framework/bin"
        );
        let _ = std::fs::remove_dir_all(&repo);
    }

    /// 兄弟目录场景：S12-2 实际踩到的坑。
    ///
    /// 真实布局是 `.../NFC/NFCToolsGUI-Rust` 与 `.../NFCToolsGUI` **平级**，
    /// 工具在后者里。向上查找是「往父目录走」，**永远看不到兄弟目录**，
    /// 于是 app 启动就报缺工具。
    ///
    /// 环境变量是唯一干净的出路——不该去猜「原版项目可能在哪」。
    #[test]
    fn framework_bin_env_rescues_sibling_layout() {
        let root = std::env::temp_dir().join(format!("nfc-fb-sib-{}", std::process::id()));
        let sibling = root.join("NFCToolsGUI").join("framework").join("bin");
        let exe_dir = root
            .join("NFC")
            .join("NFCToolsGUI-Rust")
            .join("target")
            .join("release");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&sibling).unwrap();
        std::fs::create_dir_all(&exe_dir).unwrap();

        // 不设环境变量时：找不到（这正是那个 bug）
        {
            let _guard = EnvGuard::unset(FRAMEWORK_BIN_ENV);
            assert_ne!(
                framework_bin(&exe_dir),
                sibling,
                "向上查找本就不该命中兄弟目录——本测试锁定的是「环境变量能救回来」"
            );
        } // 必须在下一段前释放：锁不可重入，两个 guard 同存即死锁

        // 设了环境变量：命中真实工具目录
        {
            let _g2 = EnvGuard::set(FRAMEWORK_BIN_ENV, &sibling);
            assert_eq!(
                framework_bin(&exe_dir),
                sibling,
                "设了 {FRAMEWORK_BIN_ENV} 就该命中它"
            );
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 环境变量优先级最高：即便 exe 旁边就有，也要用环境变量指的。
    #[test]
    fn framework_bin_env_wins_over_local_dir() {
        let root = std::env::temp_dir().join(format!("nfc-fb-env-{}", std::process::id()));
        let local = root.join("framework").join("bin");
        let override_dir = root.join("elsewhere").join("bin");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&local).unwrap();
        std::fs::create_dir_all(&override_dir).unwrap();
        let _g = EnvGuard::set(FRAMEWORK_BIN_ENV, &override_dir);
        assert_eq!(framework_bin(&root), override_dir, "环境变量应压过本地目录");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 空字符串视为「没设」——否则会去找一个空路径下的工具，静默全缺。
    #[test]
    fn framework_bin_ignores_empty_env() {
        let root = std::env::temp_dir().join(format!("nfc-fb-empty-{}", std::process::id()));
        let bin = root.join("framework").join("bin");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&bin).unwrap();
        let _g = EnvGuard::set(FRAMEWORK_BIN_ENV, std::path::Path::new(""));
        assert_eq!(
            framework_bin(&root),
            bin,
            "空值不该被当成路径，应回落到正常查找"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 集成测试此前各自实现了「先看环境变量」的逻辑，现生产路径同规则。
    /// 这条守卫防止将来有人只改测试不改生产（或反之）导致行为分叉。
    #[test]
    fn integration_tests_reuse_production_lookup() {
        let tests_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
        for f in ["real_tools.rs", "tool_cli.rs"] {
            let src = std::fs::read_to_string(tests_dir.join(f)).unwrap();
            assert!(
                src.contains(FRAMEWORK_BIN_ENV),
                "{f} 应复用生产侧的 {FRAMEWORK_BIN_ENV} 约定，而非各自硬编码"
            );
        }
    }

    #[test]
    fn missing_tools_lists_everything_when_dir_absent() {
        let d = std::env::temp_dir().join(format!("nfc-mt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        // 目录不存在 → 全部缺失
        assert_eq!(missing_tools(&d), KNOWN_TOOLS.to_vec());
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Windows 回归：产物是 `nfc-list.exe`，只查裸名会永远判为缺失，
    /// 导致启动红条永远显示。
    #[test]
    fn missing_tools_accepts_exe_suffix() {
        let d = std::env::temp_dir().join(format!("nfc-mt-exe-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        for t in KNOWN_TOOLS {
            std::fs::write(d.join(t).with_extension("exe"), b"").unwrap();
        }
        assert!(
            missing_tools(&d).is_empty(),
            "全是 .exe 时不该报缺失: {:?}",
            missing_tools(&d)
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn missing_tools_reports_only_what_is_gone() {
        let d = std::env::temp_dir().join(format!("nfc-mt-part-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        // 放 3 个，其余缺
        for t in &KNOWN_TOOLS[..3] {
            std::fs::write(d.join(t), b"").unwrap();
        }
        assert_eq!(missing_tools(&d), KNOWN_TOOLS[3..].to_vec());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn known_tools_are_plain_names_and_cover_spawn_set() {
        // 原版直接 `binPath + cmd` 拼路径，不带扩展名（Windows 靠 PATHEXT）
        for t in KNOWN_TOOLS {
            assert!(!t.contains('.'), "工具名不该带扩展名: {t}");
        }
        assert!(KNOWN_TOOLS.contains(&ALWAYS_ALLOWED));
        // 清单不能有幽灵项：每个都必须是 engine 真会 spawn 的工具名之一
        for spawned in [
            "nfc-list",
            "nfc-mfclassic",
            "nfc-mfdetect",
            "nfc-mfdict",
            "nfc-mfsetuid",
            "nfc-mflock",
            "libnfc-collect",
            "cropto1_bs",
            "mfcuk",
        ] {
            assert!(KNOWN_TOOLS.contains(&spawned), "清单漏了 {spawned}");
        }
        // S12-2b：读卡用 `mfoc`、写卡用 `nfc-mfclassic`，两个都得在。
        assert!(KNOWN_TOOLS.contains(&"mfoc"), "清单漏了读卡用的 mfoc");
        assert_eq!(
            KNOWN_TOOLS.len(),
            10,
            "清单长度变了，同步更新 package.sh 与文档"
        );
    }

    #[test]
    fn tool_path_keeps_no_extension_like_original() {
        assert_eq!(
            tool_path(Path::new("/a/b/framework/bin"), "nfc-list").as_path(),
            Path::new("/a/b/framework/bin/nfc-list")
        );
    }

    // ---------- 门禁矩阵 ----------

    #[test]
    fn gate_requires_current_device_unconditionally() {
        let st = GateState {
            has_current_device: false,
            is_device_connected: true,
            is_running_task: false,
        };
        // 连 nfc-list 也不放行：原版 currentDevice === null 时先判定
        assert_eq!(gate("nfc-list", st), Err(ToolError::NotConnected));
        assert_eq!(gate("nfc-mfoc", st), Err(ToolError::NotConnected));
    }

    #[test]
    fn gate_disconnected_blocks_everything_but_always_allowed() {
        let st = GateState {
            has_current_device: true,
            is_device_connected: false,
            is_running_task: false,
        };
        assert_eq!(gate("nfc-list", st), Ok(()));
        assert_eq!(gate("nfc-mfoc", st), Err(ToolError::NotConnected));
        assert_eq!(gate("nfc-mfsetuid", st), Err(ToolError::NotConnected));
    }

    #[test]
    fn gate_busy_blocks_even_always_allowed() {
        let st = GateState {
            has_current_device: true,
            is_device_connected: true,
            is_running_task: true,
        };
        assert_eq!(gate("nfc-list", st), Err(ToolError::Busy));
        assert_eq!(gate("nfc-mfoc", st), Err(ToolError::Busy));
    }

    #[test]
    fn gate_passes_when_connected_and_idle() {
        assert_eq!(gate("nfc-mfoc", pass_gate()), Ok(()));
    }

    #[test]
    fn gate_state_is_copyable_and_default_is_rejected() {
        let st = GateState::default();
        assert_eq!(gate("nfc-list", st), Err(ToolError::NotConnected));
        let copy = st;
        assert_eq!(st, copy);
    }

    #[test]
    fn gate_state_from_app_state() {
        let mut s = AppState::new();
        assert!(matches!(
            gate("nfc-list", GateState::from_state(&s)),
            Err(ToolError::NotConnected)
        ));
        s.current_device = Some("/dev/ttyACM0".to_string());
        s.is_device_connected = true;
        assert_eq!(gate("nfc-mfoc", GateState::from_state(&s)), Ok(()));
    }

    // ---------- 退出码分流 ----------

    #[test]
    fn exit_result_dispatch_matches_original() {
        assert_eq!(
            Exit {
                code: Some(0),
                signal: false
            }
            .result(),
            Ok(())
        );
        assert_eq!(
            Exit {
                code: None,
                signal: true
            }
            .result(),
            Err(ToolError::Killed)
        );
        assert_eq!(
            Exit {
                code: Some(3),
                signal: false
            }
            .result(),
            Err(ToolError::Failed(3))
        );
        // code 缺失且非信号（wait 失败）→ 兜底 -1
        assert_eq!(
            Exit {
                code: None,
                signal: false
            }
            .result(),
            Err(ToolError::Failed(-1))
        );
    }

    #[test]
    fn tool_error_i18n_keys_resolve() {
        assert_eq!(
            crate::i18n::t("dialog_msg_not_connected_device"),
            "请先连接设备"
        );
        assert_eq!(
            crate::i18n::t("dialog_msg_running_task"),
            "有任务进行中，不可执行"
        );
        assert_eq!(crate::i18n::t("event_task_killed"), "任务已停止");
        assert_eq!(
            ToolError::NotConnected.i18n_key(),
            Some("dialog_msg_not_connected_device")
        );
        assert_eq!(ToolError::Busy.i18n_key(), Some("dialog_msg_running_task"));
        assert_eq!(ToolError::Killed.i18n_key(), Some("event_task_killed"));
        assert_eq!(ToolError::Failed(1).i18n_key(), None);
        assert!(ToolError::SpawnFailed("x".into()).i18n_key().is_none());
    }

    // ---------- MockToolRunner ----------

    #[test]
    fn mock_rejects_gated_tools_before_recording() {
        let mut r = MockToolRunner::new(vec![]);
        let st = GateState {
            has_current_device: true,
            is_device_connected: false,
            is_running_task: false,
        };
        assert_eq!(r.start("nfc-mfoc", &[], &st), Err(ToolError::NotConnected));
        assert!(r.started.is_empty());
        assert!(!r.is_running());
    }

    #[test]
    fn mock_pump_delivers_script_in_order() {
        let mut r = MockToolRunner::new(vec![
            Chunk::stdout("head\n"),
            Chunk::stderr_chunk("boom\n"),
            Chunk::stdout("tail\n"),
        ]);
        r.start("nfc-list", &[], &pass_gate()).unwrap();
        assert!(r.is_running());
        let mut got = Vec::new();
        r.pump(&mut |c| got.push(c.clone()));
        assert_eq!(
            got,
            vec![
                Chunk::stdout("head\n"),
                Chunk::stderr_chunk("boom\n"),
                Chunk::stdout("tail\n"),
            ]
        );
        // 第二次 pump 已无输出
        let mut again = Vec::new();
        r.pump(&mut |c| again.push(c.clone()));
        assert!(again.is_empty());
    }

    #[test]
    fn mock_exit_requires_pump_first() {
        let mut r = MockToolRunner::new(vec![Chunk::stdout("a")]);
        r.start("nfc-list", &[], &pass_gate()).unwrap();
        assert_eq!(r.exit(), None);
        r.pump(&mut |_| {});
        assert_eq!(
            r.exit(),
            Some(Exit {
                code: Some(0),
                signal: false
            })
        );
        assert!(!r.is_running());
    }

    #[test]
    fn mock_exit_reports_custom_code() {
        let mut r = MockToolRunner::new(vec![]).with_exit(Some(5));
        r.start("nfc-mfoc", &["-O".into()], &pass_gate()).unwrap();
        r.pump(&mut |_| {});
        assert_eq!(r.exit().unwrap().result(), Err(ToolError::Failed(5)));
    }

    #[test]
    fn mock_kill_truncates_script_and_reports_signal() {
        let mut r = MockToolRunner::new(vec![Chunk::stdout("a\n"), Chunk::stdout("b\n")]);
        r.start("nfc-mfoc", &[], &pass_gate()).unwrap();
        let mut got = Vec::new();
        r.pump(&mut |c| got.push(c.clone()));
        r.kill();
        let mut rest = Vec::new();
        r.pump(&mut |c| rest.push(c.clone()));
        assert!(rest.is_empty());
        let e = r.exit().unwrap();
        assert_eq!(
            e,
            Exit {
                code: None,
                signal: true
            }
        );
        assert_eq!(e.result(), Err(ToolError::Killed));
    }

    #[test]
    fn mock_busy_rejects_second_start_and_records_args() {
        let mut r = MockToolRunner::new(vec![]);
        let args = vec!["-O".to_string(), "/tmp/x".to_string()];
        r.start("nfc-mfoc", &args, &pass_gate()).unwrap();
        assert_eq!(
            r.start("nfc-mflock", &[], &pass_gate()),
            Err(ToolError::Busy)
        );
        assert_eq!(r.started.len(), 1);
        assert_eq!(r.started[0].0, "nfc-mfoc");
        assert_eq!(r.started[0].1, args);
        assert_eq!(r.started[0].2, pass_gate());
    }

    #[test]
    fn mock_allows_one_task_at_a_time_after_finish() {
        let mut r = MockToolRunner::new(vec![]);
        r.start("nfc-list", &[], &pass_gate()).unwrap();
        r.pump(&mut |_| {});
        r.exit();
        r.start("nfc-mfoc", &[], &pass_gate()).unwrap();
        assert_eq!(r.started.len(), 2);
    }

    // ---------- fixture：nfc-list 成功 / 失败 ----------
    // S2 尚无输出解析器，这里只校验 fixture 本身满足判定所需的性质：
    // 含对应标记、且不含另一侧标记（互斥）。解析器落地后由 S3 的单测接管。

    #[test]
    fn fixture_nfc_list_opened_contains_marker() {
        let text = fixture_text(&format!("{FIXTURES}nfc-list-opened.txt"));
        assert!(text.contains("NFC device: NFC_Device opened"));
        // 真实 nfc-list 还会打各协议的 passive target 计数（nfc-list.c:172-186）
        assert!(text.contains("passive target(s) found"));
        assert!(!text.contains("Unable to open NFC device"));
    }

    #[test]
    fn fixture_nfc_list_unable_contains_marker() {
        let text = fixture_text(&format!("{FIXTURES}nfc-list-unable.txt"));
        assert!(text.contains("Unable to open NFC device"));
        assert!(!text.contains("NFC device: NFC_Device opened"));
    }

    // ---------- 与 state.rs 的联动 ----------

    #[test]
    fn mock_output_feeds_log_buffer_and_honours_clear_sequence() {
        let mut r = MockToolRunner::new(vec![
            Chunk::stdout("line1\n"),
            Chunk::stdout("partial"),
            Chunk::stdout("\u{1b}[2Kclean"),
        ]);
        let mut s = AppState::new();
        r.start("nfc-list", &[], &pass_gate()).unwrap();
        r.pump(&mut |c| s.apply(Event::Log(c.text.clone())));
        // partial 被截断、\33[2K 被剥离 —— 逻辑在 state.rs::append_log
        assert_eq!(s.log_buffer, "line1\nclean");
    }

    #[test]
    fn mock_exit_maps_to_task_done_event() {
        let mut r = MockToolRunner::new(vec![]).with_exit(Some(7));
        let mut s = AppState::new();
        s.is_running_task = true;
        r.start("nfc-mfoc", &[], &pass_gate()).unwrap();
        r.pump(&mut |c| s.apply(Event::Log(c.text.clone())));
        let e = r.exit().unwrap();
        s.apply(Event::TaskDone {
            code: e.code,
            killed: e.signal,
        });
        assert!(!s.is_running_task);
        assert!(s.log_buffer.contains("exit code: 7"));
    }
}
