//! 动作编排：把「用户想做什么」翻译成「跑哪个 C 工具、传什么参数」，
//! 并在工具输出回流时驱动状态机。
//!
//! 这是 S11 的核心。S0–S10 里 `AppState` 的每个字段都有归属，但**没有任何一处
//! 真正 spawn 过进程**——UI 产出的 `Action` 进了队列就被 `clear()` 丢弃。
//! 本模块补上这一层。
//!
//! 分层：`Engine` 只依赖 `ToolRunner` trait（生产 `NfcToolRunner` / 测试
//! `MockToolRunner`），不碰 egui，故可被 `--selftest` 用 mock 完整驱动。
//!
//! **当前覆盖范围**（S17 后全链路已接线）：读卡（`nfc-mfdetect` / `mfoc` /
//! `nfc-mfdict`）、写卡（`nfc-mfclassic` / `nfc-mfsetuid` / `nfc-mflock`）、
//! HardNested 多轮状态机、手机/手环操作区、S18 全加密爆破（`mfcuk`）与
//! CUID 空卡写入。未接线的动作会显式报 `ToolError::NotWired` 并记一条日志，
//! 不会静默无反应。

use std::path::PathBuf;
use std::time::Instant;

use crate::dialogs::DialogProvider;
use crate::model::keys::DEFAULT_KEYS;
use crate::model::parse::{BlockStatsParser, KeyInfoParser};
use crate::paths::Paths;
use crate::runner::{self, Chunk, GateState, ToolError, ToolRunner};
use crate::serial::{self, ConnectFlow, PortScanner, SerialTrait};
use crate::state::{AppState, DictTestConfig, Event, EventKind, HardNestedConfig, Indicator};
use crate::tasks;

/// 用户发起的一个动作。UI 产出，Engine 消费。
///
/// 刻意放在 core 而非 app：本枚举不含任何 GUI 类型，且它是
/// `command.rs::ACTION_SPECS` 里那 30 个 action 名的运行时载体，
/// 放 core 才能让 `Engine` 直接消费而不必反向依赖 app。
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// 扫描 USB 设备（对应原版 `nfc-list`，`ALWAYS_ALLOWED`）。
    ScanUsb,
    /// 连接指定设备。
    ConnUsb(String),
    /// 无参动作。
    Plain(&'static str),
    /// 带一个字符串参数的动作。载荷语义随 action 名，见 `dispatch`。
    WithArg(&'static str, String),
    /// 硬解码配置（`hard-nested-config-done`）。
    HardNestedConfig(Box<HardNestedConfig>),
    /// 字典测试配置（`dict-test-config-done`）。
    DictTestConfig(Box<DictTestConfig>),
    /// 保存 dump（`dump-editor-save`）。
    SaveDump {
        url: Option<String>,
        hex: String,
        save_as: bool,
    },
    /// 删除若干 dump（`delete-dump`）。确认框由 app 层负责。
    DeleteDumps(Vec<String>),
}

/// `readICThenExec` 的第二段：备份成功后接着要跑的命令。
///
/// 对标 `command.js:608-613`：
/// ```text
/// exec(msg, cmd, args, processHandler, (code, signal) => {
///     if (finishHandler) finishHandler(code, signal)
///     unlink(tempMFDFilePath)
/// }).then(() => printExitLog(0))
/// ```
#[derive(Debug, Clone, PartialEq)]
struct NextStage {
    tool: &'static str,
    args: Vec<String>,
    /// 收尾时删掉 `temp.mfd`（原版无条件 `unlink`）。
    remove_temp: bool,
    /// 退出后落成败文案（`command.js:120-122` 的 `code === 0` 分支）。
    report: bool,
    /// 状态指示文案（`command.js:118` 的 `statusMsg`）。
    status_key: &'static str,
    /// S18 CUID：第二段在备份成功后，用备份 block0 的 UID 生成 CUID 空卡密钥
    /// 文件，并改用大写 `W`（解锁写）+ 该密钥文件。`args[3]` 仍是源 dump。
    cuid: bool,
}

/// HardNested 闭环里当前跑的是哪一步。
///
/// 属于该闭环的工具退出后，Engine 靠它决定「接下一步还是收工」。
/// `None` = 该工具不参与 HardNested（读写卡、字典测试等）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HnStep {
    /// autoRun 的前置 `nfc-mfdetect`（`command.js:251-253`）
    Detect,
    /// `libnfc-collect` 采集 nonce
    Collect,
    /// `cropto1_bs` 破解
    Crack,
}

/// HardNested 每步结束后要用的输出产物（已从 `Running` 里取出）。
///
/// 单独一个类型而不是直接传 `Running`：`finish` 里 `run.parser` 分支会把
/// `run` 部分移动掉，HardNested 的字段得先取出来。
struct HnArtifacts {
    collect: Option<crate::model::parse::CollectParser>,
    crypto: Option<crate::model::parse::SingleKeyParser>,
    unknown_before: usize,
}

/// 一个正在跑的任务的上下文。`pump` 靠它决定输出该怎么处理。
#[derive(Debug, Default)]
struct Running {
    /// 产出该任务的 action 名，退出后据此选收尾逻辑。
    name: &'static str,
    /// mfoc / nfc-mfdict 的 ETA 进度。
    eta: Option<tasks::mfoc::EtaTracker>,
    /// `nfc-mfdetect` 的密钥解析器。
    parser: Option<KeyInfoParser>,
    /// `nfc-mfdetect` 是否带 `-O`（即是否要落 dump）。
    detect_saves_dump: bool,
    /// mfdetect 跑完后接着跑什么（`readICThenExec` 的第二段）。
    next: Option<NextStage>,
    /// 本工具属于 HardNested 闭环的哪一步。
    hn: Option<HnStep>,
    /// `libnfc-collect` 输出解析（uid / sector / key_type）。
    collect: Option<crate::model::parse::CollectParser>,
    /// `cropto1_bs` 输出解析（`Key found:`）。
    crypto: Option<crate::model::parse::SingleKeyParser>,
    /// 本轮开始时的 unknown 条数，供无进展兜底比较。
    unknown_before: usize,
    /// 第二段收尾时删掉 `temp.mfd`（`command.js:610-613`）。
    remove_temp: bool,
    /// 第二段收尾时是否落成败文案（`command.js:120-122`）。
    report: bool,
    /// 写卡/格式化第二段（`nfc-mfclassic`）的完成行解析器，用于判定
    /// 是否**全部**块写入成功（`Done, X of Y blocks written.`）。
    block_stats: Option<BlockStatsParser>,
    /// 是否从输出里抓 `UID (NFCID1):`（`nfc-mfclassic` 需要，用于 dump 文件名）。
    scan_mfoc_uid: bool,
    /// 抓到的 UID，最终传给 `mfoc::dump_save` 当默认文件名前缀。
    uid: Option<String>,
    /// 本工具只是**链路的一环**，退出后不该置空闲。
    ///
    /// `hard-nested-config-done` 的 autoRun 分支走 `readICThenExec` 的
    /// 4 参形式，`isCmdFunc = false`，其 `.then()` 只做
    /// `printStatus(statusMsg); cmd(); return;`（`command.js:601-602`），
    /// **不调 `printExitLog(0)`**——状态要留着给紧接着的 `run-hard-nested`。
    /// 故此类工具成功退出时既不置空闲也不记「运行完毕」。
    chain_continues: bool,
    /// `phw-read-uid`：mfdetect 读回成功后取 block0 缓存（`commands.rs:1114`）。
    phw_read_uid: bool,
    /// `phw-write-data`：mfdetect 读回后、写回前，把 block0 写进目标块。
    phw_patch: Option<tasks::phw::SectorBlock>,
    /// S18 CUID：本段是 CUID 写回的第二段，收尾时需删 CUID 密钥文件。
    cuid: bool,
    /// S18 全加密爆破：`run-mfcuk` 的密钥解析器（一轮可能出多把）。
    mfcuk: Option<crate::model::parse::MfcukKeyParser>,
    /// `dict-test-config-done` 的目标扇区与类型：命中密钥后据此从
    /// `unknown_key_info` 移除对应项（`command.js:396-399`）。
    dict_target: Option<(u8, crate::model::keys::KeyType)>,
}

/// 动作编排器。
///
/// 生命周期与 eframe 的一帧对齐：
/// 1. `dispatch` 若干 `Action`（可能启动工具）
/// 2. 每帧 `pump` 一次，把已到达的输出推进状态机
/// 3. 工具退出时 `pump` 内部做收尾（存密钥 / 出引导 / 报错）
pub struct Engine {
    runner: Box<dyn ToolRunner>,
    running: Option<Running>,
    /// 串口枚举器（对标原版 `SerialPort.list()`）。`PortScanner` 自带变化检测，
    /// 只有列表真的变了才推 `Event::DeviceList`。
    scanner: PortScanner,
    /// 连接握手进行中（`conn-usb-devices`）。由 `pump` 每帧驱动。
    conn: Option<ConnectFlow>,
    /// 上一次已上报的串口枚举错误。轮询每 500ms 一次，同一错误只报一次，
    /// 否则环境异常（如 udev 不可用）时日志会被同一条错误刷屏。
    scan_err: Option<String>,
    /// HardNested 闭环的状态机（`tasks::hard_nested::HardNestedRunner`）。
    ///
    /// 状态机本身在 S7 就已实现并测完（含无进展兜底），只是没人驱动它。
    hn: tasks::hard_nested::HardNestedRunner,
    /// 本轮的 HardNested 配置，`Begin` 时写入。
    hn_cfg: Option<HardNestedConfig>,
    /// 本轮采集到的 uid / sector / key_type，collectOnly 时用于默认文件名。
    hn_round: tasks::hard_nested::RoundContext,
    /// 挂起中的「保存到...」请求（S11-7）。
    ///
    /// 流程中途的保存框（`read-IC` 收尾、HardNested `collectOnly`）没法预选
    /// 路径——默认文件名含刚读出的 UID / 本轮 sector。故 engine 记下请求、
    /// 立刻返回控制权，由 app 弹完 rfd 再回调 [`Self::resolve_save`]。
    /// 非 `None` 时本轮闭环算「还在忙」，门禁据此挡住并发动作。
    pending_save: Option<crate::dialogs::PendingSave>,
    /// 挂起的保存框是不是 HardNested 的 nonce（决定续接走哪条路径）。
    hn_saving: bool,
    /// 当前任务的启动时刻（单调时钟），ETA 进度据此算已用时长 / 节流。
    ///
    /// `EtaTracker` 的 `start_ms`/`last_update_ms` 都以它为零点；不用墙钟——
    /// 系统时间前跳会算出负的剩余时间。未启动任务时为 `None`。
    eta_epoch: Option<Instant>,
}

impl Engine {
    /// 包装一个 `ToolRunner`。`MockToolRunner` 用于测试与 `--selftest`。
    ///
    /// 串口枚举默认给空的 `MockSerial`（枚举不出任何端口）——测试要验证
    /// 设备列表时用 [`Self::with_serial`] 注入 `MockSerial` / `MutableSerial`。
    pub fn new(runner: Box<dyn ToolRunner>) -> Self {
        Self {
            runner,
            running: None,
            scanner: PortScanner::new(Box::new(serial::MockSerial::default())),
            conn: None,
            scan_err: None,
            hn: tasks::hard_nested::HardNestedRunner::new(),
            hn_cfg: None,
            hn_round: tasks::hard_nested::RoundContext::default(),
            pending_save: None,
            hn_saving: false,
            eta_epoch: None,
        }
    }

    /// 换成另一个串口枚举实现（热插拔场景需要列表能在运行中变化）。
    pub fn with_serial(mut self, serial: Box<dyn SerialTrait>) -> Self {
        self.scanner = PortScanner::new(serial);
        self
    }

    /// 生产用：`framework/bin` 下的真工具 + 真串口枚举。
    pub fn with_real_tools(bin_dir: PathBuf, conf_dir: PathBuf) -> Self {
        Self::new(Box::new(runner::NfcToolRunner::new(bin_dir, conf_dir)))
            .with_serial(Self::production_serial())
    }

    /// 生产用的串口实现：`real-serial` 特性打开时是真枚举，否则退回空列表。
    fn production_serial() -> Box<dyn SerialTrait> {
        #[cfg(feature = "real-serial")]
        {
            Box::new(serial::RealSerial)
        }
        #[cfg(not(feature = "real-serial"))]
        {
            Box::new(serial::MockSerial::default())
        }
    }

    /// 是否有**任务**在跑，即 `status.isRunningTask`。
    ///
    /// 只看 `self.running`，**不**并上 `runner.is_running()`：连接握手
    /// （`nfc-list`）的进程也在同一个 runner 里，但它刻意不占任务位
    /// （见 [`Self::connect`]），否则紧随其后的其它任务会被自己的 Busy 拒掉。
    /// `cancel-task` 后底层进程要到下一次 `pump` 才真正回收，但任务位已释放，
    /// 门禁据此放行下一个任务——这与原版 `killProcess()` 后立刻可再发起一致。
    pub fn is_running(&self) -> bool {
        // 挂起的保存框也算「在忙」：此时若放行第二个任务，read-IC 的 temp.mfd
        // 会被下一次读卡覆盖，用户在上一个保存框里选好的路径就指到别人的数据上。
        // 连接握手同理：此时发起读卡会用还没握完手的端口，读出的结果不可信。
        self.running.is_some() || self.pending_save.is_some() || self.conn.is_some()
    }

    /// 挂起中的保存请求，供 app 每帧取走去弹框。
    pub fn pending_save(&self) -> Option<&crate::dialogs::PendingSave> {
        self.pending_save.as_ref()
    }

    /// 挂起的保存框是不是 HardNested 的 nonce（决定用哪个 `FilePurpose`）。
    pub fn hn_saving(&self) -> bool {
        self.hn_saving
    }

    /// 用户选完路径后回来续接收尾。
    ///
    /// `url` 为 `None` 表示取消（rfd 的 `Option` 不区分取消与后端不可用）。
    /// 取消时仍用默认名保存——对齐 `command.js:667` 的 `url || defaultName`。
    /// 返回 `false` 表示没有挂起的请求（重复回调 / 顺序错了）。
    pub fn resolve_save(
        &mut self,
        url: Option<std::path::PathBuf>,
        state: &mut AppState,
        paths: &Paths,
    ) -> bool {
        let Some(pending) = self.pending_save.take() else {
            return false;
        };
        // 释放任务位：`finish` 为此把它重新置真过。
        state.is_running_task = false;
        if self.hn_saving {
            // HardNested collectOnly 的 nonce：不是 dump，不走 dump_commit。
            self.hn_saving = false;
            self.resolve_nonces(state, &pending, url);
            return true;
        }
        let refresh_history = pending.refresh_history;
        tasks::mfoc::dump_commit(state, &pending, url);
        if refresh_history {
            // 存盘后立刻能在历史面板看到
            state.apply(Event::DictHistory(crate::dumpfiles::list(
                &paths.dumpfiles_dir(),
            )));
        }
        true
    }

    /// HardNested `collectOnly` 的续接：把 `nonces.bin` 搬到用户选的位置。
    ///
    /// 取消时原版记「未保存」并**保留** `nonces.bin`（`command.js:310-320` 的
    /// `if (!url) { printLog(not_saved); return }`）——与 `read-IC` 收尾的
    /// 「取消也用默认名存」不同，两条路径别混。
    fn resolve_nonces(
        &mut self,
        state: &mut AppState,
        pending: &crate::dialogs::PendingSave,
        url: Option<std::path::PathBuf>,
    ) {
        let Some(target) = url else {
            state.apply(Event::Log(format!(
                "\n{}\n",
                crate::i18n::t("log_msg_not_saved")
            )));
            self.hn_idle(state);
            return;
        };
        match std::fs::rename(&pending.source, &target) {
            Ok(()) => state.apply(Event::Log(format!(
                "\n\n{} {}\n",
                crate::i18n::t("log_msg_file_already_saved_to"),
                target.display()
            ))),
            Err(e) => {
                state.apply(Event::Log(format!(
                    "\n{}: {e}\n",
                    crate::i18n::t("log_msh_save_failed")
                )));
            }
        }
        self.hn_idle(state);
    }

    /// 只读访问底层 runner（自测与诊断用）。
    pub fn runner(&self) -> &dyn ToolRunner {
        self.runner.as_ref()
    }

    /// 执行一个动作。
    ///
    /// 返回 `Err` 表示**没能启动**（门禁拒绝 / 二进制缺失 / spawn 失败 /
    /// 动作未接线）；已启动的任务其退出码由 `pump` 处理，不在这里返回。
    pub fn dispatch(
        &mut self,
        action: Action,
        state: &mut AppState,
        paths: &Paths,
        dialog: &mut dyn DialogProvider,
    ) -> Result<(), ToolError> {
        match action {
            Action::ScanUsb => self.scan(state),
            Action::ConnUsb(dev) => self.connect(state, paths, &dev),
            Action::Plain(n) => self.plain(n, None, state, paths, dialog),
            Action::WithArg(n, arg) => self.plain(n, Some(arg), state, paths, dialog),
            Action::HardNestedConfig(cfg) => self.hard_nested(state, paths, &cfg),
            Action::DictTestConfig(cfg) => self.dict_test(state, &cfg),
            Action::SaveDump { .. } => {
                // 纯文件，由 app 层处理（rfd）
                Ok(())
            }
            Action::DeleteDumps(names) => {
                // 安全闸：删除不可逆，必须确认过。
                // app 层的应用内模态窗负责收集用户意愿并把结果预置进
                // `dialog`；未预置时 `confirm_danger` 返回安全默认 `false`，
                // 于是任何绕过 UI 的直接派发都删不掉东西。
                if !dialog.confirm_danger(&crate::dumpfiles::delete_confirm(&names)) {
                    return Ok(());
                }
                let dir = paths.dumpfiles_dir();
                let gone = crate::dumpfiles::delete(&dir, &names);
                state.apply(Event::DictHistory(crate::dumpfiles::list(&dir)));
                for n in gone {
                    state.apply(Event::EventItem {
                        kind: EventKind::Success,
                        text: format!("{}: {n}", crate::i18n::t("html_delete_checked")),
                    });
                }
                Ok(())
            }
        }
    }

    /// 启动前的统一入口：过门禁 + 记命令行 + 发 running 状态 + spawn。
    fn launch(
        &mut self,
        tool: &str,
        args: &[String],
        state: &mut AppState,
        status_key: &str,
        run: Running,
    ) -> Result<(), ToolError> {
        let gs = GateState::from_state(state);
        runner::gate(tool, gs)?;
        // 命令行进日志，对标原版 printLog(`${cmd} ${args}`)
        state.apply(Event::Log(format!("\n{tool} {}\n", args.join(" "))));
        self.runner.start(tool, args, &gs)?;
        state.is_running_task = true;
        state.apply(Event::Status {
            text: crate::i18n::t(status_key),
            indicator: Indicator::Running,
        });
        self.eta_epoch = Some(Instant::now());
        self.running = Some(run);
        Ok(())
    }

    /// 同 [`Self::launch`]，但状态文案由调用方给定而非 i18n key。
    ///
    /// HardNested 需要状态条带 `已解/总数` 进度，而 `launch` 内部会用
    /// `t(status_key)` 覆盖掉调用方先前 apply 的文案。拆成两个方法而不是
    /// 给 `launch` 加参数，是为了让其余十来个调用点保持原样。
    #[allow(clippy::too_many_arguments)]
    fn launch_with_status(
        &mut self,
        tool: &str,
        args: &[String],
        state: &mut AppState,
        status_text: String,
        run: Running,
    ) -> Result<(), ToolError> {
        let gs = GateState::from_state(state);
        runner::gate(tool, gs)?;
        state.apply(Event::Log(format!("\n{tool} {}\n", args.join(" "))));
        self.runner.start(tool, args, &gs)?;
        state.is_running_task = true;
        state.apply(Event::Status {
            text: status_text,
            indicator: Indicator::Running,
        });
        self.eta_epoch = Some(Instant::now());
        self.running = Some(run);
        Ok(())
    }

    /// `scan-usb-devices` → 枚举串口（对标原版 `SerialPort.list()`）。
    ///
    /// **不走 `ToolRunner`、也不过门禁**：原版的这条 action 直接调
    /// `SerialPort.list()`，跟 `exec` 无关——故它不受 `currentDevice === null`
    /// 拒绝，任何时刻都能刷新下拉列表（这正是「未连接也能扫」的语义来源）。
    /// 结果经 `Event::DeviceList` 推送；列表没变就不推，避免每 500ms 刷一次。
    fn scan(&mut self, state: &mut AppState) -> Result<(), ToolError> {
        match self.scanner.scan_if_changed() {
            Ok(Some(ports)) => {
                state.apply(Event::DeviceList(ports));
                Ok(())
            }
            Ok(None) => Ok(()),
            Err(e) => {
                let msg = format!("{}：{e}", crate::i18n::t("log_msg_serial_enum_failed"));
                if self.scan_err.as_deref() != Some(msg.as_str()) {
                    state.apply(Event::Log(format!("\n{msg}\n")));
                    self.scan_err = Some(msg);
                }
                // 枚举失败不是任务失败：列表只是没更新，别让它冒成错误事件。
                Ok(())
            }
        }
    }

    /// `conn-usb-devices`：写 `libnfc.conf` 并启动 `nfc-list` 做连接握手。
    ///
    /// 握手是异步的，后续帧由 `pump` 驱动 [`ConnectFlow`]，直到进程退出才
    /// 落 `DeviceState::Connected` / `Failed`（对标原版 `setNFCConfig` 的
    /// close 回调）。这里只做同步可完成的部分。
    ///
    /// `is_running_task` 要置真（对标原版 `exec` 的 `status.isRunningTask = true`），
    /// 否则握手期间发起读卡会用还没握完手的端口。握手进程退出时由
    /// `ConnectFlow::finish` 发出的 `TaskDone` 复位（`AppState::apply` 里
    /// `is_running_task = false`），失败落 `Failed` 也会复位。
    fn connect(&mut self, state: &mut AppState, paths: &Paths, dev: &str) -> Result<(), ToolError> {
        // R5：连接中禁下拉，故这里也拒（原版靠按钮 disabled，此处是第二道）
        if state.device_state == crate::state::DeviceState::Connecting || self.conn.is_some() {
            return Err(ToolError::Busy);
        }
        if self.running.is_some() || self.pending_save.is_some() {
            return Err(ToolError::Busy);
        }
        // 必须先记下设备再算 GateState：`gate` 对 nfc-list 仍要求
        // `has_current_device`（原版门禁在 `currentDevice === null` 时无条件拒绝）。
        state.current_device = Some(dev.to_owned());
        state.apply(Event::DeviceState(crate::state::DeviceState::Connecting));
        let gs = GateState::from_state(state);
        // S18：波特率由 `state.current_speed` 提供（设置面板可调，默认 115200）。
        let mut flow = ConnectFlow::new(dev).with_speed(state.current_speed);
        let mut emit = |e: Event| state.apply(e);
        if let Err(e) = flow.begin(paths, self.runner.as_mut(), &gs, &mut emit) {
            return Err(match e {
                serial::DeviceError::Tool(t) => t,
                other => ToolError::SpawnFailed(other.to_string()),
            });
        }
        state.is_running_task = true;
        self.conn = Some(flow);
        Ok(())
    }

    /// 无参 / 带串参的动作。`arg` 的语义随 `name`。
    fn plain(
        &mut self,
        name: &'static str,
        arg: Option<String>,
        state: &mut AppState,
        paths: &Paths,
        dialog: &mut dyn DialogProvider,
    ) -> Result<(), ToolError> {
        match name {
            "read-IC" => {
                let extra: Vec<String> = Vec::new();
                self.read_ic(name, extra, state, paths)
            }
            "done-input-keys-read-IC" => {
                // 载荷是逗号分隔的密钥串（app 层已用 parse_12hex 扫过一遍，
                // 这里再扫一次以防直接构造），转成 mfclassic 的 -k 参数。
                let keys = arg.as_deref().map(crate::model::keys::parse_12hex);
                match keys {
                    // 原版 `keys.match(...)` 匹配不到会 `.forEach` 抛 TypeError；
                    // 一个密钥都没有时不派发任何命令。
                    Some(k) if k.is_empty() => Ok(()),
                    Some(k) => {
                        let extra = k.iter().map(|key| format!("-k{key}")).collect();
                        self.read_ic(name, extra, state, paths)
                    }
                    None => self.read_ic(name, Vec::new(), state, paths),
                }
            }
            "detect-card-type" => {
                let args = tasks::detect::detect_args(paths, false, false);
                self.launch(
                    "nfc-mfdetect",
                    &args,
                    state,
                    "log_msg_start_detect_card",
                    Running {
                        name,
                        parser: Some(KeyInfoParser::new()),
                        detect_saves_dump: false,
                        ..Running::default()
                    },
                )
            }
            "cancel-task" => {
                self.runner.kill();
                self.running = None;
                state.is_running_task = false;
                state.progress = None;
                // 复位 HardNested 状态机与本轮上下文：否则取消后
                // `hard_nested_task.phase` 残留 `Collecting`/`Cracking`，与「空闲」
                // 相悖。虽下次 begin 会重置，但取消当刻的 UI 快照不该显示「破解中」。
                self.hn = tasks::hard_nested::HardNestedRunner::new();
                self.hn.sync(state);
                self.hn_cfg = None;
                self.hn_round = tasks::hard_nested::RoundContext::default();
                self.hn_saving = false;
                self.pending_save = None;
                state.apply(Event::Status {
                    text: crate::i18n::t("indicator_free"),
                    indicator: Indicator::Free,
                });
                Ok(())
            }
            // ── 写卡链路（S11-2）────────────────────────────────────
            "write-IC" => {
                // 文件已由 app 层的系统对话框选好，作为 arg 传进来，
                // 故 `prepare_write` 走 `Some` 分支、不再弹框。
                let dump = arg.unwrap_or_default();
                let tasks::card_write::PrepareOutcome::Ready(src) =
                    tasks::card_write::prepare_write(paths, Some(&dump), dialog)
                else {
                    return Ok(());
                };
                self.read_then_exec(
                    name,
                    state,
                    paths,
                    NextStage {
                        tool: "nfc-mfclassic",
                        args: tasks::card_write::mfclassic_write_args(&src, &paths.temp_mfd()),
                        remove_temp: true,
                        report: true,
                        status_key: "indicator_writing_ic_card",
                        cuid: false,
                    },
                )
            }
            // S18：CUID 空卡写入。复用 write-IC 的「备份 + 两段式」，仅第二段
            // 改用大写 W（解锁写）+ 按当前卡号生成的 CUID 空卡密钥文件。
            "cuid-write" => {
                let dump = arg.unwrap_or_default();
                let tasks::card_write::PrepareOutcome::Ready(src) =
                    tasks::card_write::prepare_write(paths, Some(&dump), dialog)
                else {
                    return Ok(());
                };
                self.read_then_exec(
                    name,
                    state,
                    paths,
                    NextStage {
                        tool: "nfc-mfclassic",
                        args: tasks::card_write::mfclassic_write_args(&src, &paths.temp_mfd()),
                        remove_temp: true,
                        report: true,
                        status_key: "indicator_writing_cuid",
                        cuid: true,
                    },
                )
            }
            "format-card" => {
                // S17：补应用内确认窗 + engine 侧 confirm_danger 第二道闸
                // （原版 `command.js:144-149` 直接开跑，误点即清空卡片）。
                if tasks::card_write::prepare_format(dialog)
                    != tasks::card_write::PrepareOutcome::ReadyNoFile
                {
                    return Ok(());
                }
                self.read_then_exec(
                    name,
                    state,
                    paths,
                    NextStage {
                        tool: "nfc-mfclassic",
                        args: tasks::card_write::mfclassic_format_args(&paths.temp_mfd()),
                        remove_temp: true,
                        // S17：格式化也纳入完成行完整性判定（原版 report:false，
                        // 格式化成败对用户不可见）。成功才删备份并报成功。
                        report: true,
                        status_key: "indicator_formatting_ic_card",
                        cuid: false,
                    },
                )
            }
            "write-ufuid-uid" => {
                // dump 路径同样由 app 预选好；block0 hex 在此读取并拼进确认
                // 正文（`command.js:193-214`）。
                let path = match arg {
                    Some(a) if !a.is_empty() => PathBuf::from(a),
                    _ => return Ok(()),
                };
                let hex =
                    match tasks::card_write::prepare_write_uid(state, paths, dialog, Some(path)) {
                        Ok(h) => h,
                        Err(_) => return Ok(()),
                    };
                self.launch(
                    "nfc-mfsetuid",
                    &tasks::card_write::mfsetuid_args(&hex),
                    state,
                    "indicator_writing_ufuid_uid",
                    Running {
                        name,
                        ..Running::default()
                    },
                )
            }
            "lock-ufuid" => {
                if tasks::card_write::prepare_lock(dialog)
                    != tasks::card_write::PrepareOutcome::ReadyNoFile
                {
                    return Ok(());
                }
                self.launch(
                    "nfc-mflock",
                    &tasks::card_write::mflock_args(),
                    state,
                    "indicator_locking_ufuid",
                    Running {
                        name,
                        ..Running::default()
                    },
                )
            }
            // ── 手机/手环操作区（S14）────────────────────────────
            "open-phw-window" => {
                state.panels.phone_wristband = true;
                Ok(())
            }
            "phw-read-uid" => self.phw_read_uid(state, paths),
            "phw-write-uid" => self.phw_write_uid(name, state, paths, dialog),
            "phw-write-data" => self.phw_write_data(name, state, paths, dialog),
            // S18：手输卡号写 UFUID（`input-uid-write` 开面板由 app 层处理，
            // 这里只处理真正跑 `nfc-mfsetuid` 的 `done-input-uid-write`）。
            "done-input-uid-write" => self.input_uid_write(name, arg, state, dialog),
            // S18 全加密爆破：`mfcuk` 直接启动，密钥解析器挂在 Running 上。
            "run-mfcuk" => {
                let args = tasks::card_write::mfcuk_args();
                self.launch(
                    "mfcuk",
                    &args,
                    state,
                    "log_msg_start_mfcuk",
                    Running {
                        name,
                        mfcuk: Some(crate::model::parse::MfcukKeyParser::new()),
                        ..Running::default()
                    },
                )
            }
            // 纯 UI / 由 app 层处理的动作：Engine 无事可做。
            // 注意 `hard-nested` / `dict-test` 在这里是「开面板」，
            // 真正跑命令的是它们的 `*-config-done` 变体。
            // S18：`input-uid-write`（开面板）/ `dump-editor-export-keys`
            //（编辑器导出，纯文件）同属 app 层。
            "input-keys-read-IC"
            | "hard-nested"
            | "dict-test"
            | "open-dump-editor"
            | "open-dump-comparator"
            | "dump-history"
            | "open-about"
            | "open-settings-window"
            | "open-history-keys"
            | "open-dict-file"
            | "save-log"
            | "open-dump-folder"
            | "input-uid-write"
            | "dump-editor-export-keys" => Ok(()),
            other => {
                // 未接线：显式记一条日志，避免"点了没反应"被当成正常。
                state.apply(Event::Log(format!("\n{other}\n")));
                Err(ToolError::NotWired(other))
            }
        }
    }

    /// `read-IC` / `done-input-keys-read-IC` → `nfc-mfclassic`。
    fn read_ic(
        &mut self,
        name: &'static str,
        extra: Vec<String>,
        state: &mut AppState,
        paths: &Paths,
    ) -> Result<(), ToolError> {
        let args = tasks::mfoc::mfoc_args(paths, &extra);
        // ETA 分母 = 默认密钥 + keys 文件里的 + 命令行传的（docs/01 §4.1：
        // 13 默认 + keys.txt 条数 + -k 条数）。此前把 keys.txt 条数按 0 估，
        // 分母偏小会让进度过早跳到 100%。
        let file_keys = std::fs::read_to_string(paths.keys_file())
            .map(|s| crate::model::keys::parse_12hex(&s).len())
            .unwrap_or(0);
        let total = tasks::mfoc::mfoc_keys_total(file_keys, &extra);
        self.launch(
            "mfoc",
            &args,
            state,
            "log_msg_start_mfoc",
            Running {
                name,
                eta: Some(tasks::mfoc::EtaTracker::new(total, 0)),
                // command.js:641-645 从输出里取 UID 当 dump 文件名前缀
                scan_mfoc_uid: true,
                ..Running::default()
            },
        )
    }

    // ── 手机/手环操作区（S14）────────────────────────────

    /// `phw-read-uid`：`nfc-mfdetect -O<temp> -f<keys>` 读当前卡，成功后取
    /// temp 的 block0 缓存进 `state.phw.block0`。对标 `commands.rs:1114`。
    fn phw_read_uid(&mut self, state: &mut AppState, paths: &Paths) -> Result<(), ToolError> {
        // phw-read-uid 只需卡号，备份完整性不强制（宽松模式，不加 -S）
        let args = tasks::detect::detect_args(paths, true, false);
        self.launch(
            "nfc-mfdetect",
            &args,
            state,
            "log_msg_phw_read_uid",
            Running {
                name: "phw-read-uid",
                parser: Some(KeyInfoParser::new()),
                detect_saves_dump: true,
                phw_read_uid: true,
                ..Running::default()
            },
        )
    }

    /// `phw-write-uid`：把已缓存的 block0 写入目标卡的 UID 位。
    /// 对标 `commands.rs:1137`。无缓存则报错；危险确认后 spawn。
    fn phw_write_uid(
        &mut self,
        name: &'static str,
        state: &mut AppState,
        _paths: &Paths,
        dialog: &mut dyn DialogProvider,
    ) -> Result<(), ToolError> {
        let Some(block0) = state
            .phw
            .block0
            .clone()
            .filter(|b| tasks::card_write::block0_is_valid(b))
        else {
            state.apply(Event::Log(format!(
                "\n{}\n",
                crate::i18n::t("dialog_msg_need_phw_read_uid_first")
            )));
            return Ok(());
        };
        // 安全闸：BCC 不自洽的 block0 写入会直接砖卡。`nfc-mfsetuid` 侧默认
        // 拒绝（不加 `-f`），此处提前拦下并落明确日志，避免用户在不知道
        // 卡号校验位有问题的情况下授权写入。
        if !tasks::phw::block0_bcc_ok(&block0) {
            state.apply(Event::Log(format!(
                "\n{}\n",
                crate::i18n::t("log_msg_block0_bcc_invalid")
            )));
            return Ok(());
        }
        if !dialog.confirm_danger(&tasks::phw::write_uid_confirm(&block0)) {
            return Ok(());
        }
        self.launch(
            "nfc-mfsetuid",
            &tasks::card_write::mfsetuid_args(&block0),
            state,
            "indicator_writing_ufuid_uid",
            Running {
                name,
                ..Running::default()
            },
        )
    }

    /// `phw-write-data`：读回目标卡 → 程序改块 → 整卡写回。
    /// 对标 `commands.rs:1176`。块号越界 / 无缓存直接报错，不 spawn。
    fn phw_write_data(
        &mut self,
        name: &'static str,
        state: &mut AppState,
        paths: &Paths,
        dialog: &mut dyn DialogProvider,
    ) -> Result<(), ToolError> {
        let Some(block0) = state
            .phw
            .block0
            .clone()
            .filter(|b| tasks::card_write::block0_is_valid(b))
        else {
            state.apply(Event::Log(format!(
                "\n{}\n",
                crate::i18n::t("dialog_msg_need_phw_read_uid_first")
            )));
            return Ok(());
        };
        let target = tasks::phw::SectorBlock {
            sector: state.phw.sector,
            block: state.phw.block,
        };
        if !target.is_valid() {
            state.apply(Event::Log(format!(
                "\n{}\n",
                crate::i18n::t("dialog_msg_phw_sector_block_invalid")
            )));
            return Ok(());
        }
        if !dialog.confirm_danger(&tasks::phw::write_data_confirm(&block0, target)) {
            return Ok(());
        }
        // 第一段读回当前卡（mfdetect -O），完成后先改块、再整卡写回。
        // S17：此备份要当整卡写回来源，故开严格模式（-S），缺块就不落盘。
        self.launch(
            "nfc-mfdetect",
            &tasks::detect::detect_args(paths, true, true),
            state,
            "log_msg_phw_read_back",
            Running {
                name,
                parser: Some(KeyInfoParser::new()),
                detect_saves_dump: true,
                next: Some(NextStage {
                    tool: "nfc-mfclassic",
                    args: tasks::card_write::mfclassic_write_args(
                        &paths.temp_mfd(),
                        &paths.temp_mfd(),
                    ),
                    remove_temp: true,
                    report: true,
                    status_key: "indicator_writing_phw_data",
                    cuid: false,
                }),
                phw_patch: Some(target),
                ..Running::default()
            },
        )
    }

    /// 读回 temp.mfd → 把 block0 写进目标块 → 写回。失败返回错误信息。
    fn patch_phw_block(
        &self,
        target: &tasks::phw::SectorBlock,
        state: &AppState,
        paths: &Paths,
    ) -> Result<(), String> {
        let Some(block0) = &state.phw.block0 else {
            return Err(crate::i18n::t("dialog_msg_need_phw_read_uid_first"));
        };
        let mut bytes = std::fs::read(paths.temp_mfd())
            .map_err(|_| crate::i18n::t("log_msg_card_read_failed"))?;
        if !tasks::phw::patch_data_block(&mut bytes, block0, *target) {
            return Err(crate::i18n::t("log_msg_card_read_failed"));
        }
        std::fs::write(paths.temp_mfd(), &bytes)
            .map_err(|_| crate::i18n::t("log_msg_card_read_failed"))?;
        Ok(())
    }

    /// S18 手输卡号写 UFUID：清洗输入 → 拼 0 块 → 危险确认 → `nfc-mfsetuid`。
    ///
    /// 对标上游 Tauri `done_input_uid_write`：8 位 UID 自动补成 0 块，32 位直接
    /// 用；BCC 不自洽时追加 `-f`（强制写入）并警示。`-q` 静默。
    fn input_uid_write(
        &mut self,
        name: &'static str,
        arg: Option<String>,
        state: &mut AppState,
        dialog: &mut dyn DialogProvider,
    ) -> Result<(), ToolError> {
        let text = arg.unwrap_or_default();
        let Some(cleaned) = tasks::card_write::clean_uid_input(&text) else {
            state.apply(Event::Log(format!(
                "\n{}\n",
                crate::i18n::t("dialog_msg_invalid_uid_input")
            )));
            return Ok(());
        };
        let block0 = if cleaned.len() == 8 {
            tasks::card_write::uid_to_block0(&cleaned)
        } else {
            cleaned
        };
        // BCC 不自洽时强制写入（对标上游 `-f` 分支）。
        let force_bcc = !tasks::phw::block0_bcc_ok(&block0);
        if !dialog.confirm_danger(&tasks::card_write::input_uid_confirm(&block0, force_bcc)) {
            return Ok(());
        }
        if force_bcc {
            state.apply(Event::Log(format!(
                "\n{}\n",
                crate::i18n::t("log_msg_bcc_force")
            )));
        }
        let mut args = tasks::card_write::mfsetuid_args(&block0);
        if force_bcc {
            args.push("-f".to_string());
        }
        self.launch(
            "nfc-mfsetuid",
            &args,
            state,
            "indicator_writing_ufuid_uid",
            Running {
                name,
                ..Running::default()
            },
        )
    }

    /// `hard-nested-config-done`：auto_run 的第一步永远是 `nfc-mfdetect`。
    ///
    /// 对标 `hard_nested::begin` 的 `config.auto_run` 分支；非 auto_run 的
    /// collect/crack 多轮循环属 S11 第二步（需要 `HardNestedRunner` 的
    /// round-trip 编排）。
    /// 启动 HardNested 闭环的第一步，对标 `command.js:248-275`
    /// （`hard-nested-config-done`）。
    fn hard_nested(
        &mut self,
        state: &mut AppState,
        paths: &Paths,
        cfg: &HardNestedConfig,
    ) -> Result<(), ToolError> {
        state.hard_nested_task.config = Some(cfg.clone());
        self.hn_cfg = Some(cfg.clone());
        self.hn_round = tasks::hard_nested::RoundContext::new(cfg.clone());
        let unknown = to_unknown(&state.unknown_key_info);
        // 原版 autoRun 起手会清空 nonces.bin（command.js:249），
        // 避免上一轮的残留 nonce 混进本轮破解。
        if cfg.auto_run {
            let _ = tasks::hard_nested::clear_nonces(paths);
        }
        let step = self.hn.begin(cfg, &unknown);
        self.hn.sync(state);
        let n = unknown.len();
        self.hn_start(step, state, paths, n)
    }

    /// 把 `Step` 变成一次工具启动。
    ///
    /// `Stop*` 系列在此落成可见反馈（记日志 + 置空闲）——「点了没反应」必须
    /// 变成看得见的终止原因，见 docs/02 §6.10「未接线动作必须显式报错」。
    fn hn_start(
        &mut self,
        step: tasks::hard_nested::Step,
        state: &mut AppState,
        paths: &Paths,
        unknown_before: usize,
    ) -> Result<(), ToolError> {
        use tasks::hard_nested::Step;
        match step {
            Step::StopNoKnownKey => {
                state.apply(Event::Log(format!(
                    "\n{}\n",
                    crate::i18n::t("log_msg_not_found_known_key")
                )));
                self.hn_idle(state);
                Ok(())
            }
            Step::StopAllDecrypted => {
                state.apply(Event::Log(format!(
                    "\n{}\n",
                    crate::i18n::t("log_msg_tried_decrypt_all_unknown_keys")
                )));
                self.hn_idle(state);
                Ok(())
            }
            Step::StopCancelled => {
                self.hn_idle(state);
                Ok(())
            }
            Step::StopNoProgress => {
                // 偏离：原版无此判据，会无限递归。见 hard_nested.rs 的说明。
                state.apply(Event::Log(format!(
                    "\n{}\n",
                    crate::i18n::t("log_msg_hard_nested_no_progress")
                )));
                self.hn_idle(state);
                Ok(())
            }
            Step::SaveNonces => {
                // 采集完 + collectOnly（`command.js:310-332`）：
                // 先判非空（`statSync(nonces).size !== 0`），再弹保存框，
                // 最后 `rename` 把 nonces.bin 搬到用户选的位置。
                if tasks::hard_nested::nonces_is_empty(paths) {
                    state.apply(Event::Log(format!(
                        "\n{}\n",
                        crate::i18n::t("log_msg_nonces_empty_not_saved")
                    )));
                    self.hn_idle(state);
                    return Ok(());
                }
                let name = self.hn_round.nonces_default_name();
                // S11-7：同 read-IC 收尾，默认文件名含本轮 sector，事先无法预选。
                // 挂起请求交 app 弹框，回来在 `resolve_nonces` 里 rename。
                self.pending_save = Some(crate::dialogs::PendingSave {
                    default_path: PathBuf::from(&name),
                    request: crate::dialogs::SaveDialogRequest {
                        title: crate::i18n::t("dialog_title_save_to"),
                        default_path: PathBuf::from(name),
                        filters: vec![crate::dialogs::FileFilter {
                            name: crate::i18n::t("file_type_bin"),
                            extensions: vec!["bin".to_string()],
                        }],
                        message: crate::i18n::t("dialog_msg_choose_save_path"),
                    },
                    source: paths.nonces_file(),
                    refresh_history: false,
                });
                self.hn_saving = true;
                Ok(())
            }
            Step::Detect => {
                let args = tasks::detect::detect_args(paths, false, false);
                self.launch(
                    "nfc-mfdetect",
                    &args,
                    state,
                    "log_msg_start_detect_card",
                    Running {
                        name: "hard-nested",
                        parser: Some(KeyInfoParser::new()),
                        detect_saves_dump: false,
                        // 原版 readICThenExec 的 4 参形式不置空闲，状态留给
                        // 下一环（command.js:601-602）
                        chain_continues: true,
                        hn: Some(HnStep::Detect),
                        unknown_before,
                        ..Running::default()
                    },
                )
            }
            Step::Collect => {
                let ctx = &self.hn_round;
                let args = ctx.collect_args(&paths.nonces_file());
                // 先算好进度文案，交给 launch 一次性写入——若分两次 apply，
                // 后者会用 i18n 的静态文案把它盖掉（`launch` 内会再 apply 一次
                // `Event::Status`，而原版 `exec()` 根本不调 printStatus，
                // 状态条停在带进度的文案上）。
                let status = self.hn_progress_text(state, false);
                self.launch_with_status(
                    tasks::hard_nested::TOOL_COLLECT,
                    &args,
                    state,
                    status,
                    Running {
                        name: "hard-nested",
                        collect: Some(crate::model::parse::CollectParser::new()),
                        hn: Some(HnStep::Collect),
                        unknown_before,
                        ..Running::default()
                    },
                )
            }
            Step::Crack => {
                let ctx = &self.hn_round;
                let args = ctx.crypto1_args(&paths.nonces_file());
                let status = self.hn_progress_text(state, true);
                self.launch_with_status(
                    tasks::hard_nested::TOOL_CRYPTO1,
                    &args,
                    state,
                    status,
                    Running {
                        name: "hard-nested",
                        crypto: Some(crate::model::parse::SingleKeyParser::cropto1()),
                        hn: Some(HnStep::Crack),
                        unknown_before,
                        ..Running::default()
                    },
                )
            }
            Step::Done => {
                self.hn_idle(state);
                Ok(())
            }
        }
    }

    /// HardNested 收工：置空闲 + 记「运行完毕」。原版 `printExitLog(0)`。
    fn hn_idle(&mut self, state: &mut AppState) {
        self.hn.sync(state);
        state.apply(Event::Status {
            text: crate::i18n::t("indicator_free"),
            indicator: Indicator::Free,
        });
        state.apply(Event::Log(format!(
            "\n{}\n",
            crate::i18n::t("log_msg_finished")
        )));
    }

    /// 状态条文案，`command.js:284`（采集中）与 `:336`（破解中）。
    ///
    /// 进度后缀**只有 autoRun 模式才带**：原版采集处写的是
    /// `${collecting_nonces}${autoRun ? " - n/total" : ""}`，破解处则是
    /// 无条件带（`command.js:336`）。故 `collecting` 时要额外看 `auto_run`。
    fn hn_progress_text(&self, state: &AppState, cracking: bool) -> String {
        let key = if cracking {
            "indicator_doing_hard_nested"
        } else {
            "indicator_collecting_nonces"
        };
        let auto = self.hn_cfg.as_ref().is_some_and(|c| c.auto_run);
        let total = self.hn.total_unknown();
        if total == 0 || (!cracking && !auto) {
            return crate::i18n::t(key);
        }
        let idx = tasks::hard_nested::HardNestedRunner::progress_text(
            state.unknown_key_info.len(),
            total,
        );
        format!("{} - {idx}", crate::i18n::t(key))
    }

    /// `dict-test-config-done` → `nfc-mfdict`。
    ///
    /// 字典路径取自 `state.dict_path`（对应原版的模块级 `dictPath` 变量），
    /// 故不需要 `Paths`。
    fn dict_test(&mut self, state: &mut AppState, cfg: &DictTestConfig) -> Result<(), ToolError> {
        let args = tasks::dict_test::dict_args(cfg, &state.dict_path);
        self.launch(
            "nfc-mfdict",
            &args,
            state,
            "log_msg_start_test_dictionary",
            Running {
                name: "dict-test-config-done",
                // 命中行是 `Found Key: `（不是 mfoc 的 `[Key: `），改由
                // `SingleKeyParser::mfdict()` 收。曾误挂 mfoc 的 `EtaTracker`：
                // 它的进度信号是 `[Key: `，字典测试永不产出，ETA 恒不更新、
                // 分母还拿 start_position 当字典总长——此处一并移除（见 docs/02 §6）。
                crypto: Some(crate::model::parse::SingleKeyParser::mfdict()),
                dict_target: Some((cfg.sector, cfg.key_type)),
                ..Running::default()
            },
        )
    }

    /// `readICThenExec` 的**第一段**：`nfc-mfdetect -O<temp> -f<keys>` 备份当前卡。
    ///
    /// 对标 `command.js:591-593`（`isSaveDumpFile` 为真时）。备份是写卡 /
    /// 格式化前的安全网：第二段启动前会检查 `temp.mfd` 是否真的存在，
    /// 不存在就中止（`command.js:603-607`）。
    fn read_then_exec(
        &mut self,
        name: &'static str,
        state: &mut AppState,
        paths: &Paths,
        next: NextStage,
    ) -> Result<(), ToolError> {
        // S17：写卡/格式化的备份段开严格模式（-S），缺块不落盘。
        let args = tasks::detect::detect_args(paths, true, true);
        self.launch(
            "nfc-mfdetect",
            &args,
            state,
            "indicator_backing_up_current_card",
            Running {
                name,
                parser: Some(KeyInfoParser::new()),
                detect_saves_dump: true,
                next: Some(next),
                ..Running::default()
            },
        )
    }

    /// `nfc-mfdetect` 退出后，若带了第二段就接上去。
    ///
    /// 关键安全闸（`command.js:603-607`）：`temp.mfd` 没生成说明备份失败，
    /// 此时**绝不**执行写卡 / 格式化，只记一条「读卡失败，未执行操作」。
    fn start_next_stage(
        &mut self,
        next: NextStage,
        state: &mut AppState,
        paths: &Paths,
        skip_name: &'static str,
    ) -> Result<(), ToolError> {
        let temp = paths.temp_mfd();
        // 备份文件缺失或为空（`command.js:597-599` 删空文件、`command.js:603`
        // 判不存在）都视为备份失败。
        let backed_up = temp.is_file() && std::fs::metadata(&temp).is_ok_and(|m| m.len() > 0);
        if !backed_up {
            state.apply(Event::Log(format!(
                "\n{}: {skip_name}\n",
                crate::i18n::t("log_msg_card_read_failed")
            )));
            state.apply(Event::Status {
                text: crate::i18n::t("indicator_error"),
                indicator: Indicator::Error,
            });
            return Ok(());
        }
        // S18 CUID：备份成功后按当前卡号生成 CUID 空卡密钥文件，第二段改
        // 大写 `W`（解锁写）+ 该密钥文件。生成失败同样中止，绝不写卡。
        if next.cuid {
            let uid_hex = match tasks::card_write::read_block0_hex(&temp) {
                Ok(h) if tasks::card_write::block0_is_valid(&h) => h,
                _ => {
                    state.apply(Event::Log(format!(
                        "\n{}: {skip_name}\n",
                        crate::i18n::t("log_msg_card_read_failed")
                    )));
                    state.apply(Event::Status {
                        text: crate::i18n::t("indicator_error"),
                        indicator: Indicator::Error,
                    });
                    return Ok(());
                }
            };
            let keyfile = paths.cuid_keyfile();
            let bytes = match tasks::card_write::cuid_keyfile_bytes(tasks::card_write::uid_prefix(
                &uid_hex,
            )) {
                Some(b) => b,
                None => {
                    state.apply(Event::Log(format!(
                        "\n{}\n",
                        crate::i18n::t("log_msg_card_read_failed")
                    )));
                    return Ok(());
                }
            };
            if std::fs::write(&keyfile, &bytes).is_err() {
                state.apply(Event::Status {
                    text: crate::i18n::t("indicator_error"),
                    indicator: Indicator::Error,
                });
                return Ok(());
            }
            crate::paths::restrict_owner(&keyfile);
            state.apply(Event::Log(format!(
                "\n{}\n",
                crate::i18n::t("log_msg_cuid_key_built")
            )));
            let src = next.args.get(3).cloned().unwrap_or_default();
            let args = tasks::card_write::cuid_write_args(std::path::Path::new(&src), &keyfile);
            return self.launch(
                next.tool,
                &args,
                state,
                next.status_key,
                Running {
                    name: next.tool,
                    remove_temp: next.remove_temp,
                    report: next.report,
                    block_stats: Some(BlockStatsParser::new()),
                    cuid: true,
                    ..Running::default()
                },
            );
        }
        self.launch(
            next.tool,
            &next.args,
            state,
            next.status_key,
            Running {
                name: next.tool,
                remove_temp: next.remove_temp,
                report: next.report,
                // S17：写卡/格式化第二段挂完成行解析器，收尾时据此判完整性。
                block_stats: Some(BlockStatsParser::new()),
                ..Running::default()
            },
        )
    }

    /// 推进一次：把已到达的输出喂给状态机，工具退出时做收尾。
    ///
    /// 每帧调一次。文本先过 `Event::Log`（core 侧的 `append_log` 会去清行
    /// 序列、保留换行），与原版 `printLog` 的规范化一致。
    ///
    /// 连接握手与普通任务共用同一个 runner，但二者互斥（`connect` 与
    /// `launch` 都拒并发），故这里二选一推进。
    pub fn pump(&mut self, state: &mut AppState, paths: &Paths) {
        if self.conn.is_some() {
            self.pump_conn(state);
            return;
        }
        let mut buf = String::new();
        self.runner.pump(&mut |c: &Chunk| buf.push_str(&c.text));
        if buf.is_empty() {
            // 没有新输出也要查退出：工具可能打完就退了
            if let Some(exit) = self.runner.exit() {
                if let Some(run) = self.running.take() {
                    self.finish(run, &exit, state, paths);
                }
            }
            return;
        }
        state.apply(Event::Log(buf.clone()));
        self.feed(&buf, state);

        if let Some(exit) = self.runner.exit() {
            if let Some(run) = self.running.take() {
                self.finish(run, &exit, state, paths);
            }
        }
    }

    /// 推进连接握手。`finish` 发出 `TaskDone`，由 `AppState::apply` 复位
    /// `is_running_task`（对标原版 `task.on('close')` 里的
    /// `status.isRunningTask = false`），故这里只需回收 `conn`。
    fn pump_conn(&mut self, state: &mut AppState) {
        let Some(mut flow) = self.conn.take() else {
            return;
        };
        flow.pump(self.runner.as_mut(), &mut |e| state.apply(e));
        let done = flow
            .finish(self.runner.as_mut(), &mut |e| state.apply(e))
            .is_some();
        if !done {
            self.conn = Some(flow);
        }
    }

    /// 把一段输出喂给当前任务的解析器。
    fn feed(&mut self, text: &str, state: &mut AppState) {
        // ETA 用单调时钟的已用时长；epoch 未设（无带 ETA 的任务启动过）时回落 0。
        let now_ms = self
            .eta_epoch
            .map(|t| t.elapsed().as_millis() as u64)
            .unwrap_or(0);
        let Some(run) = self.running.as_mut() else {
            return;
        };
        if let Some(p) = run.parser.as_mut() {
            p.push(text);
        }
        if let Some(eta) = run.eta.as_mut() {
            if let Some(u) = eta.push(text, now_ms) {
                tasks::mfoc::apply_eta(state, &u);
            }
        }
        if run.scan_mfoc_uid && run.uid.is_none() {
            if let Some(uid) = tasks::mfoc::take_uid(text) {
                run.uid = Some(uid);
            }
        }
        // 写卡/格式化第二段：抓 `Done, X of Y blocks written.` 判定完整性。
        if let Some(p) = run.block_stats.as_mut() {
            p.push(text);
        }
        // HardNested 的两个工具各自解析自己的标记
        if let Some(c) = run.collect.as_mut() {
            c.push(text);
        }
        if let Some(c) = run.crypto.as_mut() {
            c.push(text);
        }
        // S18 全加密爆破：mfcuk 一轮可出多把密钥，逐行收集
        if let Some(p) = run.mfcuk.as_mut() {
            p.push(text);
        }
    }

    /// 重建 autoRun 下一轮的配置，对标 `command.js:350-359`：
    /// `knownKey/knownSector/knownKeyType` 取 `known[0]`，
    /// `targetSector/targetKeyType` 取 `unknown[0]`。
    ///
    /// 偏离：原版直接 `knownKeyInfo[0][0]`，列表为空会抛 TypeError 把窗口带崩。
    /// 此处返回 `false` 交调用方收工。
    fn hn_advance_round(&mut self, state: &AppState) -> bool {
        let (Some((key, ks, kt)), Some((ts, tt))) =
            (state.known_key_info.first(), state.unknown_key_info.first())
        else {
            return false;
        };
        let cfg = HardNestedConfig {
            known_key: key.clone(),
            known_sector: *ks,
            known_key_type: *kt,
            target_sector: *ts,
            target_key_type: *tt,
            collect_only: false,
            auto_run: true,
        };
        self.hn_cfg = Some(cfg.clone());
        self.hn_round = tasks::hard_nested::RoundContext::new(cfg);
        true
    }

    /// HardNested 的每步收尾 → 推进状态机 → 启动下一步。
    fn hn_advance(
        &mut self,
        step: HnStep,
        run: HnArtifacts,
        exit: &runner::Exit,
        state: &mut AppState,
        paths: &Paths,
    ) {
        // 非 0 退出即中止本轮。原版 `exec` 会 reject（execUtils.js:51-52），
        // 挂在后面的 `.then()` 不执行，故不推进状态机。
        if let Err(e) = exit.result() {
            self.hn.sync(state);
            state.apply(Event::Status {
                text: crate::i18n::t("indicator_error"),
                indicator: Indicator::Error,
            });
            state.apply(Event::TaskDone {
                code: exit.code,
                killed: matches!(e, ToolError::Killed),
            });
            return;
        }
        let Some(cfg) = self.hn_cfg.clone() else {
            // 不该发生（hn_cfg 只写不清），但静默 return 会让状态条永远停在
            // 「运行中」。宁可记一条也不要无声卡死。
            state.apply(Event::Log(format!(
                "\n{}\n",
                crate::i18n::t("log_msg_hard_nested_state_lost")
            )));
            self.hn_idle(state);
            return;
        };
        let unknown = to_unknown(&state.unknown_key_info);
        let next = match step {
            HnStep::Detect => {
                let known: Vec<crate::model::keys::KeyInfo> = state
                    .known_key_info
                    .iter()
                    .map(|(k, s, t)| crate::model::keys::KeyInfo {
                        key: k.clone(),
                        sector: *s,
                        key_type: *t,
                    })
                    .collect();
                self.hn.after_detect(&known, &unknown)
            }
            HnStep::Collect => {
                // 记下本轮采集到的 uid / sector / key_type，collectOnly 的
                // 默认文件名要用（command.js:313）
                if let Some(c) = run.collect {
                    let i = c.info();
                    self.hn_round.uid = i.uid.clone();
                    self.hn_round.sector = i.sector;
                    self.hn_round.key_type = i.key_type;
                }
                self.hn.after_collect(&cfg)
            }
            HnStep::Crack => {
                // 命中则存密钥，并在队列中移除该 unknown（S18 F1：任意位置，非仅队首）
                if let Some(mut c) = run.crypto {
                    c.flush();
                    if let Some(key) = c.take() {
                        let _ = crate::model::keys::save_keys(paths, &[key]);
                    }
                }
                // 命中的扇区/类型在 unknown 里**任意位置**移除（command.js:347
                // 原为只比队首，S18 F1 修正为按值移除，乱序命中不再丢密钥）
                if let (Some(s), Some(k)) = (self.hn_round.sector, self.hn_round.key_type) {
                    let mut list = to_unknown(&state.unknown_key_info);
                    if tasks::hard_nested::remove_unknown(&mut list, s, k) {
                        state.unknown_key_info =
                            list.iter().map(|u| (u.sector, u.key_type)).collect();
                    }
                }
                // autoRun 下一轮的配置要从**当前的** known/unknown 重建
                // （command.js:350-359）。不重建就会一直用上一轮的
                // known_key / target_sector 原地打转。
                if cfg.auto_run && !self.hn_advance_round(state) {
                    // known 或 unknown 已空，无从构造下一轮。原版此处会
                    // `knownKeyInfo[0][0]` 抛 TypeError 把窗口带崩（command.js:352）。
                    self.hn.sync(state);
                    state.apply(Event::Log(format!(
                        "\n{}\n",
                        crate::i18n::t(if state.unknown_key_info.is_empty() {
                            "log_msg_tried_decrypt_all_unknown_keys"
                        } else {
                            "log_msg_not_found_known_key"
                        })
                    )));
                    self.hn_idle(state);
                    return;
                }
                // 关键：第三参必须是 pop **之后**的条数。早先误用了 pop 前的
                // 快照 `unknown.len()`，它与 `unknown_before` 恒相等
                // （两次快照之间没有 mfdetect），于是 `after_crack_guarded` 的
                // `unknown_after >= unknown_before` 永远成立，autoRun 每轮都
                // 判「无进展」并在第 1 轮后停住。
                let after = state.unknown_key_info.len();
                self.hn.after_crack_guarded(&cfg, run.unknown_before, after)
            }
        };
        let before = unknown.len();
        self.hn.sync(state);
        if let Err(e) = self.hn_start(next, state, paths, before) {
            // `ToolError` 别丢——用户要看到「为什么没跑成」
            state.apply(Event::Log(format!("\n{e}\n")));
            state.apply(Event::Status {
                text: crate::i18n::t("indicator_error"),
                indicator: Indicator::Error,
            });
        }
    }

    /// 工具退出后的收尾。
    fn finish(
        &mut self,
        mut run: Running,
        exit: &runner::Exit,
        state: &mut AppState,
        paths: &Paths,
    ) {
        state.is_running_task = false;
        state.progress = None;

        // S18 CUID：本次 CUID 写入生成的密钥文件是过程产物，收尾即删。
        if run.cuid {
            let _ = std::fs::remove_file(paths.cuid_keyfile());
        }
        // S18 全加密爆破：mfcuk 一轮可出多把密钥，无论退出码如何都先收走，
        // 落进历史密钥（下次读卡 / 字典测试可直接复用）。
        if let Some(mut p) = run.mfcuk.take() {
            p.flush();
            let keys = p.take_keys();
            if !keys.is_empty() {
                match crate::model::keys::save_keys(paths, &keys) {
                    Ok(fresh) => {
                        for k in fresh {
                            state.apply(Event::EventItem {
                                kind: EventKind::Key,
                                text: format!("mfcuk: {k}"),
                            });
                        }
                    }
                    Err(e) => {
                        state.apply(Event::Log(format!("\n{}\n", e)));
                    }
                }
            }
        }

        // mfdetect 退出时要 flush 解析器（结算末行），再落盘新密钥
        if let Some(mut p) = run.parser {
            p.flush();
            match tasks::detect::after_detect(state, &p, paths, run.detect_saves_dump) {
                Ok(_) => {
                    // 有未知密钥**且是检测动作**才给引导（`command.js:175-177`）。
                    // 备份段（写卡/格式化/CUID 的第一段）与 autoRun 前置探测也
                    // 复用 detect 解析更新 known/unknown，但**不得**推 Guidance：
                    // 否则写卡途中顶部会冒出「破解/字典」引导，与 docs/01 §3.4
                    // 「autoRun 复用 detect 解析但不推送 Guidance」相反。
                    if run.name == "detect-card-type" && !state.unknown_key_info.is_empty() {
                        let n = state.unknown_key_info.len();
                        state.apply(Event::Guidance {
                            text: tasks::detect::unknown_guidance_text(n),
                            buttons: tasks::detect::unknown_guidance_buttons(),
                        });
                    }
                }
                Err(msg) => {
                    state.apply(Event::Log(format!("\n{msg}\n")));
                }
            }
        }

        // 字典测试收尾：`Found Key: ` 命中 → 落盘密钥 → 从 unknown 移除对应项
        //（`command.js:396-399`：逐行命中即 saveKeys + 移除匹配 unknown）。
        if run.name == "dict-test-config-done" {
            if let Some(mut c) = run.crypto.take() {
                c.flush();
                if let Some(key) = c.take() {
                    let _ = tasks::dict_test::report_found_key(state, &key, paths);
                    // 按 (扇区, 类型) 在 unknown 任意位置移除（S18 F1 同款语义）
                    if let Some((sector, kt)) = run.dict_target {
                        let mut list = to_unknown(&state.unknown_key_info);
                        if tasks::hard_nested::remove_unknown(&mut list, sector, kt) {
                            state.unknown_key_info =
                                list.iter().map(|u| (u.sector, u.key_type)).collect();
                        }
                    }
                }
            }
        }

        // HardNested 闭环：工具退出后推进状态机。
        // **先于**两段式写卡与通用收尾——它自己管状态与收尾。
        if let Some(step) = run.hn {
            // 先把 hn 需要的字段取出来——上面的 parser 分支可能已部分移动 `run`
            let collect = run.collect;
            let crypto = run.crypto;
            let unknown_before = run.unknown_before;
            self.hn_advance(
                step,
                HnArtifacts {
                    collect,
                    crypto,
                    unknown_before,
                },
                exit,
                state,
                paths,
            );
            return;
        }

        match exit.result() {
            Ok(()) => {
                // 手机/手环：读回成功后取 block0 缓存 / 写回前改块（先于第二段）。
                let phw_read_uid = run.phw_read_uid;
                let phw_patch = run.phw_patch;
                if phw_read_uid {
                    match tasks::card_write::read_block0_hex(&paths.temp_mfd()) {
                        Ok(hex) if tasks::card_write::block0_is_valid(&hex) => {
                            state.phw.block0 = Some(hex.clone());
                            state.apply(Event::EventItem {
                                kind: EventKind::Success,
                                text: format!("卡号: {}", tasks::card_write::uid_prefix(&hex)),
                            });
                        }
                        _ => {
                            state.apply(Event::Log(format!(
                                "\n{}\n",
                                crate::i18n::t("log_msg_card_read_failed")
                            )));
                            state.apply(Event::Status {
                                text: crate::i18n::t("indicator_error"),
                                indicator: Indicator::Error,
                            });
                            return;
                        }
                    }
                }
                if let Some(target) = phw_patch {
                    if let Err(msg) = self.patch_phw_block(&target, state, paths) {
                        state.apply(Event::Log(format!("\n{msg}\n")));
                        state.apply(Event::Status {
                            text: crate::i18n::t("indicator_error"),
                            indicator: Indicator::Error,
                        });
                        let _ = std::fs::remove_file(paths.temp_mfd());
                        return;
                    }
                }
                // 备份段成功 → 接第二段（写卡 / 格式化）。
                // 必须判退出码：原版 `exec` 在 `code !== 0` 时 **reject**
                // （`execUtils.js:51-52`），而第二段挂在 `.then()` 上
                // （`command.js:600`），reject 后 `.then()` 不执行 ——
                // 即备份失败绝不能继续写卡。
                if let Some(next) = run.next {
                    let r = self.start_next_stage(next, state, paths, run.name);
                    if r.is_err() {
                        state.apply(Event::Status {
                            text: crate::i18n::t("indicator_error"),
                            indicator: Indicator::Error,
                        });
                    }
                    return;
                }
                // 第二段收尾：写卡 / 格式化成败文案 + 备份去留。
                //
                // S17：不再无条件删 temp。解析 `Done, X of Y blocks written.`
                // 判定是否**全部**块写入成功——成功才删备份并报「写卡成功」；
                // 部分写入 / 工具中途失败则保留备份以便恢复，并报失败。
                if run.report {
                    // 结算末行（末行无换行时 `Done, X of Y` 不能丢），再据以判定。
                    if let Some(p) = run.block_stats.as_mut() {
                        p.flush();
                    }
                    let stats = run.block_stats.as_ref().and_then(|p| p.take());
                    // CUID 走大写 `W`（解锁写）会写 block0，工具打印 `64/64`
                    // （含 block0）；普通写/格式化走小写 `w`/`f` 跳过 block0。
                    // 判据唯一实现在 `BlockStatsParser::is_write_complete`；
                    // 无完成行（None）时按退出码兜底。
                    let complete = match stats {
                        Some((w, t)) if t >= 1 => {
                            BlockStatsParser::is_write_complete((w, t), run.cuid)
                        }
                        _ => exit.code == Some(0),
                    };
                    if complete {
                        tasks::card_write::report_write_result(state, exit.code);
                        if run.remove_temp {
                            let _ = std::fs::remove_file(paths.temp_mfd());
                        }
                    } else {
                        state.apply(Event::Log(format!(
                            "\n{}\n",
                            crate::i18n::t("log_msg_write_failed")
                        )));
                        if let Some((w, t)) = stats {
                            state.apply(Event::Log(format!(
                                "\n{}（{w}/{t} 块）\n",
                                crate::i18n::t("log_msg_write_incomplete")
                            )));
                        }
                        if run.remove_temp {
                            let kept = paths.temp_mfd().display().to_string();
                            state.apply(Event::Log(format!(
                                "\n{}{}\n",
                                crate::i18n::t("log_msg_backup_kept"),
                                kept
                            )));
                        }
                    }
                } else if run.remove_temp {
                    // 非报告类第二段（当前无此路径）仍按原行为删 temp。
                    let _ = std::fs::remove_file(paths.temp_mfd());
                }
                // 读卡成功后把 temp.mfd 移进 `dumpfiles/`，并发引导
                // （`command.js:648-681`）。这是「读卡 → 写这张卡」这条
                // 主流程的必经环节：没有它 `Event::DumpSaved` 永不发出，
                // 引导条也就不会出现。
                if matches!(run.name, "read-IC" | "done-input-keys-read-IC") {
                    // S11-7：只「备好」保存请求，不在这里弹框。默认文件名含刚
                    // 读出的 UID，事先无法预选；而 `show_save_dialog` 是同步的，
                    // 在 UI 线程上真弹 rfd 会冻住窗口。app 取走 `pending_save`
                    // 弹框后回调 `resolve_save` 落盘。
                    if let Some(p) = tasks::mfoc::dump_prepare(state, paths, run.uid.as_deref()) {
                        self.pending_save = Some(p);
                        // 关键：`finish` 开头已把 `is_running_task` 置 false，
                        // 而 Busy 门禁只看这个字段。不重新置真的话，挂起期间
                        // 能再发起一个 read-IC，它会覆盖同一个 temp.mfd ——
                        // 用户在保存框里选好的路径就指到另一张卡的数据上。
                        state.is_running_task = true;
                    }
                }
                // PLAN v1：读到卡号就把这张卡追加进 registry（可查表）。
                if let Some(uid) = run.uid.as_deref() {
                    registry_append(state, paths, uid);
                }
                // 任何成功退出都要把状态落回「空闲」。
                // `printExitLog(0)`（`execUtils.js:75-78`）对**所有**工具
                // 一视同仁，不分 read-IC 还是 detect / dict。
                //
                // 例外：链路型工具（hard-nested 的第一步）原版不调
                // `printExitLog`，状态要留给下一环，故不置空闲。
                if !run.chain_continues {
                    state.apply(Event::Status {
                        text: crate::i18n::t("indicator_free"),
                        indicator: Indicator::Free,
                    });
                    state.apply(Event::Log(format!(
                        "\n{}\n",
                        crate::i18n::t("log_msg_finished")
                    )));
                }
            }
            // `Event::TaskDone` 内部已处理 killed 分支（记 event + exit log）
            Err(e) => {
                // 第一段失败时不删 temp.mfd——原版的 stage-1 `finishHandler`
                // 只存密钥、不 unlink（`command.js:595-599`），残件由下一次
                // `read_then_exec` 覆写。
                //
                // S17：第二段（写卡/格式化）失败时**保留** temp.mfd 以便恢复。
                // 原版 finishHandler 在 close 时无条件 unlink，写卡一旦部分成功
                // 又报错，用户就失去了原卡备份。此处改为保留并落日志路径。
                //
                // 注意 `run.remove_temp` 是「写卡/格式化第二段」的专属标记
                // （第一段、读卡、检测、锁卡都为 false），故下面两句只在**写卡
                // / 格式化失败**时落「写卡失败」+「备份已保留」，不会污染
                // 读卡/检测/连接等其他工具的失败日志（它们维持原行为：只置
                // error + 由 TaskDone 走 exit_log）。
                if run.remove_temp {
                    state.apply(Event::Log(format!(
                        "\n{}\n",
                        crate::i18n::t("log_msg_write_failed")
                    )));
                    let kept = paths.temp_mfd().display().to_string();
                    state.apply(Event::Log(format!(
                        "\n{}{}\n",
                        crate::i18n::t("log_msg_backup_kept"),
                        kept
                    )));
                }
                state.apply(Event::Status {
                    text: crate::i18n::t("indicator_error"),
                    indicator: Indicator::Error,
                });
                state.apply(Event::TaskDone {
                    code: exit.code,
                    killed: matches!(e, ToolError::Killed),
                });
            }
        }
    }
}

/// `AppState::unknown_key_info`（二元组）→ 状态机用的 [`UnknownKeyInfo`]。
fn to_unknown(
    list: &[(u8, crate::model::keys::KeyType)],
) -> Vec<crate::model::keys::UnknownKeyInfo> {
    list.iter()
        .map(|(s, k)| crate::model::keys::UnknownKeyInfo {
            sector: *s,
            key_type: *k,
        })
        .collect()
}

/// 默认密钥条数，供 ETA 分母用（`mfoc_keys_total` 的其中一项）。
pub const DEFAULT_KEY_COUNT: usize = DEFAULT_KEYS.len();

/// 读卡成功后把这张卡追加进 `registry.json`（PLAN v1「自动追加新卡」，docs/01
/// 决策 #8 的可查表）。
///
/// 失败一律静默：registry 是旁路可查表，写不进也不该让读卡显示失败。重复 UID
/// 由 `Registry::append` 去重。`key_a`/`key_b` 取本卡已解出的首条 A/B 密钥
/// （`state.known_key_info`）。
fn registry_append(state: &AppState, paths: &Paths, uid: &str) {
    if uid.is_empty() {
        return;
    }
    let (mut key_a, mut key_b) = (String::new(), String::new());
    for (k, _sector, kt) in &state.known_key_info {
        match kt {
            crate::model::keys::KeyType::A if key_a.is_empty() => key_a = k.clone(),
            crate::model::keys::KeyType::B if key_b.is_empty() => key_b = k.clone(),
            _ => {}
        }
    }
    let record = crate::data::CardRecord {
        uid: uid.to_owned(),
        key_a,
        key_b,
        date: crate::paths::timestamp(),
        ..crate::data::CardRecord::default()
    };
    let mut registry = crate::data::Registry::load(&paths.registry_file()).unwrap_or_default();
    let _ = registry.append_and_save(record, paths);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialogs::MockDialog;
    use crate::runner::MockToolRunner;
    use crate::state::DeviceState;

    /// 自持临时目录（守卫自持，避免 libtest 多线程互删——S8 的教训）。
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let p = std::env::temp_dir().join(format!(
                "nfc-engine-{tag}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).unwrap();
            Self(p)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn paths(tag: &str) -> (TempDir, Paths) {
        let d = TempDir::new(tag);
        let p = Paths::new(d.0.clone());
        let _ = std::fs::create_dir_all(p.dumpfiles_dir());
        (d, p)
    }

    /// 已连接的状态（门禁要求 `is_device_connected`）。
    fn connected() -> AppState {
        let mut s = AppState::new();
        s.current_device = Some("/dev/ttyACM0".into());
        s.is_device_connected = true;
        s.device_state = DeviceState::Connected;
        s
    }

    /// 把 engine 跑到工具退出为止（mock 契约：pump 一次后才认 exit）。
    fn run_to_end(e: &mut Engine, st: &mut AppState, p: &Paths, frames: usize) {
        for _ in 0..frames {
            e.pump(st, p);
        }
    }

    /// S11-7 的编排：pump 到 engine 挂起保存请求，再按 `url` 续接。
    ///
    /// 模拟 app 侧 `pending_save` → 弹框 → `resolve_save` 这条链。
    /// 返回 `resolve_save` 的结果（`false` = 没有挂起的请求）。
    fn settle_pending_save(
        e: &mut Engine,
        st: &mut AppState,
        p: &Paths,
        url: Option<std::path::PathBuf>,
    ) -> bool {
        for _ in 0..6 {
            e.pump(st, p);
        }
        e.resolve_save(url, st, p)
    }

    // ── 门禁 ──

    #[test]
    fn read_ic_is_gated_on_connection() {
        let (_d, p) = paths("gate");
        let mut e = Engine::new(Box::new(MockToolRunner::new(vec![])));
        let mut st = AppState::new(); // 未连接
        let mut dlg = MockDialog::new();
        let r = e.dispatch(Action::Plain("read-IC"), &mut st, &p, &mut dlg);
        assert_eq!(r, Err(ToolError::NotConnected));
        // 门禁拒绝时不该置 is_running_task
        assert!(!st.is_running_task);
        assert!(!e.is_running());
    }

    #[test]
    fn second_task_is_rejected_while_one_runs() {
        let (_d, p) = paths("busy");
        let mut e = Engine::new(Box::new(MockToolRunner::new(vec![])));
        let mut st = connected();
        let mut dlg = MockDialog::new();
        e.dispatch(Action::Plain("read-IC"), &mut st, &p, &mut dlg)
            .unwrap();
        assert!(st.is_running_task);
        // 第二个任务应被拒
        let r = e.dispatch(Action::Plain("detect-card-type"), &mut st, &p, &mut dlg);
        assert_eq!(r, Err(ToolError::Busy));
    }

    #[test]
    fn scan_enumerates_without_a_selected_device() {
        // 对标原版 `scan-usb-devices` → `SerialPort.list()`：不走 `exec`，
        // 故不受 `currentDevice === null` 门禁拒绝——未选设备也能刷下拉。
        let (_d, p) = paths("scan");
        let mut e = Engine::new(Box::new(MockToolRunner::new(vec![]))).with_serial(Box::new(
            serial::MockSerial::from(["/dev/ttyACM0", "/dev/ttyACM1"]),
        ));
        let mut st = AppState::new();
        let mut dlg = MockDialog::new();
        assert!(e.dispatch(Action::ScanUsb, &mut st, &p, &mut dlg).is_ok());
        assert_eq!(
            st.device_list,
            vec!["/dev/ttyACM0".to_string(), "/dev/ttyACM1".to_string()]
        );
        // 枚举不是「任务」：不占任务位，也不真的 spawn 了什么
        assert!(!st.is_running_task);
        assert!(!e.is_running());
        assert!(e.runner().started_last().is_none());
    }

    #[test]
    fn scan_is_idempotent_until_the_port_list_changes() {
        // PortScanner 自带变化检测：列表没变就不重复推 Event，
        // 故 500ms 轮询不会把 device_list 反复覆盖。
        let (_d, p) = paths("scan2");
        let serial = serial::MutableSerial::new(&["/dev/ttyACM0"]);
        let mut e = Engine::new(Box::new(MockToolRunner::new(vec![])))
            .with_serial(Box::new(serial.clone()));
        let mut st = AppState::new();
        let mut dlg = MockDialog::new();
        e.dispatch(Action::ScanUsb, &mut st, &p, &mut dlg).unwrap();
        assert_eq!(st.device_list, vec!["/dev/ttyACM0".to_string()]);
        // 第二次扫描：列表未变，仍不置任务位
        e.dispatch(Action::ScanUsb, &mut st, &p, &mut dlg).unwrap();
        assert!(!st.is_running_task);
        // 拔线 → 空列表也算变化
        serial.set(&[]);
        e.dispatch(Action::ScanUsb, &mut st, &p, &mut dlg).unwrap();
        assert!(st.device_list.is_empty());
    }

    // ── 参数构造 ──

    #[test]
    fn connect_writes_conf_and_handshake_reaches_connected() {
        // 死锁回归：早前 `connect` 只写出 libnfc.conf 就停在 `Connecting`，
        // 没有任何东西把它推进到 Connected/Failed —— UI 永远挂着「连接中」，
        // 且因为 state.device_state 一直是 Connecting，再点一次直接返回 Busy。
        let (_d, p) = paths("connect");
        let mut e = Engine::new(Box::new(MockToolRunner::new(vec![Chunk::stdout(
            "NFC device: NFC_Device opened\n",
        )])));
        let mut st = AppState::new();
        let mut dlg = MockDialog::new();
        e.dispatch(
            Action::ConnUsb("/dev/ttyACM0".into()),
            &mut st,
            &p,
            &mut dlg,
        )
        .unwrap();
        assert_eq!(st.current_device.as_deref(), Some("/dev/ttyACM0"));
        assert!(matches!(st.device_state, DeviceState::Connecting));
        assert!(p.nfc_config().is_file());
        assert!(std::fs::read_to_string(p.nfc_config())
            .unwrap()
            .contains("pn532_uart:/dev/ttyACM0:115200"));
        // 握手中算「在忙」：挡住并发任务（对标原版 exec 置 isRunningTask）
        assert!(st.is_running_task);
        assert!(e.is_running());
        run_to_end(&mut e, &mut st, &p, 8);
        assert!(
            matches!(st.device_state, DeviceState::Connected),
            "握手必须走到 Connected，不能停在 Connecting"
        );
        assert!(st.is_device_connected);
        assert!(!st.is_running_task);
        assert!(!e.is_running());
    }

    #[test]
    fn connect_unable_marker_ends_in_failed_not_stuck() {
        // 打不开设备时必须落 Failed：否则 state 停在 Connecting，
        // 下拉框被永久禁用，用户只能重开窗口。
        let (_d, p) = paths("connect_fail");
        let mut e = Engine::new(Box::new(MockToolRunner::new(vec![Chunk::stderr_chunk(
            "Unable to open NFC device\n",
        )])));
        let mut st = AppState::new();
        let mut dlg = MockDialog::new();
        e.dispatch(
            Action::ConnUsb("/dev/ttyACM0".into()),
            &mut st,
            &p,
            &mut dlg,
        )
        .unwrap();
        run_to_end(&mut e, &mut st, &p, 8);
        assert!(matches!(st.device_state, DeviceState::Failed));
        assert!(!st.is_device_connected);
        assert!(!st.is_running_task, "失败收尾也必须释放任务位");
        assert!(!e.is_running());
    }

    #[test]
    fn connect_and_task_are_mutually_exclusive() {
        // 握手期间发起读卡会被拒。两者共用一个 runner，不互斥就会把
        // 对方的进程覆盖掉。
        let (_d, p) = paths("conn_mutex");
        let mut e = Engine::new(Box::new(MockToolRunner::new(vec![Chunk::stdout(
            "NFC device: NFC_Device opened\n",
        )])));
        let mut st = AppState::new();
        let mut dlg = MockDialog::new();
        e.dispatch(
            Action::ConnUsb("/dev/ttyACM0".into()),
            &mut st,
            &p,
            &mut dlg,
        )
        .unwrap();
        assert_eq!(
            e.dispatch(Action::Plain("read-IC"), &mut st, &p, &mut dlg),
            Err(ToolError::NotConnected)
        );
        assert_eq!(
            e.dispatch(
                Action::ConnUsb("/dev/ttyACM1".into()),
                &mut st,
                &p,
                &mut dlg
            ),
            Err(ToolError::Busy),
            "再点一次连接应被拒（握手进行中）"
        );
        run_to_end(&mut e, &mut st, &p, 8);
        assert!(!e.is_running());
    }

    #[test]
    fn read_ic_builds_mfoc_args_with_output_and_keys_file() {
        let (_d, p) = paths("args");
        let mut e = Engine::new(Box::new(MockToolRunner::new(vec![])));
        let mut st = connected();
        let mut dlg = MockDialog::new();
        e.dispatch(Action::Plain("read-IC"), &mut st, &p, &mut dlg)
            .unwrap();
        let (tool, args, _) = e.runner().started_last().unwrap();
        // S12-2b：读卡 spawn 的是 `mfoc`（原版 command.js:104/159），
        // 不是 `nfc-mfclassic`——后者是位置参数风格，吃不下 -O/-f。
        assert_eq!(tool, "mfoc");
        // mfoc_args：-O<temp> + -f<keys>（command.js:608-611）
        assert!(args.contains(&format!("-O{}", p.temp_mfd().display())));
        assert!(args.contains(&format!("-f{}", p.keys_file().display())));
    }

    #[test]
    fn done_input_keys_converts_comma_list_to_dash_k() {
        // command.js:154-156：keys.match(/[0-9A-Fa-f]{12}/g) → 每个 -k<key>
        let (_d, p) = paths("inputkeys");
        let mut e = Engine::new(Box::new(MockToolRunner::new(vec![])));
        let mut st = connected();
        let mut dlg = MockDialog::new();
        e.dispatch(
            Action::WithArg(
                "done-input-keys-read-IC",
                "a1b2c3d4e5f6, ffffffff0000".into(),
            ),
            &mut st,
            &p,
            &mut dlg,
        )
        .unwrap();
        let (_, args, _) = e.runner().started_last().unwrap();
        assert!(args.contains(&"-ka1b2c3d4e5f6".to_owned()), "{args:?}");
        assert!(args.contains(&"-kffffffff0000".to_owned()), "{args:?}");
    }

    #[test]
    fn done_input_keys_with_no_hex_spawns_nothing() {
        // 原版 `keys.match(...)` 返回 null 再 `.forEach` 抛 TypeError；
        // 此处等价于「什么都不做」，不 spawn。
        let (_d, p) = paths("nohex");
        let mut e = Engine::new(Box::new(MockToolRunner::new(vec![])));
        let mut st = connected();
        let mut dlg = MockDialog::new();
        e.dispatch(
            Action::WithArg("done-input-keys-read-IC", "没有任何密钥".into()),
            &mut st,
            &p,
            &mut dlg,
        )
        .unwrap();
        assert!(!e.is_running(), "无密钥竟启动了工具");
        assert!(!st.is_running_task);
    }

    #[test]
    fn detect_card_type_uses_n_without_output_file() {
        // command.js:169-171：detect-card-type 用 -N -f<keys>，不带 -O
        let (_d, p) = paths("detect");
        let mut e = Engine::new(Box::new(MockToolRunner::new(vec![])));
        let mut st = connected();
        let mut dlg = MockDialog::new();
        e.dispatch(Action::Plain("detect-card-type"), &mut st, &p, &mut dlg)
            .unwrap();
        let (tool, args, _) = e.runner().started_last().unwrap();
        assert_eq!(tool, "nfc-mfdetect");
        assert!(args.contains(&"-N".to_owned()), "{args:?}");
        assert!(
            !args.iter().any(|a| a.starts_with("-O")),
            "detect-card-type 不该带 -O: {args:?}"
        );
    }

    #[test]
    fn dict_test_builds_mfdict_args() {
        let (_d, p) = paths("dict");
        let mut e = Engine::new(Box::new(MockToolRunner::new(vec![])));
        let mut st = connected();
        st.dict_path = std::path::PathBuf::from("/tmp/my.dic");
        let mut dlg = MockDialog::new();
        let cfg = DictTestConfig {
            sector: 5,
            key_type: crate::state::KeyType::B,
            start_position: 100,
        };
        e.dispatch(Action::DictTestConfig(Box::new(cfg)), &mut st, &p, &mut dlg)
            .unwrap();
        let (tool, args, _) = e.runner().started_last().unwrap();
        assert_eq!(tool, "nfc-mfdict");
        for want in ["-s5", "-tB", "-l100", "-d/tmp/my.dic"] {
            assert!(args.contains(&want.to_owned()), "缺 {want}: {args:?}");
        }
    }

    /// P1-1 字典命中：`Found Key: ` → 密钥落盘 + 从 unknown 移除对应项。
    /// 修复前 dict 未挂解析器，命中既不落盘也不收 unknown，破解形同虚设。
    #[test]
    fn dict_hit_saves_key_and_removes_unknown() {
        let (_d, p) = paths("dict-hit");
        let mut e = Engine::new(Box::new(
            MockToolRunner::new(vec![Chunk::stdout("Found Key: deadbeef1234\n")])
                .with_exit(Some(0)),
        ));
        let mut st = connected();
        // 预置一条 unknown：扇区 5 / KeyB（与配置目标一致）
        st.unknown_key_info = vec![(5, crate::state::KeyType::B)];
        let mut dlg = MockDialog::new();
        let cfg = DictTestConfig {
            sector: 5,
            key_type: crate::state::KeyType::B,
            start_position: 0,
        };
        e.dispatch(Action::DictTestConfig(Box::new(cfg)), &mut st, &p, &mut dlg)
            .unwrap();
        run_to_end(&mut e, &mut st, &p, 6);
        // 命中密钥应落盘（刻意避开 DEFAULT_KEYS，否则被 save_keys 正确过滤）
        let keys = std::fs::read_to_string(p.keys_file()).unwrap_or_default();
        assert!(keys.contains("deadbeef1234"), "命中密钥未落盘: {keys:?}");
        // 对应 unknown 应被移除
        assert!(
            !st.unknown_key_info.contains(&(5, crate::state::KeyType::B)),
            "命中后 unknown 仍在: {:?}",
            st.unknown_key_info
        );
    }

    /// P3-8 PLAN v1：读卡读到卡号后自动追加进 `registry.json`（可查表）。
    #[test]
    fn read_ic_appends_uid_to_registry() {
        let (_d, p) = paths("reg-append");
        let mut e = Engine::new(Box::new(
            MockToolRunner::new(vec![Chunk::stdout("UID (NFCID1): AA BB CC DD\n")])
                .with_exit(Some(0)),
        ));
        let mut st = connected();
        let mut dlg = MockDialog::new();
        e.dispatch(Action::Plain("read-IC"), &mut st, &p, &mut dlg)
            .unwrap();
        run_to_end(&mut e, &mut st, &p, 6);
        let reg = crate::data::Registry::load(&p.registry_file()).unwrap();
        assert_eq!(reg.len(), 1, "registry 应追加 1 条，实际 {}", reg.len());
        assert!(
            reg.find("AABBCCDD").is_some(),
            "追加的 UID 不对: {:?}",
            reg.uids()
        );
    }

    // ── 输出回流与收尾 ──

    #[test]
    fn pump_appends_tool_output_to_log() {
        let (_d, p) = paths("pump");
        let mut e = Engine::new(Box::new(MockToolRunner::new(vec![Chunk::stdout(
            "MIFARE 命令 1/4\n",
        )])));
        let mut st = connected();
        let mut dlg = MockDialog::new();
        e.dispatch(Action::Plain("read-IC"), &mut st, &p, &mut dlg)
            .unwrap();
        run_to_end(&mut e, &mut st, &p, 3);
        assert!(
            st.log_buffer.contains("MIFARE 命令 1/4"),
            "输出未进日志: {:?}",
            st.log_buffer
        );
    }

    #[test]
    fn successful_exit_clears_running_flag_and_status() {
        let (_d, p) = paths("ok");
        let mut e = Engine::new(Box::new(
            MockToolRunner::new(vec![Chunk::stdout("done\n")]).with_exit(Some(0)),
        ));
        let mut st = connected();
        let mut dlg = MockDialog::new();
        e.dispatch(Action::Plain("read-IC"), &mut st, &p, &mut dlg)
            .unwrap();
        assert!(st.is_running_task);
        run_to_end(&mut e, &mut st, &p, 3);
        assert!(!st.is_running_task, "退出后仍在跑");
        assert!(!e.is_running());
        assert_eq!(st.status.indicator, Indicator::Free);
        assert!(st.progress.is_none(), "退出后进度条未清");
    }

    #[test]
    fn failed_exit_sets_error_indicator() {
        let (_d, p) = paths("fail");
        let mut e = Engine::new(Box::new(
            MockToolRunner::new(vec![Chunk::stderr_chunk("boom\n")]).with_exit(Some(1)),
        ));
        let mut st = connected();
        let mut dlg = MockDialog::new();
        e.dispatch(Action::Plain("read-IC"), &mut st, &p, &mut dlg)
            .unwrap();
        run_to_end(&mut e, &mut st, &p, 3);
        assert!(!st.is_running_task);
        assert_eq!(st.status.indicator, Indicator::Error);
        assert!(
            st.events.iter().any(|ev| ev.kind == EventKind::Error),
            "未追加 error 事件: {:?}",
            st.events
        );
    }

    #[test]
    fn cancel_kills_running_task_and_resets_state() {
        // R3：cancel 清计时 + 隐藏进度
        let (_d, p) = paths("cancel");
        let mut e = Engine::new(Box::new(MockToolRunner::new(vec![])));
        let mut st = connected();
        let mut dlg = MockDialog::new();
        e.dispatch(Action::Plain("read-IC"), &mut st, &p, &mut dlg)
            .unwrap();
        st.progress = Some(42.0);
        e.dispatch(Action::Plain("cancel-task"), &mut st, &p, &mut dlg)
            .unwrap();
        assert!(!st.is_running_task);
        assert!(!e.is_running());
        assert!(st.progress.is_none(), "cancel 后进度条未清");
        assert_eq!(st.status.indicator, Indicator::Free);
    }

    // ── 未接线动作要显式报错 ──

    #[test]
    fn unwired_action_reports_not_wired_and_logs() {
        // 「点了没反应」必须变成可见错误，而不是静默返回 Ok。
        // `run-hard-nested` 不是独立的 action：它的多轮状态机（collect→crack）
        // 由 `hard-nested-config-done` 驱动，直接按名字派发须显式报 NotWired
        // （S11-5 的回归钉点，见 verify_parity「未接线动作仍显式报错」）。
        let (_d, p) = paths("unwired");
        let mut e = Engine::new(Box::new(MockToolRunner::new(vec![])));
        let mut st = connected();
        let mut dlg = MockDialog::new();
        let r = e.dispatch(Action::Plain("run-hard-nested"), &mut st, &p, &mut dlg);
        assert_eq!(r, Err(ToolError::NotWired("run-hard-nested")));
        assert!(st.log_buffer.contains("run-hard-nested"), "未记日志");
        assert!(!e.is_running());
    }

    // ── 读卡收尾落盘（读卡 → 写这张卡主流程）───────────────────────

    /// 读卡成功后必须把 temp.mfd 移进 `dumpfiles/` 并发引导。
    ///
    /// 这是「读卡 → 引导 → 写这张卡」这条主流程的必经环节。
    /// S11 前两步都漏了它：`mfoc::dump_save` 没有任何生产调用者，
    /// 于是 `Event::DumpSaved` 永不发出、引导条永远不出现。
    #[test]
    fn read_ic_saves_dump_and_emits_guidance() {
        let (_d, p) = paths("read-save");
        let mut e = Engine::new(Box::new(MockToolRunner::new(vec![Chunk::stdout(
            "UID (NFCID1): a1b2c3d4\n",
        )])));
        let mut st = connected();
        let mut dlg = MockDialog::new();
        e.dispatch(Action::Plain("read-IC"), &mut st, &p, &mut dlg)
            .unwrap();
        // 模拟 mfoc 写出了 temp.mfd
        std::fs::write(p.temp_mfd(), vec![0x5a; 64]).unwrap();
        // S11-7：pump 完只会**挂起**保存请求，app 弹框后才真正落盘
        run_to_end(&mut e, &mut st, &p, 4);
        assert!(e.pending_save().is_some(), "收尾应挂起一个保存请求");
        e.resolve_save(None, &mut st, &p);
        // temp 应已被移走
        assert!(!p.temp_mfd().exists(), "temp.mfd 未被移走");
        // dumpfiles/ 下应出现以 UID 命名的文件
        let files = crate::dumpfiles::list(&p.dumpfiles_dir());
        assert_eq!(files.len(), 1, "dumpfiles 应有 1 个文件: {files:?}");
        assert!(
            files[0].starts_with("a1b2c3d4_"),
            "文件名应以 UID 开头: {}",
            files[0]
        );
        assert!(files[0].ends_with(".mfd"), "扩展名错: {}", files[0]);
        // 引导应已发出（app 侧据此显示「写这张卡」按钮）
        assert!(
            st.events.iter().any(|e| e.kind == EventKind::Success),
            "未发成功事件，dump-saved 引导不会出现: {:?}",
            st.events
        );
        // 历史列表应刷新
        assert_eq!(st.dump_history, files);
    }

    /// dump 为空时不该产出文件（原版 command.js:650-661）。
    #[test]
    fn read_ic_with_empty_dump_saves_nothing() {
        let (_d, p) = paths("read-empty");
        let mut e = Engine::new(Box::new(MockToolRunner::new(vec![]).with_exit(Some(0))));
        let mut st = connected();
        let mut dlg = MockDialog::new();
        e.dispatch(Action::Plain("read-IC"), &mut st, &p, &mut dlg)
            .unwrap();
        // 故意不造 temp.mfd
        run_to_end(&mut e, &mut st, &p, 3);
        assert!(
            crate::dumpfiles::list(&p.dumpfiles_dir()).is_empty(),
            "空 dump 不该落盘"
        );
    }

    /// **防 A3 回归的核心用例**：autoRun 必须能跑满多轮。
    ///
    /// 曾有一个致命 bug：`after_crack_guarded` 的第三参误用了 pop **之前**的
    /// 快照长度，它与 `unknown_before` 恒相等，于是「无进展」判据永远成立，
    /// autoRun 每轮都在第 1 轮后停住——单轮测试全绿，多轮根本跑不起来。
    #[test]
    fn hard_nested_autorun_advances_multiple_rounds() {
        use crate::model::keys::KeyType as KT;
        use crate::runner::Chunk;
        // 探测输出用 nfc-mfdetect 的真实表格格式。扇区号由**命中次序**推出
        // （`KeyInfoParser::next_sector`，对应原版 `parseInt(i/2)`），
        // 故每行对应一个扇区。
        // 真实模型：mfdetect 对**每个扇区**给一行，破掉的扇区从
        // `Unknown` 变成 `Found`。行格式照抄 nfc-mfdetect.c:451-461 的
        // `fprintf(stdout, "Sector %02d - Found   Key A: %012llx ", ...)`，
        // 扇区号取自 `Sector NN -` 行头（KeyInfoParser 同原版 keyParser.js）。
        //   轮 1：扇区 0 已知，1 / 2 未知
        //   轮 2：扇区 1 已破 → Found，2 仍未知
        //   轮 3：扇区 2 也破了 → 全 Found → unknown 空 → 收工
        const K1: &str = "aaaabbbbcccc"; // 刻意避开 DEFAULT_KEYS
        let detect_r1 = || {
            Chunk::stdout(
                "Sector 00 - Found   Key A: ffffffffffff Found   Key B: ffffffffffff \n\
                 Sector 01 - Unknown Key A               Unknown Key B\n",
            )
        };
        let detect_r2 = || {
            Chunk::stdout(format!(
                "Sector 00 - Found   Key A: ffffffffffff Found   Key B: ffffffffffff \n\
                 Sector 01 - Found   Key A: {K1} Found   Key B: {K1} \n"
            ))
        };
        // 行格式照抄原版 keyParser.test.js:163-164：
        // `collecting nonces for key A of block 7 (sector 1) using known key A … (sector 0)`
        // 引擎靠这里的 `(sector N)` 认出本轮打的是哪个扇区，故必须与请求的一致。
        let collect_out = |sector: u8| {
            let block = sector * 4 + 3;
            Chunk::stdout(format!(
                "Found tag with uid 01020304050607\n\
                 collecting nonces for key A of block {block} (sector {sector}) using known key A ffffffffffff for block 0 (sector 0)\n"
            ))
        };
        let script = vec![
            vec![detect_r1()],
            vec![collect_out(1)], // 队首 unknown[0] = 扇区 1
            vec![Chunk::stdout("Key found: aaaabbbbcccc\n")],
            vec![detect_r2()],
            vec![collect_out(1)], // 队首 unknown[0] = 扇区 1
            vec![Chunk::stdout("Key found: ddddeeeeffff\n")],
            // unknown 已空 → hn_advance_round 直接收工，第 3 段 detect 不会被消费
        ];
        let mut e = Engine::new(Box::new(MockToolRunner::new(vec![]).with_scripts(script)));
        let mut st = hn_state(&[(0, KT::A)]);
        let mut dlg = MockDialog::new();
        let (_d, p) = paths("hn-multi");
        e.dispatch(
            Action::HardNestedConfig(Box::new(hn_cfg(true, false))),
            &mut st,
            &p,
            &mut dlg,
        )
        .unwrap();
        for _ in 0..40 {
            e.pump(&mut st, &p);
        }
        // 命中密钥应落盘（刻意避开 DEFAULT_KEYS，否则会被正确过滤掉）
        let keys = std::fs::read_to_string(p.keys_file()).unwrap_or_default();
        assert!(keys.contains("aaaabbbbcccc"), "轮 1 密钥未落盘: {keys:?}");
        // unknown 应被清空
        assert!(
            st.unknown_key_info.is_empty(),
            "仍有未知扇区: {:?}",
            st.unknown_key_info
        );
        // 跑满 1 轮（扇区 1 破完即全知）。靠日志计数：每次 launch 都记一行 `{tool} {args}`，
        // 这是用户实际看到的东西，不依赖任何只为测试存在的 API。
        let count = |needle: &str| st.log_buffer.matches(needle).count();
        assert_eq!(count("libnfc-collect"), 1, "应采集 1 轮");
        assert_eq!(count("cropto1_bs"), 1, "应破解 1 轮");
        // 只探测 2 次：第 2 轮破解后 unknown 清空，`hn_advance_round` 直接收工，
        // 不会再浪费一次探测（脚本里的第 3 段 detect 不会被消费）。
        assert_eq!(
            count("nfc-mfdetect"),
            2,
            "应探测 2 次（开局 + 每轮开头各一次）"
        );
        assert!(
            st.log_buffer.contains("已尝试解密全部未知密钥"),
            "收工原因不对: {:?}",
            st.log_buffer
        );
        assert!(!e.is_running(), "收工后仍在跑");
    }

    // ── 密钥列表回填（检测链路的前置环节）──────────────────────────

    /// `after_detect` 必须把解析出的 known / unknown 回填进 `AppState`。
    ///
    /// 这曾是**整条破解链路断掉的根因**：只落盘密钥、不回填列表，于是
    /// `detect-card-type` 之后 `unknown_key_info` 恒空 → 未知密钥引导永不出现、
    /// hard-nested 面板永远「无检测结果」、autoRun 必走 `StopNoKnownKey`。
    #[test]
    fn after_detect_backfills_key_lists() {
        let (_d, p) = paths("backfill");
        let mut e = Engine::new(Box::new(
            // 照抄 nfc-mfdetect.c:451-461：A 与 B 印在**同一行**（B 前只有空格），
            // 扇区号取自 `Sector NN - ` 行头。
            MockToolRunner::new(vec![Chunk::stdout(concat!(
                "Sector 00 - Found   Key A: ffffffffffff Found   Key B: ffffffffffff\n",
                "Sector 01 - Unknown Key A               Unknown Key B\n",
            ))])
            .with_exit(Some(0)),
        ));
        let mut st = connected();
        let mut dlg = MockDialog::new();
        e.dispatch(Action::Plain("detect-card-type"), &mut st, &p, &mut dlg)
            .unwrap();
        run_to_end(&mut e, &mut st, &p, 3);
        assert_eq!(
            st.known_key_info,
            vec![
                ("ffffffffffff".to_owned(), 0, crate::model::keys::KeyType::A),
                ("ffffffffffff".to_owned(), 0, crate::model::keys::KeyType::B),
            ],
            "已知密钥未回填"
        );
        assert_eq!(
            st.unknown_key_info,
            vec![
                (1, crate::model::keys::KeyType::A),
                (1, crate::model::keys::KeyType::B)
            ],
            "未知密钥未回填"
        );
    }

    /// collectOnly：采到的 nonce 必须真的被保存（`command.js:310-332`）。
    ///
    /// 曾是 `Step::SaveNonces` 直接置空闲 —— 采到的 nonce 被**静默丢弃**。
    #[test]
    fn hard_nested_collect_only_saves_nonces() {
        use crate::model::keys::KeyType as KT;
        let (_d, p) = paths("hn-save");
        let target = p.base().join("out.bin");
        let mut e = Engine::new(Box::new(MockToolRunner::new(vec![]).with_exit(Some(0))));
        let mut st = hn_state(&[(1, KT::A)]);
        // 预设保存框的答案
        let mut dlg = MockDialog::new();
        e.dispatch(
            Action::HardNestedConfig(Box::new(hn_cfg(false, true))),
            &mut st,
            &p,
            &mut dlg,
        )
        .unwrap();
        // 采集工具会写 nonces.bin
        std::fs::write(p.nonces_file(), vec![0xab; 32]).unwrap();
        // S11-7：挂起 → app 弹框回传路径 → 续接 rename
        let ok = settle_pending_save(&mut e, &mut st, &p, Some(target.clone()));
        assert!(ok, "collectOnly 应挂起一个保存请求并被续接");
        assert_eq!(
            dlg.save_calls(),
            0,
            "engine 侧不应自己弹框（那是 app 的活）"
        );
        assert!(target.is_file(), "nonce 未保存到目标路径");
        assert!(!p.nonces_file().exists(), "nonce 未从 nonces.bin 搬走");
    }

    /// collectOnly 且 nonce 为空 → 不弹框、不保存（`command.js:310` 的 size 检查）。
    #[test]
    fn hard_nested_collect_only_skips_empty_nonces() {
        use crate::model::keys::KeyType as KT;
        let (_d, p) = paths("hn-save-empty");
        let mut e = Engine::new(Box::new(MockToolRunner::new(vec![]).with_exit(Some(0))));
        let mut st = hn_state(&[(1, KT::A)]);
        let mut dlg = MockDialog::new().with_save(Some(p.base().join("x.bin")));
        e.dispatch(
            Action::HardNestedConfig(Box::new(hn_cfg(false, true))),
            &mut st,
            &p,
            &mut dlg,
        )
        .unwrap();
        // 故意不造 nonces.bin
        run_to_end(&mut e, &mut st, &p, 6);
        assert!(e.pending_save().is_none(), "nonce 为空不该挂起保存请求");
        assert_eq!(dlg.save_calls(), 0, "nonce 为空不该弹保存框");
        assert!(
            st.log_buffer.contains("未采集到 nonce"),
            "{:?}",
            st.log_buffer
        );
    }

    // ── S11-7 挂起式保存框 ─────────────────────────────────────────

    /// 挂起期间 `is_running()` 必须为真，否则第二个任务能插进来覆盖 temp.mfd，
    /// 用户在上一个保存框里选好的路径就指到别人的数据上。
    #[test]
    fn pending_save_counts_as_running() {
        let (_d, p) = paths("save-busy");
        let mut e = Engine::new(Box::new(MockToolRunner::new(vec![Chunk::stdout(
            "UID (NFCID1): aabb\n",
        )])));
        let mut st = connected();
        let mut dlg = MockDialog::new();
        e.dispatch(Action::Plain("read-IC"), &mut st, &p, &mut dlg)
            .unwrap();
        std::fs::write(p.temp_mfd(), vec![0x5a; 64]).unwrap();
        run_to_end(&mut e, &mut st, &p, 4);
        assert!(e.pending_save().is_some());
        assert!(e.is_running(), "挂起的保存框期间应算作在忙");
        // 忙 → 第二个任务被拒
        let r = e.dispatch(Action::Plain("detect-card-type"), &mut st, &p, &mut dlg);
        assert_eq!(r, Err(ToolError::Busy));
        // 续接后释放
        e.resolve_save(None, &mut st, &p);
        assert!(!e.is_running(), "续接后应释放任务位");
    }

    /// 两条路径的「取消」语义**不同**，别混：
    /// - read-IC 收尾：取消也用默认名存（`command.js:667` 的 `url || defaultName`）
    /// - collectOnly：取消则保留 nonces.bin（`command.js:310-320`）
    #[test]
    fn cancel_semantics_differ_between_the_two_save_points() {
        // ── read-IC：取消仍落盘到默认名 ──
        let (_d, p) = paths("save-cancel-dump");
        let mut e = Engine::new(Box::new(MockToolRunner::new(vec![Chunk::stdout(
            "UID (NFCID1): ccdd\n",
        )])));
        let mut st = connected();
        let mut dlg = MockDialog::new();
        e.dispatch(Action::Plain("read-IC"), &mut st, &p, &mut dlg)
            .unwrap();
        std::fs::write(p.temp_mfd(), vec![0x5a; 64]).unwrap();
        run_to_end(&mut e, &mut st, &p, 4);
        let default_path = e.pending_save().expect("应挂起").default_path.clone();
        e.resolve_save(None, &mut st, &p); // 用户取消
        assert!(
            default_path.is_file(),
            "read-IC 收尾取消也应存到默认名: {}",
            default_path.display()
        );
        drop(dlg);

        // ── collectOnly：取消则保留 nonces.bin ──
        use crate::model::keys::KeyType as KT;
        let (_d2, p2) = paths("save-cancel-nonces");
        let mut e2 = Engine::new(Box::new(MockToolRunner::new(vec![]).with_exit(Some(0))));
        let mut st2 = hn_state(&[(1, KT::A)]);
        let mut dlg2 = MockDialog::new();
        e2.dispatch(
            Action::HardNestedConfig(Box::new(hn_cfg(false, true))),
            &mut st2,
            &p2,
            &mut dlg2,
        )
        .unwrap();
        std::fs::write(p2.nonces_file(), vec![0xab; 32]).unwrap();
        settle_pending_save(&mut e2, &mut st2, &p2, None); // 用户取消
        assert!(
            p2.nonces_file().is_file(),
            "collectOnly 取消应保留 nonces.bin（别丢用户采集的数据）"
        );
    }

    /// 没有挂起请求时 `resolve_save` 必须返回 false 而不是崩。
    ///
    /// app 侧「每帧 poll 一次」会重复调它；误调不能变成 panic 或误删文件。
    #[test]
    fn resolve_save_without_pending_is_a_noop() {
        let (_d, p) = paths("save-noop");
        let mut e = Engine::new(Box::new(MockToolRunner::new(vec![])));
        let mut st = AppState::new();
        assert!(!e.resolve_save(Some(p.base().join("x.bin")), &mut st, &p));
        assert!(e.pending_save().is_none());
        assert!(!e.is_running());
    }

    /// dump 为空 → 压根不挂起请求（否则 app 会弹一个无意义的框）。
    #[test]
    fn empty_dump_parks_no_save_request() {
        let (_d, p) = paths("save-empty");
        let mut e = Engine::new(Box::new(MockToolRunner::new(vec![Chunk::stdout(
            "UID (NFCID1): eeff\n",
        )])));
        let mut st = connected();
        let mut dlg = MockDialog::new();
        e.dispatch(Action::Plain("read-IC"), &mut st, &p, &mut dlg)
            .unwrap();
        // 故意不写 temp.mfd
        run_to_end(&mut e, &mut st, &p, 4);
        assert!(e.pending_save().is_none(), "空 dump 不该挂起保存请求");
        assert!(!e.is_running());
    }

    // ── HardNested 多轮闭环（command.js:248-363）────────────────────

    /// 造一个「已连接、已知密钥 1 把、未知密钥 n 把」的状态。
    fn hn_state(unknown: &[(u8, crate::model::keys::KeyType)]) -> AppState {
        let mut st = connected();
        st.known_key_info = vec![("ffffffffffff".to_owned(), 0, crate::model::keys::KeyType::A)];
        st.unknown_key_info = unknown.to_vec();
        st
    }

    fn hn_cfg(auto: bool, collect_only: bool) -> HardNestedConfig {
        HardNestedConfig {
            known_key: "ffffffffffff".to_owned(),
            known_sector: 0,
            known_key_type: crate::model::keys::KeyType::A,
            target_sector: 1,
            target_key_type: crate::model::keys::KeyType::A,
            collect_only,
            auto_run: auto,
        }
    }

    /// autoRun 第一步是 `nfc-mfdetect`（不是直接采集），对标 command.js:251。
    #[test]
    fn hard_nested_auto_starts_with_detect() {
        let (_d, p) = paths("hn-detect");
        let mut e = Engine::new(Box::new(MockToolRunner::new(vec![])));
        let mut st = hn_state(&[(1, crate::model::keys::KeyType::A)]);
        let mut dlg = MockDialog::new();
        e.dispatch(
            Action::HardNestedConfig(Box::new(hn_cfg(true, false))),
            &mut st,
            &p,
            &mut dlg,
        )
        .unwrap();
        let (tool, _, _) = e.runner().started_last().unwrap();
        assert_eq!(tool, "nfc-mfdetect", "autoRun 首步应是探测");
    }

    /// 非 autoRun 直接进采集（command.js:275）。
    #[test]
    fn hard_nested_manual_starts_with_collect() {
        let (_d, p) = paths("hn-collect");
        let mut e = Engine::new(Box::new(MockToolRunner::new(vec![])));
        let mut st = hn_state(&[(1, crate::model::keys::KeyType::A)]);
        let mut dlg = MockDialog::new();
        e.dispatch(
            Action::HardNestedConfig(Box::new(hn_cfg(false, false))),
            &mut st,
            &p,
            &mut dlg,
        )
        .unwrap();
        let (tool, args, _) = e.runner().started_last().unwrap();
        assert_eq!(tool, "libnfc-collect");
        // 7 个位置参数，扇区已换算成 trailer 块号 4*s+3
        assert_eq!(args.len(), 7, "{args:?}");
        assert_eq!(args[1], "3", "扇区 0 → trailer 块 3");
        assert_eq!(args[3], "7", "扇区 1 → trailer 块 7");
        assert_eq!(args[5], "bin");
    }

    /// 探测后 known 为空 → 停（command.js:256-262）。
    #[test]
    fn hard_nested_stops_when_no_known_key() {
        let (_d, p) = paths("hn-noknown");
        let mut e = Engine::new(Box::new(MockToolRunner::new(vec![]).with_exit(Some(0))));
        let mut st = hn_state(&[(1, crate::model::keys::KeyType::A)]);
        // 探测后没有已知密钥
        st.known_key_info.clear();
        st.unknown_key_info = vec![];
        let mut dlg = MockDialog::new();
        e.dispatch(
            Action::HardNestedConfig(Box::new(hn_cfg(true, false))),
            &mut st,
            &p,
            &mut dlg,
        )
        .unwrap();
        run_to_end(&mut e, &mut st, &p, 3);
        assert!(!e.is_running(), "无已知密钥却继续跑了");
        assert!(
            st.log_buffer.contains("未发现已知密钥"),
            "{:?}",
            st.log_buffer
        );
    }

    /// 采集后接破解（collectOnly=false），command.js:334-339。
    #[test]
    fn hard_nested_collect_advances_to_crack() {
        let (_d, p) = paths("hn-crack");
        let mut e = Engine::new(Box::new(MockToolRunner::new(vec![]).with_exit(Some(0))));
        let mut st = hn_state(&[(1, crate::model::keys::KeyType::A)]);
        let mut dlg = MockDialog::new();
        e.dispatch(
            Action::HardNestedConfig(Box::new(hn_cfg(false, false))),
            &mut st,
            &p,
            &mut dlg,
        )
        .unwrap();
        run_to_end(&mut e, &mut st, &p, 3);
        let (tool, args, _) = e.runner().started_last().unwrap();
        assert_eq!(tool, "cropto1_bs", "采集后应接破解");
        assert_eq!(args.len(), 1, "cropto1_bs 只收 nonces 路径");
    }

    /// collectOnly → 采集后不破解，收工（command.js:310-332）。
    #[test]
    fn hard_nested_collect_only_does_not_crack() {
        let (_d, p) = paths("hn-collectonly");
        let mut e = Engine::new(Box::new(MockToolRunner::new(vec![]).with_exit(Some(0))));
        let mut st = hn_state(&[(1, crate::model::keys::KeyType::A)]);
        let mut dlg = MockDialog::new();
        e.dispatch(
            Action::HardNestedConfig(Box::new(hn_cfg(false, true))),
            &mut st,
            &p,
            &mut dlg,
        )
        .unwrap();
        run_to_end(&mut e, &mut st, &p, 3);
        let (tool, _, _) = e.runner().started_last().unwrap();
        assert_eq!(tool, "libnfc-collect", "collectOnly 不该跑 cropto1_bs");
        assert!(!e.is_running());
    }

    /// 破解命中 → 存密钥 + 在 unknown 队列移除对应项（S18 F1：任意位置）。
    #[test]
    fn hard_nested_crack_saves_key_and_pops_unknown() {
        let (_d, p) = paths("hn-hit");
        let mut e = Engine::new(Box::new(
            MockToolRunner::new(vec![Chunk::stdout("Key found: deadbeef1234\n")])
                .with_exit(Some(0)),
        ));
        let mut st = hn_state(&[(1, crate::model::keys::KeyType::A)]);
        let mut dlg = MockDialog::new();
        e.dispatch(
            Action::HardNestedConfig(Box::new(hn_cfg(false, false))),
            &mut st,
            &p,
            &mut dlg,
        )
        .unwrap();
        // 采集段输出：让 hn_round 记下扇区 1 / 类型 A
        run_to_end(&mut e, &mut st, &p, 3);
        // 现在是 cropto1_bs；再泵一次让它的输出被解析
        for _ in 0..3 {
            e.pump(&mut st, &p);
        }
        // 密钥应已落盘
        let keys = std::fs::read_to_string(p.keys_file()).unwrap_or_default();
        assert!(keys.contains("deadbeef1234"), "命中的密钥未落盘: {keys:?}");
    }

    /// 工具非 0 退出 → 不推进状态机（原版 exec 会 reject）。
    #[test]
    fn hard_nested_nonzero_exit_does_not_advance() {
        let (_d, p) = paths("hn-fail");
        let mut e = Engine::new(Box::new(MockToolRunner::new(vec![]).with_exit(Some(1))));
        let mut st = hn_state(&[(1, crate::model::keys::KeyType::A)]);
        let mut dlg = MockDialog::new();
        e.dispatch(
            Action::HardNestedConfig(Box::new(hn_cfg(false, false))),
            &mut st,
            &p,
            &mut dlg,
        )
        .unwrap();
        run_to_end(&mut e, &mut st, &p, 3);
        // 采集失败就不该启动 cropto1_bs
        let (tool, _, _) = e.runner().started_last().unwrap();
        assert_eq!(tool, "libnfc-collect", "采集失败却接上了破解");
        assert!(!e.is_running());
        assert_eq!(st.status.indicator, Indicator::Error);
    }

    // ── 写卡链路（S11-2）──────────────────────────────────────────

    /// 写卡的前置顺序：文件已由 app 预选 → 只剩确认 → 确认后才启动备份段。
    #[test]
    fn write_ic_without_confirm_launches_nothing() {
        let (_d, p) = paths("write-no-confirm");
        std::fs::write(p.dumpfiles_dir().join("a.mfd"), vec![0x5a; 32]).unwrap();
        let mut e = Engine::new(Box::new(MockToolRunner::new(vec![])));
        let mut st = connected();
        let src = p.dumpfiles_dir().join("a.mfd").display().to_string();
        // 空确认队列 → 安全默认值 false
        let mut dlg = MockDialog::new();
        e.dispatch(Action::WithArg("write-IC", src), &mut st, &p, &mut dlg)
            .unwrap();
        assert!(!e.is_running(), "未确认竟启动了写卡");
        assert!(!st.is_running_task);
        assert_eq!(dlg.confirm_calls(), 1);
    }

    /// 带 dumpId 时不弹文件框（app 已预选），确认通过后启动**备份段**。
    #[test]
    fn write_ic_confirmed_starts_backup_stage() {
        let (_d, p) = paths("write-ok");
        std::fs::write(p.dumpfiles_dir().join("a.mfd"), vec![0x5a; 32]).unwrap();
        let mut e = Engine::new(Box::new(MockToolRunner::new(vec![])));
        let mut st = connected();
        let mut dlg = MockDialog::new().with_confirm(true);
        e.dispatch(
            Action::WithArg("write-IC", "a.mfd".into()),
            &mut st,
            &p,
            &mut dlg,
        )
        .unwrap();
        assert_eq!(dlg.open_calls(), 0, "带 dumpId 不该弹文件框");
        let (tool, args, _) = e.runner().started_last().unwrap();
        assert_eq!(tool, "nfc-mfdetect", "第一段应是备份");
        assert!(args.contains(&format!("-O{}", p.temp_mfd().display())));
    }

    /// 备份成功后接第二段：`nfc-mfclassic w A u <src> <temp> f`。
    #[test]
    fn write_ic_second_stage_uses_confirmed_source() {
        let (_d, p) = paths("write-stage2");
        let src = p.dumpfiles_dir().join("a.mfd");
        std::fs::write(&src, vec![0x5a; 32]).unwrap();
        let mut e = Engine::new(Box::new(MockToolRunner::new(vec![])));
        let mut st = connected();
        let mut dlg = MockDialog::new().with_confirm(true);
        e.dispatch(
            Action::WithArg("write-IC", "a.mfd".into()),
            &mut st,
            &p,
            &mut dlg,
        )
        .unwrap();
        // 造出备份，让第二段能接上
        std::fs::write(p.temp_mfd(), vec![0x5a; 64]).unwrap();
        run_to_end(&mut e, &mut st, &p, 3);
        let (tool, args, _) = e.runner().started_last().unwrap();
        assert_eq!(tool, "nfc-mfclassic");
        assert_eq!(
            args,
            vec![
                "w",
                "A",
                "u",
                &src.display().to_string(),
                &p.temp_mfd().display().to_string(),
                "f"
            ]
            .into_iter()
            .map(String::from)
            .collect::<Vec<_>>()
        );
    }

    /// 备份缺失时**绝不**执行写卡（`command.js:603-607` 的安全闸）。
    #[test]
    fn write_ic_backup_failure_blocks_second_stage() {
        let (_d, p) = paths("write-guard");
        std::fs::write(p.dumpfiles_dir().join("a.mfd"), vec![0x5a; 32]).unwrap();
        let mut e = Engine::new(Box::new(MockToolRunner::new(vec![])));
        let mut st = connected();
        let mut dlg = MockDialog::new().with_confirm(true);
        e.dispatch(
            Action::WithArg("write-IC", "a.mfd".into()),
            &mut st,
            &p,
            &mut dlg,
        )
        .unwrap();
        run_to_end(&mut e, &mut st, &p, 3);
        // 工具仍是第一段 —— 第二段没启动
        let (tool, _, _) = e.runner().started_last().unwrap();
        assert_eq!(tool, "nfc-mfdetect", "备份缺失却启动了写卡");
        assert_eq!(st.status.indicator, Indicator::Error);
        assert!(st.log_buffer.contains("读卡失败"));
    }

    /// 备份文件大小为 0 同样视为备份失败（`command.js:597` 删空文件）。
    #[test]
    fn write_ic_empty_backup_blocks_second_stage() {
        let (_d, p) = paths("write-empty-backup");
        std::fs::write(p.dumpfiles_dir().join("a.mfd"), vec![0x5a; 32]).unwrap();
        let mut e = Engine::new(Box::new(MockToolRunner::new(vec![])));
        let mut st = connected();
        let mut dlg = MockDialog::new().with_confirm(true);
        e.dispatch(
            Action::WithArg("write-IC", "a.mfd".into()),
            &mut st,
            &p,
            &mut dlg,
        )
        .unwrap();
        std::fs::write(p.temp_mfd(), b"").unwrap();
        run_to_end(&mut e, &mut st, &p, 3);
        let (tool, _, _) = e.runner().started_last().unwrap();
        assert_eq!(tool, "nfc-mfdetect", "空备份却启动了写卡");
    }

    // ── S17：逐块核对 + 备份去留 ──────────────────────────────────

    /// S17：写卡**全部**成功（63/64，block0 未写）→ 删 temp + 报「写卡成功」。
    #[test]
    fn write_ic_complete_deletes_temp_and_reports_success() {
        let (_d, p) = paths("write-complete");
        std::fs::write(p.dumpfiles_dir().join("a.mfd"), vec![0x5a; 32]).unwrap();
        let mut e = Engine::new(Box::new(
            MockToolRunner::new(vec![])
                .with_scripts(vec![
                    vec![],
                    vec![Chunk::stdout(
                        "Writing 64 blocks |\nDone, 63 of 64 blocks written.\n",
                    )],
                ])
                .with_exits(vec![Some(0), Some(0)]),
        ));
        let mut st = connected();
        let mut dlg = MockDialog::new().with_confirm(true);
        e.dispatch(
            Action::WithArg("write-IC", "a.mfd".into()),
            &mut st,
            &p,
            &mut dlg,
        )
        .unwrap();
        // 造出备份，让第二段能接上
        std::fs::write(p.temp_mfd(), vec![0x5a; 64]).unwrap();
        run_to_end(&mut e, &mut st, &p, 6);
        assert!(
            st.log_buffer.contains("写卡成功"),
            "实际: {}",
            st.log_buffer
        );
        assert!(!p.temp_mfd().exists(), "成功后应删除 temp 备份");
    }

    /// S17：写卡**部分**成功（60/64）→ 保留 temp + 报「写卡失败」+「写入不完整」。
    #[test]
    fn write_ic_partial_write_keeps_temp_and_reports_failure() {
        let (_d, p) = paths("write-partial");
        std::fs::write(p.dumpfiles_dir().join("a.mfd"), vec![0x5a; 32]).unwrap();
        let mut e = Engine::new(Box::new(
            MockToolRunner::new(vec![])
                .with_scripts(vec![
                    vec![],
                    vec![Chunk::stdout("Done, 60 of 64 blocks written.\n")],
                ])
                .with_exits(vec![Some(0), Some(0)]),
        ));
        let mut st = connected();
        let mut dlg = MockDialog::new().with_confirm(true);
        e.dispatch(
            Action::WithArg("write-IC", "a.mfd".into()),
            &mut st,
            &p,
            &mut dlg,
        )
        .unwrap();
        std::fs::write(p.temp_mfd(), vec![0x5a; 64]).unwrap();
        run_to_end(&mut e, &mut st, &p, 6);
        assert!(
            st.log_buffer.contains("写卡失败"),
            "实际: {}",
            st.log_buffer
        );
        assert!(
            st.log_buffer.contains("写入不完整"),
            "实际: {}",
            st.log_buffer
        );
        assert!(p.temp_mfd().exists(), "部分写入应保留备份");
        assert!(
            st.log_buffer.contains("原卡备份已保留"),
            "实际: {}",
            st.log_buffer
        );
    }

    /// S17：写卡工具非 0 退出（无完成行）→ 保留 temp + 报失败。
    #[test]
    fn write_ic_tool_failure_keeps_temp() {
        let (_d, p) = paths("write-fail");
        std::fs::write(p.dumpfiles_dir().join("a.mfd"), vec![0x5a; 32]).unwrap();
        let mut e = Engine::new(Box::new(
            MockToolRunner::new(vec![])
                .with_scripts(vec![
                    vec![],
                    vec![Chunk::stdout("Error: tag was removed\n")],
                ])
                .with_exits(vec![Some(0), Some(1)]),
        ));
        let mut st = connected();
        let mut dlg = MockDialog::new().with_confirm(true);
        e.dispatch(
            Action::WithArg("write-IC", "a.mfd".into()),
            &mut st,
            &p,
            &mut dlg,
        )
        .unwrap();
        std::fs::write(p.temp_mfd(), vec![0x5a; 64]).unwrap();
        run_to_end(&mut e, &mut st, &p, 6);
        assert!(
            st.log_buffer.contains("写卡失败"),
            "实际: {}",
            st.log_buffer
        );
        assert!(p.temp_mfd().exists(), "工具失败应保留备份");
    }

    /// S18 CUID：大写 `W` 解锁写会写 block0，工具打印 `64/64`（含 block0），
    /// 全部成功 = `w == t`。修复前第二段用 `w == t - 1` 判据（按小写 `w` 的
    /// 「跳过 block0」假设），会把 CUID 的 64/64 误判为写入不完整。
    #[test]
    fn cuid_write_full_64_of_64_reports_success() {
        let (_d, p) = paths("cuid-complete");
        std::fs::write(p.dumpfiles_dir().join("a.mfd"), vec![0x5a; 32]).unwrap();
        let mut e = Engine::new(Box::new(
            MockToolRunner::new(vec![])
                .with_scripts(vec![
                    vec![],
                    vec![Chunk::stdout("Done, 64 of 64 blocks written.\n")],
                ])
                .with_exits(vec![Some(0), Some(0)]),
        ));
        let mut st = connected();
        let mut dlg = MockDialog::new().with_confirm(true);
        e.dispatch(
            Action::WithArg("cuid-write", "a.mfd".into()),
            &mut st,
            &p,
            &mut dlg,
        )
        .unwrap();
        // 造出含合法 block0 的备份：CUID 密钥文件要从这里取卡号，第二段才接得上。
        std::fs::write(p.temp_mfd(), vec![0x5a; 64]).unwrap();
        run_to_end(&mut e, &mut st, &p, 8);
        assert!(
            st.log_buffer.contains("写卡成功"),
            "CUID 64/64 应判写卡成功，实际: {}",
            st.log_buffer
        );
        assert!(!p.temp_mfd().exists(), "成功后应删除 temp 备份");
    }

    /// `format-card` S17：补确认框（原版 `command.js:144-149` 直接开跑，误点即清空）。
    /// 未确认不启动，确认后才跑备份段。
    #[test]
    fn format_card_needs_confirm() {
        let (_d, p) = paths("format");
        let mut e = Engine::new(Box::new(MockToolRunner::new(vec![])));
        let mut st = connected();
        let mut dlg = MockDialog::new(); // 空队列 → 拒绝
        e.dispatch(Action::Plain("format-card"), &mut st, &p, &mut dlg)
            .unwrap();
        assert_eq!(dlg.confirm_calls(), 1, "format-card 应过 confirm_danger");
        assert!(!e.is_running(), "未确认不应启动备份段");
        assert!(!st.is_running_task);
    }

    /// `format-card` 确认后启动备份段（`nfc-mfdetect -O`）。
    #[test]
    fn format_card_confirmed_starts_backup_stage() {
        let (_d, p) = paths("format-ok");
        let mut e = Engine::new(Box::new(MockToolRunner::new(vec![])));
        let mut st = connected();
        let mut dlg = MockDialog::new().with_confirm(true);
        e.dispatch(Action::Plain("format-card"), &mut st, &p, &mut dlg)
            .unwrap();
        let (tool, args, _) = e.runner().started_last().unwrap();
        assert_eq!(tool, "nfc-mfdetect");
        assert!(args.contains(&format!("-O{}", p.temp_mfd().display())));
    }

    /// `lock-ufuid` 只过确认框，确认后 `nfc-mflock -q`。
    #[test]
    fn lock_ufuid_confirmed_runs_mflock() {
        let (_d, p) = paths("lock");
        let mut e = Engine::new(Box::new(MockToolRunner::new(vec![])));
        let mut st = connected();
        let mut dlg = MockDialog::new().with_confirm(true);
        e.dispatch(Action::Plain("lock-ufuid"), &mut st, &p, &mut dlg)
            .unwrap();
        let (tool, args, _) = e.runner().started_last().unwrap();
        assert_eq!(tool, "nfc-mflock");
        assert_eq!(args, vec!["-q".to_owned()]);
    }

    /// `lock-ufuid` 未确认时不启动任何工具。
    #[test]
    fn lock_ufuid_unconfirmed_launches_nothing() {
        let (_d, p) = paths("lock-no");
        let mut e = Engine::new(Box::new(MockToolRunner::new(vec![])));
        let mut st = connected();
        let mut dlg = MockDialog::new();
        e.dispatch(Action::Plain("lock-ufuid"), &mut st, &p, &mut dlg)
            .unwrap();
        assert!(!e.is_running());
    }

    /// `write-ufuid-uid`：读 block0 → 确认 → `nfc-mfsetuid -q <hex>`。
    #[test]
    fn write_uid_confirmed_runs_mfsetuid() {
        let (_d, p) = paths("uid");
        std::fs::write(p.dumpfiles_dir().join("u.mfd"), vec![0x11; 32]).unwrap();
        let mut e = Engine::new(Box::new(MockToolRunner::new(vec![])));
        let mut st = connected();
        let mut dlg = MockDialog::new().with_confirm(true);
        e.dispatch(
            Action::WithArg(
                "write-ufuid-uid",
                p.dumpfiles_dir().join("u.mfd").display().to_string(),
            ),
            &mut st,
            &p,
            &mut dlg,
        )
        .unwrap();
        assert_eq!(dlg.open_calls(), 0, "路径已预选，不该弹文件框");
        // 确认正文应带上 block0（command.js:214）
        assert!(dlg.confirm_requests[0].message.contains(&"11".repeat(16)));
        let (tool, args, _) = e.runner().started_last().unwrap();
        assert_eq!(tool, "nfc-mfsetuid");
        assert_eq!(args, vec!["-q".to_owned(), "11".repeat(16)]);
    }

    /// 转储太短 → 读不出 block0 → 报 TooShort 且**不弹**确认框。
    #[test]
    fn write_uid_short_dump_reports_and_skips_confirm() {
        let (_d, p) = paths("uid-short");
        std::fs::write(p.dumpfiles_dir().join("s.mfd"), vec![0x11; 8]).unwrap();
        let mut e = Engine::new(Box::new(MockToolRunner::new(vec![])));
        let mut st = connected();
        let mut dlg = MockDialog::new().with_confirm(true);
        e.dispatch(
            Action::WithArg(
                "write-ufuid-uid",
                p.dumpfiles_dir().join("s.mfd").display().to_string(),
            ),
            &mut st,
            &p,
            &mut dlg,
        )
        .unwrap();
        assert_eq!(dlg.confirm_calls(), 0);
        assert!(!e.is_running());
        assert!(st.log_buffer.contains("转储文件太短"));
    }

    /// 成功退出后状态必须落回「空闲」——原版 `printExitLog(0)` 对所有工具
    /// 一视同仁（`execUtils.js:75-78`）。S11 第一步只对 read-IC 做了这件事，
    /// 导致 detect / dict 跑完后指示灯一直停在「运行中」。
    #[test]
    fn successful_detect_exit_clears_status() {
        for name in ["read-IC", "detect-card-type"] {
            let (_d, p) = paths(&format!("free-{name}"));
            let mut e = Engine::new(Box::new(MockToolRunner::new(vec![]).with_exit(Some(0))));
            let mut st = connected();
            let mut dlg = MockDialog::new();
            e.dispatch(Action::Plain(name), &mut st, &p, &mut dlg)
                .unwrap();
            assert_eq!(st.status.indicator, Indicator::Running, "{name} 启动后");
            run_to_end(&mut e, &mut st, &p, 3);
            assert_eq!(
                st.status.indicator,
                Indicator::Free,
                "{name} 成功后指示灯仍停在运行中"
            );
            assert!(st.log_buffer.contains("运行完毕"), "{name} 缺收尾日志");
        }
    }

    #[test]
    fn pure_ui_actions_are_no_ops_for_the_engine() {
        // 这些由 app 层处理（面板开关 / rfd），Engine 收下即止
        let (_d, p) = paths("pureui");
        let mut e = Engine::new(Box::new(MockToolRunner::new(vec![])));
        let mut st = connected();
        let mut dlg = MockDialog::new();
        for name in [
            "input-keys-read-IC",
            "hard-nested",
            "dict-test",
            "open-about",
            "open-dump-editor",
            "save-log",
        ] {
            let r = e.dispatch(Action::Plain(name), &mut st, &p, &mut dlg);
            assert_eq!(r, Ok(()), "{name} 不该是错误");
            assert!(!e.is_running(), "{name} 不该启动工具");
        }
    }

    #[test]
    fn delete_dumps_removes_files_and_refreshes_history() {
        let (_d, p) = paths("delete");
        std::fs::write(p.dumpfiles_dir().join("a.mfd"), b"x").unwrap();
        std::fs::write(p.dumpfiles_dir().join("b.mfd"), b"x").unwrap();
        let mut e = Engine::new(Box::new(MockToolRunner::new(vec![])));
        let mut st = AppState::new();
        // S11-2：删除不可逆，确认通过后才能真删
        let mut dlg = MockDialog::new().with_confirm(true);
        e.dispatch(
            Action::DeleteDumps(vec!["a.mfd".to_owned()]),
            &mut st,
            &p,
            &mut dlg,
        )
        .unwrap();
        // 历史应刷新为只剩 b.mfd
        assert_eq!(st.dump_history, vec!["b.mfd".to_owned()]);
        // 确认正文的 detail 应带上待删文件名（command.js:511）
        assert!(dlg.confirm_requests[0].message.contains("a.mfd"));
    }

    /// 安全闸：未经确认的删除必须**一个文件都不动**。
    ///
    /// 这是绕过 UI 直接派发 `Action::DeleteDumps` 时的最后一道防线。
    #[test]
    fn delete_dumps_without_confirm_removes_nothing() {
        let (_d, p) = paths("delete-no-confirm");
        std::fs::write(p.dumpfiles_dir().join("a.mfd"), b"x").unwrap();
        let mut e = Engine::new(Box::new(MockToolRunner::new(vec![])));
        let mut st = AppState::new();
        // 空队列 → confirm_danger 返回安全默认 false
        let mut dlg = MockDialog::new();
        e.dispatch(
            Action::DeleteDumps(vec!["a.mfd".to_owned()]),
            &mut st,
            &p,
            &mut dlg,
        )
        .unwrap();
        assert!(
            p.dumpfiles_dir().join("a.mfd").exists(),
            "未确认竟把文件删了"
        );
        assert_eq!(dlg.confirm_calls(), 1, "确认框必须被调用");
    }

    /// 用户在确认框点「取消」同样不删。
    #[test]
    fn delete_dumps_cancelled_keeps_files() {
        let (_d, p) = paths("delete-cancel");
        std::fs::write(p.dumpfiles_dir().join("a.mfd"), b"x").unwrap();
        let mut e = Engine::new(Box::new(MockToolRunner::new(vec![])));
        let mut st = AppState::new();
        let mut dlg = MockDialog::new().with_confirm(false);
        e.dispatch(
            Action::DeleteDumps(vec!["a.mfd".to_owned()]),
            &mut st,
            &p,
            &mut dlg,
        )
        .unwrap();
        assert!(p.dumpfiles_dir().join("a.mfd").exists());
    }

    #[test]
    fn connect_writes_libnfc_conf_and_sets_connecting() {
        let (_d, p) = paths("connect");
        let mut e = Engine::new(Box::new(MockToolRunner::new(vec![])));
        let mut st = AppState::new();
        let mut dlg = MockDialog::new();
        e.dispatch(
            Action::ConnUsb("/dev/ttyACM0".into()),
            &mut st,
            &p,
            &mut dlg,
        )
        .unwrap();
        assert_eq!(st.device_state, DeviceState::Connecting);
        assert_eq!(st.current_device.as_deref(), Some("/dev/ttyACM0"));
        let conf = std::fs::read_to_string(p.nfc_config()).unwrap();
        assert!(conf.contains("/dev/ttyACM0"), "libnfc.conf 未含设备名");
    }

    #[test]
    fn connect_is_rejected_while_already_connecting() {
        // R5 的第二道防线
        let (_d, p) = paths("connect2");
        let mut e = Engine::new(Box::new(MockToolRunner::new(vec![])));
        let mut st = AppState::new();
        st.device_state = DeviceState::Connecting;
        let mut dlg = MockDialog::new();
        let r = e.dispatch(
            Action::ConnUsb("/dev/ttyACM0".into()),
            &mut st,
            &p,
            &mut dlg,
        );
        assert_eq!(r, Err(ToolError::Busy));
    }
}
