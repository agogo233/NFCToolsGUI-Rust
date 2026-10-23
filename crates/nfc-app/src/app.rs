//! S8 主窗口：自绘标题栏 + 引导条 + 三列按钮 + 控制台（事件/日志/任务条）。
//!
//! S16 重排后布局自上而下：
//! `title_bar`（logo + 应用名 + 设备下拉/状态胶囊 + 右侧控制） →
//! `missing_tools_bar` → `guidance_bar` → `button_area` → `console`
//! （控制台头部 + 最近事件 + 日志 + 任务条）。

use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use egui::{Align, Frame, Layout, RichText, ScrollArea, Sense, Ui};

use nfc_core::engine::{Action, Engine};
use nfc_core::i18n::t;
use nfc_core::serial::{DEVICE_NONE, DEVICE_PREFIX};
use nfc_core::state::{AppState, DeviceState, Event, EventKind, Indicator};
use nfc_core::theme::Theme;

use crate::dialogs_host::{self, DialogHost};
use crate::dialogs_ui;
use crate::panels;
use crate::theme_ui::{self, event_color};

/// 原版日志首行（`index.html:141`）。
pub const LOG_GREETING: &str = "Hello cardman!";
/// 事件条最多保留的行数（`index.js:117` `EVENT_MAX`）。
const EVENT_MAX: usize = 30;
/// 热插拔轮询间隔（docs/03 S8）。
pub const HOTPLUG_INTERVAL: Duration = Duration::from_millis(500);
/// 计时器步进间隔（`index.js:40` `setInterval(..., 1000)`）。
pub const TICK: Duration = Duration::from_secs(1);

/// 事件列表展开时的高度（px）。S16：默认折叠为“最近事件”一行，展开才显示全量。
pub const EVENT_LIST_MAX_H: f32 = 96.0;
/// S16：控制台头部（标题 + 工具按钮）高度。
pub const CONSOLE_HEADER_H: f32 = 30.0;
/// S16：任务条（状态 / 进度 / 计时）高度。
pub const TASK_BAR_H: f32 = 32.0;
/// S16：日志最小可见高度，防止最小窗口把日志挤没。
pub const LOG_MIN_H: f32 = 120.0;
/// 控制台最小预留高度：头部 + 最近事件一行 + 日志最小 120 + 任务条 32 +
/// 间距与内边距的保守和。主操作区滚动 `max_height` 以此为界，
/// 保证最小窗口 / 缺工具条时任务条/进度条始终可见。
/// 事件列表展开时 `draw` 会按 `EVENT_LIST_MAX_H` 动态追加预留。
pub const CONSOLE_RESERVE_H: f32 = 232.0;

/// 主题模式三态（对标 Electron `nativeTheme.themeSource`）。
///
/// 原版是 `main.js:31-39` 的 `dark` / `light` / `system` 三个 ipc handler，
/// `system` 交给系统。此处三态都自己解析：`System` 每帧问 egui
/// （`Context::system_theme()`），故系统切换主题时能自动跟随。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ThemeMode {
    Dark,
    Light,
    /// 跟随系统（`nativeTheme.themeSource = 'system'`）。
    #[default]
    System,
}

impl ThemeMode {
    /// 解析成实际调色板。`system_dark` 来自 `Context::system_theme()`。
    pub fn resolve(self, system_dark: bool) -> Theme {
        match self {
            ThemeMode::Dark => nfc_core::theme::dark(),
            ThemeMode::Light => nfc_core::theme::light(),
            ThemeMode::System => {
                if system_dark {
                    nfc_core::theme::dark()
                } else {
                    nfc_core::theme::light()
                }
            }
        }
    }

    /// 同步给 egui，让其原生部件（`ComboBox` 下拉、tooltip）跟着换底色。
    pub fn egui_preference(self) -> egui::ThemePreference {
        match self {
            ThemeMode::Dark => egui::ThemePreference::Dark,
            ThemeMode::Light => egui::ThemePreference::Light,
            ThemeMode::System => egui::ThemePreference::System,
        }
    }
}

/// 引导条按钮：`(action 名, 按钮文案, 动作参数)`。
///
/// 第三元素为动作入参：`DumpSaved` 引导「写这张卡」直接带 dumpId
/// （对标 `command.js:127-130` 的带参跳确认框）；其余引导不传参（`None`，
/// 等同旧行为——弹文件框选择）。
pub type GuidanceButton = (&'static str, String, Option<String>);

/// 应用的全部 UI 状态。
pub struct NfcApp {
    pub state: AppState,
    /// 引导条（`(文案, 按钮列表)`），`None` 为隐藏。
    pub guidance: Option<(String, Vec<GuidanceButton>)>,
    /// S15：冷启动引导已展示过（一次性，避免每帧刷屏 / 连接后反复弹）。
    pub cold_guided: bool,
    /// 锁滚动（`index.js:3` `isLockScroll`）。
    pub lock_scroll: bool,
    /// S16：控制台事件列表是否展开（默认折叠为“最近事件”）。
    pub events_expanded: bool,
    /// 任务计时器已启动（`index.js:4` `isTimerRunning`）。
    pub timer_running: bool,
    /// 计时秒数（`index.js:6`）。
    pub timer_second: u32,
    /// 上次计时步进时刻。
    last_tick: Instant,
    /// 上次热插拔轮询时刻。
    last_hotplug: Instant,

    /// 待派发给 core 的动作队列。
    pub pending: Vec<Action>,
    /// 窗口关闭请求。
    pub close_requested: bool,
    /// 主题模式三态（S9 settings 面板切换）。
    pub theme_mode: ThemeMode,
    /// 当前调色板。由 `theme_mode` + 系统主题解析而来，故每帧可能变。
    pub theme: Theme,
    /// S9 面板的表单状态。
    pub forms: panels::Forms,
    /// 动作编排器（S11）。`pending` 由它消费，不再每帧丢弃。
    pub engine: Engine,
    /// 路径表，供 `Engine` 构造工具参数。
    pub paths: nfc_core::paths::Paths,
    /// 对话框提供者（S11-2）。
    ///
    /// 生产路径上它不再是「假」实现：app 的应用内确认窗点确定后，把 `true`
    /// 预置进 `confirm_results`，engine 侧的 `prepare_*` 便取到现成答案，
    /// 全程不弹系统框。文件框由 [`Self::dialogs`] 单独负责。
    pub dialogs: nfc_core::dialogs::ScriptedDialog,
    /// 需用户确认的动作，与「已获批准」成对存放。
    ///
    /// 与 [`Self::pending`] 分开是因为批准必须**跟着动作走**：见
    /// [`Self::consume_pending`] 里关于热插拔抢批的说明。
    pub pending_with_approval: VecDeque<(Action, bool)>,
    /// 系统文件对话框的线程宿主。
    pub dialogs_host: DialogHost,
    /// 待用户确认的危险操作模态窗。
    pub confirm: Option<dialogs_ui::ConfirmModal>,
    /// `save-log` 待写出的日志快照。系统弹窗是异步的，得先把内容存住。
    pub log_pending: Option<String>,
    /// 文件框 / 工具目录的启动期探测结论。
    ///
    /// **刻意偏离原版**：`framework/bin` 是 gitignore 的（不在仓库里），
    /// 原版缺失时要等用户点下按钮才在 `spawn` 处抛 IO 错误——用户看到的是
    /// 一条看不懂的英文报错。此处在启动时就给出明确引导。
    pub tool_dir_ok: bool,
    /// 工具目录（exe 相对定位，见 `main.rs`）。
    pub bin_dir: PathBuf,
    /// 缺的具体工具名（`KNOWN_TOOLS` 的子集）。
    pub missing_tools: Vec<&'static str>,
    /// 是否已经就「无桌面门户」提示过用户，避免每帧刷屏。
    pub warned_no_portal: bool,
    /// 本机系统文件对话框是否可用（启动时探测一次，见 `dialogs_host`）。
    ///
    /// 存成字段而非每次现算，一是探测有 `stat` 开销，二是 `--selftest` 需要
    /// 能在无头 CI 里把它置真来跑完整的文件流程。
    pub file_dialogs_ok: bool,
    /// 自绘标题栏左上角的 logo 纹理。生产路径由 `main.rs` 注入，
    /// 测试 / 无头环境为 `None`（只少个标识，不影响任何功能）。
    pub logo: Option<egui::TextureHandle>,
}

impl NfcApp {
    pub fn new(mut state: AppState) -> Self {
        let now = Instant::now();
        // 原版 main.js 启动时 `nativeTheme.themeSource` 未经设置，
        // Electron 默认 'system'，故这里也默认 System。
        let theme_mode = ThemeMode::default();
        // 内置字典兜底：原版模块级 `dictPath` 指向打包资源，此处置解析值
        // （打包=exe 旁，开发=仓库根），让「字典测试」开箱可用；用户选过
        // 字典后由 `apply_dict_choice` 覆盖。
        state.dict_path = nfc_core::paths::dict_file();
        Self {
            state,
            guidance: None,
            cold_guided: false,
            lock_scroll: false,
            events_expanded: false,
            timer_running: false,
            timer_second: 0,
            last_tick: now,
            last_hotplug: now,
            pending: Vec::new(),
            close_requested: false,
            theme_mode,
            // 首帧拿不到系统主题（`system_theme()` 在 winit 事件到达前为 None），
            // 此时按深色起步；下一帧 `sync_theme` 会纠正。
            theme: theme_mode.resolve(true),
            forms: panels::Forms::default(),
            engine: Engine::new(Box::new(nfc_core::runner::MockToolRunner::new(Vec::new()))),
            paths: nfc_core::paths::Paths::from_app_data(),
            dialogs: nfc_core::dialogs::ScriptedDialog::new(),
            pending_with_approval: VecDeque::new(),
            dialogs_host: DialogHost::spawn(),
            confirm: None,
            log_pending: None,
            tool_dir_ok: true,
            bin_dir: std::path::PathBuf::new(),
            missing_tools: Vec::new(),
            warned_no_portal: false,
            file_dialogs_ok: dialogs_host::file_dialogs_supported(),
            logo: None,
        }
        .with_no_portal_warning()
    }

    /// 设置自绘标题栏左上角的 logo（`main.rs` 注入；`--selftest` 不设）。
    pub fn with_logo(mut self, logo: Option<egui::TextureHandle>) -> Self {
        self.logo = logo;
        self
    }

    /// Linux 上没有桌面门户时，启动即给一条明确提示。
    ///
    /// rfd 的阻塞 API 不区分「用户取消」与「后端不可用」（`dialogs_host` 的
    /// 已知限制），所以在无头 / SSH 环境下点「写卡」会表现为毫无反应。
    /// 启动时提前讲清楚，好过让人反复点。
    fn with_no_portal_warning(mut self) -> Self {
        if !self.file_dialogs_ok {
            self.warned_no_portal = true;
            self.push_event(
                EventKind::Error,
                "未检测到桌面门户服务（DBus 会话总线不可用），系统文件对话框不可用。\n\
                 写卡 / 存日志 / 选字典等需要选文件的操作将无法完成。",
            );
            self.state.apply(Event::Log(
                "\n[警告] 未检测到 XDG 桌面门户，系统文件对话框不可用。\n".into(),
            ));
        }
        self
    }

    /// 换成真工具，并做 `framework/bin` 启动自检。S11 第二步由 `main.rs` 调用。
    ///
    /// `bin_dir` 必须由调用方按 **exe 相对**规则算好（见 `main.rs`）——
    /// 不能拿 `paths.base()`（用户数据目录）去向上找，那会一路找到 `$HOME`。
    pub fn with_real_engine(mut self, engine: Engine, bin_dir: std::path::PathBuf) -> Self {
        self.engine = engine;
        self.bin_dir = bin_dir;
        self.check_tool_dir();
        self
    }

    /// 探测 `framework/bin` 下缺哪些工具，缺失则给一条明确的启动引导。
    ///
    /// 原版无此步骤：缺失只在 `spawn` 时报 IO 错误。
    pub fn check_tool_dir(&mut self) {
        let bin = self.bin_dir.clone();
        let missing = nfc_core::runner::missing_tools(&bin);
        self.tool_dir_ok = missing.is_empty();
        self.missing_tools = missing;
        if self.tool_dir_ok {
            return;
        }
        let list = self.missing_tools.join("、");
        self.state.apply(Event::Log(format!(
            "\n{}\n{}: {}\n{}\n",
            t("html_framework_bin_missing"),
            t("html_framework_bin_missing_short"),
            list,
            bin.display()
        )));
        self.push_event(EventKind::Error, &t("html_framework_bin_missing"));
    }

    /// 换成带脚本的 `MockToolRunner`，供 `--selftest` 驱动完整的
    /// 「派发 → 输出回流 → 退出收尾」链路。
    pub fn with_mock_backend(mut self, script: Vec<nfc_core::runner::Chunk>) -> Self {
        self.engine = Engine::new(Box::new(nfc_core::runner::MockToolRunner::new(script)));
        self
    }

    /// 跑一帧 `logic` 的动作消费部分（`--selftest` 用；真机走 eframe 钩子）。
    ///
    /// 每次 `dispatch` 前后都把「已确认」预置队列清空：若某次派发因为门禁被拒
    /// 而没走到 `confirm_danger`，那个 `true` 绝不能留给下一个动作——否则一次
    /// 误批会让后续的写卡 / 锁卡 / 删除**跳过**自己的确认框直接执行。
    ///
    /// 批准与动作**成对入队**（`pending_with_approval`），而不是靠队列位置
    /// 碰运气：热插拔每 500ms 会往 `pending` 里塞一个 `ScanUsb`（`tick`），
    /// 若批准单独存在、派发时按位置取，就可能先被 `ScanUsb` 消费掉，导致用户
    /// 在「锁死 UFUID」上点了确定却什么都没发生。
    pub fn consume_pending(&mut self) {
        // 先进先出：先处理成对入队的（动作, 已批准）。
        while let Some((action, approved)) = self.pending_with_approval.pop_front() {
            if approved {
                self.dialogs.confirm_results.push(true);
            }
            self.dispatch_one(action);
        }
        // 再处理裸动作（热插拔轮询、面板直发等）。
        for action in std::mem::take(&mut self.pending) {
            self.dispatch_one(action);
        }
    }

    /// 派发单个动作：预置本次批准 → dispatch → 清空批准。
    fn dispatch_one(&mut self, action: Action) {
        let r = self
            .engine
            .dispatch(action, &mut self.state, &self.paths, &mut self.dialogs);
        // 安全网：本次派发没消耗掉的批准一律丢弃。
        self.dialogs.confirm_results.clear();
        if let Err(e) = r {
            self.on_dispatch_error(&e);
        }
    }

    /// 把 engine 挂起的保存请求投递去弹框（每帧一次）。
    ///
    /// `engine.pending_save()` 一直非空直到 `resolve_save` 把它取走，故这里
    /// 还要看 `dialogs_host` 是否已在等——否则每帧都会重复投递、弹出几十个框。
    fn pump_pending_save(&mut self) {
        if self.dialogs_host.is_pending() {
            return;
        }
        let Some(pending) = self.engine.pending_save() else {
            return;
        };
        let purpose = if self.engine.hn_saving() {
            dialogs_host::FilePurpose::NoncesSave
        } else {
            dialogs_host::FilePurpose::ReadIcSave
        };
        self.dialogs_host
            .request_save(purpose, pending.request.clone());
    }

    /// 一个系统文件框回来了。
    ///
    /// 具体续接逻辑按 `purpose` 分派。**取消（`path == None`）一律静默收场**——
    /// 原版用户关掉文件框也是静默返回（`command.js:134` 的
    /// `if (result["canceled"] === true) return`），不记错误。
    fn on_file_reply(&mut self, reply: dialogs_host::FileReply) {
        // S11-7：这两条要**连取消一起**送回 engine——取消的语义两边不同
        // （read-IC 收尾取消＝用默认名存；collectOnly 取消＝保留 nonces.bin），
        // 由 `Engine::resolve_save` 自己分派，故必须在下面那个 `let Some(path)`
        // 早退**之前**处理。
        match reply.purpose {
            dialogs_host::FilePurpose::ReadIcSave | dialogs_host::FilePurpose::NoncesSave => {
                self.engine
                    .resolve_save(reply.path, &mut self.state, &self.paths);
                return;
            }
            _ => {}
        }
        let Some(path) = reply.path else {
            // 原版 `save-log` 取消时会留一条「未保存」（command.js:547）
            if reply.purpose == dialogs_host::FilePurpose::SaveLog {
                self.state
                    .apply(Event::Log(format!("\n{}\n", t("log_msg_not_saved"))));
            }
            return;
        };
        match reply.purpose {
            dialogs_host::FilePurpose::WriteDump => {
                self.confirm = Some(dialogs_ui::ConfirmModal::new(
                    dialogs_ui::ConfirmKind::WriteDump {
                        dump: path.to_string_lossy().into_owned(),
                    },
                    &dialogs_ui::danger_title(),
                    t("dialog_msg_confirm_write_dump"),
                ));
            }
            dialogs_host::FilePurpose::WriteUidDump => {
                // 原版 `command.js:210-214` 把 block0 hex **显示给用户看**
                // 再让用户确认。engine 侧的 `prepare_write_uid` 虽然也会重读
                // 并拼进 `confirm_danger` 的正文，但那次确认框不再呈现，
                // 所以必须在这里自己读一次、显示出来——否则用户是在不知道
                // 将写入哪个 0 块的情况下授权改写 UFUID。
                //
                // `dump` 必须存**完整路径**：它要作为载荷交给 engine，
                // `prepare_write_uid` 拿它去读文件。展示用的文件名另取。
                let full = path.to_string_lossy().into_owned();
                let name = path
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default();
                match nfc_core::tasks::card_write::read_block0_hex(&path) {
                    Ok(hex) if nfc_core::tasks::card_write::block0_is_valid(&hex) => {
                        self.confirm = Some(dialogs_ui::ConfirmModal::new(
                            dialogs_ui::ConfirmKind::WriteUid { dump: full },
                            &dialogs_ui::danger_title(),
                            nfc_core::tasks::card_write::write_uid_confirm(&hex).message,
                        ));
                    }
                    Ok(_) => self.push_event(
                        EventKind::Error,
                        &format!("{name}: {}", t("log_msg_dump_file_too_short")),
                    ),
                    Err(e) => self.push_event(
                        EventKind::Error,
                        &format!("{name}: {} ({e})", t("log_msg_read_dump_file_failed")),
                    ),
                }
            }
            // command.js:375-378：写回 dictPath 并把文件名显示在字典面板上
            dialogs_host::FilePurpose::DictFile => {
                let name = path
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default();
                self.state.dict_path = path;
                self.forms.dict_test.file_name = name;
            }
            // command.js:546-556
            dialogs_host::FilePurpose::SaveLog => {
                let content = self.log_pending.take().unwrap_or_default();
                match std::fs::write(&path, content) {
                    Ok(()) => {
                        let name = path.to_string_lossy().into_owned();
                        self.push_event(EventKind::Success, &name);
                        self.state.apply(Event::Log(format!(
                            "\n\n{} {name}\n",
                            t("log_msg_file_already_saved_to")
                        )));
                    }
                    Err(e) => {
                        self.state
                            .apply(Event::Log(format!("\n{}\n", t("log_msh_save_failed"))));
                        self.push_event(EventKind::Error, &format!("{e}"));
                    }
                }
            }
            // command.js:423-432：读进来按 32 hex 切块、每 4 块一扇区。
            // 传**完整路径**：用户在文件框选的 dump 可能在 dumpfiles 之外，
            // 「保存」须写回原文件（P2-4）。
            dialogs_host::FilePurpose::EditorFile => {
                self.load_dump_from_path(&path);
            }
            // command.js:449-458：另存为
            dialogs_host::FilePurpose::EditorSaveAs => {
                self.write_editor_to(&path);
            }
            // command.js:477-486
            dialogs_host::FilePurpose::ComparatorA => self.load_comparator_side(true, &path),
            dialogs_host::FilePurpose::ComparatorB => self.load_comparator_side(false, &path),
            // S11-7：这两个由 `pump_pending_save` 投递、在上面的 match 之前
            // 就已 `return` 了，走不到这里。
            dialogs_host::FilePurpose::ReadIcSave | dialogs_host::FilePurpose::NoncesSave => {}
            // S18：CUID 空卡写入——选好源 dump 后进确认窗（engine 再过一道闸）。
            dialogs_host::FilePurpose::CuidDump => {
                self.confirm = Some(
                    dialogs_ui::ConfirmModal::new(
                        dialogs_ui::ConfirmKind::CuidWrite {
                            dump: path.to_string_lossy().into_owned(),
                        },
                        &dialogs_ui::danger_title(),
                        t("dialog_msg_confirm_cuid_write"),
                    )
                    .with_code(dialogs_ui::random_code()),
                );
            }
            // S18：编辑器导出密钥字典——把当前编辑器内容的密钥写进用户选的文件。
            dialogs_host::FilePurpose::ExportKeys => {
                let lines = nfc_core::model::dumpcheck::key_dict(&self.forms.editor.sectors);
                let body = lines.join("\n");
                match std::fs::write(&path, body) {
                    Ok(()) => {
                        let name = path.to_string_lossy().into_owned();
                        self.push_event(EventKind::Success, &name);
                        self.state.apply(Event::Log(format!(
                            "\n\n{} {name}\n",
                            t("log_msg_file_already_saved_to")
                        )));
                    }
                    Err(e) => {
                        self.state
                            .apply(Event::Log(format!("\n{}\n", t("log_msh_save_failed"))));
                        self.push_event(EventKind::Error, &format!("{e}"));
                    }
                }
            }
        }
    }

    /// 把「用户已点确定」与动作成对入队。
    pub fn push_approved(&mut self, action: Action) {
        self.pending_with_approval.push_back((action, true));
    }

    /// 确认窗被点掉了。返回 `true` 表示应当真的关窗。
    ///
    /// 点「确定」时把批准与动作成对入队（见 [`Self::push_approved`]），
    /// engine 侧照常调 `confirm_danger`，拿到的就是预置值。未点确定时队列里
    /// 没有任何预置值，engine 拿到安全默认值 `false`，写卡 / 锁卡 / 删除
    /// 一律拒绝。
    pub fn on_confirm_result(&mut self, m: dialogs_ui::ConfirmModal, ok: bool) -> bool {
        if !ok {
            return false;
        }
        match m.kind {
            dialogs_ui::ConfirmKind::CloseWhileRunning => true,
            dialogs_ui::ConfirmKind::WriteDump { dump } => {
                self.push_approved(Action::WithArg("write-IC", dump));
                false
            }
            dialogs_ui::ConfirmKind::WriteUid { dump } => {
                self.push_approved(Action::WithArg("write-ufuid-uid", dump));
                false
            }
            dialogs_ui::ConfirmKind::Lock => {
                self.push_approved(Action::Plain("lock-ufuid"));
                false
            }
            dialogs_ui::ConfirmKind::DeleteDumps(names) => {
                self.push_approved(Action::DeleteDumps(names));
                false
            }
            dialogs_ui::ConfirmKind::PhwWriteUid { .. } => {
                self.push_approved(Action::Plain("phw-write-uid"));
                false
            }
            dialogs_ui::ConfirmKind::PhwWriteData { .. } => {
                self.push_approved(Action::Plain("phw-write-data"));
                false
            }
            dialogs_ui::ConfirmKind::FormatCard => {
                self.push_approved(Action::Plain("format-card"));
                false
            }
            // S18：CUID 空卡写入确认后，源 dump 作为载荷交给 engine。
            dialogs_ui::ConfirmKind::CuidWrite { dump } => {
                self.push_approved(Action::WithArg("cuid-write", dump));
                false
            }
            // S18：手输卡号写 UFUID 确认后，原始输入文本作为载荷交给 engine
            // （engine 侧 `input_uid_write` 再做清洗 / 拼块 / BCC 判定）。
            dialogs_ui::ConfirmKind::UidInputWrite { text } => {
                self.push_approved(Action::WithArg("done-input-uid-write", text));
                false
            }
        }
    }

    /// 把「引擎拒绝执行」翻译成用户看得懂的话。
    fn on_dispatch_error(&mut self, e: &nfc_core::runner::ToolError) {
        let text = match e.i18n_key() {
            Some(k) => t(k),
            None => e.to_string(),
        };
        self.push_event(EventKind::Error, &text);
        self.state.apply(Event::Log(format!("\n{text}\n")));
    }

    /// 依 `theme_mode` 重新解析调色板并刷到 egui。每帧调用。
    pub fn sync_theme(&mut self, ctx: &egui::Context) {
        // egui 的 `system_theme()` 在原生后端已接 winit 的事件流，
        // 拿不到时（首帧 / 某些平台）退回深色，与 `Paint::is_dark` 的判定一致。
        let system_dark = ctx.system_theme().is_none_or(|t| t == egui::Theme::Dark);
        let resolved = self.theme_mode.resolve(system_dark);
        // 同步给 egui，让 `ComboBox` 弹层 / tooltip 跟着换底色
        ctx.set_theme(self.theme_mode.egui_preference());
        if resolved != self.theme {
            self.theme = resolved;
            // 两桶都写：egui 按活动桶取 `Visuals`，只写活动桶会让另一桶永远是
            // 出厂色。本写入幂等且与 `resolved` 无关，故首次写入靠 `main.rs`
            // 的启动钩子补上——首帧守卫可能不触发。
            theme_ui::apply_visuals(ctx);
        }
    }

    /// 当前调色板的 newtype（供 theme_ui 转换用）。
    pub fn paint(&self) -> theme_ui::Paint {
        theme_ui::Paint(self.theme)
    }

    /// 把 core 事件翻译成 UI 侧状态。
    ///
    /// `Event::Guidance` / `BinaryData` 等在 `AppState::apply` 里是 no-op
    /// （core 不该知道 UI 长什么样），必须在此显式消费。
    pub fn apply(&mut self, e: Event) {
        match e {
            Event::Guidance { text, buttons } => {
                self.guidance = Some((
                    text,
                    buttons
                        .into_iter()
                        .map(|(action, label)| (action, label, None))
                        .collect(),
                ));
            }
            Event::DumpSaved(path) => {
                let name = path
                    .file_name()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default();
                self.push_event(EventKind::Success, &name.to_string());
                // 「写这张卡」直接带 dumpId（对标 command.js:127-130）：保存的是
                // 裸文件名，engine 的 `resolve_dump_id` 会拼 `dumpfiles/`。
                // 名为空（无 file_name 的极端路径）时退回 None，仍走选文件框。
                let dump_arg = if name.is_empty() {
                    None
                } else {
                    Some(name.clone())
                };
                self.guidance = Some((
                    name.to_string(),
                    vec![
                        ("write-IC", t("html_write_this_dump"), dump_arg),
                        ("open-dump-folder", t("html_open_dump_folder"), None),
                    ],
                ));
            }
            Event::Status { indicator, .. } => {
                // index.js:28 — running 启计时；free / error 停计时
                match indicator {
                    Indicator::Running => self.timer_running = true,
                    Indicator::Free | Indicator::Error => self.reset_timer(true),
                }
            }
            other => self.state.apply(other),
        }
    }

    /// 追加一条事件摘要，超出 `EVENT_MAX` 丢最旧的（`index.js:124`）。
    pub fn push_event(&mut self, kind: EventKind, text: &str) {
        self.state.events.push(nfc_core::state::EventItem {
            kind,
            text: text.to_owned(),
        });
        while self.state.events.len() > EVENT_MAX {
            self.state.events.remove(0);
        }
    }

    /// 重置状态（`index.js:132` `resetStatus`）。
    pub fn reset_timer(&mut self, _from_main: bool) {
        self.timer_running = false;
        self.timer_second = 0;
    }

    /// `App::logic` 的全部内容。eframe 的 [`eframe::Frame`] 字段全
    /// `pub(crate)`，测试里构造不了；自检要用它模拟「窗被遮挡时 eframe
    /// 只调 `logic` 不调 `ui`」，故把与 `Frame` 无关的本体放在这里。
    pub fn logic_body(&mut self, ctx: &egui::Context) {
        // 全局快捷键先于任何绘制处理，保证关掉面板的那一帧不再画它。
        self.handle_keys(ctx);

        // S16：全局排版 token 每帧同步一次；只写 spacing/text_styles，
        // 不会覆盖 `sync_theme` 设置的 `Visuals`。
        theme_ui::apply_style(ctx);

        // 每帧推进要先于任何绘制：这样下面画出来的就是刚 pump 过的新状态。
        self.tick();

        // 主题三态每帧解析一次：S9 settings 面板可切，且 System 模式要跟
        // 随系统主题变化（用户在系统设置里改深浅色时立即生效）。
        self.sync_theme(ctx);

        // S15：冷启动引导（未连接设备时出现一次「先选设备」）
        self.maybe_cold_start_guidance();

        // 面板在主窗口之后画，保证模态浮在最上层
        panels::show(ctx, self);

        // S11-2：危险确认用应用内模态窗（S11 第一步之前是 guidance 按钮）。
        if let Some(m) = self.confirm.clone() {
            let danger = self.paint().color(self.paint().danger);
            if let Some(ok) = dialogs_ui::show(ctx, &m, danger) {
                self.confirm = None;
                if self.on_confirm_result(m, ok) {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
        }

        if self.close_requested {
            self.close_requested = false;
            // R1：运行中关窗要二次确认
            if self.state.is_running_task {
                self.confirm = Some(dialogs_ui::close_while_running_modal());
            } else {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }

        // S11：`pending` 不再丢弃，逐条交给 `Engine` 真正执行。
        self.consume_pending();
    }

    /// 全局快捷键：Esc 取消确认窗 / 关闭最上层面板；Ctrl(⌘)+S 保存转储。
    ///
    /// Esc 一律朝安全侧收敛：确认窗视为「取消」，面板只关不跑动作。
    fn handle_keys(&mut self, ctx: &egui::Context) {
        let (esc, save) = ctx.input(|i| {
            let mod_down = i.modifiers.ctrl || i.modifiers.command;
            (
                i.key_pressed(egui::Key::Escape),
                mod_down && i.key_pressed(egui::Key::S),
            )
        });

        // 转储编辑器开着、校验通过且没有确认窗挡在上面时才保存；
        // "0" = 非另存为（与面板按钮一致）。
        if save
            && self.confirm.is_none()
            && self.state.panels.dump_editor
            && self.forms.editor.can_save()
        {
            self.exec("dump-editor-save", Some("0".to_owned()));
            return;
        }
        if !esc {
            return;
        }
        if self.confirm.is_some() {
            self.confirm = None;
            return;
        }
        // 面板按 `Panel::ALL` 顺序绘制，最后画的在最上层，故只关最上层那一个。
        panels::close_topmost(&mut self.state.panels);
    }

    /// 连接中禁用设备下拉（R5）。**由 `device_state` 派生**，不单独存字段——
    /// 早前那个手写字段从未被任何代码写入，恒为 `false`，R5 形同虚设。
    pub fn connecting_now(&self) -> bool {
        matches!(self.state.device_state, DeviceState::Connecting)
    }

    /// 每帧推进计时器与热插拔轮询。
    pub fn tick(&mut self) {
        // S11：无论有没有任务在跑都 pump 一次——kill 之后仍可能有残余输出，
        // 且 `exit()` 的观察契约要求 pump 先行。
        self.engine.pump(&mut self.state, &self.paths);

        // S11-2：系统文件框在别的线程上阻塞着，这里每帧收一次结果。
        while let Some(reply) = self.dialogs_host.poll() {
            self.on_file_reply(reply);
        }

        // S11-7：engine 挂起中的保存请求（read-IC 收尾 / collectOnly）在这里
        // 弹框。必须在 pump **之后**：请求是收尾时才产生的。
        self.pump_pending_save();

        let now = Instant::now();
        if self.timer_running && now.duration_since(self.last_tick) >= TICK {
            // 落后多帧时补齐，避免卡顿后计时漂移
            let elapsed = now.duration_since(self.last_tick).as_secs();
            self.timer_second += elapsed.max(1) as u32;
            self.last_tick = now;
        }
        if now.duration_since(self.last_hotplug) >= HOTPLUG_INTERVAL {
            self.pending.push(Action::ScanUsb);
            self.last_hotplug = now;
        }
    }

    /// 把 `dumpfiles/` 下的一个 dump 读进编辑器。
    ///
    /// 对标 `command.js:423-432` 的 `fs.readFile` + 按 32 hex 切块、每 4 块
    /// 一扇区。读失败不 panic：原版是 `if (err) throw err`（整个窗口崩），
    /// 此处只把错误记成事件，让 UI 继续可用。
    pub fn load_dump_into_editor(&mut self, file_name: &str) -> bool {
        // 只接受裸文件名——与 `dumpfiles::delete/rename` 同一道防线；
        // 名字含分隔符（或 `..`）时直接拒绝，不拼任何路径。
        if !nfc_core::dumpfiles::is_plain_name(file_name) {
            self.push_event(EventKind::Error, &format!("{file_name}: 非法的转储文件名"));
            return false;
        }
        let dir = self.paths.dumpfiles_dir();
        let path = dir.join(file_name);
        match std::fs::read(&path) {
            Ok(bytes) => {
                let name = nfc_core::tasks::dict_test::dict_file_name(file_name).to_owned();
                match self.forms.editor.load(Some(name), &bytes) {
                    Ok(()) => {
                        // 记住完整路径：「保存」要写回这个 dumpfiles 里的文件。
                        self.forms.editor.origin = Some(path);
                        true
                    }
                    Err(e) => {
                        self.push_event(EventKind::Error, &e);
                        false
                    }
                }
            }
            Err(e) => {
                self.push_event(EventKind::Error, &format!("{file_name}: {e}"));
                false
            }
        }
    }

    /// 从**完整路径**载入 dump 进编辑器（系统文件框 / `dump-editor-choose-file`
    /// 带参）。
    ///
    /// 与 [`Self::load_dump_into_editor`]（历史列表的裸名 → dumpfiles）不同：
    /// 这里路径由用户在文件框选定或作为参数传入，是外部绝对路径，**不过**
    /// `is_plain_name` 校验，直接读该路径并记住 `origin`——否则外部 dump 打开后
    /// 「保存」会被写回 dumpfiles 下的同名副本（P2-4 修复前的缺陷）。
    pub fn load_dump_from_path(&mut self, path: &std::path::Path) -> bool {
        let name = nfc_core::tasks::dict_test::dict_file_name(&path.to_string_lossy()).to_owned();
        match std::fs::read(path) {
            Ok(bytes) => match self.forms.editor.load(Some(name), &bytes) {
                Ok(()) => {
                    self.forms.editor.origin = Some(path.to_path_buf());
                    true
                }
                Err(e) => {
                    self.push_event(EventKind::Error, &e);
                    false
                }
            },
            Err(e) => {
                self.push_event(EventKind::Error, &format!("{e}"));
                false
            }
        }
    }

    /// 把编辑器的当前内容写成二进制转储。
    ///
    /// 对标 `command.js:436-443`：`Buffer.from(hexData, "hex")` → 落盘 →
    /// 通知编辑器更新当前文件名。
    fn write_editor_to(&mut self, path: &std::path::Path) {
        // `from_sector_lines` 自身就会拒绝空列表（`DumpError::Empty`）与非 hex
        // 内容，故不必另加空值检查。
        let bytes = match nfc_core::model::dump::from_sector_lines(&self.forms.editor.sectors) {
            Ok(b) => b,
            Err(e) => {
                self.push_event(EventKind::Error, &e.to_string());
                self.state
                    .apply(Event::Log(format!("\n{}\n", t("log_msh_save_failed"))));
                return;
            }
        };
        match std::fs::write(path, &bytes) {
            Ok(()) => {
                if let Some(name) = path.file_name() {
                    self.forms.editor.file_name = Some(name.to_string_lossy().into_owned());
                }
                // 记住落点：后续「保存」继续写回这里（另存为尤其重要）。
                self.forms.editor.origin = Some(path.to_path_buf());
                self.forms.editor.dirty = false;
                let name = path.to_string_lossy().into_owned();
                self.push_event(EventKind::Success, &name);
                self.state.apply(Event::Log(format!(
                    "\n\n{} {name}\n",
                    t("log_msg_file_already_saved_to")
                )));
            }
            Err(e) => {
                self.state
                    .apply(Event::Log(format!("\n{}\n", t("log_msh_save_failed"))));
                self.push_event(EventKind::Error, &format!("{e}"));
            }
        }
    }

    /// 把一个 dump 读进比较器的某一侧（`is_a` 为真则 A 侧）。
    fn load_comparator_side(&mut self, is_a: bool, path: &std::path::Path) {
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) => {
                self.push_event(EventKind::Error, &format!("{e}"));
                return;
            }
        };
        let sectors = match nfc_core::model::dump::to_sector_lines(&bytes) {
            Ok(s) => s,
            Err(e) => {
                self.push_event(EventKind::Error, &e.to_string());
                return;
            }
        };
        let name = path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        if is_a {
            self.forms.comparator.a = Some(sectors);
            self.forms.comparator.a_name = Some(name);
        } else {
            self.forms.comparator.b = Some(sectors);
            self.forms.comparator.b_name = Some(name);
        }
    }

    /// 格式化计时（`index.js:43-47`）。`None` = 未在计时（显示空）。
    pub fn timer_text(&self) -> Option<String> {
        if !self.timer_running {
            return None;
        }
        Some(format_duration(self.timer_second))
    }

    /// 投递一个「选文件」请求，构造请求体并交给工作线程。
    ///
    /// 请求体逐字对标原版各 `showOpenDialog` 的 options。
    pub fn open_file_for(&mut self, purpose: dialogs_host::FilePurpose) {
        if !self.file_dialogs_ok {
            self.warn_no_portal();
            return;
        }
        let req = match purpose {
            // command.js:130-138；CuidDump 是 S18 CUID 空卡写入的源 dump，同款过滤器
            dialogs_host::FilePurpose::WriteDump
            | dialogs_host::FilePurpose::WriteUidDump
            | dialogs_host::FilePurpose::CuidDump => {
                nfc_core::tasks::card_write::open_dump_request(&self.paths.dumpfiles_dir())
            }
            // command.js:416-422 / command.js:470-476：原版这里把
            // `defaultPath` 写成 `dictPath`（笔误），选转储的框会默认停在
            // 字典文件上。此处修正为 dumpfiles 目录（与写卡选文件同款），
            // 偏离登记于 docs/02 §6.30。
            dialogs_host::FilePurpose::EditorFile
            | dialogs_host::FilePurpose::ComparatorA
            | dialogs_host::FilePurpose::ComparatorB => nfc_core::dialogs::OpenDialogRequest {
                title: t("dialog_title_choose_dump_file"),
                default_path: self.paths.dumpfiles_dir(),
                button_label: t("dialog_button_open"),
                filters: vec![nfc_core::dialogs::dump_filter("Dump Files")],
                message: t("dialog_msg_choose_dump_file"),
            },
            // command.js:368-374
            dialogs_host::FilePurpose::DictFile => nfc_core::dialogs::OpenDialogRequest {
                title: t("dialog_title_choose_dictionary_file"),
                default_path: self.state.dict_path.clone(),
                button_label: t("dialog_button_open"),
                filters: vec![nfc_core::dialogs::FileFilter {
                    name: t("file_type_dict"),
                    extensions: vec!["txt".to_owned(), "dic".to_owned()],
                }],
                message: t("dialog_msg_choose_dictionary_file"),
            },
            // 保存类不走「打开」框。
            // S11-7：同 ReadIcSave 等挂起保存，不由 `save_file_for` 发起。
            dialogs_host::FilePurpose::SaveLog
            | dialogs_host::FilePurpose::EditorSaveAs
            | dialogs_host::FilePurpose::ReadIcSave
            | dialogs_host::FilePurpose::NoncesSave
            // S18：导出密钥是「保存到」，走 `save_file_for`，不在这里弹打开框。
            | dialogs_host::FilePurpose::ExportKeys => return,
        };
        self.dialogs_host.request_open(purpose, req);
    }

    /// 投递一个「保存到」请求。
    ///
    /// `default_path` 可为**裸文件名**（原版 `defaultPath` 就是裸名，`command.js:541` /
    /// `:450`），此时 `dialogs_host::run_save` 取不到 `parent()`，保存框停在系统
    /// 上次使用目录 / 进程 CWD；也可为**完整路径**（如编辑器另存时传 origin），
    /// 此时 `run_save` 拆出文件名与目录，并把目录设为保存框起始目录。
    pub fn save_file_for(
        &mut self,
        purpose: dialogs_host::FilePurpose,
        default_path: PathBuf,
        filters: Vec<nfc_core::dialogs::FileFilter>,
    ) {
        if !self.file_dialogs_ok {
            self.warn_no_portal();
            return;
        }
        let req = nfc_core::dialogs::SaveDialogRequest {
            title: t("dialog_title_save_to"),
            default_path,
            filters,
            message: t("dialog_msg_choose_save_path"),
        };
        self.dialogs_host.request_save(purpose, req);
    }

    /// 系统文件框不可用时提示一次（不刷屏）。
    fn warn_no_portal(&mut self) {
        if self.warned_no_portal {
            return;
        }
        self.warned_no_portal = true;
        self.push_event(
            EventKind::Error,
            "未检测到桌面门户服务，系统文件对话框不可用（详见启动日志）",
        );
    }

    /// 派发一个动作。
    pub fn exec(&mut self, name: &'static str, arg: Option<String>) {
        match name {
            "scan-usb-devices" => self.pending.push(Action::ScanUsb),
            "conn-usb-devices" => {
                if let Some(dev) = arg {
                    self.pending.push(Action::ConnUsb(dev));
                }
            }
            // S9：面板开关就地处理（core 侧只是 UI 标志，不进动作队列）
            "open-settings-window" => panels::open(&mut self.state.panels, panels::Panel::Settings),
            "open-about" => panels::open(&mut self.state.panels, panels::Panel::About),
            // S14：手机/手环面板开关。每次打开重置扇区/块输入（对齐上游默认 0/1）。
            "open-phw-window" => {
                self.forms.phw = panels::PhwForm::default();
                panels::open(&mut self.state.panels, panels::Panel::PhoneWristband);
            }
            "input-keys-read-IC" => {
                // 原版 inputkeys.html 的输入框默认值 `ffffffffffff,`
                // 且每次打开都重置（`createInputKeysWindow` 每次新建窗口）。
                self.forms.input_keys.clear();
                self.forms.input_keys.push_str(panels::INPUT_KEYS_DEFAULT);
                panels::open(&mut self.state.panels, panels::Panel::InputKeys);
            }
            "hard-nested" => {
                // 预填检测结果（command.js:240-246）：有已知/未知密钥就带上，
                // 没有就开空面板（`catch` 分支 → 全禁用 + 提示）。
                self.forms.hard_nested.reset_from(&self.state);
                panels::open(&mut self.state.panels, panels::Panel::HardNested);
            }
            "dict-test" => {
                self.forms.dict_test.reset_from(&self.state);
                panels::open(&mut self.state.panels, panels::Panel::DictTest);
            }
            // S10：dump 三面板
            "open-dump-editor" => {
                // 原版 `command.js:412-413`：
                //   `createDumpEditorWindow(file ? path.join(dumpsFolder, file) : null)`
                // 带文件名进来（从历史面板点「编辑转储」）就直接载入该 dump，
                // 不带（工具菜单点开）则开空编辑器等用户选文件。
                self.forms.editor = panels::EditorForm::default();
                if let Some(name) = arg {
                    self.load_dump_into_editor(&name);
                }
                panels::open(&mut self.state.panels, panels::Panel::DumpEditor);
            }
            "open-dump-comparator" => {
                // 载荷是 `A` / `B` 两个文件名（`command.js:461`）；带参进来时
                // 直接载入两侧，不让用户再选一遍。
                self.forms.comparator = panels::ComparatorForm::default();
                if let Some(pair) = arg {
                    if let Some((a, b)) = pair.split_once('\u{1}') {
                        // 两侧都只接受裸文件名（同 `dumpfiles::delete/rename` 的防线）。
                        // 非法名时记错误但仍开面板——与 dump-editor「载入失败也开面板」
                        // 的行为保持一致；空面板 + 一条错误，用户自己再选。
                        if !nfc_core::dumpfiles::is_plain_name(a)
                            || !nfc_core::dumpfiles::is_plain_name(b)
                        {
                            self.push_event(EventKind::Error, "非法的转储文件名");
                        } else {
                            let dir = self.paths.dumpfiles_dir();
                            self.load_comparator_side(true, &dir.join(a));
                            self.load_comparator_side(false, &dir.join(b));
                        }
                    }
                }
                panels::open(&mut self.state.panels, panels::Panel::DumpComparator);
            }
            "dump-history" => {
                // 原版每次打开都重新 readdir（`command.js:498-503`），
                // 故清空勾选，避免选中已不存在的文件。
                self.forms.history_selected.clear();
                panels::open(&mut self.state.panels, panels::Panel::DumpHistory);
            }
            "cancel-task" => {
                // R3：空闲时 cancel 也要清计时 + 隐藏进度
                self.reset_timer(true);
                self.state.progress = None;
                self.pending.push(Action::Plain(name));
            }
            // S11-2：需要文件框 / 危险确认的动作**不直接派发**，改为先把
            // 系统弹窗投递到工作线程；结果回来后再走确认窗 → 派发。
            // 「选哪个文件」与「是否真的写」是两步，故这里只管第一步。
            "write-IC" => match arg {
                // 引导条「写这张卡」带 dumpId：原版直接跳到确认框
                // （command.js:127-130），故不弹文件框。
                Some(dump) => {
                    // 与 comparator 同一道防线：dumpId 是裸文件名
                    // （engine 会拼 dumpfiles/），带分隔符的一律拒绝。
                    if !nfc_core::dumpfiles::is_plain_name(&dump) {
                        self.push_event(EventKind::Error, "非法的转储文件名");
                    } else {
                        self.confirm = Some(dialogs_ui::ConfirmModal::new(
                            dialogs_ui::ConfirmKind::WriteDump { dump },
                            &dialogs_ui::danger_title(),
                            t("dialog_msg_confirm_write_dump"),
                        ));
                    }
                }
                None => self.open_file_for(dialogs_host::FilePurpose::WriteDump),
            },
            "write-ufuid-uid" => self.open_file_for(dialogs_host::FilePurpose::WriteUidDump),
            "lock-ufuid" => {
                self.confirm = Some(dialogs_ui::ConfirmModal::new(
                    dialogs_ui::ConfirmKind::Lock,
                    &dialogs_ui::danger_title(),
                    t("dialog_msg_card_will_be_locked"),
                ));
            }
            // S17：手机/手环两个写动作此前**没有**应用内确认窗，engine 侧
            // `confirm_danger` 拿不到预置值一律返回 false，等于按钮点了没反应。
            // 此处补上确认窗（正文含缓存的卡号 / 目标扇区块），确认后才派发。
            "phw-write-uid" => {
                let Some(block0) = self.state.phw.block0.clone() else {
                    self.pending.push(Action::Plain(name));
                    return;
                };
                self.confirm = Some(dialogs_ui::ConfirmModal::new(
                    dialogs_ui::ConfirmKind::PhwWriteUid {
                        block0: block0.clone(),
                    },
                    &dialogs_ui::danger_title(),
                    nfc_core::tasks::phw::write_uid_confirm(&block0).message,
                ));
            }
            "phw-write-data" => {
                let Some(block0) = self.state.phw.block0.clone() else {
                    self.pending.push(Action::Plain(name));
                    return;
                };
                let sector = self.state.phw.sector;
                let block = self.state.phw.block;
                self.confirm = Some(dialogs_ui::ConfirmModal::new(
                    dialogs_ui::ConfirmKind::PhwWriteData {
                        block0: block0.clone(),
                        sector,
                        block,
                    },
                    &dialogs_ui::danger_title(),
                    nfc_core::tasks::phw::write_data_confirm(
                        &block0,
                        nfc_core::tasks::phw::SectorBlock { sector, block },
                    )
                    .message,
                ));
            }
            "read-IC" | "detect-card-type" => match arg {
                Some(a) => self.pending.push(Action::WithArg(name, a)),
                None => self.pending.push(Action::Plain(name)),
            },
            // S17：格式化卡片补应用内确认窗（原版 `command.js:144-149` 直接开跑，
            // 误点即清空卡片）。确认后 engine 侧 `prepare_format` 再过一道闸。
            "format-card" => {
                self.confirm = Some(dialogs_ui::ConfirmModal::new(
                    dialogs_ui::ConfirmKind::FormatCard,
                    &dialogs_ui::danger_title(),
                    t("dialog_msg_confirm_format_card"),
                ));
            }
            // command.js:538-556。默认文件名是**裸名**（原版无 path.join），
            // 过滤器 txt。日志内容先快照存住——弹窗是异步的，
            // 等它回来时缓冲区可能已被追加。
            "save-log" => {
                let default =
                    PathBuf::from(format!("NFCTools_log_{}", nfc_core::paths::timestamp()));
                self.log_pending = Some(arg.unwrap_or_else(|| self.state.log_buffer.clone()));
                self.save_file_for(
                    dialogs_host::FilePurpose::SaveLog,
                    default,
                    vec![nfc_core::dialogs::FileFilter {
                        name: t("file_type_txt"),
                        extensions: vec!["txt".to_owned()],
                    }],
                );
            }
            "open-dict-file" => self.open_file_for(dialogs_host::FilePurpose::DictFile),
            // command.js:415-422：带路径则直接读，不弹框
            "dump-editor-choose-file" => match arg {
                Some(p) => {
                    // 带参数进来的是完整路径（面板内触发选文件的回调直接塞路径），
                    // 按路径载入而非 basename——否则外部 dump 会被 is_plain_name 拒。
                    self.load_dump_from_path(std::path::Path::new(&p));
                }
                None => self.open_file_for(dialogs_host::FilePurpose::EditorFile),
            },
            // command.js:435-458：`saveAs` 为真才弹框，过滤器是 mfd/dump
            "dump-editor-save" => {
                let save_as = arg.as_deref().map(|a| a == "1").unwrap_or(false);
                let current = self
                    .forms
                    .editor
                    .file_name
                    .clone()
                    .unwrap_or_else(|| "dump.mfd".to_owned());
                if save_as {
                    // 另存对话框默认落在 origin（打开时的目录/文件名），
                    // 未载入过文件才退回 dumpfiles/<current>。
                    let default = self
                        .forms
                        .editor
                        .origin
                        .clone()
                        .unwrap_or_else(|| self.paths.dumpfiles_dir().join(&current));
                    self.save_file_for(
                        dialogs_host::FilePurpose::EditorSaveAs,
                        default,
                        vec![nfc_core::dialogs::dump_filter(&t("file_type_dump"))],
                    );
                } else if let Some(origin) =
                    self.forms.editor.origin.clone().filter(|o| o.is_file())
                {
                    // 有完整来源（外部文件或 dumpfiles 内文件）且仍存在 → 写回原处。
                    // origin 被外部删除/改名（`!is_file`）时落到下方 dumpfiles 回落，
                    // 避免在已失效的旧路径上悄悄重建文件。
                    self.write_editor_to(&origin);
                } else {
                    // 无 origin（从未载入）或 origin 已失效：回落 dumpfiles/<file_name>。
                    if !nfc_core::dumpfiles::is_plain_name(&current) {
                        self.push_event(EventKind::Error, "非法的转储文件名");
                        return;
                    }
                    self.write_editor_to(&self.paths.dumpfiles_dir().join(&current));
                }
            }
            "dump-comparator-choose-file" => {
                // 载荷是 "A" / "B"（`dumpComparator.js` 的 type）
                let is_a = arg.as_deref() != Some("B");
                self.open_file_for(if is_a {
                    dialogs_host::FilePurpose::ComparatorA
                } else {
                    dialogs_host::FilePurpose::ComparatorB
                });
            }
            // command.js:491-495 / command.js:409：交给操作系统打开
            "open-dump-folder" | "open-history-keys" => {
                let target = if name == "open-history-keys" {
                    self.paths.keys_file()
                } else {
                    self.paths.dumpfiles_dir()
                };
                // 键文件首次运行时尚未创建，原版 `cp.exec` 会静默失败；
                // 此处记一条事件，让人知道为什么没反应。
                if let Err(msg) = open_in_os(&target) {
                    self.push_event(EventKind::Error, &msg);
                }
            }
            // 面板确认类（S9 面板按钮直接构造带配置的动作，不走字符串 arg）
            "done-input-keys-read-IC" => {
                if let Some(a) = arg {
                    self.pending.push(Action::WithArg(name, a));
                }
            }
            // S18：CUID 空卡写入。带 dumpId（引导条带）直接进确认窗，
            // 不带则弹文件框选源 dump。
            "cuid-write" => match arg {
                Some(dump) => {
                    self.confirm = Some(
                        dialogs_ui::ConfirmModal::new(
                            dialogs_ui::ConfirmKind::CuidWrite { dump },
                            &dialogs_ui::danger_title(),
                            t("dialog_msg_confirm_cuid_write"),
                        )
                        .with_code(dialogs_ui::random_code()),
                    );
                }
                None => self.open_file_for(dialogs_host::FilePurpose::CuidDump),
            },
            // S18：手输卡号写 UFUID。开 UidInput 面板，载荷是初始文本（空串）。
            "input-uid-write" => {
                self.forms.input_uid = String::new();
                panels::open(&mut self.state.panels, panels::Panel::UidInput);
            }
            // S18：面板「确定」后在此先过 app 层确认（危险操作随机码 B6），
            // 确认后 `on_confirm_result` 才把输入文本作为载荷派发给 engine
            // （engine 侧 `input_uid_write` 再做清洗 / 拼块 / BCC 判定）。
            // 正文复用 core 的 `input_uid_confirm`，让用户在一处看到与真正
            // 写入一致的 0 块 hex（含强制 BCC 警示）——与 `phw-write-uid`
            // 用 `write_uid_confirm` 构造正文的做法一致。
            "done-input-uid-write" => {
                let Some(text) = arg else {
                    return;
                };
                // 与 engine `input_uid_write` 同源：清洗 → 拼 0 块 → 判 BCC。
                // 非法输入不弹确认框，只记一条错误。
                let Some(cleaned) = nfc_core::tasks::card_write::clean_uid_input(&text) else {
                    self.state.apply(Event::Log(format!(
                        "\n{}\n",
                        t("dialog_msg_invalid_uid_input")
                    )));
                    self.push_event(EventKind::Error, &t("dialog_msg_invalid_uid_input"));
                    return;
                };
                let block0 = if cleaned.len() == 8 {
                    nfc_core::tasks::card_write::uid_to_block0(&cleaned)
                } else {
                    cleaned
                };
                let force_bcc = !nfc_core::tasks::phw::block0_bcc_ok(&block0);
                self.confirm = Some(
                    dialogs_ui::ConfirmModal::new(
                        dialogs_ui::ConfirmKind::UidInputWrite { text },
                        &dialogs_ui::danger_title(),
                        nfc_core::tasks::card_write::input_uid_confirm(&block0, force_bcc).message,
                    )
                    .with_code(dialogs_ui::random_code()),
                );
            }
            // S18：编辑器体检/修复/导出。check/fix 是纯 UI 动作（不进队列）；
            // export 弹保存框，把当前编辑器内容的密钥字典另存为文件。
            "dump-editor-export-keys" => {
                let default =
                    PathBuf::from(format!("NFCTools_keys_{}", nfc_core::paths::timestamp()));
                self.save_file_for(
                    dialogs_host::FilePurpose::ExportKeys,
                    default,
                    vec![nfc_core::dialogs::FileFilter {
                        name: t("file_type_txt"),
                        extensions: vec!["txt".to_owned()],
                    }],
                );
            }
            // S18：全加密爆破（mfcuk）。纯启动，无文件框、无确认窗。
            "run-mfcuk" => self.pending.push(Action::Plain(name)),
            // 面板类（S10）与 about（S11）暂由后续里程碑接管
            other => self.pending.push(Action::Plain(other)),
        }
    }
}

/// 把文件 / 目录交给操作系统打开。
///
/// 对标 `shell.openPath`（`command.js:493`）与
/// `cp.exec("notepad.exe" / "open")`（`command.js:409`）。用 `open` crate 而
/// 不是手搓 `Command`：它按平台选好打开方式，且**不等待**子进程——后者很关键，
/// 文本编辑器会一直开着，等它退出等于卡死。
fn open_in_os(target: &std::path::Path) -> Result<(), String> {
    open::that_detached(target).map_err(|e| format!("{target:?}: {e}"))
}

/// 事件条时间戳（`index.js:118` 的 `HH:MM:SS`，本地时区，S18 F3 修正）。
///
/// 实现委托 [`nfc_core::paths::event_stamp`]（原版 `getHours()` 即本地时区）。
pub fn event_stamp() -> String {
    nfc_core::paths::event_stamp()
}

/// 秒 → `HH:MM:SS`（`index.js:43-47`）。
pub fn format_duration(total: u32) -> String {
    let (h, m, s) = (total / 3600, (total / 60) % 60, total % 60);
    format!("{h:02}:{m:02}:{s:02}")
}

/// 控制台头部的普通按钮：文案 + 悬停说明。
fn tip_button(ui: &mut Ui, text: &str, tip: &str) -> bool {
    ui.button(text).on_hover_text(tip).clicked()
}

/// 标题栏的可拖拽空白区间：`bar` 去掉 `blockers` 覆盖的横向范围。
///
/// 拖拽区必须避开所有可点击控件：egui 命中测试会把压在最上层的纯拖拽
/// 控件当成按下目标，按钮因此收不到点击；只有点在按钮外
/// `interact_radius`（默认 5px）内，才会回退命中最近的按钮——这正是
/// 「只有贴着边框点才有效」的成因。故按控件矩形把标题栏切成空白段。
fn title_bar_drag_spans(bar: egui::Rect, blockers: &[egui::Rect]) -> Vec<egui::Rect> {
    let mut cuts: Vec<(f32, f32)> = blockers
        .iter()
        .map(|r| {
            (
                r.left().clamp(bar.left(), bar.right()),
                r.right().clamp(bar.left(), bar.right()),
            )
        })
        .filter(|(l, r)| r > l)
        .collect();
    cuts.sort_by(|a, b| a.0.total_cmp(&b.0));

    let mut spans = Vec::new();
    let mut x = bar.left();
    for (left, right) in cuts {
        if left > x {
            spans.push(egui::Rect::from_min_max(
                egui::pos2(x, bar.top()),
                egui::pos2(left, bar.bottom()),
            ));
        }
        x = x.max(right);
    }
    if x < bar.right() {
        spans.push(egui::Rect::from_min_max(
            egui::pos2(x, bar.top()),
            egui::pos2(bar.right(), bar.bottom()),
        ));
    }
    spans
}

// ───────────────────────────── 布局 ─────────────────────────────

impl NfcApp {
    /// 主窗口内容。自绘标题栏故不用 egui 的 `TopBottomPanel`——
    /// 那会额外画一条分隔线，与原版「无边框、整体自绘」不符。
    pub fn draw(&mut self, ui: &mut Ui) {
        // 每帧推进在 `App::logic` 里做（早于本方法）：窗被最小化/遮挡时
        // eframe 只调 `logic` 不调 `ui`，放在这里会让子进程输出无人排空。
        self.title_bar(ui);
        if !self.tool_dir_ok {
            self.missing_tools_bar(ui);
        }
        self.guidance_bar(ui);
        // 主操作区包一层垂直滚动：窗口过矮（820×640 / 缺工具条）时按钮区自身
        // 滚动，而不是把最底部的任务条/进度条挤出视口。
        // 为控制台预留最小高度；内容不足 max_height 时按内容收缩，空间全给控制台。
        // 事件列表展开时事件区多占 EVENT_LIST_MAX_H，`log_h` 的 `LOG_MIN_H` 下限
        // 吸收不了这 96px，故预留要跟着加大，否则任务条仍会被挤出视口。
        let reserve = CONSOLE_RESERVE_H
            + if self.events_expanded {
                EVENT_LIST_MAX_H
            } else {
                0.0
            };
        let actions_max_h = (ui.available_height() - reserve - 6.0).max(160.0);
        ScrollArea::vertical()
            .id_salt("main-actions")
            .max_height(actions_max_h)
            .auto_shrink([false, true])
            .show(ui, |ui| self.button_area(ui));
        ui.add_space(6.0);
        self.console(ui);
    }

    /// `framework/bin` 缺失时的红条（S12 交付项：启动自检引导）。
    ///
    /// 刻意偏离原版：原版没有这一步，缺失只会在用户点按钮后于 `spawn`
    /// 抛一条看不懂的 IO 错误。
    fn missing_tools_bar(&mut self, ui: &mut Ui) {
        let paint = self.paint();
        Frame::NONE
            .fill(paint.color(paint.danger))
            .inner_margin(egui::Margin::symmetric(8, 6))
            .show(ui, |ui| {
                ui.vertical(|ui| {
                    ui.label(RichText::new(t("html_framework_bin_missing")).strong());
                    ui.label(format!(
                        "{} {}\n{}",
                        t("html_framework_bin_missing_short"),
                        self.missing_tools.join("、"),
                        self.bin_dir.display()
                    ));
                });
            });
    }

    /// 自绘标题栏：左 logo + 应用名 + 设备下拉/状态胶囊 + 工具菜单，
    /// 右侧为主题切换 / 设置 / 最小化 / 关闭（R2 无边框自绘）。
    ///
    /// 无边框窗口原生不可拖动，故把「可点击控件之外」的空白区间做成拖拽区。
    /// 拖拽区**绝不能覆盖按钮**：egui 命中测试会把最上层的纯拖拽控件当成
    /// 按下目标，按钮会收不到点击（详见 `title_bar_drag_spans`）。
    fn title_bar(&mut self, ui: &mut Ui) {
        let paint = self.paint();
        // 可点击控件的位置：拖拽区只在这些矩形之外生成。
        // 新增标题栏控件时必须登记，否则会被拖拽区盖住、点不动。
        let mut blockers: Vec<egui::Rect> = Vec::new();
        let bar = Frame::NONE
            .fill(paint.color(paint.toolbar))
            .inner_margin(egui::Margin::symmetric(10, 6))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    // 左上角 logo（20px，见 `main.rs::load_title_logo`）。
                    if let Some(tex) = &self.logo {
                        ui.add(egui::Image::new(tex).max_size(egui::vec2(20.0, 20.0)));
                    }
                    ui.label(RichText::new(t("app_name")).strong().size(15.0))
                        .on_hover_text(format!(
                            "{} {} {}",
                            t("html_current_version"),
                            env!("APP_VERSION"),
                            env!("BUILDER")
                        ));
                    blockers.push(self.device_combo(ui).interact_rect);
                    self.device_status_chip(ui);
                    blockers.push(self.tools_button(ui).interact_rect);
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let close = ui
                            .add_sized(
                                [32.0, 24.0],
                                egui::Button::new(RichText::new("×").size(14.0)),
                            )
                            .on_hover_text(t("html_close"));
                        blockers.push(close.interact_rect);
                        if close.clicked() {
                            self.close_requested = true;
                        }
                        let minimize = ui
                            .add_sized(
                                [32.0, 24.0],
                                egui::Button::new(RichText::new("–").size(14.0)),
                            )
                            .on_hover_text(t("html_minimize"));
                        blockers.push(minimize.interact_rect);
                        if minimize.clicked() {
                            ui.ctx()
                                .send_viewport_cmd(egui::ViewportCommand::Minimized(true));
                        }
                        blockers.push(self.theme_toggle_button(ui).interact_rect);
                        // S16：设置只保留标题栏一个入口，工具菜单不再重复挂。
                        let settings = ui.button(RichText::new(t("html_settings")));
                        blockers.push(settings.interact_rect);
                        if settings.clicked() {
                            self.exec("open-settings-window", None);
                        }
                    });
                });
            })
            .response
            .rect;
        // `CLICK | DRAG` 不带 `FOCUSABLE`，不会给空白标题栏制造键盘焦点。
        for (i, span) in title_bar_drag_spans(bar, &blockers).into_iter().enumerate() {
            let drag = ui.interact(
                span,
                ui.id().with(("title_bar_drag", i)),
                Sense::CLICK | Sense::DRAG,
            );
            if drag.drag_started_by(egui::PointerButton::Primary) {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
            }
        }
    }

    /// 标题栏主题切换钮：自绘小圆点，深 = 实心、浅 = 空心、系统 = 半实心。
    ///
    /// 用 `Painter` 画而非 `Button`：三种态里一种是「半个圆」，表情域字符
    /// （如 ☾/◐）跨平台渲染不稳定，自绘最稳。返回响应供标题栏登记可点击区。
    fn theme_toggle_button(&mut self, ui: &mut Ui) -> egui::Response {
        let paint = self.paint();
        let mode = self.theme_mode;
        // 悬停文案带上当前模式名，点之前就知道会切到哪
        let mode_key = match mode {
            ThemeMode::Dark => "html_toggle_dark_mode",
            ThemeMode::Light => "html_toggle_light_mode",
            ThemeMode::System => "html_follow_system_mode",
        };
        let tip = format!("{}（{}）", t("html_switch_theme"), t(mode_key));
        let (rect, resp) = ui.allocate_exact_size(egui::vec2(32.0, 24.0), egui::Sense::click());
        let c = rect.center();
        let fg = paint.color(paint.text);
        let painter = ui.painter();
        match mode {
            ThemeMode::Dark => {
                painter.circle_filled(c, 7.0, fg);
            }
            ThemeMode::Light => {
                painter.circle_stroke(c, 7.0, egui::Stroke::new(1.5, fg));
            }
            ThemeMode::System => {
                // 左半实心 + 全圈描边，示意「跟随」
                let left = egui::Rect::from_min_size(
                    rect.min,
                    egui::vec2(rect.width() / 2.0, rect.height()),
                );
                painter.with_clip_rect(left).circle_filled(c, 7.0, fg);
                painter.circle_stroke(c, 7.0, egui::Stroke::new(1.5, fg));
            }
        }
        let resp = resp.on_hover_text(tip);
        if resp.clicked() {
            self.cycle_theme();
        }
        resp
    }

    /// 主题三态循环：深 → 浅 → 系统 → 深（标题栏小圆钮用）。
    pub fn cycle_theme(&mut self) {
        self.theme_mode = match self.theme_mode {
            ThemeMode::Dark => ThemeMode::Light,
            ThemeMode::Light => ThemeMode::System,
            ThemeMode::System => ThemeMode::Dark,
        };
    }

    /// 设备下拉。R5 连接中禁用。返回按钮响应，供标题栏登记可点击区。
    fn device_combo(&mut self, ui: &mut Ui) -> egui::Response {
        // ComboBox 的闭包借用 `self` 时，`self.state` 已被不可变借用，
        // 故先把要用的数据取到局部变量。
        let devices = self.state.device_list.clone();
        let current = self.state.current_device.clone();
        let mut picked: Option<String> = None;
        // R5：连接中禁用整个下拉（不只是清空列表）。
        // 用 `add_enabled_ui` 而非 `add_enabled`：后者要求闭包本身是一个
        // `Widget`——`ComboBox::show_ui` 返回的是 `InnerResponse`，两者不兼容。
        let resp = ui
            .add_enabled_ui(!self.connecting_now(), |ui| {
                egui::ComboBox::from_id_salt("usb-port")
                    .width(220.0)
                    .selected_text(
                        current
                            .as_deref()
                            .map(|d| d.replace(DEVICE_PREFIX, ""))
                            .filter(|s| !s.is_empty() && s != DEVICE_NONE)
                            .unwrap_or_else(|| t("html_choose_device")),
                    )
                    .show_ui(ui, |ui| {
                        for dev in &devices {
                            // 对标 index.js:79 的
                            // `v.replace(mainProcess.devicePrefix, "")`
                            // ——是**子串**替换而非前缀剥离。
                            let label = dev.replace(DEVICE_PREFIX, "");
                            if ui
                                .selectable_label(current.as_deref() == Some(dev.as_str()), label)
                                .clicked()
                            {
                                picked = Some(dev.clone());
                            }
                        }
                    })
                    .response
            })
            .inner;
        // 闭包结束后借用释放，才可再可变借用 self。
        if let Some(dev) = picked {
            self.exec("conn-usb-devices", Some(dev));
        }
        resp
    }

    /// 工具按钮 + 下拉菜单（`index.html:25-36` 的 `.tools-list`）。
    ///
    /// **刻意偏离**：原版菜单只有 5 项（`index.html:31-35`），而 settings
    /// 的入口按钮在 `index.html:38-40` 被**注释掉了**——即原版的
    /// `createSettingsWindow` / `settings.html` / `open-settings-window`
    /// 三个部分都实现好了却从主窗口进不去。此处把 settings 作为第 6 项
    /// 挂在菜单末尾，否则 S9 实现的 settings 面板是死代码。
    /// 登记于 docs/02 §6.6。
    ///
    /// 用 `ComboBox` 承载而非自绘：它自带「点外面自动收起」，与原版
    /// `dropdown-menu` 的行为一致，省掉手写开关逻辑。
    ///
    /// 返回按钮响应，供标题栏登记可点击区。
    fn tools_button(&mut self, ui: &mut Ui) -> egui::Response {
        /// 菜单项（action, i18n key）。S16：恢复原版 5 项；设置入口改由标题栏承担。
        const ITEMS: [(&str, &str); 5] = [
            ("open-history-keys", "html_edit_history_key"),
            ("open-dump-editor", "html_dump_editor"),
            ("open-dump-comparator", "html_dump_comparator"),
            ("dump-history", "html_dump_history"),
            ("open-about", "html_about"),
        ];

        // S15：文字 + ComboBox 自带箭头，替掉表情符号 🛠（跨平台渲染漂移）
        egui::ComboBox::from_id_salt("tools-menu")
            .selected_text(RichText::new(t("html_tools")))
            .show_ui(ui, |ui| {
                for (action, key) in ITEMS {
                    if ui.selectable_label(false, t(key)).clicked() {
                        self.exec(action, None);
                    }
                }
            })
            .response
    }

    /// S16：设备状态胶囊。合并原来独立的设备状态条，避免“下拉 + 状态条”两处重复。
    fn device_status_chip(&self, ui: &mut Ui) {
        let paint = self.paint();
        let dot = paint.device_dot(self.state.device_state);
        let text = match self.state.device_state {
            DeviceState::None => t("html_no_device"),
            DeviceState::Connecting => t("html_device_connecting"),
            DeviceState::Connected => t("html_device_connected"),
            DeviceState::Failed => t("html_device_failed"),
        };
        let inner = ui.horizontal(|ui| {
            ui.colored_label(dot, RichText::new("●").size(10.0));
            ui.label(RichText::new(&text).size(12.0));
        });
        inner.response.on_hover_text(&text);
    }

    /// S15：冷启动引导。未连接设备时展示一次「请先选择设备并连接」+
    /// 「刷新设备列表」按钮；点掉按钮或设备连上后即收起，且不会重复弹出。
    pub fn maybe_cold_start_guidance(&mut self) {
        let none = matches!(self.state.device_state, DeviceState::None);
        // 冷启动引导的唯一标记：按钮动作是 `scan-usb-devices`
        let is_cold_hint = self
            .guidance
            .as_ref()
            .is_some_and(|(_, btns)| btns.iter().any(|(a, _, _)| *a == "scan-usb-devices"));
        if is_cold_hint && !none {
            self.guidance = None;
            self.cold_guided = true;
            return;
        }
        if none && !self.cold_guided {
            self.cold_guided = true;
            self.guidance = Some((
                t("html_guidance_connect_first"),
                vec![("scan-usb-devices", t("html_refresh_device_list"), None)],
            ));
        }
    }

    /// 引导条（`index.js:52-63`）。S16：改为可换行卡片，避免窄窗口溢出。
    fn guidance_bar(&mut self, ui: &mut Ui) {
        let paint = self.paint();
        let Some((text, buttons)) = self.guidance.clone() else {
            return;
        };
        Frame::NONE
            .fill(paint.color(paint.surface2))
            .corner_radius(egui::CornerRadius::same(nfc_core::theme::RADIUS[1] as u8))
            .inner_margin(egui::Margin::symmetric(12, 8))
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(text).size(nfc_core::theme::FONT_SIZE[1]));
                    for (action, label, arg) in buttons {
                        if ui.button(label).clicked() {
                            self.exec(action, arg.clone());
                            self.guidance = None;
                        }
                    }
                });
            });
    }

    /// 三列按钮区（`index.html:73-95`）：读卡 / 破解 / 写卡。
    ///
    /// S16：三列改为卡片化，按“工作流步骤”编号；按钮增加二级样式；破解列
    /// 增加检测结果摘要；写卡列把手机/手环独立成“工作流”分组，UFUID 组仍
    /// 放在最后并以危险按钮收尾。
    fn button_area(&mut self, ui: &mut Ui) {
        /// 列内元素：按钮、分组小标题，或检测结果摘要。
        enum Item<'a> {
            Btn(&'a str, String, BtnStyle, &'a str),
            Group(&'a str),
            DetectSummary,
        }
        /// 按钮样式档位。S16 在原版三档之外补了“次主操作”。
        #[derive(Clone, Copy, PartialEq)]
        enum BtnStyle {
            Normal,
            Primary,
            Secondary,
            Danger,
        }

        let known = self.state.known_key_info.len();
        let unknown = self.state.unknown_key_info.len();
        let cols: [(String, Vec<Item<'_>>); 3] = [
            (
                t("html_read_area"),
                vec![
                    Item::Btn(
                        "read-IC",
                        t("html_read_card"),
                        BtnStyle::Primary,
                        "html_tip_read_ic",
                    ),
                    Item::Btn(
                        "input-keys-read-IC",
                        t("html_read_card_with_known_key"),
                        BtnStyle::Normal,
                        "html_tip_read_known_key",
                    ),
                    Item::Btn(
                        "detect-card-type",
                        t("html_detect_card"),
                        BtnStyle::Secondary,
                        "html_tip_detect_card",
                    ),
                ],
            ),
            (
                t("html_crack_area"),
                vec![
                    Item::Btn(
                        "hard-nested",
                        t("html_hard_nested"),
                        BtnStyle::Normal,
                        "html_tip_hard_nested",
                    ),
                    Item::Btn(
                        "dict-test",
                        t("html_test_dictionary"),
                        BtnStyle::Normal,
                        "html_tip_dict_test",
                    ),
                    Item::DetectSummary,
                ],
            ),
            (
                t("html_write_area"),
                vec![
                    Item::Btn(
                        "write-IC",
                        t("html_write_card"),
                        BtnStyle::Primary,
                        "html_tip_write_ic",
                    ),
                    Item::Btn(
                        "format-card",
                        t("html_format_card"),
                        BtnStyle::Danger,
                        "html_tip_format_card",
                    ),
                    Item::Group("html_workflow_group"),
                    Item::Btn(
                        "open-phw-window",
                        t("html_phw_open"),
                        BtnStyle::Normal,
                        "html_tip_phw",
                    ),
                    Item::Group("html_ufuid_group"),
                    Item::Btn(
                        "write-ufuid-uid",
                        t("html_write_ufuid_uid"),
                        BtnStyle::Normal,
                        "html_tip_write_ufuid_uid",
                    ),
                    Item::Btn(
                        "lock-ufuid",
                        t("html_lock_ufuid"),
                        BtnStyle::Danger,
                        "html_tip_lock_ufuid",
                    ),
                    // S18：CUID 空卡写入 / 手输卡号 / 全加密爆破
                    Item::Btn(
                        "cuid-write",
                        t("html_cuid_write"),
                        BtnStyle::Normal,
                        "html_tip_cuid_write",
                    ),
                    Item::Btn(
                        "input-uid-write",
                        t("html_write_uid_input"),
                        BtnStyle::Normal,
                        "html_tip_write_uid_input",
                    ),
                    Item::Btn(
                        "run-mfcuk",
                        t("html_mfcuk_run"),
                        BtnStyle::Normal,
                        "html_tip_mfcuk_run",
                    ),
                ],
            ),
        ];
        let paint = self.paint();
        let primary = paint.color(paint.primary);
        let danger_fill = paint.color(paint.danger_strong);
        let text_inverse = paint.color(paint.text_inverse);
        let surface = paint.color(paint.surface);
        let surface2 = paint.color(paint.surface2);
        let text = paint.color(paint.text);
        let text_secondary = paint.color(paint.text_secondary);
        let divider = paint.color(paint.divider);

        ui.spacing_mut().item_spacing.x = nfc_core::theme::SPACING[2];
        ui.columns(cols.len(), |cols_ui| {
            for (i, (title, items)) in cols.iter().enumerate() {
                let ui = &mut cols_ui[i];
                Frame::NONE
                    .fill(surface)
                    .corner_radius(egui::CornerRadius::same(nfc_core::theme::RADIUS[1] as u8))
                    .inner_margin(egui::Margin::symmetric(12, 10))
                    .show(ui, |ui| {
                        ui.set_min_height(220.0);
                        ui.spacing_mut().item_spacing.y = 6.0;
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(format!("{} {}", i + 1, title))
                                    .strong()
                                    .size(nfc_core::theme::FONT_SIZE[3]),
                            );
                        });
                        ui.add_space(8.0);
                        for item in items {
                            match item {
                                Item::Btn(action, label, style, tip) => {
                                    let (fill, fg, h, fs, strong, secondary) = match style {
                                        BtnStyle::Primary => {
                                            (primary, text_inverse, 40.0, 15.0, true, false)
                                        }
                                        BtnStyle::Secondary => (
                                            egui::Color32::TRANSPARENT,
                                            primary,
                                            34.0,
                                            13.0,
                                            false,
                                            true,
                                        ),
                                        BtnStyle::Danger => {
                                            (danger_fill, text_inverse, 34.0, 13.0, false, false)
                                        }
                                        BtnStyle::Normal => {
                                            (surface2, text, 30.0, 13.0, false, false)
                                        }
                                    };
                                    let w = ui.available_width();
                                    let mut label = RichText::new(label).color(fg).size(fs);
                                    if strong {
                                        label = label.strong();
                                    }
                                    let mut btn = egui::Button::new(label)
                                        .fill(fill)
                                        .min_size(egui::vec2(w, h));
                                    if secondary {
                                        btn = btn.stroke(egui::Stroke::new(1.0, primary));
                                    }
                                    // 按 `command` 门禁置灰：未连接设备 / 有任务运行中 /
                                    // 正在连接时不响应点击，并在悬停时说明原因——
                                    // 取代「点了之后才在日志里看到报错」。
                                    let gate = nfc_core::command::spec(action)
                                        .map(|s| nfc_core::command::gate(s, &self.state));
                                    let enabled = match &gate {
                                        Some(Err(_)) => false,
                                        Some(Ok(())) | None => true,
                                    };
                                    let reason = match &gate {
                                        Some(Err(e)) => t(e.i18n_key()),
                                        _ => String::new(),
                                    };
                                    let r = if enabled {
                                        ui.add_enabled(true, btn).on_hover_text(t(tip))
                                    } else {
                                        ui.add_enabled(false, btn).on_disabled_hover_text(reason)
                                    };
                                    if r.clicked() {
                                        self.exec(action, None);
                                    }
                                }
                                Item::Group(label) => {
                                    ui.add_space(6.0);
                                    let full = ui.max_rect();
                                    let r = ui
                                        .vertical_centered(|ui| {
                                            ui.label(
                                                RichText::new(t(label))
                                                    .color(text_secondary)
                                                    .size(nfc_core::theme::FONT_SIZE[0]),
                                            );
                                        })
                                        .response
                                        .rect;
                                    let y = r.center().y;
                                    let stroke = egui::Stroke::new(1.0, divider);
                                    ui.painter().hline(
                                        egui::Rangef::new(full.left(), r.left() - 8.0),
                                        y,
                                        stroke,
                                    );
                                    ui.painter().hline(
                                        egui::Rangef::new(r.right() + 8.0, full.right()),
                                        y,
                                        stroke,
                                    );
                                    ui.add_space(2.0);
                                }
                                Item::DetectSummary => {
                                    ui.add_space(8.0);
                                    Frame::NONE
                                        .fill(surface2)
                                        .corner_radius(egui::CornerRadius::same(
                                            nfc_core::theme::RADIUS[0] as u8,
                                        ))
                                        .inner_margin(egui::Margin::symmetric(8, 6))
                                        .show(ui, |ui| {
                                            ui.label(
                                                RichText::new(t("html_detect_summary"))
                                                    .strong()
                                                    .size(nfc_core::theme::FONT_SIZE[0]),
                                            );
                                            if known == 0 && unknown == 0 {
                                                ui.label(
                                                    RichText::new(t("html_not_detected"))
                                                        .color(text_secondary)
                                                        .size(nfc_core::theme::FONT_SIZE[0]),
                                                );
                                            } else {
                                                ui.label(
                                                    RichText::new(format!(
                                                        "{} {known} / {} {unknown}",
                                                        t("html_detect_known"),
                                                        t("html_detect_unknown")
                                                    ))
                                                    .size(nfc_core::theme::FONT_SIZE[0]),
                                                );
                                            }
                                        });
                                }
                            }
                            ui.add_space(6.0);
                        }
                    });
            }
        });
    }

    /// S16：控制台 = 头部工具条 + 最近事件（可展开）+ 日志 + 任务条。
    fn console(&mut self, ui: &mut Ui) {
        let paint = self.paint();
        self.console_header(ui);
        self.event_strip(ui);

        // 日志高度吃掉剩余空间；再扣任务条、间距与日志卡片的内边距。
        let log_h = (ui.available_height() - TASK_BAR_H - 16.0 - 12.0).max(LOG_MIN_H);
        self.log_panel(ui, log_h, paint);
        ui.add_space(4.0);
        self.task_bar(ui, paint);
    }

    /// 控制台头部：左侧标题，右侧自动滚动 / 停止 / 复制 / 保存 / 清空。
    fn console_header(&mut self, ui: &mut Ui) {
        let paint = self.paint();
        let danger = paint.color(paint.danger);
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(t("html_console"))
                    .strong()
                    .size(nfc_core::theme::FONT_SIZE[2]),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if tip_button(ui, &t("html_clear_log"), &t("html_tip_clear_log")) {
                    self.state.log_buffer.clear();
                }
                if tip_button(ui, &t("html_save_log"), &t("html_tip_save_log")) {
                    let text = self.state.log_buffer.clone();
                    self.exec("save-log", Some(text));
                }
                if tip_button(ui, &t("html_copy_log"), &t("html_tip_copy_log")) {
                    ui.ctx().copy_text(self.state.log_buffer.clone());
                }
                let stop =
                    egui::Button::new(RichText::new(t("html_stop_task")).color(danger).strong());
                if ui
                    .add_enabled(self.state.is_running_task, stop)
                    .on_hover_text(t("html_tip_stop_task"))
                    .on_disabled_hover_text(t("html_tip_stop_idle"))
                    .clicked()
                {
                    self.exec("cancel-task", None);
                }
                // S16：把“锁滚动/自动滚动”改成带勾选状态的自动滚动开关。
                let mut auto = !self.lock_scroll;
                if ui
                    .checkbox(&mut auto, t("html_unlock_scroll"))
                    .on_hover_text(t("html_tip_autoscroll"))
                    .changed()
                {
                    self.lock_scroll = !auto;
                }
            });
        });
    }

    /// 最近事件一行 + 可展开的全量事件列表。
    fn event_strip(&mut self, ui: &mut Ui) {
        if self.state.events.is_empty() {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(t("html_no_events"))
                        .color(self.paint().color(self.paint().text_secondary)),
                );
            });
            return;
        }

        let last = self.state.events.last().expect("events 非空");
        let tag = match last.kind {
            EventKind::Key => t("html_event_key"),
            EventKind::Success => t("html_event_success"),
            EventKind::Error => t("html_event_error"),
        };
        let color = event_color(self.theme, last.kind);
        let latest = RichText::new(format!("[{tag}] {} {}", event_stamp(), last.text)).color(color);

        ui.horizontal(|ui| {
            // 右侧先放展开/收起，再把最新事件截断在中间，避免长事件把按钮挤没。
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let toggle = if self.events_expanded {
                    t("html_collapse")
                } else {
                    t("html_expand")
                };
                if ui.small_button(toggle).clicked() {
                    self.events_expanded = !self.events_expanded;
                }
                ui.add(egui::Label::new(latest).truncate())
                    .on_hover_text(&last.text);
                ui.label(
                    RichText::new(t("html_events"))
                        .strong()
                        .size(nfc_core::theme::FONT_SIZE[0]),
                );
            });
        });

        if self.events_expanded {
            ScrollArea::vertical()
                .id_salt("event-list")
                .stick_to_bottom(!self.lock_scroll)
                .max_height(EVENT_LIST_MAX_H)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    for e in &self.state.events {
                        let tag = match e.kind {
                            EventKind::Key => t("html_event_key"),
                            EventKind::Success => t("html_event_success"),
                            EventKind::Error => t("html_event_error"),
                        };
                        let color = event_color(self.theme, e.kind);
                        ui.label(
                            RichText::new(format!("[{tag}] {} {}", event_stamp(), e.text))
                                .color(color),
                        )
                        .on_hover_text(&e.text);
                    }
                });
        }
    }

    /// 日志区：`console_bg` 卡片 + 可选中复制的多行等宽输入，保留自动滚底 / 锁滚动。
    fn log_panel(&mut self, ui: &mut Ui, log_h: f32, paint: theme_ui::Paint) {
        Frame::NONE
            .fill(paint.color(paint.console_bg))
            .corner_radius(egui::CornerRadius::same(nfc_core::theme::RADIUS[0] as u8))
            .inner_margin(egui::Margin::symmetric(8, 6))
            .show(ui, |ui| {
                let mut editable = self.state.log_buffer.clone();
                ScrollArea::vertical()
                    .id_salt("log")
                    .stick_to_bottom(!self.lock_scroll)
                    .max_height(log_h)
                    .show(ui, |ui| {
                        ui.add(
                            egui::TextEdit::multiline(&mut editable)
                                .desired_width(f32::INFINITY)
                                .font(egui::TextStyle::Monospace)
                                .interactive(true),
                        );
                    });
            });
    }

    /// 任务条：状态 + 进度 + 计时；系统文件框等待提示也挂在这里。
    fn task_bar(&mut self, ui: &mut Ui, paint: theme_ui::Paint) {
        let dot = paint.status_dot(self.state.status.indicator);
        let text = if self.state.status.text.is_empty() {
            t("indicator_free")
        } else {
            self.state.status.text.clone()
        };
        let timer = self.timer_text().unwrap_or_else(|| "--:--:--".to_owned());
        let frac = self
            .state
            .progress
            .map_or(0.0, |p| p.clamp(0.0, 100.0) / 100.0);

        ui.horizontal(|ui| {
            ui.horizontal(|ui| {
                ui.colored_label(dot, RichText::new("●").size(10.0))
                    .on_hover_text(&text);
                ui.label(text.clone()).on_hover_text(&text);
                if let Some(p) = self.dialogs_host.pending_purpose() {
                    ui.separator();
                    dialogs_ui::pending_hint(ui, p.pending_text());
                }
            });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(format!("{} {timer}", t("html_timer")));
                ui.separator();
                let progress = egui::ProgressBar::new(frac).fill(paint.color(paint.primary));
                let progress = if self.state.progress.is_some() {
                    progress.show_percentage()
                } else {
                    progress
                };
                ui.add(progress);
            });
        });
    }
}

impl eframe::App for NfcApp {
    /// eframe 0.36 的 `App::ui` 拿到的是根 `Ui`（不是 `Context`），
    /// 绘制主窗口；而面板这类浮层要另拿 `Context`，故走 `logic` 这个钩子。
    ///
    /// **注意顺序**：eframe 在 `update` 里先调 `logic` 再调 `ui`
    /// （`epi_integration.rs` 里两个 `profiling::scope!` 的顺序），
    /// 与旧注释写的相反。更要紧的是窗被**最小化或遮挡**时 eframe 走
    /// `update_logic_only`，**只调 `logic`、根本不调 `ui`**——
    /// 所以每帧推进（`tick`：pump 子进程输出 / 收文件框结果 / 弹挂起的保存框 /
    /// 推进计时器与热插拔轮询）必须放在 `logic` 里；放在 `draw`（即 `ui`）里
    /// 的后果是用户一最小化，子进程输出就没人排空，进程变僵尸、进度条冻结。
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.logic_body(ctx);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.draw(ui);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(left: f32, right: f32) -> egui::Rect {
        egui::Rect::from_min_max(egui::pos2(left, 0.0), egui::pos2(right, 30.0))
    }

    #[test]
    fn drag_spans_without_blockers() {
        let spans = title_bar_drag_spans(rect(0.0, 100.0), &[]);
        assert_eq!(spans, vec![rect(0.0, 100.0)]);
    }

    #[test]
    fn drag_spans_split_around_controls() {
        let spans = title_bar_drag_spans(rect(0.0, 100.0), &[rect(10.0, 20.0), rect(30.0, 40.0)]);
        assert_eq!(
            spans,
            vec![rect(0.0, 10.0), rect(20.0, 30.0), rect(40.0, 100.0)]
        );
    }

    #[test]
    fn drag_spans_merge_overlaps_and_clamp() {
        let spans = title_bar_drag_spans(
            rect(0.0, 100.0),
            &[rect(-10.0, 20.0), rect(15.0, 35.0), rect(110.0, 130.0)],
        );
        assert_eq!(spans, vec![rect(35.0, 100.0)]);
    }
}
