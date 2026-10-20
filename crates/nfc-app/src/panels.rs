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

use egui::{Align, Context, CornerRadius, Layout, Margin, RichText, ScrollArea, Ui};

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
const SIZE_DUMP_EDITOR: [f32; 2] = [720.0, 640.0];
const SIZE_DUMP_COMPARATOR: [f32; 2] = [760.0, 620.0];
const SIZE_DUMP_HISTORY: [f32; 2] = [520.0, 560.0];
/// 手机/手环面板。S16：原版 400×360 在加入滚动/步骤提示后偏小，调到 440×420。
const SIZE_PHW: [f32; 2] = [440.0, 420.0];

/// 面板标识。顺序即绘制顺序（决定叠放层级）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Panel {
    Settings,
    InputKeys,
    HardNested,
    DictTest,
    DumpEditor,
    DumpComparator,
    DumpHistory,
    About,
    PhoneWristband,
}

impl Panel {
    const ALL: [Panel; 9] = [
        Panel::Settings,
        Panel::InputKeys,
        Panel::HardNested,
        Panel::DictTest,
        Panel::DumpEditor,
        Panel::DumpComparator,
        Panel::DumpHistory,
        Panel::About,
        Panel::PhoneWristband,
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
            Panel::DumpEditor => "html_dump_editor",
            Panel::DumpComparator => "html_dump_comparator",
            Panel::DumpHistory => "html_dump_history",
            Panel::About => "html_about",
            Panel::PhoneWristband => "html_phw_title",
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
            Panel::DumpEditor => SIZE_DUMP_EDITOR,
            Panel::DumpComparator => SIZE_DUMP_COMPARATOR,
            Panel::DumpHistory => SIZE_DUMP_HISTORY,
            Panel::PhoneWristband => SIZE_PHW,
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
                | Self::DumpEditor
                | Self::DumpComparator
                | Self::DumpHistory
                | Self::PhoneWristband
        )
    }

    /// 该面板当前是否打开。
    pub fn is_open(self, p: &nfc_core::state::Panels) -> bool {
        match self {
            Panel::Settings => p.settings,
            Panel::InputKeys => p.input_keys,
            Panel::HardNested => p.hard_nested,
            Panel::DictTest => p.dict_test,
            Panel::DumpEditor => p.dump_editor,
            Panel::DumpComparator => p.dump_comparator,
            Panel::DumpHistory => p.dump_history,
            Panel::About => p.about,
            Panel::PhoneWristband => p.phone_wristband,
        }
    }

    fn set_open(self, p: &mut nfc_core::state::Panels, v: bool) {
        match self {
            Panel::Settings => p.settings = v,
            Panel::InputKeys => p.input_keys = v,
            Panel::HardNested => p.hard_nested = v,
            Panel::DictTest => p.dict_test = v,
            Panel::DumpEditor => p.dump_editor = v,
            Panel::DumpComparator => p.dump_comparator = v,
            Panel::DumpHistory => p.dump_history = v,
            Panel::About => p.about = v,
            Panel::PhoneWristband => p.phone_wristband = v,
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
}

/// 文本框 → 扇区号。原版 `parseInt` 得 `NaN` 时工具会报错，此处兜底为 0。
fn parse_sector(text: &str) -> u8 {
    text.trim().parse().unwrap_or(0)
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

/// S9 各面板的表单状态。
#[derive(Clone, Debug, Default)]
pub struct Forms {
    /// inputKeys 的文本框内容。
    pub input_keys: String,
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
    /// 另加一个「✓」按钮作为鼠标用户的后备（egui 的
    /// `key_pressed(Enter)` 在 `TextEdit` 内不一定被识别为提交）。
    pub history_rename: Option<(String, String)>,
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
    /// 有未保存改动。
    pub dirty: bool,
}

impl EditorForm {
    /// 从 dump 字节载入（对标 `command.js:425-432` 的分组）。
    pub fn load(&mut self, name: Option<String>, bytes: &[u8]) -> Result<(), String> {
        self.sectors = dump::to_sector_lines(bytes).map_err(|e| e.to_string())?;
        self.file_name = name;
        self.dirty = false;
        Ok(())
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
fn commit_rename(dir: &std::path::Path, old: &str, new: &str, app: &mut NfcApp) {
    if new.trim().is_empty() || new == old {
        app.forms.history_rename = None;
        return;
    }
    match nfc_core::dumpfiles::rename(dir, old, new) {
        Ok(_) => {
            for s in app.forms.history_selected.iter_mut() {
                if s == old {
                    *s = new.to_owned();
                }
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
        Panel::DumpEditor => dump_editor(ui, app),
        Panel::DumpComparator => dump_comparator(ui, app),
        Panel::DumpHistory => dump_history(ui, app),
        Panel::PhoneWristband => phone_wristband(ui, app),
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
        ui.horizontal(|ui| {
            ui.label(RichText::new(t("html_data_dir")).strong());
            ui.add(egui::Label::new(RichText::new(dump_dir.display().to_string())).truncate())
                .on_hover_text(dump_dir.display().to_string());
            open_dump = ui.button(t("html_open_dump_folder")).clicked();
        });
        ui.horizontal(|ui| {
            ui.label(RichText::new(t("html_tool_dir")).strong());
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
        ui.horizontal(|ui| {
            ui.label(RichText::new(t("html_current_version")).strong());
            ui.label(env!("APP_VERSION"));
            ui.label(env!("BUILDER"));
        });
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
    if close {
        return false;
    }
    true
}

/// `inputkeys.html`：标题 + 密钥输入框 + 取消/确定。
fn input_keys(ui: &mut Ui, app: &mut NfcApp) -> bool {
    ui.label(t("html_please_input_known_keys_split_by_comma"));
    ui.add_space(4.0);
    ui.add(egui::TextEdit::singleline(&mut app.forms.input_keys).desired_width(f32::INFINITY));

    let (cancel, ok) = footer(ui, &t("dialog_button_ok"), true);
    if ok {
        // command.js:154-157 —— 先用正则扫出所有 12 位 hex，逐个转 `-k<key>`。
        // 原版 `keys.match(...)` 匹配不到会返回 null 再 `.forEach` 抛 TypeError，
        // 此处空结果不派发（等价于"什么都不做"）。
        let keys = nfc_core::model::keys::parse_12hex(&app.forms.input_keys);
        if !keys.is_empty() {
            app.pending
                .push(Action::WithArg("done-input-keys-read-IC", keys.join(",")));
        }
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
        }
    });

    let (cancel, ok) = footer(ui, &t("html_execute"), !no_config);
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

    let (cancel, ok) = footer(ui, &t("dialog_button_ok"), !f.no_config);
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

const REPO_URL: &str = "https://github.com/GSWXXN/NFCToolsGUI";
const LICENSE_URL: &str = "https://github.com/GSWXXN/NFCToolsGUI/blob/main/LICENSE";

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
            .corner_radius(CornerRadius::same(nfc_core::theme::RADIUS[0] as u8))
            .inner_margin(Margin::symmetric(8, 6))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(t("html_phw_card_no")).strong());
                    ui.label(RichText::new(&card_no).monospace());
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
                ui.label(t("html_phw_range"));
            });

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
            if ui.add_enabled(has_block0, write_uid).clicked() {
                app.exec("phw-write-uid", None);
            }
            if ui.add_enabled(has_block0, write_data).clicked() {
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
    if close {
        return false;
    }
    true
}

/// 文本框 → 无符号扇区/块号（原版 `parseInt` 得 NaN 时兜底为 0，越界由引擎拦）。
fn parse_u32(text: &str) -> u32 {
    text.trim().parse().unwrap_or(0)
}

// ───────────────────────── S10 dump 三面板 ─────────────────────────

/// 原版图例的四段配色 token（`dumpEditor.html:31-37` + `tokens.css`）。
/// egui 无 `<span class="uid">`，故按段着色，token 与原版一致。
fn legend_color(paint: crate::theme_ui::Paint, which: u8) -> egui::Color32 {
    match which {
        0 => paint.color(paint.uid),
        1 => paint.color(paint.key_a),
        2 => paint.color(paint.access),
        _ => paint.color(paint.key_b),
    }
}

/// `dumpEditor.html`：每扇区一个 4 行 hex 编辑框 + 底部文件条 + 图例。
///
/// 原版是 `contenteditable` 的 div，`getHexData()` 取 `innerText` 再去掉
/// 换行（`dumpEditor.js:14`）。此处用 `TextEdit::multiline` 承载，
/// 语义等价；差异是原版可粘贴任意文本后由 `checkData` 判定，
/// egui 侧同样交给 `EditorForm::validate`。
fn dump_editor(ui: &mut Ui, app: &mut NfcApp) -> bool {
    let danger = app.paint().color(app.paint().danger);
    let legend = [
        (t("html_uid_manufacture_info"), 0u8),
        (t("html_key_a"), 1),
        (t("html_key_b"), 3),
        (t("html_access_control"), 2),
    ];
    let paint = app.paint();
    let legend_colors: Vec<egui::Color32> = legend
        .iter()
        .map(|(_, w)| legend_color(paint, *w))
        .collect();

    if app.forms.editor.sectors.is_empty() {
        ui.vertical_centered(|ui| {
            ui.add_space(24.0);
            ui.label(t("html_current_file") + "-");
            ui.add_space(8.0);
        });
    } else {
        // 编辑区：逐扇区一个多行输入
        let mut dirty = false;
        ScrollArea::vertical().show(ui, |ui| {
            for (si, sector) in app.forms.editor.sectors.iter_mut().enumerate() {
                // 校验（`dumpEditor.js:98-118` 的 checkData）
                let invalid = sector.len() != dump::BLOCKS_PER_SECTOR
                    || sector
                        .iter()
                        .any(|l| l.chars().count() != dump::BLOCK_SIZE * 2);
                // S15：非法扇区整块红框包裹（原版 styles.invalid 的做法），
                // 替代行尾「!」——框住比一个感叹号更难看漏
                let mut frame = egui::Frame::NONE
                    .inner_margin(egui::Margin::symmetric(6, 4))
                    .corner_radius(egui::CornerRadius::same(4));
                if invalid {
                    frame = frame.stroke(egui::Stroke::new(1.5, danger));
                }
                frame.show(ui, |ui| {
                    ui.horizontal(|ui| {
                        // 扇区号固定窄列，右对齐贴输入框
                        ui.set_width(56.0);
                        ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                            ui.add_space(6.0);
                            ui.label(
                                RichText::new(format!("{}{si}", t("html_sector")))
                                    .strong()
                                    .color(if invalid {
                                        danger
                                    } else {
                                        paint.color(paint.text)
                                    }),
                            );
                        });
                    });
                    // 把 4 行拼成带换行的单串，交给一个 multiline 输入
                    let mut text = sector.join("\n");
                    let resp = ui.add(
                        egui::TextEdit::multiline(&mut text)
                            .desired_rows(4)
                            .desired_width(f32::INFINITY)
                            .font(egui::TextStyle::Monospace),
                    );
                    if resp.changed() {
                        dirty = true;
                        *sector = text.split('\n').map(str::to_owned).collect();
                    }
                });
                ui.add_space(4.0);
            }
        });
        if dirty {
            app.forms.editor.dirty = true;
        }
    }

    // 底部文件条（`dumpEditor.html:16-27`）
    ui.separator();
    let name = app
        .forms
        .editor
        .file_name
        .clone()
        .unwrap_or_else(|| "-".to_owned());
    ui.horizontal(|ui| {
        ui.label(format!("{}{name}", t("html_current_file")));
    });
    ui.horizontal(|ui| {
        if ui.button(t("html_open_file")).clicked() {
            // 必须走 exec：engine 不认这个名字（不在纯 UI 名单里），
            // 直接 push 会弹出 NotWired 错误事件。
            app.exec("dump-editor-choose-file", None);
        }
        // 校验通过才允许保存（原版 checkData(true) 失败会 alert）
        let ok = app.forms.editor.can_save();
        if ui
            .add_enabled(ok, egui::Button::new(t("html_save_file")))
            .clicked()
        {
            // "0" = 非另存为；hex 由 app 侧从编辑器表单现取
            app.exec("dump-editor-save", Some("0".to_owned()));
        }
        if ui
            .add_enabled(ok, egui::Button::new(t("html_save_as_file")))
            .clicked()
        {
            // "1" = 另存为，弹系统保存框
            app.exec("dump-editor-save", Some("1".to_owned()));
        }
    });

    // 图例（`dumpEditor.html:29-39`）——原版的「更新颜色」是重算高亮，
    // egui 每帧按 token 着色本就实时，故该按钮无实际作用，此处只显示图例。
    ui.horizontal(|ui| {
        ui.label(format!("{}{}", t("html_caption"), ""));
        for ((label, _), color) in legend.iter().zip(&legend_colors) {
            ui.colored_label(*color, format!("{label} | "));
        }
    });

    true
}

/// `dumpComparator.html`：A/B 选文件 + 逐字符 diff 高亮。
fn dump_comparator(ui: &mut Ui, app: &mut NfcApp) -> bool {
    let paint = app.paint();
    let mark = paint.color(paint.mark_bg);
    let mark_fg = paint.color(paint.mark_fg);

    // 顶部选文件条（`dumpComparator.html:12-18`）
    ui.horizontal(|ui| {
        ui.label(format!("{} (A): ", t("html_dump")));
        ui.label(
            app.forms
                .comparator
                .a_name
                .clone()
                .unwrap_or_else(|| t("html_choose_file")),
        );
        if ui.button("…").clicked() {
            app.exec("dump-comparator-choose-file", Some("A".to_owned()));
        }
    });
    ui.horizontal(|ui| {
        ui.label(format!("{} (B): ", t("html_dump")));
        ui.label(
            app.forms
                .comparator
                .b_name
                .clone()
                .unwrap_or_else(|| t("html_choose_file")),
        );
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

/// `dumpHistory.html`：文件列表 + 底部四个按钮。
fn dump_history(ui: &mut Ui, app: &mut NfcApp) -> bool {
    // 列表来自 core 的 dumpfiles::list（只列 .mfd）
    let dir = nfc_core::paths::Paths::from_app_data().dumpfiles_dir();
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
        ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
            for f in &files {
                let sel = app.forms.history_selected.iter().any(|s| s == f);
                let mut checked = sel;
                if ui.checkbox(&mut checked, f).clicked() {
                    if checked {
                        app.forms.history_selected.push(f.clone());
                    } else {
                        app.forms.history_selected.retain(|s| s != f);
                    }
                }
                let editing = app
                    .forms
                    .history_rename
                    .as_ref()
                    .is_some_and(|(o, _)| o == f);
                if editing {
                    let (_, text) = app.forms.history_rename.as_mut().unwrap();
                    let r = ui.add(egui::TextEdit::singleline(text).desired_width(160.0));
                    // 原版是「失焦**或**回车」提交（`dumpHistory.js:34`）。
                    // egui 两者都能做：失焦用 `Response::lost_focus()`，
                    // 回车直接查输入。注意**不能**写成 `&&`——单行 TextEdit
                    // 按回车不会失焦，而失焦那一帧通常也没有回车，
                    // 条件互斥会让回车提交永远不触发。
                    if r.lost_focus() || ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        commit = app.forms.history_rename.take();
                    }
                    if ui.small_button("✓").clicked() {
                        commit = app.forms.history_rename.take();
                    }
                    if ui.small_button("✗").clicked() {
                        app.forms.history_rename = None;
                    }
                } else if ui
                    .small_button("✎")
                    .on_hover_text(t("html_rename"))
                    .clicked()
                {
                    app.forms.history_rename = Some((f.clone(), f.clone()));
                }
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
        ui.horizontal(|ui| {
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
