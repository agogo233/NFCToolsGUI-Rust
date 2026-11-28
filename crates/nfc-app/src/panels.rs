//! 模态面板宿主：把 `AppState::panels` 的开关映射到 egui `Window`。
//!
//! S9 填充 settings / inputKeys / hardNested / dictTest / about 五项；
//! S10 填充 dump editor / comparator / history 三项。
//!
//! 每个面板的 DOM 顺序、字段默认值、禁用规则都逐条对标
//! `renderer/html/*.html`。**特别地**，原版有两处反直觉行为必须保留：
//!
//! 1. hardNested：勾了「自动执行」会把**其余全部输入**禁用
//!    （`hardNested.html:92-105` `checkboxOnclick`），且提交时
//!    `collectOnly` 被强制为 `false`（`:65` `autoRun.checked ? false : ...`）。
//! 2. hardNested / dictTest：没有检测结果时 `create*Window()` 不传参，
//!    面板收到 `config === undefined` → 显示红色提示 + 禁用全部输入
//!    （`hardNested.html:80-89`、`dictTest.html:60-66`）。

use egui::{Align, Context, Layout, RichText, ScrollArea, Ui};

use nfc_core::i18n::t;
use nfc_core::model::dump::{self, Sector};
use nfc_core::state::{AppState, DictTestConfig, Event, EventKind, HardNestedConfig, KeyType};

use nfc_core::engine::Action;

use crate::app::{NfcApp, ThemeMode};

/// inputKeys 输入框的默认值（`inputkeys.html:12` `value="ffffffffffff,"`）。
///
/// 末尾那个逗号是原版就带的，照抄。
pub const INPUT_KEYS_DEFAULT: &str = "ffffffffffff,";

/// 各面板的默认尺寸（`windows.js` 的 `width` / `height`）。
///
/// 高度原版对 win32 有 +40 的分支（要容纳 Windows 额外标题栏），
/// egui 无此差异，故只取非 win32 的值。
const SIZE_SETTINGS: [f32; 2] = [400.0, 510.0];
const SIZE_INPUT_KEYS: [f32; 2] = [400.0, 200.0];
const SIZE_HARD_NESTED: [f32; 2] = [500.0, 610.0];
const SIZE_DICT_TEST: [f32; 2] = [450.0, 390.0];
const SIZE_ABOUT: [f32; 2] = [600.0, 480.0];
const SIZE_DUMP_COMPARATOR: [f32; 2] = [760.0, 620.0];
const SIZE_DUMP_HISTORY: [f32; 2] = [520.0, 560.0];
/// 手机/手环面板。S16：原版 400×360 在加入滚动/步骤提示后偏小，调到 440×420。
const SIZE_PHW: [f32; 2] = [440.0, 420.0];

/// S18：手输卡号写 UFUID 面板。
const SIZE_UID_INPUT: [f32; 2] = [420.0, 300.0];

/// P2-9：卡历史档案面板（registry.json 列表 + 快捷载入）。
const SIZE_CARD_REGISTRY: [f32; 2] = [560.0, 520.0];

/// 面板标识。顺序即绘制顺序（决定叠放层级）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Panel {
    Settings,
    InputKeys,
    HardNested,
    DictTest,
    DumpComparator,
    DumpHistory,
    About,
    PhoneWristband,
    UidInput,
    /// P2-9：卡历史档案（registry.json 列表）。
    CardRegistry,
}

impl Panel {
    const ALL: [Panel; 10] = [
        Panel::Settings,
        Panel::InputKeys,
        Panel::HardNested,
        Panel::DictTest,
        Panel::DumpComparator,
        Panel::DumpHistory,
        Panel::About,
        Panel::PhoneWristband,
        Panel::UidInput,
        Panel::CardRegistry,
    ];

    /// 面板标题（走 i18n，与原版 `windows.js` 的 `title` 一致）。
    ///
    /// 原版的子窗口**都没有 `title` 属性**（`windows.js` 里只给
    /// `dialog_title_prompt` 那一处设了），Electron 回落到 HTML 的
    /// `<title>` 标签；而 settings / inputkeys / hardNested 三个 HTML
    /// 连 `<title>` 都没有。egui 的 `Window` 必须有标题，故 Settings 与
    /// HardNested 用了语义化的 i18n key（dictTest / about 则与原版
    /// `<title>` 逐字一致）。属外观层面偏离，登记于 docs/02 §6.6。
    pub fn title(self) -> &'static str {
        match self {
            Panel::Settings => "html_settings", // 面板无标题文字，用固定串
            Panel::InputKeys => "html_please_input_known_keys",
            Panel::HardNested => "html_hard_nested",
            Panel::DictTest => "html_please_input_known_keys", // 原版 dictTest.html 复用同一标题 key
            Panel::DumpComparator => "html_dump_comparator",
            Panel::DumpHistory => "html_dump_history",
            Panel::About => "html_about",
            Panel::PhoneWristband => "html_phw_title",
            Panel::UidInput => "html_uid_input_title",
            Panel::CardRegistry => "html_card_registry_title",
        }
    }

    /// 默认尺寸。尺寸取自 `windows.js` 的 `create*Window`。
    fn default_size(self) -> [f32; 2] {
        match self {
            Panel::Settings => SIZE_SETTINGS,
            Panel::InputKeys => SIZE_INPUT_KEYS,
            Panel::HardNested => SIZE_HARD_NESTED,
            Panel::DictTest => SIZE_DICT_TEST,
            Panel::About => SIZE_ABOUT,
            Panel::DumpComparator => SIZE_DUMP_COMPARATOR,
            Panel::DumpHistory => SIZE_DUMP_HISTORY,
            Panel::PhoneWristband => SIZE_PHW,
            Panel::UidInput => SIZE_UID_INPUT,
            Panel::CardRegistry => SIZE_CARD_REGISTRY,
        }
    }

    /// S16：面板骨架是否允许用户拖拽调大小。
    ///
    /// 高频面板默认打开；设置 / 输入密钥 / 关于保持固定尺寸，避免误拖。
    pub fn resizable(self) -> bool {
        matches!(
            self,
            Self::HardNested
                | Self::DictTest
                | Self::DumpComparator
                | Self::DumpHistory
                | Self::PhoneWristband
                | Self::CardRegistry
        )
    }

    /// 该面板当前是否打开。
    pub fn is_open(self, p: &nfc_core::state::Panels) -> bool {
        match self {
            Panel::Settings => p.settings,
            Panel::InputKeys => p.input_keys,
            Panel::HardNested => p.hard_nested,
            Panel::DictTest => p.dict_test,
            Panel::DumpComparator => p.dump_comparator,
            Panel::DumpHistory => p.dump_history,
            Panel::About => p.about,
            Panel::PhoneWristband => p.phone_wristband,
            Panel::UidInput => p.uid_input,
            Panel::CardRegistry => p.card_registry,
        }
    }

    fn set_open(self, p: &mut nfc_core::state::Panels, v: bool) {
        match self {
            Panel::Settings => p.settings = v,
            Panel::InputKeys => p.input_keys = v,
            Panel::HardNested => p.hard_nested = v,
            Panel::DictTest => p.dict_test = v,
            Panel::DumpComparator => p.dump_comparator = v,
            Panel::DumpHistory => p.dump_history = v,
            Panel::About => p.about = v,
            Panel::PhoneWristband => p.phone_wristband = v,
            Panel::UidInput => p.uid_input = v,
            Panel::CardRegistry => p.card_registry = v,
        }
    }
}

/// 打开一个面板。
pub fn open(panels: &mut nfc_core::state::Panels, p: Panel) {
    p.set_open(panels, true);
}

/// 关闭一个面板。
pub fn close(panels: &mut nfc_core::state::Panels, p: Panel) {
    p.set_open(panels, false);
}

/// 关闭最上层打开的面板（Esc 用）。
///
/// 面板按 `Panel::ALL` 顺序绘制，最后画的浮在最上层，故倒序关第一个。
pub fn close_topmost(panels: &mut nfc_core::state::Panels) {
    for p in Panel::ALL.iter().rev() {
        if p.is_open(panels) {
            p.set_open(panels, false);
            return;
        }
    }
}

// ───────────────────────── 表单状态 ─────────────────────────

/// 硬解码面板的表单（对标 `hardNested.html` 的 5 个 input + 2 个 checkbox）。
///
/// 字段是 `String`/`String` 而非 `u8`：原版 input 是文本框，提交时才
/// `parseInt`。非法输入在原版会变成 `NaN` 并一路传给工具，此处改为
/// 提交时校验并给 0（见 [`HardNestedForm::config`]）——已在 docs/02 §6 登记。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HardNestedForm {
    /// 「自动执行」。勾上后其余输入全禁用。
    pub auto_run: bool,
    pub known_key: String,
    pub known_sector: String,
    pub known_key_type: KeyType,
    pub target_sector: String,
    pub target_key_type: KeyType,
    /// 「只采集不计算」。auto_run 勾上时强制 false。
    pub collect_only: bool,
    /// 无检测结果 → 显示提示 + 全禁用。
    pub no_config: bool,
}

impl Default for HardNestedForm {
    /// 字段默认值逐个对标 `hardNested.html:19-52`。
    fn default() -> Self {
        Self {
            auto_run: false,
            known_key: "ffffffffffff".to_owned(),
            known_sector: "0".to_owned(),
            known_key_type: KeyType::A,
            target_sector: "0".to_owned(),
            target_key_type: KeyType::A,
            collect_only: false,
            no_config: true,
        }
    }
}

impl HardNestedForm {
    /// 打开面板时按检测结果预填（`command.js:240-246`）。
    ///
    /// 原版是 `try { createHardNestedWindow({...knownKeyInfo[0], ...unknownKeyInfo[0]}) }
    /// catch { createHardNestedWindow() }`——数组为空则抛异常走无参分支。
    pub fn reset_from(&mut self, state: &AppState) {
        *self = Self::default();
        let (Some(known), Some(unknown)) =
            (state.known_key_info.first(), state.unknown_key_info.first())
        else {
            // 无参数分支：保持默认值 + no_config，面板会禁用全部输入
            return;
        };
        let (known_key, known_sector, known_key_type) = known;
        let (target_sector, target_key_type) = unknown;
        self.known_key.clone_from(known_key);
        self.known_sector = known_sector.to_string();
        self.known_key_type = *known_key_type;
        self.target_sector = target_sector.to_string();
        self.target_key_type = *target_key_type;
        self.no_config = false;
    }

    /// 原版 `checkboxOnclick`：勾上 auto_run 就禁用其余全部输入。
    pub fn others_disabled(&self) -> bool {
        self.auto_run
    }

    /// 提交用的配置。`collectOnly` 在 auto_run 下强制 false（`hardNested.html:65`）。
    pub fn config(&self) -> HardNestedConfig {
        HardNestedConfig {
            known_key: self.known_key.trim().to_owned(),
            known_sector: parse_sector(&self.known_sector),
            known_key_type: self.known_key_type,
            target_sector: parse_sector(&self.target_sector),
            target_key_type: self.target_key_type,
            collect_only: self.collect_only && !self.auto_run,
            auto_run: self.auto_run,
        }
    }

    /// 前置校验：known_key 须为 12 位 hex，两个扇区须在 0..=39（兼容 4K）。
    /// 返回第一个命中的 i18n key（docs/02 §6.42）。原版无此校验，越界靠工具报错。
    pub fn validation_error(&self) -> Option<&'static str> {
        if !is_12hex(&self.known_key) {
            return Some("dialog_msg_key_hex_invalid");
        }
        if !in_sector_range(&self.known_sector) || !in_sector_range(&self.target_sector) {
            return Some("dialog_msg_sector_range_invalid");
        }
        None
    }
}

/// 字典测试面板的表单（对标 `dictTest.html:15-36`）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DictTestForm {
    pub sector: String,
    pub key_type: KeyType,
    pub start_position: String,
    /// 无检测结果 → 显示提示 + 禁用扇区/起始位置/类型/确定。
    pub no_config: bool,
    /// 字典文件名（`#dict-file-name`），选完字典后回填（`command.js:378`）。
    pub file_name: String,
}

impl DictTestForm {
    /// 打开面板时按检测结果预填（`command.js:383-392`）。
    pub fn reset_from(&mut self, state: &AppState) {
        // 原版的 sector 字段默认值是 "0"（`dictTest.html:22`），
        // 故不能靠 `Default` 的空串。
        *self = Self {
            sector: "0".to_owned(),
            key_type: KeyType::A,
            start_position: "0".to_owned(),
            no_config: true,
            file_name: String::new(),
        };
        let Some((sector, key_type)) = state.unknown_key_info.first() else {
            return;
        };
        self.sector = sector.to_string();
        self.key_type = *key_type;
        self.start_position = "0".to_owned();
        self.no_config = false;
    }

    pub fn config(&self) -> DictTestConfig {
        DictTestConfig {
            sector: parse_sector(&self.sector),
            key_type: self.key_type,
            start_position: self.start_position.parse().unwrap_or(0),
        }
    }

    /// 前置校验：扇区须在 0..=39（兼容 4K，docs/02 §6.42）。
    pub fn validation_error(&self) -> Option<&'static str> {
        if !in_sector_range(&self.sector) {
            return Some("dialog_msg_sector_range_invalid");
        }
        None
    }
}

/// 文本框 → 扇区号。原版 `parseInt` 得 `NaN` 时工具会报错，此处兜底为 0。
fn parse_sector(text: &str) -> u8 {
    text.trim().parse().unwrap_or(0)
}

/// hard-nested / dict-test 的扇区范围：0..=39（兼容 4K 卡，docs/02 §6.42）。
///
/// 空串视作 0（`parse_sector` 兜底），合法。
fn in_sector_range(text: &str) -> bool {
    let t = text.trim();
    if t.is_empty() {
        return true;
    }
    t.parse::<u8>().map(|v| v <= 39).unwrap_or(false)
}

/// 是否为恰好 12 位 hex（known_key 前置校验，docs/02 §6.42）。
fn is_12hex(text: &str) -> bool {
    let t = text.trim();
    t.len() == 12 && t.chars().all(|c| c.is_ascii_hexdigit())
}

/// 手机/手环面板表单（对标 `phoneWristband.html` 的扇区/块两个 input）。
///
/// 默认扇区 0、块 1（块 0 只读、块 3 是扇区尾块，均不可写，见 docs/05 §3）。
#[derive(Clone, Debug)]
pub struct PhwForm {
    pub sector: String,
    pub block: String,
}

impl Default for PhwForm {
    fn default() -> Self {
        Self {
            sector: "0".to_owned(),
            block: "1".to_owned(),
        }
    }
}

impl PhwForm {
    /// 前置校验：扇区 0..=15、块 1..=2（沿用 `SectorBlock::is_valid` 的 1K 语义，
    /// docs/02 §6.42）。原版靠引擎侧拦，这里提前到输入时提示。
    pub fn validation_error(&self) -> Option<&'static str> {
        let sector = parse_u32(&self.sector);
        let block = parse_u32(&self.block);
        if sector <= nfc_core::tasks::phw::SECTOR_MAX
            && (nfc_core::tasks::phw::BLOCK_MIN..=nfc_core::tasks::phw::BLOCK_MAX).contains(&block)
        {
            None
        } else {
            Some("dialog_msg_phw_sector_block_invalid")
        }
    }
}

/// S9 各面板的表单状态。
#[derive(Clone, Debug, Default)]
pub struct Forms {
    /// inputKeys 的文本框内容。
    pub input_keys: String,
    /// S18：手输卡号面板的输入框内容。
    pub input_uid: String,
    pub hard_nested: HardNestedForm,
    pub dict_test: DictTestForm,
    /// S14：手机/手环面板的扇区/块文本框内容。
    pub phw: PhwForm,
    /// S10：dump 编辑器的每扇区 4 行文本。
    pub editor: EditorForm,
    /// S10：比较器的 A/B 两侧扇区数据。`None` = 尚未选文件。
    pub comparator: ComparatorForm,
    /// S10：历史列表中勾选的文件名。
    pub history_selected: Vec<String>,
    /// S11-2：正在重命名的文件 `(原名, 编辑框当前内容)`，`None` = 不在编辑。
    ///
    /// 对标 `dumpHistory.js:29-53`：点铅笔后文件名变输入框，回车或失焦提交。
    /// egui 侧失焦用 `Response::lost_focus()`、回车查 `ui.input()`，两者皆可，
    /// 另加一个「确定」按钮作为鼠标用户的后备（egui 的
    /// `key_pressed(Enter)` 在 `TextEdit` 内不一定被识别为提交）。
    pub history_rename: Option<(String, String)>,
    /// S11-2：点「重命名」后下一帧让重命名输入框自动获焦。
    ///
    /// egui 不会把当帧的鼠标点击转成新控件的聚焦请求（控件是下一帧才画出来
    /// 的），否则用户得先点铅笔、再点一次输入框才能开始打字。
    pub history_focus_request: bool,
}

/// 转储编辑器的 hex 显示方式（docs/02 §6.40）。
///
/// `Continuous` 是原版形态（一整个块的 32 个 hex 连成一串），
/// `Byte` 是每字节空一格且大写（`00 01 02 03 ...`），便于对照
/// 资料逐个字节定位。两者共用同一份 `sectors`，只是屏幕上怎么摆。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ViewMode {
    #[default]
    Continuous,
    Byte,
}

/// `dumpEditor.html` 的表单：每扇区一个 4 行文本框。
///
/// 原版每扇区是**一个** `contenteditable` 的 div，内含 4 行
/// （`dumpEditor.js:71`），不是 64 个独立块输入框。此处保持「每扇区一个
/// 多行输入」的结构，故编辑粒度是行而非块。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EditorForm {
    /// 每扇区 4 行，行长 32 hex。
    pub sectors: Vec<Sector>,
    /// 当前文件名（`#current-file`，只取 basename）。
    pub file_name: Option<String>,
    /// 编辑器当前文件的**完整路径**（外部绝对路径或 `dumpfiles/` 内路径）。
    ///
    /// 决定「保存」写回哪里：`Some(origin)` 直接写 origin，否则回落
    /// `dumpfiles/<file_name>`。与 `file_name`（只取 basename 供展示）分离——
    /// 原版只存 basename，导致从外部目录打开的 dump「保存」被写回 dumpfiles
    /// 下的同名副本而非原文件（登记见 docs/02 §6）。
    pub origin: Option<std::path::PathBuf>,
    /// 有未保存改动。
    pub dirty: bool,
    /// hex 显示方式。只改屏幕排布，`sectors` 不受影响，故保存 / 写卡走的
    /// 仍是同一份数据（docs/02 §6.40）。
    pub view_mode: ViewMode,
    /// 字节视图下的显示缓存：每扇区一个含空格、大写的 4 行文本。
    ///
    /// 不能每帧拿 `sectors` 重新排版——那会变成「用户刚打出来的字符串」的
    /// 加空格版本，而 egui 的光标位置以「上一帧画出来的字符串」为基准，
    /// 重排会让每次击键后光标跳回行首。故编辑后把原文存进来，下一帧原样
    /// 画回去；仅在切模式 / 载入 / 修复时重建。
    pub byte_view: Vec<String>,
    /// 最近一次「载入」失败的原因（结构不合法 / 文件读不出）。
    ///
    /// 载入**失败不清空**编辑器后，`sectors` 可能仍显示上一份有效数据，
    /// 失败必须独立可见：中栏编辑器据此在标题行下渲染失败条（与是否为
    /// 空无关）。成功载入时清除；用户开始编辑 / 修复内容时一并收起（旧
    /// 失败不再是现状），至下次载入前持续保留。
    pub load_error: Option<String>,
}

/// 一行 32 hex → `00 01 02 03 ...`（大写、每字节之间空一格）。
fn to_byte_view_line(line: &str) -> String {
    let flat: String = line.chars().filter(|c| !c.is_whitespace()).collect();
    flat.chars()
        .collect::<Vec<char>>()
        .chunks(2)
        .map(|pair| {
            pair.iter()
                .copied()
                .collect::<String>()
                .to_ascii_uppercase()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

impl EditorForm {
    /// 从 dump 字节载入（对标 `command.js:425-432` 的分组）。
    pub fn load(&mut self, name: Option<String>, bytes: &[u8]) -> Result<(), String> {
        let sectors = match dump::to_sector_lines(bytes) {
            Ok(s) => s,
            Err(e) => {
                // 失败原因留在表单上（不 panic、不清空已有 sectors）：中栏
                // 编辑器据此渲染失败条。
                let msg = e.to_string();
                self.load_error = Some(msg.clone());
                return Err(msg);
            }
        };
        self.sectors = sectors;
        self.file_name = name;
        self.dirty = false;
        // 载入新内容即作废旧来源：调用方按需重设 origin，避免 file_name 与
        // origin 指向不同文件（两处 `load_dump_*` 都在成功处赋 origin）。
        self.origin = None;
        // 成功载入即清除上次失败提示
        self.load_error = None;
        // 显示缓存随数据一起重建，否则切到字节视图会看到上一份 dump 的旧内容
        self.rebuild_byte_view();
        Ok(())
    }

    /// 切换到另一种 hex 显示方式（连续 ↔ 字节分组）。
    pub fn toggle_view(&mut self) {
        self.view_mode = match self.view_mode {
            ViewMode::Continuous => ViewMode::Byte,
            ViewMode::Byte => ViewMode::Continuous,
        };
        // 双向都重建：切去字节视图时保证缓存是最新的；切回连续视图时清掉
        // 上次留在缓存里的（可能已被编辑过的）排布，下次进来重新从 sectors 生成
        self.rebuild_byte_view();
    }

    /// 用 `sectors` 重建字节视图显示缓存。
    pub fn rebuild_byte_view(&mut self) {
        self.byte_view = self
            .sectors
            .iter()
            .map(|sector| {
                // 收成 closure 而不是直接传函数项：`Sector` 是 `Vec<String>`，
                // `.iter()` 产出 `&String`，而 `.map(fn)` 不做 deref 转换（E0631）
                sector
                    .iter()
                    .map(|line| to_byte_view_line(line))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .collect()
    }

    /// 结构 + 内容校验（`dumpEditor.js:98-118` 的 `checkData`）。
    ///
    /// `check_content = true` 对应「保存 / 另存为」，
    /// `false` 对应「更新颜色」（原版后者只查行数与行长）。
    ///
    /// 注意**空扇区列表会通过**：原版 `.each` 遍历空集合返回 `true`
    /// （vacuous truth），此处一致。真正的把关在 [`Self::can_save`]。
    pub fn validate(&self, check_content: bool) -> Result<(), String> {
        dump::validate(&self.sectors, check_content).map_err(|e| e.to_string())
    }

    /// 保存按钮是否可用。
    ///
    /// 对标 `dumpEditor.js:16` 的
    /// `if (!currentFilePath || !getHexData()) { alert(...); return }`
    /// 加上 `:20` 的 `checkData(true)`——**两道**检查都要过：
    /// 1. 已打开文件且有内容（否则 `Buffer.from("")` 会写出 0 字节文件）
    /// 2. 结构 + 字符集校验通过
    pub fn can_save(&self) -> bool {
        self.file_name.is_some() && !self.sectors.is_empty() && self.validate(true).is_ok()
    }

    /// 全部扇区拼成一份连续 hex（`dumpEditor.js:14` 的
    /// `$('.binary-input').map(...).get().join('').replace(/\n/g, '')`）。
    pub fn flat_hex(&self) -> String {
        self.sectors
            .iter()
            .flat_map(|s| s.iter())
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join("")
    }
}

/// `dumpComparator.html` 的表单：A/B 两份 dump。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ComparatorForm {
    pub a: Option<Vec<Sector>>,
    pub b: Option<Vec<Sector>>,
    pub a_name: Option<String>,
    pub b_name: Option<String>,
}

impl ComparatorForm {
    /// 两侧都齐了才比对（`dumpComparator.js:28` 的
    /// `if (dumpA.length > 0 && dumpB.length > 0)`）。
    pub fn ready(&self) -> bool {
        self.a.as_ref().is_some_and(|v| !v.is_empty())
            && self.b.as_ref().is_some_and(|v| !v.is_empty())
    }

    /// 逐字符 diff（`dumpComparator.js:91` 的 `showMarkedDump(dumpA, dumpB, 4, 32)`）。
    pub fn diff(&self) -> Vec<(dump::SectorDiff, dump::SectorDiff)> {
        match (&self.a, &self.b) {
            (Some(a), Some(b)) => dump::diff_sectors(a, b),
            _ => Vec::new(),
        }
    }
}

/// 历史面板的底部按钮启用规则（`dumpHistory.js:100-115` 的
/// `updateBottomButtonStatus`）。
///
/// 勾选 1 个 → 编辑 / 写入可用、比较禁用；
/// 勾选 2 个 → 比较可用、编辑 / 写入禁用；
/// 其他     → 三个全禁用。删除按钮**始终**可用。
pub fn history_button_states(selected: usize) -> (bool, bool, bool) {
    match selected {
        1 => (true, false, true),
        2 => (false, true, false),
        _ => (false, false, false),
    }
}

/// 提交一次重命名，对标 `command.js:524-531`。
///
/// 原版 `fs.rename` 出错即 `throw`（整个窗口崩掉）；此处只把错误记成事件。
/// 成功后同步勾选集合——被改名的文件若正被勾选，得跟着改名，否则下一帧
/// `history_selected.retain(...)` 会把它当成已删除的文件静默剔掉。
///
/// 目标名先过 [`nfc_core::dumpfiles::with_mfd_ext`] 归一化：`list()` 只收
/// `.mfd` 文件，改成别的后缀（含尾随空格）会让文件从历史面板「消失」、
/// 再也无法管理；输入 `old.mfd` 的去后缀名（如 `old`）等价于无操作。
fn commit_rename(dir: &std::path::Path, old: &str, new: &str, app: &mut NfcApp) {
    if new.trim().is_empty() || new == old {
        app.forms.history_rename = None;
        return;
    }
    let new = nfc_core::dumpfiles::with_mfd_ext(new.trim());
    if new == old {
        app.forms.history_rename = None;
        return;
    }
    match nfc_core::dumpfiles::rename(dir, old, &new) {
        Ok(_) => {
            for s in app.forms.history_selected.iter_mut() {
                if s == old {
                    *s = new.clone();
                }
            }
            // 同步编辑器来源（P3）：若「当前文件」正是被改名的这份转储，
            // origin / 文件名跟着走，否则「保存」会在旧路径上悄悄重建文件。
            let old_path = dir.join(old);
            if app.forms.editor.origin.as_deref() == Some(old_path.as_path()) {
                app.forms.editor.origin = Some(dir.join(&new));
            }
            if app.forms.editor.file_name.as_deref() == Some(old) {
                app.forms.editor.file_name = Some(new.clone());
            }
            app.state.apply(Event::Log(format!("\n{old} → {new}\n")));
        }
        Err(e) => app.push_event(EventKind::Error, &e.to_string()),
    }
}

/// `commit_rename` 的测试入口（`--selftest` 用）。
pub fn commit_rename_for_test(dir: &std::path::Path, old: &str, new: &str, app: &mut NfcApp) {
    commit_rename(dir, old, new, app)
}

/// 重命名输入框本帧的三方交互 → 该做什么。
///
/// 三个信号在同一帧可能**同时**成立，尤其是点「确定」：按钮拿到焦点会让
/// TextEdit 失焦，于是 `lost_focus` 与 `confirm` 同为真。旧实现先按
/// `lost_focus` 提交、再按 `confirm` 提交，第二次 `Option::take` 取到
/// `None` 把结果覆盖掉，重命名被静默丢弃。此处把优先级定死：
/// 取消 > 确认按钮 > 失焦/回车，且只提交一次。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenameIntent {
    /// 什么都不做。
    None,
    /// 提交（提交内容由调用方 `take()` 取）。
    Commit,
    /// 放弃这次编辑。
    Cancel,
}

pub fn rename_intent(lost_focus: bool, enter: bool, confirm: bool, cancel: bool) -> RenameIntent {
    if cancel {
        return RenameIntent::Cancel;
    }
    if confirm || lost_focus || enter {
        return RenameIntent::Commit;
    }
    RenameIntent::None
}

// ───────────────────────── 绘制 ─────────────────────────

/// 把所有打开的面板画出来。
pub fn show(ctx: &Context, app: &mut NfcApp) {
    for p in Panel::ALL {
        if !p.is_open(&app.state.panels) {
            continue;
        }
        let mut open = true;
        // `body` 返回 `false` 表示面板自己要求关闭（点了取消/确定）。
        // egui 的 `.open(&mut open)` 只管标题栏的 ✕，两者都要认。
        let mut keep = true;
        let [w, h] = p.default_size();
        let screen = ctx.content_rect();
        let pos = screen.center() - egui::vec2(w, h) * 0.5;
        let mut win = egui::Window::new(t(p.title()))
            .resizable(p.resizable())
            .collapsible(false)
            // 点击标题栏 ✕ 后快速淡出（egui 自带 ✕ 无背景按压反馈，
            // 淡出动画补上「点了有反应」的观感）。依赖 `.open(&mut open)`。
            .fade_out(true)
            .default_size([w, h])
            .min_width(320.0)
            .min_height(160.0)
            .constrain_to(ctx.content_rect().shrink(16.0))
            .default_pos(egui::pos2(
                pos.x.max(screen.min.x + 16.0),
                pos.y.max(screen.min.y + 16.0),
            ));
        if p.resizable() {
            win = win
                .max_width((screen.width() - 32.0).max(w))
                .max_height((screen.height() - 32.0).max(h));
        }
        win.open(&mut open).show(ctx, |ui| keep = body(p, ui, app));
        if !open || !keep {
            close(&mut app.state.panels, p);
        }
    }
}

/// 单个面板的内容。返回 `false` 表示要关闭该面板。
fn body(p: Panel, ui: &mut Ui, app: &mut NfcApp) -> bool {
    match p {
        Panel::Settings => settings(ui, app),
        Panel::InputKeys => input_keys(ui, app),
        Panel::HardNested => hard_nested(ui, app),
        Panel::DictTest => dict_test(ui, app),
        Panel::About => about(ui),
        Panel::DumpComparator => dump_comparator(ui, app),
        Panel::DumpHistory => dump_history(ui, app),
        Panel::PhoneWristband => phone_wristband(ui, app),
        Panel::UidInput => uid_input(ui, app),
        Panel::CardRegistry => card_registry(ui, app),
    }
}

/// 底部按钮行：取消在左、主按钮在右（`subWindow.css` 的 `.final-button`）。
///
/// 返回 `(点了取消, 点了主按钮)`。
fn footer(ui: &mut Ui, primary_label: &str, primary_enabled: bool) -> (bool, bool) {
    let mut cancel = false;
    let mut primary = false;
    ui.add_space(8.0);
    ui.separator();
    ui.add_space(4.0);
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        if ui
            .add_enabled(primary_enabled, egui::Button::new(primary_label))
            .clicked()
        {
            primary = true;
        }
        if ui.button(t("dialog_button_cancel")).clicked() {
            cancel = true;
        }
    });
    (cancel, primary)
}

/// S16：给高频面板的表单主体加一层滚动，底部按钮仍可钉在面板末尾。
fn scroll_body<R>(ui: &mut Ui, footer_reserve: f32, add_contents: impl FnOnce(&mut Ui) -> R) -> R {
    let body_h = (ui.available_height() - footer_reserve).max(120.0);
    ScrollArea::vertical()
        .max_height(body_h)
        .auto_shrink([false, true])
        .show(ui, add_contents)
        .inner
}

/// `settings.html`：三个主题模式单选 + 关闭。
///
/// S15：改为单选列表且**点选即生效**——主题切换属即时反馈型操作，
/// 原版「选完再点取消/确定」反而多一步；关闭按钮也因此不再是「取消」
/// （没有可取消的待定状态），改用「关闭」文案。
fn settings(ui: &mut Ui, app: &mut NfcApp) -> bool {
    /// （按钮 i18n key, 模式），顺序即 `settings.html:13-15`。
    const MODES: [(&str, ThemeMode); 3] = [
        ("html_toggle_dark_mode", ThemeMode::Dark),
        ("html_toggle_light_mode", ThemeMode::Light),
        ("html_follow_system_mode", ThemeMode::System),
    ];

    // S16：设置面板不再是“只有一个主题选项”的空壳，补上数据目录 / 工具目录 / 版本。
    ui.label(RichText::new(t("html_appearance")).strong());
    for (key, mode) in MODES {
        let selected = app.theme_mode == mode;
        let r = ui.add(egui::RadioButton::new(selected, t(key)));
        if r.clicked() {
            app.theme_mode = mode;
        }
        ui.add_space(4.0);
    }

    ui.add_space(8.0);
    ui.separator();

    let dump_dir = app.paths.dumpfiles_dir();
    let tool_dir = app.bin_dir.clone();
    let tool_ok = app.tool_dir_ok;
    let missing = app.missing_tools.clone();
    let mut open_dump = false;

    scroll_body(ui, 56.0, |ui| {
        // 五项配置改用 Grid 对齐标签列：原 `ui.horizontal` 各标签字数不一，
        // 值列（路径 / 版本 / 下拉 / 滑块）左缘参差。Grid 按最宽标签
        // （「MFOC 探测次数」）自动定列宽，值列统一左缘对齐。
        egui::Grid::new("settings-rows")
            .num_columns(2)
            .spacing(egui::vec2(12.0, 8.0))
            .show(ui, |ui| {
                ui.label(RichText::new(t("html_data_dir")).strong());
                ui.horizontal(|ui| {
                    ui.add(
                        egui::Label::new(RichText::new(dump_dir.display().to_string())).truncate(),
                    )
                    .on_hover_text(dump_dir.display().to_string());
                    open_dump = ui.button(t("html_open_dump_folder")).clicked();
                });
                ui.end_row();

                ui.label(RichText::new(t("html_tool_dir")).strong());
                ui.horizontal(|ui| {
                    let path = if tool_dir.as_os_str().is_empty() {
                        "-".to_owned()
                    } else {
                        tool_dir.display().to_string()
                    };
                    ui.add(egui::Label::new(RichText::new(path.clone())).truncate())
                        .on_hover_text(&path);
                    if !tool_ok {
                        ui.colored_label(ui.visuals().error_fg_color, missing.join("、"));
                    }
                });
                ui.end_row();

                ui.label(RichText::new(t("html_current_version")).strong());
                ui.horizontal(|ui| {
                    ui.label(env!("APP_VERSION"));
                    ui.label(env!("BUILDER"));
                });
                ui.end_row();

                ui.label(RichText::new(t("html_speed")).strong());
                let speeds: [(u32, &str); 5] = [
                    (115200, "115200"),
                    (230400, "230400"),
                    (460800, "460800"),
                    (921600, "921600"),
                    (19200, "19200"),
                ];
                let cur = app.state.current_speed;
                egui::ComboBox::from_id_salt("conn_speed")
                    .selected_text(
                        speeds
                            .iter()
                            .find(|(s, _)| *s == cur)
                            .map(|(_, l)| *l)
                            .unwrap_or("115200"),
                    )
                    .width(110.0)
                    .show_ui(ui, |ui| {
                        for (s, l) in speeds {
                            if ui.selectable_label(s == cur, l).clicked() {
                                app.state.current_speed = s;
                            }
                        }
                    });
                ui.end_row();

                ui.label(RichText::new(t("html_mfoc_probes")).strong());
                ui.add(egui::Slider::new(&mut app.state.mfoc_probes, 1..=255))
                    .on_hover_text(t("html_mfoc_probes_tip"));
                ui.end_row();
            });
        // S18 B2：通信速度 + 深层扫描改完**不立即生效**——要等下次连接
        // （`connect()` 才读 `state.current_speed` / `GateState.deep_scan`），
        // 故文案提示「将在重连设备后生效」。复选框与提示非标签值行，不入 Grid。
        ui.checkbox(&mut app.state.deep_scan, t("html_deep_scan"))
            .on_hover_text(t("html_deep_scan"));
        if app.state.is_device_connected {
            ui.label(RichText::new(t("html_applies_after_reconnect")).size(11.0))
                .on_hover_text(t("html_applies_after_reconnect"));
        }
    });

    if open_dump {
        app.exec("open-dump-folder", None);
    }

    ui.add_space(8.0);
    let close = ui
        .with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.button(t("html_close")).clicked()
        })
        .inner;
    !close
}

/// inputKeys 面板点「确定」后的处理。返回 `true` 表示**保持面板打开**。
///
/// 抽成独立函数是为了能被 `--selftest` 直接驱动（egui 的按钮点击在测试
/// 上下文里拿不到），UI 部分只负责调用它。
pub fn confirm_input_keys(app: &mut NfcApp) -> bool {
    // command.js:154-157 —— 先用正则扫出所有 12 位 hex，逐个转 `-k<key>`。
    // 原版 `keys.match(...)` 匹配不到会返回 null 再 `.forEach` 抛 TypeError，
    // 此处空结果不派发（等价于"什么都不做"）。
    let keys = nfc_core::model::keys::parse_12hex(&app.forms.input_keys);
    if keys.is_empty() {
        // 不要静默关窗：用户会以为「确定」没生效或读卡已开始。
        // 直接给出与硬解码面板同一套校验文案（12 位 hex 的要求一致）。
        app.push_event(EventKind::Error, &t("dialog_msg_key_hex_invalid"));
        // 保持面板打开，方便用户就地修正。
        return true;
    }
    app.pending
        .push(Action::WithArg("done-input-keys-read-IC", keys.join(",")));
    false
}

/// `inputkeys.html`：标题 + 密钥输入框 + 取消/确定。
fn input_keys(ui: &mut Ui, app: &mut NfcApp) -> bool {
    ui.label(t("html_please_input_known_keys_split_by_comma"));
    ui.add_space(4.0);
    ui.add(egui::TextEdit::singleline(&mut app.forms.input_keys).desired_width(f32::INFINITY));

    let (cancel, ok) = footer(ui, &t("dialog_button_ok"), true);
    if ok {
        return !confirm_input_keys(app);
    }
    !cancel
}

/// S18：手输卡号写 UFUID（`input-uid-write` 开面板）。
///
/// 输入 8 位 UID 或 32 位完整 0 块；点「确定」把原文本交给
/// `done-input-uid-write`，由其做清洗 → 拼 0 块 → B6 危险确认码确认 → 派发，
/// 最后 engine 的 `input_uid_write` 拼块 → BCC 判定 → `nfc-mfsetuid` 写入。
fn uid_input(ui: &mut Ui, app: &mut NfcApp) -> bool {
    ui.label(RichText::new(t("html_uid_input_title")).strong());
    ui.add_space(4.0);
    ui.add(
        egui::TextEdit::singleline(&mut app.forms.input_uid)
            .desired_width(f32::INFINITY)
            .hint_text(t("html_uid_input_placeholder")),
    );
    ui.add_space(4.0);

    let (cancel, ok) = footer(ui, &t("dialog_button_ok"), true);
    if ok {
        app.exec("done-input-uid-write", Some(app.forms.input_uid.clone()));
        return false;
    }
    !cancel
}

/// `hardNested.html`：auto_run + 已知信息 + 目标扇区 + collect_only。
fn hard_nested(ui: &mut Ui, app: &mut NfcApp) -> bool {
    // 先取调色板：`app.forms` 的可变借用在整个函数期间都活着，
    // 期间不能再借用 `app`（含 `app.paint()`）。
    let danger = app.paint().color(app.paint().danger);
    let f = &mut app.forms.hard_nested;

    // 勾上 auto_run 后其余输入全禁用（`hardNested.html:92-105`）
    if ui
        .checkbox(&mut f.auto_run, t("html_auto_run"))
        .on_hover_text(t("html_auto_run"))
        .changed()
        && f.auto_run
    {
        f.collect_only = false;
    }
    if f.auto_run {
        ui.label(RichText::new(t("html_auto_run_hint")).color(danger));
    }
    let no_config = f.no_config;
    let dis = f.others_disabled() || no_config;
    let mut detect_now = false;

    ui.separator();
    scroll_body(ui, 56.0, |ui| {
        ui.label(RichText::new(t("html_known_info")).strong());
        egui::Grid::new("hard-nested-known")
            .num_columns(2)
            .spacing(egui::vec2(12.0, 8.0))
            .show(ui, |ui| {
                ui.label(t("html_known_key"));
                ui.add_enabled(
                    !dis,
                    egui::TextEdit::singleline(&mut f.known_key).desired_width(180.0),
                );
                ui.end_row();
                ui.label(t("html_sector"));
                ui.add_enabled(
                    !dis,
                    egui::TextEdit::singleline(&mut f.known_sector).desired_width(60.0),
                );
                ui.end_row();
                ui.label(t("html_key_type"));
                key_type_radio(ui, &mut f.known_key_type, !dis);
            });

        ui.add_space(8.0);
        ui.label(RichText::new(t("html_target_sector_info")).strong());
        egui::Grid::new("hard-nested-target")
            .num_columns(2)
            .spacing(egui::vec2(12.0, 8.0))
            .show(ui, |ui| {
                ui.label(t("html_sector"));
                ui.add_enabled(
                    !dis,
                    egui::TextEdit::singleline(&mut f.target_sector).desired_width(60.0),
                );
                ui.end_row();
                ui.label(t("html_key_type"));
                key_type_radio(ui, &mut f.target_key_type, !dis);
            });

        ui.add_space(8.0);
        ui.add_enabled(
            !dis,
            egui::Checkbox::new(&mut f.collect_only, t("html_collect_only")),
        );

        if no_config {
            ui.colored_label(danger, t("html_detect_card_first"));
            // 死路自救：没有检测结果时除了提示，再给一条直达「检测卡片加密」
            // 的按钮——否则用户要自己关面板→点检测→再开面板。
            // （此处只读 UI 状态，`app` 的可变借用已在上方 `f` 上；
            // 点击动作放到闭包外用 flag 触发。）
            if ui.button(t("html_detect_card")).clicked() {
                detect_now = true;
            }
        }
    });
    if detect_now {
        app.exec("detect-card-type", None);
        return false;
    }

    // docs/02 §6.42：known_key 12 位 hex + 两扇区 0..=39 前置校验，越界禁用「执行」+ 红字。
    let verr = f.validation_error();
    if let Some(key) = verr {
        ui.colored_label(danger, t(key));
    }
    let (cancel, ok) = footer(ui, &t("html_execute"), !no_config && verr.is_none());
    if ok {
        app.pending
            .push(Action::HardNestedConfig(Box::new(f.config())));
        return false;
    }
    !cancel
}

/// A/B 单选（`hardNested.html:25-29` / `dictTest.html:25-29` 的 radio-group）。
fn key_type_radio(ui: &mut Ui, cur: &mut KeyType, enabled: bool) {
    for kt in [KeyType::A, KeyType::B] {
        let label = format!("Key {}", kt.as_str());
        let sel = *cur == kt;
        if ui
            .add_enabled(enabled, egui::RadioButton::new(sel, label))
            .clicked()
        {
            *cur = kt;
        }
    }
}

/// 字典文件行（`dictTest.html:15-19`）：当前文件名 + 「选择文件」按钮。
///
/// 独立成函数的原因：点按钮要在闭包外调 `app.exec`，而调用方的
/// `&mut app.forms.dict_test` 借用必须已经结束。
fn dict_file_row(ui: &mut Ui, dict_path: &str, app: &mut NfcApp) {
    let mut choose = false;
    ui.horizontal(|ui| {
        ui.label(t("html_dictionary_file"));
        let name = nfc_core::tasks::dict_test::dict_file_name(dict_path);
        let shown = if name.is_empty() {
            // 未选文件时显示「内置字典」（`:17`）
            t("html_build_in_dictionary")
        } else {
            name.to_owned()
        };
        ui.label(shown);
        choose = ui.button(t("html_choose_file")).clicked();
    });
    if choose {
        app.exec("open-dict-file", None);
    }
}

/// `dictTest.html`：字典文件 + 扇区 + 类型 + 起始位置。
fn dict_test(ui: &mut Ui, app: &mut NfcApp) -> bool {
    let danger = app.paint().color(app.paint().danger);
    let dict_path = app.state.dict_path.to_string_lossy().into_owned();

    ui.label(t("html_input_test_info"));
    ui.add_space(4.0);
    dict_file_row(ui, &dict_path, app);

    let f = &mut app.forms.dict_test;
    let dis = f.no_config;

    scroll_body(ui, 56.0, |ui| {
        egui::Grid::new("dict-test")
            .num_columns(2)
            .spacing(egui::vec2(12.0, 8.0))
            .show(ui, |ui| {
                ui.label(t("html_sector"));
                ui.add_enabled(
                    !dis,
                    egui::TextEdit::singleline(&mut f.sector).desired_width(60.0),
                );
                ui.end_row();
                ui.label(t("html_key_type"));
                key_type_radio(ui, &mut f.key_type, !dis);
                ui.end_row();
                ui.label(t("html_start_position"));
                ui.add_enabled(
                    !dis,
                    egui::TextEdit::singleline(&mut f.start_position).desired_width(80.0),
                );
            });

        if f.no_config {
            ui.colored_label(danger, t("html_detect_card_first"));
        }
    });

    // docs/02 §6.42：扇区 0..=39 前置校验，越界禁用「确定」+ 红字。
    let verr = f.validation_error();
    if let Some(key) = verr {
        ui.colored_label(danger, t(key));
    }
    let (cancel, ok) = footer(ui, &t("dialog_button_ok"), !f.no_config && verr.is_none());
    if ok {
        app.pending
            .push(Action::DictTestConfig(Box::new(f.config())));
        return false;
    }
    !cancel
}

/// 开源项目清单：C 工具（vendored 在 framework/source/）+ Rust 依赖 crate。
const OPEN_SOURCE: [(&str, &str, &str); 18] = [
    // ── C 工具（vendored 源码 / 编译产物）──
    ("mfoc", "https://github.com/nfc-tools/mfoc", "GPL-2.0"),
    (
        "crypto1_bs",
        "https://github.com/aczid/crypto1_bs",
        "GPL-2.0",
    ),
    (
        "cropto1_bs",
        "https://github.com/vk496/cropto1_bs",
        "GPL-2.0",
    ),
    (
        "MifareOneTool",
        "https://github.com/xcicode/MifareOneTool",
        "GPL-3.0",
    ),
    ("libnfc", "https://github.com/nfc-tools/libnfc", "LGPL-3.0"),
    (
        "nfc-mflock",
        "https://github.com/duament/nfc-mflock",
        "BSD-2-Clause",
    ),
    (
        "nfc-mfsetuid",
        "https://github.com/GSWXXN/NFCToolsGUI/tree/main/source/nfc-mfsetuid",
        "BSD-2-Clause",
    ),
    // ── Rust 依赖 crate（MIT OR Apache-2.0）──
    ("egui", "https://github.com/emilk/egui", "MIT OR Apache-2.0"),
    (
        "eframe",
        "https://github.com/emilk/egui",
        "MIT OR Apache-2.0",
    ),
    (
        "serde",
        "https://github.com/serde-rs/serde",
        "MIT OR Apache-2.0",
    ),
    (
        "serde_json",
        "https://github.com/serde-rs/json",
        "MIT OR Apache-2.0",
    ),
    (
        "directories",
        "https://github.com/dirs-rs/directories",
        "MIT OR Apache-2.0",
    ),
    (
        "thiserror",
        "https://github.com/dtolnay/thiserror",
        "MIT OR Apache-2.0",
    ),
    (
        "regex",
        "https://github.com/rust-lang/regex",
        "MIT OR Apache-2.0",
    ),
    (
        "serialport",
        "https://github.com/joseph-lacaba/serialport",
        "MIT OR Apache-2.0",
    ),
    (
        "image",
        "https://github.com/image-rs/image",
        "MIT OR Apache-2.0",
    ),
    ("rfd", "https://github.com/boltex/rfd", "MIT OR Apache-2.0"),
    ("open", "https://github.com/open/open", "MIT OR Apache-2.0"),
];

const REPO_URL: &str = "https://github.com/agogo233/NFCToolsGUI-Rust";
const LICENSE_URL: &str = "https://github.com/agogo233/NFCToolsGUI-Rust/blob/main/LICENSE";

/// `about.html`：图标 + 版本 + 编译者 + 仓库 + 许可证 + 开源清单。
fn about(ui: &mut Ui) -> bool {
    ScrollArea::vertical().show(ui, |ui| {
        ui.heading(t("app_name"));
        ui.label(t("html_version_code") + env!("APP_VERSION"));
        ui.label(t("html_builder") + env!("BUILDER"));
        ui.label(t("html_open_source_repository"));
        ui.hyperlink_to(REPO_URL, REPO_URL);
        ui.label(t("html_license"));
        ui.hyperlink_to("AGPL-3.0", LICENSE_URL);

        ui.separator();
        // §6.29：快捷键在 UI 里没有别的入口，放在「关于」里登记一份，
        // 否则用户根本不会想到有 Esc / Ctrl+S。
        ui.label(RichText::new(t("html_shortcuts")).strong());
        ui.label(t("html_shortcut_close_panel"));
        ui.label(t("html_shortcut_save_dump"));

        ui.separator();
        ui.label(t("html_this_program_use_following_open_source_project"));
        for (name, url, license) in OPEN_SOURCE {
            ui.horizontal(|ui| {
                ui.label(RichText::new(name).strong());
                ui.hyperlink_to(t("html_source_code"), url);
                ui.label("-");
                ui.hyperlink_to(format!("{} ({license})", t("html_license")), url);
            });
        }
    });
    true
}

// ───────────────────────── S14 手机/手环操作区 ─────────────────────────

/// `phoneWristband.html`：五步引导 + 卡号显示 + 扇区/块输入 + 三个动作按钮。
///
/// 对标 `commands.rs` 的 `phw_read_uid` / `phw_write_uid` / `phw_write_data`。
/// 卡号（block0）由「读取卡号」缓存进 `state.phw.block0`，两个写按钮据此启用。
fn phone_wristband(ui: &mut Ui, app: &mut NfcApp) -> bool {
    let card_no = app.state.phw.block0.as_deref().unwrap_or("--").to_owned();
    let has_block0 = app.state.phw.block0.is_some();
    let paint = app.paint();
    let primary = paint.color(paint.primary);
    let text_inverse = paint.color(paint.text_inverse);
    let danger = paint.color(paint.danger);

    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new(t("html_phw_guidance")).size(nfc_core::theme::FONT_SIZE[0]));
    });
    ui.separator();

    scroll_body(ui, 56.0, |ui| {
        egui::Frame::NONE
            .fill(paint.color(paint.surface2))
            .corner_radius(egui::CornerRadius::same(nfc_core::theme::RADIUS[0] as u8))
            .inner_margin(egui::Margin::symmetric(8, 6))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(t("html_phw_card_no")).strong());
                    // 32 位 hex 压线面板宽度，截断防溢出（卡号定长，正常不截）。
                    ui.add(egui::Label::new(RichText::new(&card_no).monospace()).truncate());
                });
            });

        if !has_block0 {
            ui.colored_label(danger, t("dialog_msg_need_phw_read_uid_first"));
        }

        egui::Grid::new("phw-sector-block")
            .num_columns(2)
            .spacing(egui::vec2(12.0, 8.0))
            .show(ui, |ui| {
                ui.label(t("html_phw_sector"));
                ui.add(egui::TextEdit::singleline(&mut app.forms.phw.sector).desired_width(56.0));
                ui.end_row();
                ui.label(t("html_phw_block"));
                ui.add(egui::TextEdit::singleline(&mut app.forms.phw.block).desired_width(56.0));
                ui.end_row();
            });
        // 范围提示独立成整行（原先挤在 Grid 左列、右列留空，观感别扭）
        ui.add_space(4.0);
        ui.label(t("html_phw_range"));

        // docs/02 §6.42：扇区 0..=15、块 1..=2 前置红字提示。读卡号不依赖扇区/块，
        // 故不禁用 read；写数据扇区仍由引擎侧 is_valid 兜底，这里把提示提前到输入时。
        if let Some(key) = app.forms.phw.validation_error() {
            ui.colored_label(danger, t(key));
        }

        ui.add_space(8.0);
        let read = egui::Button::new(
            RichText::new(t("html_phw_read_uid"))
                .strong()
                .color(text_inverse),
        )
        .fill(primary);
        let write_uid = egui::Button::new(t("html_phw_write_uid"));
        let write_data = egui::Button::new(t("html_phw_write_data"));

        ui.horizontal(|ui| {
            if ui.add(read).clicked() {
                app.exec("phw-read-uid", None);
            }
            // 扇区/块非法时直接禁用写数据：面板已红字提示，若仍可点，用户
            // 会在「已经报错」的状态下按下按钮、再由引擎回一条同样的错误，
            // 显得像是没拦住。读卡号不依赖扇区/块，故 `read` 不受影响。
            let fields_ok = app.forms.phw.validation_error().is_none();
            if ui.add_enabled(has_block0, write_uid).clicked() {
                app.exec("phw-write-uid", None);
            }
            if ui
                .add_enabled(has_block0 && fields_ok, write_data)
                .clicked()
            {
                app.state.phw.sector = parse_u32(&app.forms.phw.sector);
                app.state.phw.block = parse_u32(&app.forms.phw.block);
                app.exec("phw-write-data", None);
            }
        });
    });

    ui.add_space(8.0);
    ui.separator();
    ui.add_space(4.0);
    let close = ui
        .with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.button(t("html_close")).clicked()
        })
        .inner;
    !close
}

/// 文本框 → 无符号扇区/块号（原版 `parseInt` 得 NaN 时兜底为 0，越界由引擎拦）。
fn parse_u32(text: &str) -> u32 {
    text.trim().parse().unwrap_or(0)
}

// ───────────────────────── S10 dump 三面板 ─────────────────────────

/// `dumpComparator.html`：A/B 选文件 + 逐字符 diff 高亮。
fn dump_comparator(ui: &mut Ui, app: &mut NfcApp) -> bool {
    let paint = app.paint();
    let mark = paint.color(paint.mark_bg);
    let mark_fg = paint.color(paint.mark_fg);

    // 顶部选文件条（`dumpComparator.html:12-18`）
    ui.horizontal(|ui| {
        ui.label(format!("{} (A): ", t("html_dump")));
        let name = app
            .forms
            .comparator
            .a_name
            .clone()
            .unwrap_or_else(|| t("html_choose_file"));
        ui.add(egui::Label::new(RichText::new(&name)).truncate())
            .on_hover_text(name);
        if ui.button("…").clicked() {
            app.exec("dump-comparator-choose-file", Some("A".to_owned()));
        }
    });
    ui.horizontal(|ui| {
        ui.label(format!("{} (B): ", t("html_dump")));
        let name = app
            .forms
            .comparator
            .b_name
            .clone()
            .unwrap_or_else(|| t("html_choose_file"));
        ui.add(egui::Label::new(RichText::new(&name)).truncate())
            .on_hover_text(name);
        if ui.button("…").clicked() {
            app.exec("dump-comparator-choose-file", Some("B".to_owned()));
        }
    });
    ui.separator();

    // 比对区（`dumpComparator.js:99-113`）：逐扇区，A 行在上 B 行在下
    //
    // S15：改为 A/B 并排双列——原版逐块「A 一行 B 一行」上下堆叠，
    // 每扇区 8 行；并排后每扇区 4 行，左右横着对照，滚动距离减半。
    if !app.forms.comparator.ready() {
        ui.vertical_centered(|ui| {
            ui.add_space(24.0);
            ui.label(t("html_choose_file"));
        });
        return true;
    }
    let diffs = app.forms.comparator.diff();
    ScrollArea::vertical().show(ui, |ui| {
        ui.columns(2, |cols| {
            for (ci, tag) in ["A", "B"].into_iter().enumerate() {
                cols[ci].label(
                    RichText::new(tag)
                        .strong()
                        .color(paint.color(paint.primary)),
                );
            }
        });
        for (si, (da, db)) in diffs.iter().enumerate() {
            ui.label(RichText::new(format!("{} {si}", t("html_sector"))).strong());
            ui.columns(2, |cols| {
                for (ci, side) in [da, db].into_iter().enumerate() {
                    let ui = &mut cols[ci];
                    for segs in side.iter() {
                        render_diff_row(ui, segs, mark, mark_fg);
                    }
                }
            });
            ui.add_space(4.0);
        }
    });
    true
}

/// 画一行 diff 分段。差异字符用 `--color-mark-bg/--color-mark-fg`（原版 `<mark>`）。
///
/// egui 的 `Label` 没有 `background()`，故差异字符包一层 `Frame::fill`。
/// `DiffSeg::Same` 已是合并后的整段，故每行最多 32 个控件。
fn render_diff_row(
    ui: &mut Ui,
    segs: &[dump::DiffSeg],
    mark: egui::Color32,
    mark_fg: egui::Color32,
) {
    if segs.is_empty() {
        // 原版对不上的一侧是空串 `""`（`dumpComparator.js:74`）
        ui.label("");
        return;
    }
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        for s in segs {
            let txt = RichText::new(s.text()).monospace();
            if s.is_diff() {
                egui::Frame::NONE
                    .fill(mark)
                    .inner_margin(0.0)
                    .show(ui, |ui| {
                        ui.label(txt.color(mark_fg));
                    });
            } else {
                ui.label(txt);
            }
        }
    });
}

// ───────────────────────── P2-9 卡历史档案 ─────────────────────────

/// 卡历史档案：读 `registry.json`（每次解卡自动追加，`engine::registry_append`）
/// 展示 UID / 日期 / 已知密钥，并可按 UID 快捷查找对应 dump 载入编辑器。
///
/// 在 `dumpfiles/` 里找与卡号 `uid` 同源的转储文件。
///
/// 读卡落盘的文件名是 `{uid}_{时间戳}.mfd`（`paths::dump_file_name`），编辑器
/// 「写入此转储」的副本是 `{uid}_…_edit_{时间戳}.mfd`——两者的**首个下划线段**
/// 都是卡号，故按 `stem.split_once('_')` 前缀匹配（旧实现用全等比较，stem 带
/// 时间戳后缀永远匹配不上，「编辑转储」按钮因此从未出现过）。
///
/// 非 hex 卡号一律跳过：`dump_file_name` 对非 hex 卡号兜底成 `card_…`，多个
/// 不同卡号会落到同一个兜底文件上，前缀匹配会张冠李戴。
pub fn registry_dump_for(dump_dir: &std::path::Path, uid: &str) -> Option<String> {
    if uid.len() < 6 || !uid.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    nfc_core::dumpfiles::list(dump_dir).into_iter().find(|f| {
        f.trim_end_matches(".mfd")
            .split_once('_')
            .is_some_and(|(uid_part, _)| uid_part.eq_ignore_ascii_case(uid))
    })
}

/// registry 是**旁路可查表**：读失败（不存在/损坏）只显示空表提示，不报错弹窗。
fn card_registry(ui: &mut Ui, app: &mut NfcApp) -> bool {
    let path = app.paths.registry_file();
    let registry = nfc_core::data::Registry::load(&path).unwrap_or_default();
    let paint = app.paint();

    if registry.is_empty() {
        ui.vertical_centered(|ui| {
            ui.add_space(24.0);
            ui.label(t("html_card_registry_empty"));
            ui.label(
                RichText::new(t("html_card_registry_empty_hint"))
                    .size(12.0)
                    .color(paint.color(paint.text_secondary)),
            );
        });
    } else {
        let mut open_uid: Option<String> = None;
        let list_h = (ui.available_height() - 8.0).max(120.0);
        ScrollArea::vertical()
            .max_height(list_h)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                egui::Grid::new("card-registry")
                    .num_columns(4)
                    .spacing(egui::vec2(12.0, 6.0))
                    .striped(true)
                    .show(ui, |ui| {
                        ui.label(RichText::new("UID").strong().size(12.0));
                        ui.label(RichText::new(t("html_current_file")).strong().size(12.0));
                        ui.label(RichText::new(t("html_detect_summary")).strong().size(12.0));
                        ui.label(RichText::new(t("html_date")).strong().size(12.0));
                        ui.end_row();
                        for r in &registry.registry {
                            ui.label(
                                RichText::new(&r.uid)
                                    .monospace()
                                    .size(12.0)
                                    .color(paint.color(paint.uid)),
                            );
                            // 卡号 → dumpfiles 里按卡号前缀匹配的 dump 文件
                            let dump = registry_dump_for(&app.paths.dumpfiles_dir(), &r.uid);
                            match dump {
                                Some(f) => {
                                    if ui
                                        .small_button(
                                            RichText::new(t("html_dump_editor")).size(11.0),
                                        )
                                        .on_hover_text(f.clone())
                                        .clicked()
                                    {
                                        open_uid = Some(f);
                                    }
                                }
                                None => {
                                    ui.label(
                                        RichText::new("-")
                                            .size(12.0)
                                            .color(paint.color(paint.text_secondary)),
                                    );
                                }
                            }
                            let keys = match (r.key_a.is_empty(), r.key_b.is_empty()) {
                                (false, false) => "A+B",
                                (false, true) => "A",
                                (true, false) => "B",
                                (true, true) => "-",
                            };
                            ui.label(RichText::new(keys).size(12.0));
                            ui.label(
                                RichText::new(&r.date)
                                    .size(12.0)
                                    .color(paint.color(paint.text_secondary)),
                            );
                            ui.end_row();
                        }
                    });
            });
        if let Some(f) = open_uid {
            app.exec("open-dump-editor", Some(f));
            return false;
        }
    }

    ui.add_space(8.0);
    ui.separator();
    ui.add_space(4.0);
    let close = ui
        .with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.button(t("html_close")).clicked()
        })
        .inner;
    !close
}

fn dump_history(ui: &mut Ui, app: &mut NfcApp) -> bool {
    // 列表来自 core 的 dumpfiles::list（只列 .mfd）
    // 目录随注入的 app.paths 走：`--selftest` 会把它换成临时目录，
    // 用 from_app_data() 会绕过注入、读到真实用户数据目录。
    let dir = app.paths.dumpfiles_dir();
    let files = nfc_core::dumpfiles::list(&dir);
    // 勾选集合与实际文件求交，剔除已被删的
    app.forms.history_selected.retain(|f| files.contains(f));

    if files.is_empty() {
        ui.vertical_centered(|ui| {
            ui.add_space(24.0);
            ui.label("-");
        });
    } else {
        // 提交重命名：把编辑框内容落盘并刷新历史。
        let mut commit: Option<(String, String)> = None;
        // 高度跟随面板窗口：预留底部按钮行 56，保底 120（原写死 360，矮窗口会溢出）。
        let list_h = (ui.available_height() - 56.0).max(120.0);
        ScrollArea::vertical()
            .max_height(list_h)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                for f in &files {
                    // 一行一项（勾选 + 名字 + 操作按钮）。外层是竖排，若不套
                    // horizontal，「重命名」/ 输入框 /「确定」「取消」会各占一行，像断开的碎片。
                    ui.horizontal(|ui| {
                        let sel = app.forms.history_selected.iter().any(|s| s == f);
                        let mut checked = sel;
                        let editing = app
                            .forms
                            .history_rename
                            .as_ref()
                            .is_some_and(|(o, _)| o == f);
                        // 编辑中勾选框不再重复显示旧名——原版是文件名**变成**输入框
                        // （`dumpHistory.js:29-53`），旧名留在标签上会一行两遍名字。
                        let box_resp = if editing {
                            ui.checkbox(&mut checked, "")
                        } else {
                            ui.checkbox(&mut checked, f.as_str())
                        };
                        if box_resp.clicked() {
                            if checked {
                                app.forms.history_selected.push(f.clone());
                            } else {
                                app.forms.history_selected.retain(|s| s != f);
                            }
                        }
                        if editing {
                            // `editing` 由 `history_rename.as_ref().is_some_and(...)` 判定，
                            // 进入时必为 `Some`；用 `if let` 解构而非 unwrap（防结构漂移）。
                            if let Some((_, text)) = app.forms.history_rename.as_mut() {
                                let autofocus = app.forms.history_focus_request
                                    && !ui.input(|i| i.pointer.any_click());
                                app.forms.history_focus_request = false;
                                // 留出尾部「确定」「取消」两个文字按钮的位置，避免输入框越界压住按钮。
                                let width = (ui.available_width() - 110.0).max(80.0);
                                let r =
                                    ui.add(egui::TextEdit::singleline(text).desired_width(width));
                                if autofocus {
                                    r.request_focus();
                                }
                                // 先取按钮结果、再判失焦：点「取消」会让输入框同一帧失焦，
                                // 判定在前会把「取消」误当成提交（blur 提交本身对标原版）。
                                let confirm = ui.small_button(t("dialog_button_ok")).clicked();
                                let cancel = ui.small_button(t("dialog_button_cancel")).clicked();
                                // 三方信号同帧成立的顺序问题见 `rename_intent` 的文档。
                                match rename_intent(
                                    r.lost_focus(),
                                    ui.input(|i| i.key_pressed(egui::Key::Enter)),
                                    confirm,
                                    cancel,
                                ) {
                                    RenameIntent::Commit => {
                                        commit = app.forms.history_rename.take();
                                    }
                                    RenameIntent::Cancel => {
                                        app.forms.history_rename = None;
                                    }
                                    RenameIntent::None => {}
                                }
                            }
                        } else if ui.small_button(t("html_rename")).clicked() {
                            app.forms.history_rename = Some((f.clone(), f.clone()));
                            app.forms.history_focus_request = true;
                        }
                    });
                }
            });
        if let Some((old, new)) = commit {
            commit_rename(&dir, &old, &new, app);
        }
    }

    ui.separator();
    // 按钮启用规则见 `history_button_states`
    let n = app.forms.history_selected.len();
    let (can_edit, can_compare, can_write) = history_button_states(n);

    ui.horizontal(|ui| {
        // 窄窗口下 3 个按钮装不下一行，改自动换行（docs/02 §6.29）。
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(can_compare, egui::Button::new(t("html_compute_dump")))
                .clicked()
            {
                if let [a, b] = app.forms.history_selected.as_slice() {
                    // 载荷用 \u{1} 分隔两个文件名（原版是 `{A, B}` 对象，
                    // 此处走字符串通道，见 NfcApp::exec 的 "open-dump-comparator"）。
                    app.exec("open-dump-comparator", Some(format!("{a}\u{1}{b}")));
                }
            }
            if ui
                .add_enabled(can_edit, egui::Button::new(t("html_edit_dump")))
                .clicked()
            {
                if let Some(f) = app.forms.history_selected.first() {
                    app.exec("open-dump-editor", Some(f.clone()));
                }
            }
            if ui
                .add_enabled(can_write, egui::Button::new(t("html_write_this_dump")))
                .clicked()
            {
                if let Some(f) = app.forms.history_selected.first() {
                    // 走 exec 而不是直接 push：write-IC 必须先过危险确认窗。
                    app.exec("write-IC", Some(f.clone()));
                }
            }
        });
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            // 删除始终可用（`dumpHistory.html:18` 无 disabled 属性）
            if ui.button(t("html_delete_checked")).clicked() {
                let sel = app.forms.history_selected.clone();
                if !sel.is_empty() {
                    // S11-2：删除不可逆，先弹应用内确认窗（原版 `command.js:507`
                    // 的 `showMessageBoxSync`）。engine 侧还有一道
                    // `confirm_danger` 闸，未获批时拒绝执行。
                    app.confirm = Some(crate::dialogs_ui::delete_dumps_modal(&sel));
                }
            }
        });
    });
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn esc_closes_the_topmost_open_panel() {
        // CardRegistry 在 `Panel::ALL` 里最靠后，故浮在最上层。
        let mut p = nfc_core::state::Panels {
            settings: true,
            dump_comparator: true,
            card_registry: true,
            ..Default::default()
        };
        close_topmost(&mut p);
        assert!(!p.card_registry);
        assert!(p.settings && p.dump_comparator);
    }

    #[test]
    fn close_topmost_noop_when_nothing_open() {
        let mut p = nfc_core::state::Panels::default();
        close_topmost(&mut p);
        assert!(!p.any_open());
    }

    // ── docs/02 §6.42 面板输入前置校验 ──

    #[test]
    fn in_sector_range_accepts_0_to_39_and_empty() {
        assert!(in_sector_range(""));
        assert!(in_sector_range("0"));
        assert!(in_sector_range("39"));
        assert!(!in_sector_range("40"));
        assert!(!in_sector_range("99"));
        assert!(!in_sector_range("abc"));
        assert!(!in_sector_range("-1"));
    }

    #[test]
    fn is_12hex_accepts_exactly_12_hex() {
        assert!(is_12hex("ffffffffffff"));
        assert!(is_12hex("FFFFFFFFFFFF"));
        assert!(is_12hex("0123456789ab"));
        assert!(!is_12hex("fffffffffff"), "11 位");
        assert!(!is_12hex("fffffffffffffff"), "15 位");
        assert!(!is_12hex("zzzzzzzzzzzz"), "非 hex");
        assert!(!is_12hex(""), "空串不是合法密钥");
    }

    #[test]
    fn hard_nested_validation_rejects_bad_key_and_sector() {
        let valid = HardNestedForm {
            known_key: "ffffffffffff".into(),
            known_sector: "0".into(),
            target_sector: "39".into(),
            ..Default::default()
        };
        assert_eq!(valid.validation_error(), None, "合法配置应通过");

        let bad_key = HardNestedForm {
            known_key: "xyz".into(),
            ..Default::default()
        };
        assert_eq!(
            bad_key.validation_error(),
            Some("dialog_msg_key_hex_invalid"),
            "非 12 位 hex 应报密钥错"
        );

        let bad_sector = HardNestedForm {
            known_key: "ffffffffffff".into(),
            target_sector: "99".into(),
            ..Default::default()
        };
        assert_eq!(
            bad_sector.validation_error(),
            Some("dialog_msg_sector_range_invalid"),
            "扇区 99 应报范围错"
        );
    }

    #[test]
    fn dict_test_validation_rejects_out_of_range_sector() {
        let ok = DictTestForm {
            sector: "0".into(),
            ..Default::default()
        };
        assert_eq!(ok.validation_error(), None);

        let four_k = DictTestForm {
            sector: "39".into(),
            ..Default::default()
        };
        assert_eq!(four_k.validation_error(), None, "39 是 4K 合法扇区");

        let bad = DictTestForm {
            sector: "99".into(),
            ..Default::default()
        };
        assert_eq!(
            bad.validation_error(),
            Some("dialog_msg_sector_range_invalid")
        );
    }

    #[test]
    fn phw_validation_follows_sector_max_15_block_1_to_2() {
        let valid = |sector: &str, block: &str| PhwForm {
            sector: sector.into(),
            block: block.into(),
        };
        assert_eq!(valid("0", "1").validation_error(), None, "默认 0/1 合法");
        assert_eq!(valid("15", "2").validation_error(), None, "15/2 是 1K 上界");
        assert_eq!(
            valid("16", "2").validation_error(),
            Some("dialog_msg_phw_sector_block_invalid"),
            "扇区 16 超出 1K"
        );
        assert_eq!(
            valid("0", "0").validation_error(),
            Some("dialog_msg_phw_sector_block_invalid")
        );
        assert_eq!(
            valid("0", "3").validation_error(),
            Some("dialog_msg_phw_sector_block_invalid")
        );
    }
}
