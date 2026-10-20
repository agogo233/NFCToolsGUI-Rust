//! 核心状态与事件模型。对标原版 `src/status.js` + IPC 消息。
//!
//! `Event` 是单向数据流的消息载体；`AppState::apply` 消费任务运行期与工具推送类事件，
//! 保证状态转换可单测、可回放。
//!
//! UI 主动发起的配置类变更（面板开合、密钥文本、面板内配置等）不经事件，
//! 由调用方直接赋值。

use std::path::PathBuf;

use crate::i18n;
use crate::paths::DEVICE_SPEED;

/// 事件列表上限（对标原版保留最近 30 条）。
pub const EVENT_LIMIT: usize = 30;

/// 设备连接状态，驱动顶部状态条配色。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DeviceState {
    #[default]
    None,
    Connecting,
    Connected,
    Failed,
}

/// 状态指示器：空闲 / 运行 / 出错。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Indicator {
    #[default]
    Free,
    Running,
    Error,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct StatusInfo {
    pub text: String,
    pub indicator: Indicator,
}

/// 事件条目类型。原版事件条三色：`key` / `error` / `success`。
///
/// 其中 `success` 在原版由 renderer 本地产生（dump-saved），此处收拢进 core。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventKind {
    /// 密钥相关（如 save_keys 新增）
    Key,
    /// 任务失败 / 被终止
    Error,
    /// 操作成功（dump 已保存）
    Success,
}

#[derive(Clone, Debug, PartialEq)]
pub struct EventItem {
    pub kind: EventKind,
    pub text: String,
}

/// Mifare 密钥类型。
///
/// 全项目**唯一**定义在 [`crate::model::keys::KeyType`]。此处 re-export 是为了
/// 保持 `state::*` 的既有引用面不变；S1 曾在 state.rs 另立一份同名枚举，
/// 造成 `state::KeyType` 与 `model::keys::KeyType` 互不兼容的双类型。
pub use crate::model::keys::KeyType;

/// BinaryData 事件的目标面板。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DumpTarget {
    #[default]
    Editor,
    ComparatorA,
    ComparatorB,
}

/// 一个扇区：4 行，每行 32 位 hex（16 字节）。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Sector {
    pub lines: [String; 4],
}

/// 1024B dump：64 扇区。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Dump {
    pub path: PathBuf,
    pub bytes: Vec<u8>,
    pub sectors: Vec<Sector>,
}

/// hard-nested 配置。
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct HardNestedConfig {
    pub known_key: String,
    pub known_sector: u8,
    pub known_key_type: KeyType,
    pub target_sector: u8,
    pub target_key_type: KeyType,
    pub collect_only: bool,
    pub auto_run: bool,
}

impl HardNestedConfig {
    /// 扇区 → trailer 块号，`4 * sector + 3`（等价 `(sector+1)*4-1`，且无 u8 溢出）。
    pub fn trailer_block(sector: u8) -> u32 {
        u32::from(sector) * 4 + 3
    }
}

/// 字典测试配置。
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DictTestConfig {
    pub sector: u8,
    pub key_type: KeyType,
    pub start_position: u32,
}

/// 8 个面板的可见性（对标原版 8 个 BrowserWindow）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Panels {
    pub settings: bool,
    pub input_keys: bool,
    pub hard_nested: bool,
    pub dict_test: bool,
    pub dump_editor: bool,
    pub dump_comparator: bool,
    pub dump_history: bool,
    pub about: bool,
    pub phone_wristband: bool,
}

impl Panels {
    pub fn any_open(&self) -> bool {
        self.settings
            || self.input_keys
            || self.hard_nested
            || self.dict_test
            || self.dump_editor
            || self.dump_comparator
            || self.dump_history
            || self.about
            || self.phone_wristband
    }
}

/// 手机/手环操作区的面板状态。
#[derive(Clone, Debug)]
pub struct PhwPanelState {
    /// 已读取到的卡号（母卡 block0 大写 hex，32 字符）。
    ///
    /// 为空时两个写按钮禁用——写卡号前必须先「读取卡号」。
    pub block0: Option<String>,
    /// 写数据扇区的目标扇区号。
    pub sector: u32,
    /// 写数据扇区的目标块号，默认块 1（块 0 只读、块 3 是扇区尾块）。
    pub block: u32,
}

impl Default for PhwPanelState {
    fn default() -> Self {
        Self {
            block0: None,
            sector: 0,
            block: 1,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct HardNestedPanel {
    pub visible: bool,
    pub config: Option<HardNestedConfig>,
    /// 无可用密钥时的提示
    pub hint: Option<String>,
    /// 无可用密钥时禁用执行
    pub disabled: bool,
}

#[derive(Clone, Debug, Default)]
pub struct DictTestPanel {
    pub visible: bool,
    pub config: Option<DictTestConfig>,
    pub hint: Option<String>,
    pub disabled: bool,
}

#[derive(Clone, Debug, Default)]
pub struct DumpEditorState {
    pub visible: bool,
    pub url: Option<PathBuf>,
    pub hex: Option<String>,
    pub dirty: bool,
}

#[derive(Clone, Debug, Default)]
pub struct DumpComparatorState {
    pub visible: bool,
    pub a: Option<PathBuf>,
    pub b: Option<PathBuf>,
}

/// hard-nested 任务阶段（状态机见 docs/01 §5.1）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum HardNestedPhase {
    #[default]
    Idle,
    AutoDetecting,
    Collecting,
    SavingNonces,
    Cracking,
}

#[derive(Clone, Debug, Default)]
pub struct HardNestedTask {
    pub phase: HardNestedPhase,
    pub config: Option<HardNestedConfig>,
}

/// 全局应用状态。
#[derive(Clone, Debug, Default)]
pub struct AppState {
    pub device_list: Vec<String>,
    pub current_device: Option<String>,
    pub current_speed: u32,
    pub is_device_connected: bool,
    pub device_state: DeviceState,
    pub is_running_task: bool,
    pub progress: Option<f32>,
    pub status: StatusInfo,
    pub log_buffer: String,
    pub events: Vec<EventItem>,
    /// (key hex, sector, A/B)
    pub known_key_info: Vec<(String, u8, KeyType)>,
    /// (sector, A/B)
    pub unknown_key_info: Vec<(u8, KeyType)>,
    pub total_unknown_keys: usize,
    pub dict_path: PathBuf,
    pub panels: Panels,
    pub input_keys_text: String,
    pub hard_nested: HardNestedPanel,
    pub dict_test: DictTestPanel,
    pub dump_editor: Option<DumpEditorState>,
    pub dump_comparator: DumpComparatorState,
    pub dump_history: Vec<String>,
    pub hard_nested_task: HardNestedTask,
    pub phw: PhwPanelState,
}

impl AppState {
    /// 以合理默认值构造（波特率固定 115200）。
    pub fn new() -> Self {
        Self {
            current_speed: DEVICE_SPEED,
            ..Default::default()
        }
    }

    /// 消费一个事件并更新自身。UI 主动发起的配置类事件不在此落盘。
    pub fn apply(&mut self, e: Event) {
        match e {
            Event::Log(s) => self.append_log(&s),
            Event::Status { text, indicator } => {
                self.status.text = text;
                self.status.indicator = indicator;
            }
            Event::DeviceList(v) => self.device_list = v,
            Event::DeviceState(s) => {
                self.device_state = s;
                self.is_device_connected = matches!(s, DeviceState::Connected);
            }
            Event::Progress(p) => self.progress = p,
            Event::EventItem { kind, text } => self.push_event(EventItem { kind, text }),
            Event::DictHistory(v) => self.dump_history = v,
            Event::DumpSaved(p) => {
                if let Some(f) = p.file_name().map(|s| s.to_string_lossy().into_owned()) {
                    if !self.dump_history.contains(&f) {
                        self.dump_history.push(f.clone());
                    }
                    // 对标 renderer `onDumpSaved`：每次保存都追加一条 success 事件
                    self.push_event(EventItem {
                        kind: EventKind::Success,
                        text: format!("{}{f}", i18n::t("event_dump_saved")),
                    });
                }
            }
            Event::SavedBinary { url } => {
                if let Some(ed) = self.dump_editor.as_mut() {
                    ed.url = Some(url);
                    ed.dirty = false;
                }
            }
            Event::TaskDone { code, killed } => {
                self.is_running_task = false;
                // 对标 execUtils.js::exec 的 close 回调；code == 0 的收尾由调用方触发
                if code != Some(0) {
                    if killed {
                        self.push_event(EventItem {
                            kind: EventKind::Error,
                            text: i18n::t("event_task_killed"),
                        });
                        self.exit_log(false);
                    } else {
                        let c = code.unwrap_or_default();
                        self.log_buffer.push_str(&format!("\nexit code: {c}"));
                        self.push_event(EventItem {
                            kind: EventKind::Error,
                            text: format!("{}{c}", i18n::t("event_task_failed")),
                        });
                        self.exit_log(true);
                    }
                }
            }
            // UI 主动发起或仅供渲染，不落状态
            Event::Guidance { .. }
            | Event::BinaryData { .. }
            | Event::DictFileName(_)
            | Event::HardNestedConfig(_)
            | Event::DictTestConfig(_) => {}
        }
    }

    /// 追加事件条目，超过 `EVENT_LIMIT` 时逐出最旧。
    fn push_event(&mut self, item: EventItem) {
        if self.events.len() >= EVENT_LIMIT {
            self.events.remove(0);
        }
        self.events.push(item);
    }

    /// 追加日志文本。对标 renderer `onUpdateLogOutput`：块内含 `\33[2K` 时，
    /// 先把已有日志截断回最后一个 `\n`（无 `\n` 则清空），再剥离该转义序列后追加。
    fn append_log(&mut self, chunk: &str) {
        if chunk.contains("\u{1b}[2K") {
            // 原版 index.js:21-23 —— `substring(0, lastIndexOf('\n'))`。
            // 保留 `\n` 本身（截断点即换行符的下标，不含它），只剩换行时
            // lastIndexOf 返回 -1，JS 的 substring(0, -1) 等价 substring(0, 0)
            // 即清空。
            match self.log_buffer.rfind('\n') {
                Some(pos) => self.log_buffer.truncate(pos + 1),
                None => self.log_buffer.clear(),
            }
            self.log_buffer.push_str(&chunk.replace("\u{1b}[2K", ""));
        } else {
            self.log_buffer.push_str(chunk);
        }
    }

    /// 对标 execUtils.js::printExitLog：失败时置出错，否则复位为空闲，并追加日志尾。
    fn exit_log(&mut self, failed: bool) {
        self.status = if failed {
            StatusInfo {
                text: i18n::t("indicator_error"),
                indicator: Indicator::Error,
            }
        } else {
            StatusInfo {
                text: i18n::t("indicator_free"),
                indicator: Indicator::Free,
            }
        };
        self.log_buffer.push_str(&format!(
            "\n{}",
            i18n::t(if failed {
                "log_msg_error"
            } else {
                "log_msg_finished"
            })
        ));
    }
}

/// 单向数据流事件。对标原版 `sendToMainWindow(channel, payload)`。
#[derive(Debug, Clone)]
pub enum Event {
    /// 追加一个日志块。含 `\33[2K` 时先截断当前行再剥离（见 `append_log`）。
    Log(String),
    Status {
        text: String,
        indicator: Indicator,
    },
    DeviceList(Vec<String>),
    DeviceState(DeviceState),
    Progress(Option<f32>),
    EventItem {
        kind: EventKind,
        text: String,
    },
    /// 弹引导框；`buttons` 为 (action 名, 文案)
    Guidance {
        text: String,
        buttons: Vec<(&'static str, String)>,
    },
    DumpSaved(PathBuf),
    BinaryData {
        url: PathBuf,
        sectors: Vec<Sector>,
        target: DumpTarget,
    },
    HardNestedConfig(HardNestedConfig),
    DictTestConfig(DictTestConfig),
    DictHistory(Vec<String>),
    DictFileName(String),
    SavedBinary {
        url: PathBuf,
    },
    TaskDone {
        code: Option<i32>,
        killed: bool,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_sets_fixed_speed() {
        let s = AppState::new();
        assert_eq!(s.current_speed, DEVICE_SPEED);
        assert!(!s.is_device_connected);
        assert!(!s.is_running_task);
        assert!(s.panels == Panels::default());
        assert!(!s.panels.any_open());
        assert_eq!(s.hard_nested_task.phase, HardNestedPhase::Idle);
    }

    #[test]
    fn key_type_parse_and_char() {
        assert_eq!(KeyType::parse_lenient("A"), KeyType::A);
        assert_eq!(KeyType::parse_lenient("a"), KeyType::A);
        assert_eq!(KeyType::parse_lenient("B"), KeyType::B);
        assert_eq!(KeyType::parse_lenient("b"), KeyType::B);
        assert_eq!(KeyType::parse_lenient("垃圾"), KeyType::A);
        assert_eq!(KeyType::parse_lenient(""), KeyType::A);
        assert_eq!(KeyType::parse_lenient(" B "), KeyType::B);
        assert_eq!(KeyType::A.as_char(), 'A');
        assert_eq!(KeyType::B.as_char(), 'B');
    }

    #[test]
    fn trailer_block_mapping() {
        // 扇区 → trailer 块：(sector + 1) * 4 - 1
        assert_eq!(HardNestedConfig::trailer_block(0), 3);
        assert_eq!(HardNestedConfig::trailer_block(1), 7);
        assert_eq!(HardNestedConfig::trailer_block(9), 39);
        assert_eq!(HardNestedConfig::trailer_block(39), 159);
    }

    #[test]
    fn apply_log_and_status() {
        let mut s = AppState::new();
        s.apply(Event::Log("abc".into()));
        s.apply(Event::Log("def".into()));
        assert_eq!(s.log_buffer, "abcdef");

        s.apply(Event::Status {
            text: "读卡中".into(),
            indicator: Indicator::Running,
        });
        assert_eq!(s.status.text, "读卡中");
        assert_eq!(s.status.indicator, Indicator::Running);
    }

    #[test]
    fn log_chunk_with_clear_sequence_truncates_current_line() {
        let mut s = AppState::new();
        s.apply(Event::Log("line1\n".into()));
        s.apply(Event::Log("partial".into()));
        s.apply(Event::Log("\u{1b}[2Knew".into()));
        assert_eq!(s.log_buffer, "line1\nnew");
    }

    #[test]
    fn log_clear_sequence_without_newline_drops_everything() {
        // 对标 JS substring(0, -1) === substring(0, 0) 的边界
        let mut s = AppState::new();
        s.apply(Event::Log("no newline yet".into()));
        s.apply(Event::Log("\u{1b}[2Kclean".into()));
        assert_eq!(s.log_buffer, "clean");
    }

    #[test]
    fn log_clear_sequence_is_stripped_everywhere() {
        let mut s = AppState::new();
        s.apply(Event::Log("a\n".into()));
        s.apply(Event::Log("\u{1b}[2Kmid\u{1b}[2Ktail".into()));
        // 原版 index.js:22-24：只要 chunk 里**含**清行序列，就截断一次
        // （substring(0, lastIndexOf('\n'))，保留换行本身），再用
        // replace("\33[2K", "") 去掉**全部**清行序列。
        // 故 "a\n" 保留、两个清行序列都被剥掉，midtail 追加在后。
        assert_eq!(s.log_buffer, "a\nmidtail");
    }

    #[test]
    fn apply_device_state_syncs_connected_flag() {
        let mut s = AppState::new();
        s.apply(Event::DeviceState(DeviceState::Connecting));
        assert!(matches!(s.device_state, DeviceState::Connecting));
        assert!(!s.is_device_connected);

        s.apply(Event::DeviceState(DeviceState::Connected));
        assert!(s.is_device_connected);

        s.apply(Event::DeviceState(DeviceState::Failed));
        assert!(!s.is_device_connected);
    }

    #[test]
    fn apply_device_list_replaces() {
        let mut s = AppState::new();
        s.apply(Event::DeviceList(vec!["/dev/ttyACM0".into()]));
        assert_eq!(s.device_list.len(), 1);
        s.apply(Event::DeviceList(vec![]));
        assert!(s.device_list.is_empty());
    }

    #[test]
    fn events_are_capped_at_limit() {
        let mut s = AppState::new();
        for i in 0..(EVENT_LIMIT + 5) {
            s.apply(Event::EventItem {
                kind: EventKind::Key,
                text: format!("k{i}"),
            });
        }
        assert_eq!(s.events.len(), EVENT_LIMIT);
        // 最旧被逐出，最新保留
        assert_eq!(
            s.events.first().unwrap().text,
            format!("k{}", EVENT_LIMIT + 5 - EVENT_LIMIT)
        );
        assert_eq!(
            s.events.last().unwrap().text,
            format!("k{}", EVENT_LIMIT + 4)
        );
    }

    #[test]
    fn apply_dump_saved_dedups_history_but_pushes_event() {
        let mut s = AppState::new();
        s.apply(Event::DumpSaved(PathBuf::from("/x/a.mfd")));
        s.apply(Event::DumpSaved(PathBuf::from("/y/a.mfd")));
        // 同文件名的 dump 只在历史里出现一次
        assert_eq!(s.dump_history, vec!["a.mfd".to_string()]);
        // 但每次保存都追加一条 success 事件
        assert_eq!(s.events.len(), 2);
        assert!(s.events.iter().all(|e| e.kind == EventKind::Success));
        assert_eq!(s.events[0].text, "转储文件已保存: a.mfd");
    }

    #[test]
    fn apply_dict_history_replaces() {
        let mut s = AppState::new();
        s.apply(Event::DumpSaved(PathBuf::from("/x/a.mfd")));
        s.apply(Event::DictHistory(vec!["b.mfd".into(), "c.mfd".into()]));
        assert_eq!(
            s.dump_history,
            vec!["b.mfd".to_string(), "c.mfd".to_string()]
        );
    }

    #[test]
    fn apply_saved_binary_resets_dirty() {
        let mut s = AppState::new();
        s.dump_editor = Some(DumpEditorState {
            visible: true,
            url: Some(PathBuf::from("/x/a.mfd")),
            hex: None,
            dirty: true,
        });
        s.apply(Event::SavedBinary {
            url: PathBuf::from("/x/a.mfd"),
        });
        assert!(!s.dump_editor.as_ref().unwrap().dirty);
        assert_eq!(
            s.dump_editor.as_ref().unwrap().url,
            Some(PathBuf::from("/x/a.mfd"))
        );
    }

    #[test]
    fn apply_task_done_success_and_failure() {
        let mut s = AppState::new();
        s.is_running_task = true;
        s.status = StatusInfo {
            text: "读卡中".into(),
            indicator: Indicator::Running,
        };

        // code == 0：只清运行标记，状态条收尾由调用方触发
        s.apply(Event::TaskDone {
            code: Some(0),
            killed: false,
        });
        assert!(!s.is_running_task);
        assert_eq!(s.status.indicator, Indicator::Running);
        assert!(s.events.is_empty());
        assert!(s.log_buffer.is_empty());

        // 非零退出：事件条 error + 状态条出错
        s.apply(Event::TaskDone {
            code: Some(3),
            killed: false,
        });
        assert_eq!(s.status.indicator, Indicator::Error);
        assert_eq!(s.status.text, "出错");
        assert!(s.log_buffer.contains("exit code: 3"));
        assert!(s.log_buffer.contains("### 运行出错 ###"));
        assert!(s.events.iter().any(|e| e.text == "任务异常退出, 退出码 3"));

        // 被终止：事件条 error，但状态条复位为空闲（对标 printExitLog(2)）
        s.status.indicator = Indicator::Running;
        s.apply(Event::TaskDone {
            code: Some(1),
            killed: true,
        });
        assert_eq!(s.status.indicator, Indicator::Free);
        assert_eq!(s.status.text, "空闲");
        assert!(s.log_buffer.contains("### 运行完毕 ###"));
        assert!(s.events.iter().any(|e| e.text == "任务已停止"));

        // 信号终止且 code 缺失
        s.apply(Event::TaskDone {
            code: None,
            killed: true,
        });
        assert_eq!(s.status.indicator, Indicator::Free);
    }

    #[test]
    fn apply_progress_sets_field() {
        let mut s = AppState::new();
        assert_eq!(s.progress, None);
        s.apply(Event::Progress(Some(0.5)));
        assert_eq!(s.progress, Some(0.5));
        s.apply(Event::Progress(None));
        assert_eq!(s.progress, None);
    }

    #[test]
    fn ui_initiated_events_are_noops() {
        let mut s = AppState::new();
        s.apply(Event::Guidance {
            text: "检测到未知密钥".into(),
            buttons: vec![("hard-nested", "破解".into())],
        });
        s.apply(Event::BinaryData {
            url: PathBuf::from("/x/a.mfd"),
            sectors: vec![Sector::default()],
            target: DumpTarget::Editor,
        });
        s.apply(Event::DictFileName("d.dic".into()));
        s.apply(Event::HardNestedConfig(HardNestedConfig::default()));
        s.apply(Event::DictTestConfig(DictTestConfig::default()));

        assert!(s.events.is_empty());
        assert!(s.dump_history.is_empty());
        assert!(s.hard_nested_task.config.is_none());
        assert!(s.log_buffer.is_empty());
    }

    #[test]
    fn event_variants_roundtrip_debug() {
        let e = Event::DeviceList(vec!["a".into()]);
        assert!(format!("{e:?}").contains("DeviceList"));
        let e2 = Event::TaskDone {
            code: None,
            killed: true,
        };
        assert!(format!("{e2:?}").contains("killed: true"));
    }
}
