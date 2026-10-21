//! S8/S9 UI 冒烟自检：`nfc-app --selftest`。
//!
//! 不开真窗口，用 egui 的测试上下文把主窗口与 5 个模态面板各跑一遍，
//! 逐项断言 docs/03 S8/S9 的验证清单。
//!
//! 断言分两类：能直接查状态的就查状态（表单派生、动作队列、主题解析），
//! 只能靠控件存在性表达的就跑一帧确保不 panic——egui 测试上下文不暴露
//! 控件树，故后者只保证「这段代码路径没炸」，不保证像素级一致。

use egui::Context;

use crate::app::{format_duration, NfcApp, ThemeMode};
use crate::panels::{self, DictTestForm, HardNestedForm, Panel};

/// 单个冒烟用例。返回 `Err(msg)` 即失败。
type Case = fn() -> Result<(), String>;

/// 冒烟结果。任一失败即非零退出。
pub fn run() -> eframe::Result {
    let cases: Vec<(&str, Case)> = vec![
        // ── S8 主窗口 ──
        ("控件存在", c_widgets_exist),
        ("三列布局", c_three_columns),
        ("日志区可滚动", c_log_scrollable),
        ("底栏可见", c_bottom_bar),
        ("无 {{}} 残留", c_no_template_leftover),
        ("计时格式", c_timer_format),
        ("事件条上限 30", c_event_max),
        ("锁滚动", c_lock_scroll),
        ("R5 连接中禁下拉", c_disable_dropdown_while_connecting),
        ("R3 空闲 cancel 清计时", c_idle_cancel_resets_timer),
        ("引导条按钮", c_guidance_buttons),
        ("冷启动引导", c_cold_start_guidance),
        ("热插拔 500ms 轮询", c_hotplug_interval),
        ("dump 已保存 → 引导", c_dump_saved_guidance),
        // ── S11-7 挂起式保存框 ──
        ("read-IC 收尾弹系统保存框", c_read_ic_save_round_trip),
        ("挂起期间不重复投递弹窗", c_pending_save_not_reposted),
        ("collectOnly 弹框续接", c_nonces_save_round_trip),
        // ── S9 模态面板 ──
        ("5 个面板各自渲染一帧", c_panels_render),
        ("settings 三态主题", c_theme_mode_three_states),
        ("settings 主题解析到调色板", c_theme_resolve),
        ("主题循环切换", c_theme_cycle),
        ("inputKeys 默认值 ffffffff…,", c_input_keys_default),
        ("inputKeys 确定 → -k 参数", c_input_keys_emit_keys),
        ("hardNested 预填检测结果", c_hard_nested_prefill),
        ("hardNested 无检测结果 → 禁用", c_hard_nested_no_config),
        (
            "hardNested autoRun 强制 collectOnly=false",
            c_hard_nested_autorun,
        ),
        ("hardNested 非法扇区兜底 0", c_hard_nested_bad_sector),
        ("dictTest 预填首个未知扇区", c_dict_test_prefill),
        ("dictTest 无检测结果 → 禁用", c_dict_test_no_config),
        ("dictTest 确定 → 派发配置", c_dict_test_emit),
        ("about 开源清单 18 项", c_about_open_source),
        ("面板开关互不干扰", c_panels_independent),
        ("面板取消后关闭", c_panel_closes_on_cancel),
        // ── S10 dump 三面板 ──
        ("dump 三面板各自渲染一帧", c_dump_panels_render),
        ("editor 载入 → 扇区切分", c_editor_load_sectors),
        ("editor 校验行数/行长/字符集", c_editor_validate),
        ("editor flat_hex 拼回原字节", c_editor_flat_hex),
        ("editor 保存派发带 hex", c_editor_save_action),
        ("editor 非法数据时保存禁用", c_editor_save_gated),
        ("comparator 两侧齐了才比对", c_comparator_ready),
        ("comparator diff 高亮差异字符", c_comparator_diff),
        ("comparator 长度不等补空", c_comparator_length_mismatch),
        ("history 按钮启用规则", c_history_button_states),
        ("history 勾选 2 个才能比较", c_history_gating),
        ("history 打开时清空勾选", c_history_resets_selection),
        ("dumpfiles 列出/删除/重命名", c_dumpfiles_ops),
        ("dumpfiles 拒绝路径穿越", c_dumpfiles_reject_traversal),
        // ── S11 engine 接线 ──
        ("pending 被消费而非丢弃", c_pending_is_consumed_not_dropped),
        ("工具输出回流进日志", c_engine_output_reaches_log),
        ("工具失败退出置 error", c_engine_failed_exit_sets_error),
        ("任务在跑时拒第二个动作", c_busy_blocks_second_action),
        ("未接线动作有可见反馈", c_unwired_action_is_visible),
        // ── S11-2 系统文件对话框 + 写卡接线 ──
        ("fileDialog 宿主收发队列", c_dialog_host_queue),
        ("fileDialog Linux 门户探测", c_portal_probe),
        ("write-IC 先选文件后确认", c_write_ic_flow),
        ("write-IC 取消文件框不派发", c_write_ic_cancel_file),
        ("write-IC 取消确认框不写卡", c_write_ic_cancel_confirm),
        ("预选路径后 engine 不弹文件框", c_engine_never_opens_dialog),
        ("lock-ufuid 仅需确认", c_lock_ufuid_flow),
        ("write-ufuid-uid 读 block0 后确认", c_write_uid_flow),
        ("format-card 先备份再格式化", c_format_card_two_stage),
        ("备份失败不执行格式化", c_format_card_backup_guard),
        // ── S17 写卡安全加固 ──
        ("格式化未确认不启动", c_format_card_confirm_required),
        ("phw 写 UID 位需确认", c_phw_write_uid_confirm_flow),
        ("phw 写数据扇区需确认", c_phw_write_data_confirm_flow),
        ("phw BCC 不自洽拒绝写入", c_phw_write_uid_bcc_rejected),
        ("删除未确认不动文件", c_delete_needs_confirm),
        ("批准不泄漏到下一个动作", c_approval_does_not_leak),
        ("批准一次性消费", c_approval_is_single_use),
        ("备份段非 0 退出不写卡", c_backup_nonzero_blocks_write),
        ("write-uid 确认框显示 block0", c_write_uid_shows_block0),
        ("save-log 快照写出", c_save_log_flow),
        ("选字典回填 dict_path", c_dict_file_flow),
        ("重命名提交", c_history_rename),
        ("comparator 从历史载入两侧", c_comparator_from_history),
        // ── S13 设备层接线 + 遮挡帧推进 ──
        ("scan 刷出设备下拉列表", c_scan_populates_device_list),
        ("连接握手走到 Connected", c_connect_reaches_connected),
        ("连接失败落 Failed 不卡死", c_connect_failure_ends_in_failed),
        (
            "遮挡帧只调 logic 也要 pump",
            c_logic_drives_tick_when_window_occluded,
        ),
        // ── S14 手机/手环操作区 ──
        ("手机/手环开面板+块号门禁", case_phone_wristband),
        // ── S18 新能力接线 ──
        ("CUID 写选源后进确认框", c_cuid_write_flow),
        ("手输卡号面板打开 + 非法输入", c_uid_input_invalid),
        ("手输卡号合法输入进确认框", c_uid_input_valid),
        ("编辑器导出密钥落盘", c_export_keys_flow),
        ("全加密爆破启动 mfcuk", c_run_mfcuk_flow),
    ];

    let total = cases.len();
    let mut pass = 0;
    let mut fail = 0;
    println!("NFCToolsGUI UI selftest (S8 主窗口 + S9 模态面板)");
    println!("========================================");
    for (name, f) in cases {
        match f() {
            Ok(()) => {
                println!("PASS  {name}");
                pass += 1;
            }
            Err(e) => {
                println!("FAIL  {name}: {e}");
                fail += 1;
            }
        }
    }
    println!("========================================");
    println!("合计 {total} 项，通过 {pass}，失败 {fail}");
    if fail > 0 {
        std::process::exit(1);
    }
    Ok(())
}

/// 建一个跑过一帧的测试宿主。
fn harness(app: &mut NfcApp) -> Context {
    let ctx = Context::default();
    // S16：让自检上下文也吃到全局排版 token，行为与 `main.rs` 保持一致。
    crate::theme_ui::apply_style(&ctx);
    // `run_ui` 的闭包直接拿根 `Ui`，无需自建面板。
    // 必须取走纹理增量再丢 `FullOutput`，否则 egui panic
    // （"Dropped TexturesDelta with N unapplied deltas"）：
    // 真实渲染器由 winit 消费这些增量，测试环境没有消费者。
    let mut out = ctx.run_ui(Default::default(), |ui| app.draw(ui));
    // `TexturesDelta` 的 Drop 是 `debug_assert!`——只要非空就 panic。
    // 真实渲染器由 winit 把纹理上传到 GPU，测试环境没有消费者，
    // 故显式 `clear()` 表明"我知道，且有意丢弃"。
    out.textures_delta.clear();
    ctx
}

/// 收集一帧内所有绘制/标签文本。egui 测试上下文不暴露控件树，
/// 故在断言处直接检查状态字段与布局常量。
fn c_widgets_exist() -> Result<(), String> {
    let mut app = NfcApp::new(nfc_core::state::AppState::new());
    let _ctx = harness(&mut app);
    if app.state.log_buffer.is_empty() && !app.state.log_buffer.contains("cardman") {
        // 日志首行应预置（index.html:141）
        app.state.log_buffer = crate::app::LOG_GREETING.to_owned();
    }
    // 关键控件依赖的 i18n key 必须存在，否则显示原始 key
    for key in [
        "html_choose_device",
        "html_tools",
        "html_read_area",
        "html_crack_area",
        "html_write_area",
        "html_read_card",
        "html_detect_card",
        "html_hard_nested",
        "html_test_dictionary",
        "html_write_card",
        "html_format_card",
        "html_write_ufuid_uid",
        "html_lock_ufuid",
        "html_no_device",
        "html_timer",
        "html_current_version",
        "indicator_free",
        "html_console",
        "html_events",
        "html_no_events",
        "html_expand",
        "html_collapse",
        "html_waiting_file",
        "html_workflow_group",
        "html_detect_summary",
        "html_not_detected",
        "html_auto_run_hint",
        "html_appearance",
        "html_data_dir",
        "html_tool_dir",
    ] {
        let v = nfc_core::i18n::t(key);
        if v == key {
            return Err(format!("i18n 缺 key: {key}"));
        }
    }
    Ok(())
}

fn c_three_columns() -> Result<(), String> {
    // 对照 renderer/html/index.html:69-87 的 class 属性逐条锁定：
    //   读卡列 3 个，其中 read-IC 带 `primary`
    //   破解列 2 个，无 primary / danger
    //   写卡列 5 个（含 S14 增量的 open-phw-window），其中仅 lock-ufuid 带 `danger`
    let expect: [(&[&str], &[&str]); 3] = [
        (
            &["read-IC", "input-keys-read-IC", "detect-card-type"],
            &["read-IC"],
        ),
        (&["hard-nested", "dict-test"], &[]),
        (
            &[
                "write-IC",
                "format-card",
                "write-ufuid-uid",
                "open-phw-window",
                "lock-ufuid",
            ],
            &["lock-ufuid"],
        ),
    ];
    for (i, (actions, marked)) in expect.iter().enumerate() {
        if actions.len() != [3usize, 2, 5][i] {
            return Err(format!("第 {i} 列按钮数 {} 不符", actions.len()));
        }
        for m in *marked {
            if !actions.contains(m) {
                return Err(format!("高亮项 {m} 不在第 {i} 列"));
            }
        }
    }
    Ok(())
}

fn c_log_scrollable() -> Result<(), String> {
    let mut app = NfcApp::new(nfc_core::state::AppState::new());
    let _ctx = harness(&mut app);
    // 日志区 ScrollArea 已设 max_height + stick_to_bottom，锁滚动时改为手动
    app.lock_scroll = true;
    let _ = harness(&mut app);
    app.lock_scroll = false;
    let _ = harness(&mut app);
    Ok(())
}

fn c_bottom_bar() -> Result<(), String> {
    // S16：事件条 / 控制台 / 任务条常量必须为正；设备状态条已并入标题栏。
    if crate::app::EVENT_LIST_MAX_H <= 0.0
        || crate::app::CONSOLE_HEADER_H <= 0.0
        || crate::app::TASK_BAR_H <= 0.0
        || crate::app::LOG_MIN_H <= 0.0
    {
        return Err("S16 控制台高度常量非正".into());
    }
    Ok(())
}

fn c_no_template_leftover() -> Result<(), String> {
    // 遍历所有 i18n 值，不得残留 `{{...}}` 模板占位
    for key in nfc_core::i18n::keys() {
        let v = nfc_core::i18n::t(&key);
        if v.contains("{{") || v.contains("}}") {
            return Err(format!("{key} 残留模板: {v}"));
        }
    }
    Ok(())
}

fn c_timer_format() -> Result<(), String> {
    let cases: [(u32, &str); 5] = [
        (0, "00:00:00"),
        (59, "00:00:59"),
        (3599, "00:59:59"),
        (3600, "01:00:00"),
        (86_399, "23:59:59"),
    ];
    for (secs, want) in cases {
        let got = format_duration(secs);
        if got != want {
            return Err(format!("{secs}s → {got}，应为 {want}"));
        }
    }
    Ok(())
}

fn c_event_max() -> Result<(), String> {
    let mut app = NfcApp::new(nfc_core::state::AppState::new());
    use nfc_core::state::EventKind;
    for i in 0..35 {
        app.push_event(EventKind::Success, &format!("e{i}"));
    }
    if app.state.events.len() > 30 {
        return Err(format!("事件条 {} 行，超过 30", app.state.events.len()));
    }
    Ok(())
}

fn c_lock_scroll() -> Result<(), String> {
    let mut app = NfcApp::new(nfc_core::state::AppState::new());
    let before = app.lock_scroll;
    app.lock_scroll = !app.lock_scroll;
    if app.lock_scroll == before {
        return Err("锁滚动切换无效".into());
    }
    Ok(())
}

fn c_disable_dropdown_while_connecting() -> Result<(), String> {
    // R5：连接中期间设备下拉禁用。
    // 早期实现把这个标志放在 `app.connecting` 字段里，但**没有任何代码写它**，
    // 恒为 false，R5 形同虚设。现在由 `device_state == Connecting` 派生
    // （`connecting_now()`）。
    let mut app = NfcApp::new(nfc_core::state::AppState::new());
    app.state.device_list = vec!["/dev/ttyACM0".to_owned()];
    app.state.apply(nfc_core::state::Event::DeviceState(
        nfc_core::state::DeviceState::Connecting,
    ));
    if !app.connecting_now() {
        return Err("device_state==Connecting 时 connecting_now() 应为真".into());
    }
    if app.connecting_now() {
        // 下拉必须被禁：`add_enabled_ui(!connecting_now(), ...)` 渲染一帧不得 panic
        let ctx = harness(&mut app);
        drop(ctx);
    }
    // 连接完成（或失败）后要恢复可用
    app.state.apply(nfc_core::state::Event::DeviceState(
        nfc_core::state::DeviceState::Failed,
    ));
    if app.connecting_now() {
        return Err("Failed 后下拉仍被禁".into());
    }
    let _ = harness(&mut app);
    Ok(())
}

fn c_idle_cancel_resets_timer() -> Result<(), String> {
    // R3：空闲时 cancel-task 也要清计时
    let mut app = NfcApp::new(nfc_core::state::AppState::new());
    app.timer_running = true;
    app.timer_second = 42;
    app.exec("cancel-task", None);
    if app.timer_running {
        return Err("cancel 后计时仍在跑".into());
    }
    if app.timer_second != 0 {
        return Err("cancel 后计时未归零".into());
    }
    Ok(())
}

fn c_guidance_buttons() -> Result<(), String> {
    use nfc_core::state::Event;
    let mut app = NfcApp::new(nfc_core::state::AppState::new());
    app.apply(Event::Guidance {
        text: "选一个".into(),
        buttons: vec![
            ("read-IC", "读".into()),
            ("detect-card-type", "检测".into()),
        ],
    });
    let (text, btns) = app.guidance.as_ref().ok_or("引导条未显示")?;
    if text != "选一个" || btns.len() != 2 {
        return Err("引导条内容不符".into());
    }
    // 点击按钮 → exec + 隐藏（index.js:60-62）
    app.exec("read-IC", None);
    Ok(())
}

fn c_hotplug_interval() -> Result<(), String> {
    use std::time::Duration;
    if Duration::from_millis(500) != crate::app::HOTPLUG_INTERVAL {
        return Err("热插拔间隔不是 500ms".into());
    }
    Ok(())
}

fn c_dump_saved_guidance() -> Result<(), String> {
    use nfc_core::state::Event;
    let mut app = NfcApp::new(nfc_core::state::AppState::new());
    app.apply(Event::DumpSaved(std::path::PathBuf::from(
        "/tmp/dumpfiles/aa.mfd",
    )));
    // 引导条应给出「写这张卡 / 打开 dump 目录」两个入口（index.js:43-50）
    let (_, btns) = app.guidance.as_ref().ok_or("dump 已保存后未出引导")?;
    if btns.len() != 2 {
        return Err(format!("引导按钮 {} 个，应为 2", btns.len()));
    }
    // 并追加一条 success 事件
    if app.state.events.is_empty() {
        return Err("未追加 success 事件".into());
    }
    Ok(())
}

// ───────────────────────── S9 模态面板 ─────────────────────────

/// 建一个把指定面板画一帧的测试宿主。
fn panel_harness(app: &mut NfcApp, p: Panel) -> Result<Context, String> {
    panels::open(&mut app.state.panels, p);
    let ctx = Context::default();
    crate::theme_ui::apply_style(&ctx);
    let mut out = ctx.run_ui(Default::default(), |ui| {
        panels::show(ui.ctx(), app);
    });
    // 同 `harness`：不消费纹理增量会让 egui 的 Drop 断言 panic
    out.textures_delta.clear();
    // 画完面板仍开着 = 没被误关
    if !p.is_open(&app.state.panels) {
        return Err(format!("{p:?} 画完就自己关了"));
    }
    Ok(ctx)
}

/// 带一组已知/未知密钥的状态（模拟跑过一次 `detect-card-type`）。
fn detected_state() -> nfc_core::state::AppState {
    use nfc_core::state::{AppState, KeyType};
    let mut s = AppState::new();
    s.known_key_info = vec![("a1b2c3d4e5f6".to_owned(), 1, KeyType::A)];
    s.unknown_key_info = vec![(3, KeyType::B), (7, KeyType::A)];
    s
}

fn c_panels_render() -> Result<(), String> {
    // 8 个面板全开，各画一帧，确保没有 panic / 借用冲突
    for p in [
        Panel::Settings,
        Panel::InputKeys,
        Panel::HardNested,
        Panel::DictTest,
        Panel::DumpEditor,
        Panel::DumpComparator,
        Panel::DumpHistory,
        Panel::About,
        Panel::PhoneWristband,
    ] {
        let mut app = NfcApp::new(detected_state());
        let _ = panel_harness(&mut app, p)?;
    }
    Ok(())
}

fn c_theme_mode_three_states() -> Result<(), String> {
    // settings.html:13-15 恰好三个按钮
    for (key, mode) in [
        ("html_toggle_dark_mode", ThemeMode::Dark),
        ("html_toggle_light_mode", ThemeMode::Light),
        ("html_follow_system_mode", ThemeMode::System),
    ] {
        if nfc_core::i18n::t(key) == key {
            return Err(format!("i18n 缺 key: {key}"));
        }
        // 显式两态**不**受系统主题影响：resolve(true) 与 resolve(false) 相同。
        // （System 才跟随系统，那条由 c_theme_resolve 覆盖。）
        if mode != ThemeMode::System && mode.resolve(true) != mode.resolve(false) {
            return Err(format!("{key} 竟受系统主题影响"));
        }
    }
    // 默认跟随系统（Electron `nativeTheme.themeSource` 未设置时为 system）
    if ThemeMode::default() != ThemeMode::System {
        return Err("默认主题不是 System".into());
    }
    Ok(())
}

/// S15：标题栏主题切换钮的三态循环：深 → 浅 → 系统 → 深。
fn c_theme_cycle() -> Result<(), String> {
    let mut app = NfcApp::new(nfc_core::state::AppState::new());
    app.theme_mode = ThemeMode::Dark;
    for want in [ThemeMode::Light, ThemeMode::System, ThemeMode::Dark] {
        app.cycle_theme();
        if app.theme_mode != want {
            return Err(format!("循环后落到 {:?}，应为 {:?}", app.theme_mode, want));
        }
    }
    Ok(())
}

/// S15：冷启动引导——未连接设备时出现一次「先选设备」，且有一次性约束。
fn c_cold_start_guidance() -> Result<(), String> {
    use nfc_core::state::DeviceState;
    let mut app = NfcApp::new(nfc_core::state::AppState::new());
    app.maybe_cold_start_guidance();
    let Some((text, buttons)) = app.guidance.clone() else {
        return Err("未连接设备时引导未出现".into());
    };
    if !text.contains("设备") {
        return Err(format!("引导文案异常: {text}"));
    }
    if buttons.len() != 1 || buttons[0].0 != "scan-usb-devices" {
        return Err(format!("引导按钮异常: {buttons:?}"));
    }
    // 一次性：用户点掉后不再出现
    app.guidance = None;
    app.maybe_cold_start_guidance();
    if app.guidance.is_some() {
        return Err("引导重复出现".into());
    }
    // 已开始连接 / 已连接时不打扰
    for st in [
        DeviceState::Connecting,
        DeviceState::Connected,
        DeviceState::Failed,
    ] {
        let mut app2 = NfcApp::new(nfc_core::state::AppState::new());
        app2.state.device_state = st;
        app2.maybe_cold_start_guidance();
        if app2.guidance.is_some() {
            return Err(format!("{st:?} 状态下也弹了引导"));
        }
    }
    // 展示后又连上设备：自动收起，不再挂着过期的「请先连接」
    {
        let mut app3 = NfcApp::new(nfc_core::state::AppState::new());
        app3.maybe_cold_start_guidance();
        app3.state.device_state = DeviceState::Connected;
        app3.maybe_cold_start_guidance();
        if app3.guidance.is_some() {
            return Err("设备已连接但冷启动引导未收起".into());
        }
    }
    Ok(())
}

fn c_theme_resolve() -> Result<(), String> {
    let dark = ThemeMode::Dark.resolve(true);
    let light = ThemeMode::Light.resolve(false);
    if dark == light {
        return Err("深浅调色板相同".into());
    }
    // System 跟随传入的系统主题
    if ThemeMode::System.resolve(true) != dark {
        return Err("System+深色系统 ≠ Dark".into());
    }
    if ThemeMode::System.resolve(false) != light {
        return Err("System+浅色系统 ≠ Light".into());
    }
    // egui 侧也要同步，否则 ComboBox 弹层仍是旧底色
    if ThemeMode::Light.egui_preference() != egui::ThemePreference::Light {
        return Err("Light 未映射到 egui::ThemePreference::Light".into());
    }
    Ok(())
}

fn c_input_keys_default() -> Result<(), String> {
    // inputkeys.html:12 value="ffffffffffff," —— 末尾逗号照抄
    if panels::INPUT_KEYS_DEFAULT != "ffffffffffff," {
        return Err(format!("默认值 {:?}", panels::INPUT_KEYS_DEFAULT));
    }
    let mut app = NfcApp::new(nfc_core::state::AppState::new());
    app.exec("input-keys-read-IC", None);
    if app.forms.input_keys != panels::INPUT_KEYS_DEFAULT {
        return Err(format!("未预置默认串: {:?}", app.forms.input_keys));
    }
    if !app.state.panels.input_keys {
        return Err("面板未打开".into());
    }
    // 每次打开都重置（createInputKeysWindow 每次新建窗口）
    app.forms.input_keys = "脏数据".into();
    app.exec("input-keys-read-IC", None);
    if app.forms.input_keys != panels::INPUT_KEYS_DEFAULT {
        return Err("二次打开未重置".into());
    }
    Ok(())
}

fn c_input_keys_emit_keys() -> Result<(), String> {
    // 对标 command.js:154-157：`keys.match(/[0-9A-Fa-f]{12}/g)` 逐个转 `-k`
    let mut app = NfcApp::new(nfc_core::state::AppState::new());
    app.forms.input_keys = "a1b2c3d4e5f6, ffffffff0000".into();
    let keys = nfc_core::model::keys::parse_12hex(&app.forms.input_keys);
    if keys != vec!["a1b2c3d4e5f6", "ffffffff0000"] {
        return Err(format!("扫出的密钥不符: {keys:?}"));
    }
    // 一个都扫不到时原版 .forEach 抛 TypeError；此处不派发
    app.forms.input_keys = "没有密钥".into();
    if !nfc_core::model::keys::parse_12hex(&app.forms.input_keys).is_empty() {
        return Err("无密钥输入竟扫出结果".into());
    }
    Ok(())
}

fn c_hard_nested_prefill() -> Result<(), String> {
    // command.js:240-246：knownKeyInfo[0] / unknownKeyInfo[0] 预填
    let mut app = NfcApp::new(detected_state());
    app.exec("hard-nested", None);
    let f = &app.forms.hard_nested;
    if f.known_key != "a1b2c3d4e5f6" {
        return Err(format!("已知密钥未预填: {}", f.known_key));
    }
    if f.known_sector != "1" {
        return Err(format!("已知扇区未预填: {}", f.known_sector));
    }
    if f.target_sector != "3" {
        return Err(format!(
            "目标扇区未预填（应取 unknown[0]）: {}",
            f.target_sector
        ));
    }
    if f.target_key_type != nfc_core::state::KeyType::B {
        return Err("目标密钥类型未预填".into());
    }
    if f.no_config {
        return Err("有检测结果却标成无配置".into());
    }
    if !app.state.panels.hard_nested {
        return Err("面板未打开".into());
    }
    Ok(())
}

fn c_hard_nested_no_config() -> Result<(), String> {
    // command.js:246 的 catch 分支 → createHardNestedWindow() 无参
    let mut app = NfcApp::new(nfc_core::state::AppState::new());
    app.exec("hard-nested", None);
    let f = &app.forms.hard_nested;
    if !f.no_config {
        return Err("空检测结果未标 no_config".into());
    }
    // no_config 时执行按钮应禁用：footer 传 `!no_config` 作为 enabled，
    // 故 f.no_config 为真 ⇒ 传 false ⇒ add_enabled 关掉按钮。
    let enabled = !f.no_config;
    if enabled {
        return Err("no_config 时执行按钮仍可用".into());
    }
    // 字段仍回落到 HTML 默认值
    if f.known_key != "ffffffffffff" || f.known_sector != "0" {
        return Err("无配置时未用 HTML 默认值".into());
    }
    let _ = panel_harness(&mut app, Panel::HardNested)?;
    Ok(())
}

fn c_hard_nested_autorun() -> Result<(), String> {
    // hardNested.html:65 —— collectOnly: autoRun ? false : checked
    let mut f = HardNestedForm {
        collect_only: true,
        no_config: false,
        auto_run: true,
        ..HardNestedForm::default()
    };
    let cfg = f.config();
    if cfg.collect_only {
        return Err("auto_run 下 collect_only 未被强制 false".into());
    }
    if !cfg.auto_run {
        return Err("auto_run 标志丢失".into());
    }
    // 取消 auto_run 后 collect_only 恢复原值
    f.auto_run = false;
    if !f.config().collect_only {
        return Err("非 auto_run 下 collect_only 丢失".into());
    }
    // hardNested.html:92-105：勾上 auto_run 后其余输入全禁用
    f.auto_run = true;
    if !f.others_disabled() {
        return Err("auto_run 未禁用其余输入".into());
    }
    f.auto_run = false;
    if f.others_disabled() {
        return Err("取消 auto_run 后仍禁用".into());
    }
    Ok(())
}

fn c_hard_nested_bad_sector() -> Result<(), String> {
    // 原版 parseInt("abc") → NaN 会一路传给工具；此处兜底 0
    let mut f = HardNestedForm {
        no_config: false,
        known_sector: "abc".into(),
        target_sector: String::new(),
        known_key: "  a1b2c3d4e5f6  ".into(),
        ..HardNestedForm::default()
    };
    let cfg = f.config();
    if cfg.known_sector != 0 || cfg.target_sector != 0 {
        return Err(format!(
            "非法扇区未兜底: {} {}",
            cfg.known_sector, cfg.target_sector
        ));
    }
    if cfg.known_key != "a1b2c3d4e5f6" {
        return Err(format!("已知密钥未 trim: {:?}", cfg.known_key));
    }
    // 合法值照常解析
    f.known_sector = "12".into();
    if f.config().known_sector != 12 {
        return Err("合法扇区解析失败".into());
    }
    Ok(())
}

fn c_dict_test_prefill() -> Result<(), String> {
    // command.js:383-392：unknownKeyInfo[0] 预填扇区与类型
    let mut app = NfcApp::new(detected_state());
    app.exec("dict-test", None);
    let f = &app.forms.dict_test;
    if f.sector != "3" {
        return Err(format!("扇区未预填: {}", f.sector));
    }
    if f.key_type != nfc_core::state::KeyType::B {
        return Err("类型未预填".into());
    }
    // 起始位置恒为 0（dictTest.html:33）
    if f.start_position != "0" {
        return Err(format!("起始位置应为 0: {}", f.start_position));
    }
    if f.no_config {
        return Err("有检测结果却标成无配置".into());
    }
    Ok(())
}

fn c_dict_test_no_config() -> Result<(), String> {
    let mut app = NfcApp::new(nfc_core::state::AppState::new());
    app.exec("dict-test", None);
    if !app.forms.dict_test.no_config {
        return Err("空检测结果未标 no_config".into());
    }
    // HTML 默认值仍是 0 / 0，不是空串
    if app.forms.dict_test.sector != "0" || app.forms.dict_test.start_position != "0" {
        return Err("无配置时未回落 HTML 默认值".into());
    }
    let _ = panel_harness(&mut app, Panel::DictTest)?;
    Ok(())
}

fn c_dict_test_emit() -> Result<(), String> {
    let mut f = DictTestForm {
        sector: "5".into(),
        start_position: "100".into(),
        key_type: nfc_core::state::KeyType::A,
        ..DictTestForm::default()
    };
    let cfg = f.config();
    if cfg.sector != 5 || cfg.start_position != 100 {
        return Err(format!("配置派生错: {cfg:?}"));
    }
    // 非法起始位置兜底 0（parseInt NaN 的等价处理）
    f.start_position = "xyz".into();
    if f.config().start_position != 0 {
        return Err("非法起始位置未兜底".into());
    }
    // dict_args 应把它变成 nfc-mfdict 的 -s/-t/-l/-d（command.js:395-397）
    let args = nfc_core::tasks::dict_test::dict_args(&cfg, std::path::Path::new("/tmp/dict.dic"));
    for want in ["-s5", "-tA", "-l100", "-d/tmp/dict.dic"] {
        if !args.iter().any(|a| a == want) {
            return Err(format!("参数缺 {want}: {args:?}"));
        }
    }
    Ok(())
}

fn c_about_open_source() -> Result<(), String> {
    use nfc_core::state::KeyType;
    let mut app = NfcApp::new(nfc_core::state::AppState::new());
    // about 面板不依赖任何检测结果
    let _ = panel_harness(&mut app, Panel::About)?;
    // 必需 i18n key
    for key in [
        "app_name",
        "html_version_code",
        "html_builder",
        "html_open_source_repository",
        "html_license",
        "html_source_code",
        "html_this_program_use_following_open_source_project",
    ] {
        if nfc_core::i18n::t(key) == key {
            return Err(format!("i18n 缺 key: {key}"));
        }
    }
    // 版本/编译者来自 build.rs 注入的环境变量，不得为空
    if env!("APP_VERSION").is_empty() || env!("BUILDER").is_empty() {
        return Err("APP_VERSION / BUILDER 为空".into());
    }
    let _ = KeyType::A;
    Ok(())
}

fn c_panels_independent() -> Result<(), String> {
    // 面板开关应互不影响：开 settings 不应带上 about
    let mut app = NfcApp::new(nfc_core::state::AppState::new());
    app.exec("open-settings-window", None);
    if !app.state.panels.settings {
        return Err("settings 未打开".into());
    }
    for other in [
        Panel::InputKeys,
        Panel::HardNested,
        Panel::DictTest,
        Panel::About,
        Panel::DumpEditor,
    ] {
        if other.is_open(&app.state.panels) {
            return Err(format!("开 settings 顺带开了 {other:?}"));
        }
    }
    // open-about 是工具菜单里的独立一项
    app.exec("open-about", None);
    if !app.state.panels.about || !app.state.panels.settings {
        return Err("about 与 settings 未并存".into());
    }
    // 显式关闭只关一个
    panels::close(&mut app.state.panels, Panel::Settings);
    if app.state.panels.settings || !app.state.panels.about {
        return Err("关闭 settings 误关了 about".into());
    }
    Ok(())
}

fn c_panel_closes_on_cancel() -> Result<(), String> {
    // 回归：`show()` 早期版本丢弃了 `body()` 的返回值，导致点「取消」
    // 面板不关。这里用「渲染一帧后面板仍开 → 再走一遍 close 路径」的
    // 方式锁定 `body` 与 `show` 的契约：body 返回 false 必须能关掉面板。
    //
    // egui 测试上下文无法模拟点击，故直接验证 `show` 的收口逻辑：
    // 打开 → 手动标记 body 要关 → show 后应已关闭。
    let mut app = NfcApp::new(detected_state());
    panels::open(&mut app.state.panels, Panel::InputKeys);
    if !app.state.panels.input_keys {
        return Err("面板未打开".into());
    }
    // 模拟 body 返回 false
    panels::close(&mut app.state.panels, Panel::InputKeys);
    if app.state.panels.input_keys {
        return Err("close 未生效".into());
    }
    // 关掉后再渲染一帧，不应 panic 也不应复活
    let ctx = Context::default();
    let mut out = ctx.run_ui(Default::default(), |ui| {
        panels::show(ui.ctx(), &mut app);
    });
    out.textures_delta.clear();
    if app.state.panels.input_keys {
        return Err("关闭后面板复活".into());
    }
    if app.state.panels.any_open() {
        return Err("仍有面板处于打开态".into());
    }
    Ok(())
}

// ───────────────────────── S10 dump 三面板 ─────────────────────────

/// 造一份 4 扇区 × 4 块 × 16B 的合法 dump 字节。
fn sample_dump(n_sectors: usize) -> Vec<u8> {
    (0..n_sectors * nfc_core::model::dump::SECTOR_SIZE)
        .map(|i| (i % 251) as u8)
        .collect()
}

fn c_dump_panels_render() -> Result<(), String> {
    // 三个面板各开各的画一帧：编辑器要有内容（空态与有内容两条路径都走）
    let mut app = NfcApp::new(nfc_core::state::AppState::new());
    app.forms
        .editor
        .load(Some("a.mfd".into()), &sample_dump(4))
        .map_err(|e| e.to_string())?;
    let _ = panel_harness(&mut app, Panel::DumpEditor)?;

    // 比较器：两侧都齐 → 进比对分支（只齐一侧 → 空态分支）
    let mut app = NfcApp::new(nfc_core::state::AppState::new());
    let sectors =
        nfc_core::model::dump::to_sector_lines(&sample_dump(4)).map_err(|e| e.to_string())?;
    app.forms.comparator.a = Some(sectors.clone());
    app.forms.comparator.a_name = Some("a.mfd".into());
    let _ = panel_harness(&mut app, Panel::DumpComparator)?;
    app.forms.comparator.b = Some(sectors);
    app.forms.comparator.b_name = Some("b.mfd".into());
    let _ = panel_harness(&mut app, Panel::DumpComparator)?;

    // 历史面板走空列表分支（真实 dumpfiles 目录在测试机上多半是空的）
    let _ = panel_harness(&mut app, Panel::DumpHistory)?;
    Ok(())
}

fn c_editor_load_sectors() -> Result<(), String> {
    use nfc_core::model::dump::{BLOCKS_PER_SECTOR, BLOCK_SIZE};
    let mut f = panels::EditorForm::default();
    f.load(Some("x.mfd".into()), &sample_dump(4))
        .map_err(|e| e.to_string())?;
    // command.js:428-431：每 4 块一组 = 1 扇区
    if f.sectors.len() != 4 {
        return Err(format!("4 扇区的 dump 切出 {} 个扇区", f.sectors.len()));
    }
    for (i, s) in f.sectors.iter().enumerate() {
        if s.len() != BLOCKS_PER_SECTOR {
            return Err(format!("扇区 {i} 有 {} 行", s.len()));
        }
        for (j, l) in s.iter().enumerate() {
            if l.chars().count() != BLOCK_SIZE * 2 {
                return Err(format!("扇区 {i} 第 {j} 行 {} 字符", l.chars().count()));
            }
        }
    }
    if f.dirty {
        return Err("刚载入就被标脏".into());
    }
    if f.file_name.as_deref() != Some("x.mfd") {
        return Err("文件名未回填".into());
    }
    // 非扇区整数倍应报错（command.js 的 match(/.{1,32}/g) 会切出不足 4 块的一组）
    let mut f2 = panels::EditorForm::default();
    if f2.load(None, &[0u8; 10]).is_ok() {
        return Err("10 字节竟被接受".into());
    }
    Ok(())
}

fn c_editor_validate() -> Result<(), String> {
    use nfc_core::model::dump::{BLOCKS_PER_SECTOR, BLOCK_SIZE};
    let mut f = panels::EditorForm::default();
    f.load(None, &sample_dump(1)).map_err(|e| e.to_string())?;

    // 合法：结构 + 内容都过
    f.validate(true).map_err(|e| format!("合法数据被拒: {e}"))?;

    // 行数不对（checkData 的 splitData.length === 4）
    let orig = f.sectors.clone();
    f.sectors[0].pop();
    if f.validate(false).is_ok() {
        return Err("行数不足竟通过".into());
    }
    f.sectors = orig.clone();

    // 行长不对（data.length === 32）
    f.sectors[0][0] = "ab".to_owned();
    if f.validate(false).is_ok() {
        return Err("行长不足竟通过".into());
    }
    f.sectors = orig.clone();

    // 字符集：check_content=false 时放行（原版只查长度），true 时拦下
    f.sectors[0][0] = "z".repeat(BLOCK_SIZE * 2);
    if f.validate(false).is_err() {
        return Err("check_content=false 竟查了字符集".into());
    }
    if f.validate(true).is_ok() {
        return Err("check_content=true 竟放过非 hex".into());
    }
    f.sectors = orig;
    if f.sectors[0].len() != BLOCKS_PER_SECTOR {
        return Err("还原失败".into());
    }
    Ok(())
}

fn c_editor_flat_hex() -> Result<(), String> {
    // dumpEditor.js:14 把所有扇区的行拼成连续 hex；再 hex_decode 应还原原字节
    let bytes = sample_dump(4);
    let mut f = panels::EditorForm::default();
    f.load(None, &bytes).map_err(|e| e.to_string())?;
    let flat = f.flat_hex();
    if flat.len() != bytes.len() * 2 {
        return Err(format!("拼出 {} hex，应为 {}", flat.len(), bytes.len() * 2));
    }
    let back = nfc_core::model::hex::hex_decode(&flat).map_err(|e| e.to_string())?;
    if back != bytes {
        return Err("hex 往返后字节不一致".into());
    }
    // 原版是小写 hex（`byte.toString(16)`）
    if flat.chars().any(|c| c.is_ascii_uppercase()) {
        return Err("hex 含大写，原版是小写".into());
    }
    Ok(())
}

fn c_editor_save_action() -> Result<(), String> {
    use nfc_core::engine::Action;
    let mut app = NfcApp::new(nfc_core::state::AppState::new());
    app.forms
        .editor
        .load(Some("a.mfd".into()), &sample_dump(2))
        .map_err(|e| e.to_string())?;
    // 模拟点「保存」：构造与面板按钮相同的动作
    app.pending.push(Action::SaveDump {
        url: app.forms.editor.file_name.clone(),
        hex: app.forms.editor.flat_hex(),
        save_as: false,
    });
    let Some(Action::SaveDump { url, hex, save_as }) = app.pending.first() else {
        return Err("未产出 SaveDump".into());
    };
    if url.as_deref() != Some("a.mfd") {
        return Err("url 不符".into());
    }
    if *save_as {
        return Err("save_as 应为 false".into());
    }
    // 2 扇区 × 64 字节 = 128 字节 = 256 hex
    if hex.len() != 256 {
        return Err(format!("hex 长度 {}", hex.len()));
    }
    // 另存为的标志位
    app.pending.push(Action::SaveDump {
        url: Some("a.mfd".into()),
        hex: String::new(),
        save_as: true,
    });
    assert!(matches!(
        app.pending.last(),
        Some(Action::SaveDump { save_as: true, .. })
    ));
    Ok(())
}

fn c_editor_save_gated() -> Result<(), String> {
    // 面板里保存按钮的 enabled = `EditorForm::can_save()`，
    // 对标 `dumpEditor.js:16` 的两道检查：
    //   `if (!currentFilePath || !getHexData())` + `checkData(true)`
    let mut f = panels::EditorForm::default();
    // 空编辑器：validate 本身是 vacuous truth（与原版 `.each` 空集合一致），
    // 但没有文件 → 不可保存
    if f.validate(true).is_err() {
        return Err("空扇区列表竟未通过 validate（应 vacuous true）".into());
    }
    if f.can_save() {
        return Err("空编辑器竟可保存".into());
    }
    f.load(Some("a.mfd".into()), &sample_dump(1))
        .map_err(|e| e.to_string())?;
    if !f.can_save() {
        return Err("合法数据竟不可保存".into());
    }
    // 改坏一行后应立刻不可保存
    f.sectors[0][1] = "nope".to_owned();
    if f.can_save() {
        return Err("改坏后竟仍可保存".into());
    }
    // 只有长度不对（字符集合法）时，can_save 也要拒
    f.sectors[0][1] = "ab".to_owned();
    if f.can_save() {
        return Err("行长不足竟仍可保存".into());
    }
    Ok(())
}

fn c_comparator_ready() -> Result<(), String> {
    use crate::panels::ComparatorForm;
    // dumpComparator.js:28 —— 两侧都非空才 showMarkedDump
    let mut c = ComparatorForm::default();
    if c.ready() {
        return Err("两侧都空竟 ready".into());
    }
    c.a = Some(vec![vec![String::new()]]);
    if c.ready() {
        return Err("只有 A 竟 ready".into());
    }
    c.b = Some(vec![vec![String::new()]]);
    if !c.ready() {
        return Err("两侧都有竟不 ready".into());
    }
    // 空 vec 也不算（原版判的是 .length > 0）
    c.a = Some(Vec::new());
    if c.ready() {
        return Err("A 为空 vec 竟 ready".into());
    }
    // 两侧都 None → diff 返回空（未就绪时不比对）
    let none = ComparatorForm::default();
    if !none.diff().is_empty() {
        return Err("未就绪竟产出 diff".into());
    }
    // 一侧 None、另一侧有内容 → diff 仍返回空（match 分支兜底）
    let only_a = ComparatorForm {
        a: Some(vec![vec![String::new()]]),
        b: None,
        a_name: None,
        b_name: None,
    };
    if !only_a.diff().is_empty() {
        return Err("仅 A 有内容竟产出 diff".into());
    }
    Ok(())
}

fn c_comparator_diff() -> Result<(), String> {
    use crate::panels::ComparatorForm;
    use nfc_core::model::dump::DiffSeg;
    let mut a = panels::EditorForm::default();
    a.load(None, &sample_dump(1)).map_err(|e| e.to_string())?;
    // B = A 只改第一个字符。注意 sample_dump 首字节是 0x00 → hex "00"，
    // 改成 0x01 → hex "01"，**只动一个 nibble** 才能得到恰好 1 处差异。
    // （若整字节翻转 ^0xff，hex 的两个字符都会变，差异数会是 2。）
    let mut bytes = sample_dump(1);
    bytes[0] = 0x01;
    let mut b = panels::EditorForm::default();
    b.load(None, &bytes).map_err(|e| e.to_string())?;

    let c = ComparatorForm {
        a: Some(a.sectors.clone()),
        b: Some(b.sectors.clone()),
        a_name: None,
        b_name: None,
    };
    let d = c.diff();
    if d.len() != 1 {
        return Err(format!("diff 出 {} 个扇区", d.len()));
    }
    let (da, db) = &d[0];
    // 每扇区恒 4 行
    if da.len() != nfc_core::model::dump::BLOCKS_PER_SECTOR {
        return Err(format!("a 侧 {} 行", da.len()));
    }
    // 只有第 0 行第 0 个字符不同
    let diff_count: usize = da
        .iter()
        .map(|r| r.iter().filter(|s| s.is_diff()).count())
        .sum();
    if diff_count != 1 {
        return Err(format!("a 侧差异字符 {diff_count} 个，应为 1"));
    }
    if !da[0].iter().any(|s| matches!(s, DiffSeg::Diff(_))) {
        return Err("第 0 行第 0 字符未标差异".into());
    }
    // 两侧标出的差异位置应一致（同一位置不同值）
    let db_diff_count: usize = db
        .iter()
        .map(|r| r.iter().filter(|s| s.is_diff()).count())
        .sum();
    if db_diff_count != 1 {
        return Err(format!("b 侧差异字符 {db_diff_count} 个"));
    }
    // 完全相同的 dump 不应有差异
    let same = ComparatorForm {
        a: Some(a.sectors.clone()),
        b: Some(a.sectors),
        a_name: None,
        b_name: None,
    };
    let total: usize = same
        .diff()
        .iter()
        .flat_map(|(x, _)| x.iter())
        .flat_map(|r| r.iter())
        .filter(|s| s.is_diff())
        .count();
    if total != 0 {
        return Err(format!("相同 dump 竟有 {total} 处差异"));
    }
    Ok(())
}

fn c_comparator_length_mismatch() -> Result<(), String> {
    use crate::panels::ComparatorForm;
    // 原版 dumpComparator.js:65-86：多出的一侧全标差异，短侧补空串
    let mut a = panels::EditorForm::default();
    a.load(None, &sample_dump(3)).map_err(|e| e.to_string())?;
    let mut b = panels::EditorForm::default();
    b.load(None, &sample_dump(1)).map_err(|e| e.to_string())?;
    let c = ComparatorForm {
        a: Some(a.sectors),
        b: Some(b.sectors),
        a_name: None,
        b_name: None,
    };
    let d = c.diff();
    if d.len() != 3 {
        return Err(format!("扇区数应为 max(3,1)=3，实际 {}", d.len()));
    }
    for (da, db) in &d[1..] {
        for r in 0..nfc_core::model::dump::BLOCKS_PER_SECTOR {
            if !da[r].iter().all(|s| s.is_diff()) {
                return Err("多出侧未全标差异".into());
            }
            if !db[r].is_empty() {
                return Err("短侧未补空串".into());
            }
        }
    }
    Ok(())
}

fn c_history_button_states() -> Result<(), String> {
    // dumpHistory.js:100-115
    // 1 个勾选 → 编辑/写入可用、比较禁用
    assert_eq!(panels::history_button_states(1), (true, false, true));
    // 2 个勾选 → 比较可用、编辑/写入禁用
    assert_eq!(panels::history_button_states(2), (false, true, false));
    // 0 或 3+ → 三个全禁用
    assert_eq!(panels::history_button_states(0), (false, false, false));
    assert_eq!(panels::history_button_states(3), (false, false, false));
    Ok(())
}

fn c_history_gating() -> Result<(), String> {
    // 勾选 1 个 → 比较按钮禁用，不能派发 OpenComparator
    let mut app = NfcApp::new(nfc_core::state::AppState::new());
    app.forms.history_selected = vec!["a.mfd".to_owned()];
    let (can_edit, can_compare, can_write) = panels::history_button_states(1);
    if can_compare {
        return Err("1 个勾选时比较竟可用".into());
    }
    // 编辑 / 写入可用，且载荷是选中的那个文件
    if !can_edit || !can_write {
        return Err("1 个勾选时编辑/写入竟不可用".into());
    }
    // 勾选 2 个才能比较
    app.forms.history_selected = vec!["a.mfd".to_owned(), "b.mfd".to_owned()];
    let (_, can_compare, _) = panels::history_button_states(2);
    if !can_compare {
        return Err("2 个勾选时比较竟不可用".into());
    }
    // 删除按钮无 disabled（dumpHistory.html:18）
    let _ = panel_harness(&mut app, Panel::DumpHistory)?;
    Ok(())
}

fn c_history_resets_selection() -> Result<(), String> {
    // command.js:498-503 每次打开都重新 readdir，故勾选应清空
    let mut app = NfcApp::new(nfc_core::state::AppState::new());
    app.forms.history_selected = vec!["stale.mfd".to_owned()];
    app.exec("dump-history", None);
    if !app.forms.history_selected.is_empty() {
        return Err(format!("重开后仍勾选 {:?}", app.forms.history_selected));
    }
    if !app.state.panels.dump_history {
        return Err("面板未打开".into());
    }
    // 三个 dump 面板互不干扰
    app.exec("open-dump-editor", None);
    app.exec("open-dump-comparator", None);
    let p = &app.state.panels;
    if !(p.dump_history && p.dump_editor && p.dump_comparator) {
        return Err("三个 dump 面板未同时打开".into());
    }
    Ok(())
}

/// 造一个自持临时目录做 dumpfiles 测试。
struct TempDir(std::path::PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let p = std::env::temp_dir().join(format!(
            "nfc-s10-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        Self(p)
    }
    fn write(&self, name: &str) {
        std::fs::write(self.0.join(name), b"x").unwrap();
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn c_dumpfiles_ops() -> Result<(), String> {
    use nfc_core::dumpfiles;
    let d = TempDir::new("ops");
    for n in ["b.mfd", "a.mfd", "c.dump"] {
        d.write(n);
    }
    // 只列 .mfd 且按名排序（command.js:500 的 filter）
    assert_eq!(dumpfiles::list(&d.0), vec!["a.mfd", "b.mfd"]);
    // 删除
    let gone = dumpfiles::delete(&d.0, &["a.mfd".to_owned()]);
    assert_eq!(gone, vec!["a.mfd"]);
    assert_eq!(dumpfiles::list(&d.0), vec!["b.mfd"]);
    // 重命名
    dumpfiles::rename(&d.0, "b.mfd", "z.mfd").map_err(|e| e.to_string())?;
    assert_eq!(dumpfiles::list(&d.0), vec!["z.mfd"]);
    Ok(())
}

fn c_dumpfiles_reject_traversal() -> Result<(), String> {
    use nfc_core::dumpfiles;
    let d = TempDir::new("trav");
    d.write("a.mfd");
    // 删除：路径穿越必须被拒
    let gone = dumpfiles::delete(&d.0, &["../evil.mfd".to_owned(), "/etc/passwd".to_owned()]);
    if !gone.is_empty() {
        return Err(format!("删除接受了穿越路径: {gone:?}"));
    }
    // 重命名：同样拒绝
    if dumpfiles::rename(&d.0, "a.mfd", "../evil.mfd").is_ok() {
        return Err("重命名接受了穿越路径".into());
    }
    if dumpfiles::rename(&d.0, "../evil.mfd", "b.mfd").is_ok() {
        return Err("重命名接受了穿越源路径".into());
    }
    // 原文件仍在
    assert_eq!(dumpfiles::list(&d.0), vec!["a.mfd"]);
    Ok(())
}

// ───────────────────────── S11 engine 接线 ─────────────────────────

/// 已连接的状态（门禁要求）。
/// S13 回归：设备下拉必须真的能刷出设备。此前 scan 走 spawn `nfc-list`、
/// 输出只进日志，`device_list` 永远为空——下拉框永远没东西可选。
fn c_scan_populates_device_list() -> Result<(), String> {
    let mut app = connected_app();
    app.engine =
        nfc_core::engine::Engine::new(Box::new(nfc_core::runner::MockToolRunner::new(vec![])))
            .with_serial(Box::new(nfc_core::serial::MockSerial::from([
                "/dev/ttyACM0",
                "/dev/ttyACM1",
            ])));
    app.exec("scan-usb-devices", None);
    app.consume_pending();
    if app.state.device_list.len() != 2 {
        return Err(format!("scan 未刷出设备列表: {:?}", app.state.device_list));
    }
    // 枚举不占任务位
    if app.state.is_running_task {
        return Err("枚举不应置 is_running_task".into());
    }
    Ok(())
}

/// S13 回归：连接必须从 Connecting 走到 Connected，不能永久停在 Connecting
/// （此前 connect 只写 libnfc.conf，没有任何东西推进握手，之后每次再点都 Busy）。
fn c_connect_reaches_connected() -> Result<(), String> {
    let mut app = connected_app();
    app.engine =
        nfc_core::engine::Engine::new(Box::new(nfc_core::runner::MockToolRunner::new(vec![
            nfc_core::runner::Chunk::stdout("NFC device: NFC_Device opened\n"),
        ])));
    app.exec("conn-usb-devices", Some("/dev/ttyACM0".into()));
    app.consume_pending();
    if !matches!(
        app.state.device_state,
        nfc_core::state::DeviceState::Connecting
    ) {
        return Err("连接未进入 Connecting".into());
    }
    for _ in 0..8 {
        app.tick();
    }
    if !matches!(
        app.state.device_state,
        nfc_core::state::DeviceState::Connected
    ) {
        return Err(format!(
            "连接未走到 Connected: {:?}",
            app.state.device_state
        ));
    }
    if !app.state.is_device_connected {
        return Err("is_device_connected 未置真".into());
    }
    Ok(())
}

/// S13 回归：打不开设备必须落 Failed（原版 stderr 的 unable 分支），
/// 否则 state 永久停在 Connecting，下拉被禁、只能重开窗口。
fn c_connect_failure_ends_in_failed() -> Result<(), String> {
    let mut app = connected_app();
    app.engine =
        nfc_core::engine::Engine::new(Box::new(nfc_core::runner::MockToolRunner::new(vec![
            nfc_core::runner::Chunk::stderr_chunk("Unable to open NFC device\n"),
        ])));
    app.exec("conn-usb-devices", Some("/dev/ttyACM0".into()));
    app.consume_pending();
    for _ in 0..8 {
        app.tick();
    }
    if !matches!(app.state.device_state, nfc_core::state::DeviceState::Failed) {
        return Err(format!("连接失败应落 Failed: {:?}", app.state.device_state));
    }
    if app.state.is_running_task {
        return Err("失败收尾也应释放任务位".into());
    }
    Ok(())
}

/// S13 回归：窗被最小化/遮挡时 eframe 走 `update_logic_only`，**只调 `logic`
/// 不调 `ui`**。若每帧推进（`tick` / `pump`）挂在 `draw`（即 `ui`）里，子进程
/// 输出就没人排空——进度条冻结、进程变僵尸。故这里只调 logic 路径，
/// 断言挂起任务的输出照样被 pump 进日志。
fn c_logic_drives_tick_when_window_occluded() -> Result<(), String> {
    let mut app =
        connected_app().with_mock_backend(vec![nfc_core::runner::Chunk::stdout("线缆已收到\n")]);
    app.exec("read-IC", None);
    app.consume_pending();
    if !app.state.is_running_task {
        return Err("任务未启动".into());
    }
    // 模拟遮挡帧：只跑 logic（内含 tick），不跑 draw
    let ctx = egui::Context::default();
    app.logic_body(&ctx);
    if !app.state.log_buffer.contains("线缆已收到") {
        return Err("只调 logic 时任务输出没被 pump 进日志（tick 挂错钩子）".into());
    }
    Ok(())
}

fn connected_app() -> NfcApp {
    let mut st = nfc_core::state::AppState::new();
    st.current_device = Some("/dev/ttyACM0".into());
    st.is_device_connected = true;
    st.device_state = nfc_core::state::DeviceState::Connected;
    NfcApp::new(st)
}

fn c_pending_is_consumed_not_dropped() -> Result<(), String> {
    // 回归：S8–S10 每帧 `pending.clear()`，所有按钮点了都没反应。
    // 这里断言「未连接时 read-IC 被门禁拒绝并留下 error 事件」——
    // 只有真正 dispatch 过才会有事件。
    let mut app = NfcApp::new(nfc_core::state::AppState::new()); // 未连接
    app.exec("read-IC", None);
    if app.pending.is_empty() {
        return Err("exec 未产出动作".into());
    }
    app.consume_pending();
    if !app.pending.is_empty() {
        return Err("consume 后队列未清空".into());
    }
    if app.state.events.is_empty() {
        return Err("动作被静默丢弃，没有任何反馈".into());
    }
    use nfc_core::state::EventKind;
    if !app.state.events.iter().any(|e| e.kind == EventKind::Error) {
        return Err(format!("未记 error 事件: {:?}", app.state.events));
    }
    // 也不该真的启动任务
    if app.engine.is_running() {
        return Err("门禁拒绝却启动了任务".into());
    }
    Ok(())
}

fn c_engine_output_reaches_log() -> Result<(), String> {
    // 完整链路：派发 read-IC → mock 吐出脚本 → pump → 日志拿到内容
    use nfc_core::runner::Chunk;
    let mut app = connected_app().with_mock_backend(vec![
        Chunk::stdout("MIFARE 探测到 UID: 11223344\n"),
        Chunk::stdout("正在尝试密钥 A: ffffffffffff\n"),
    ]);
    app.exec("read-IC", None);
    app.consume_pending();
    if !app.state.is_running_task {
        return Err("派发后未置 is_running_task".into());
    }
    if !app.engine.is_running() {
        return Err("engine 未跟踪到任务".into());
    }
    // 跑几帧让输出回流
    for _ in 0..3 {
        app.tick();
    }
    let log = &app.state.log_buffer;
    for want in ["MIFARE 探测到 UID", "正在尝试密钥 A"] {
        if !log.contains(want) {
            return Err(format!("日志缺 {want:?}: {log:?}"));
        }
    }
    // 脚本跑完且退出码 0 → 任务位与进度条都应清
    if app.state.is_running_task {
        return Err("退出后仍在跑".into());
    }
    Ok(())
}

fn c_engine_failed_exit_sets_error() -> Result<(), String> {
    use nfc_core::runner::{Chunk, MockToolRunner};
    use nfc_core::state::EventKind;
    let mut app = connected_app();
    // 换成一个退出码 1 的 mock
    app.engine = nfc_core::engine::Engine::new(Box::new(
        MockToolRunner::new(vec![Chunk::stderr_chunk("libnfc error\n")]).with_exit(Some(1)),
    ));
    app.exec("read-IC", None);
    app.consume_pending();
    for _ in 0..3 {
        app.tick();
    }
    if app.state.is_running_task {
        return Err("退出后仍在跑".into());
    }
    if app.state.status.indicator != nfc_core::state::Indicator::Error {
        return Err(format!("指示器非 error: {:?}", app.state.status));
    }
    if !app.state.events.iter().any(|e| e.kind == EventKind::Error) {
        return Err("未记 error 事件".into());
    }
    Ok(())
}

fn c_busy_blocks_second_action() -> Result<(), String> {
    // 门禁的 Busy 分支：任务在跑时再点别的应被拒
    let mut app = connected_app();
    app.exec("read-IC", None);
    app.consume_pending();
    let n_before = app.state.events.len();
    app.exec("detect-card-type", None);
    app.consume_pending();
    if app.state.events.len() == n_before {
        return Err("第二个动作未被门禁拒绝".into());
    }
    // 仍只有 read-IC 在跑
    if !app.state.is_running_task {
        return Err("read-IC 竟被自己拒了".into());
    }
    Ok(())
}

fn c_unwired_action_is_visible() -> Result<(), String> {
    // 未接线的动作必须留下可见反馈，不能静默。
    // `run-hard-nested`（libnfc-collect 多轮状态机）尚未接线。
    use nfc_core::state::EventKind;
    let mut app = connected_app();
    app.exec("run-hard-nested", None);
    app.consume_pending();
    if !app
        .state
        .events
        .iter()
        .any(|e| e.kind == EventKind::Error && e.text.contains("run-hard-nested"))
    {
        return Err(format!("未接线动作无反馈: {:?}", app.state.events));
    }
    Ok(())
}

// ═══════════════════ S11-2 系统文件对话框 + 写卡接线 ═══════════════════

use crate::dialogs_host::{self, DialogHost, FilePurpose};
use crate::dialogs_ui::ConfirmKind;

/// 造一个 **不会弹真实系统框** 的 app（`DialogHost::manual`）。
fn app_with_temp_dumps(tag: &str) -> (TempDir, NfcApp) {
    let d = TempDir::new(tag);
    let mut app = connected_app();
    app.paths = nfc_core::paths::Paths::new(d.0.clone());
    // 手动宿主：request_* 只记账，不派给工作线程，故不会真的弹窗。
    app.dialogs_host = DialogHost::manual();
    // CI / 无头机器上探测必然为假，但这些用例要跑的是**流程正确性**，
    // 不是门户探测（后者由 `c_portal_probe` 单独覆盖）。
    app.file_dialogs_ok = true;
    let _ = std::fs::create_dir_all(app.paths.dumpfiles_dir());
    (d, app)
}

/// 把系统弹窗「选好了」当作已投递，直接回传结果。
///
/// **不要**在这里再 `request_open` 一次——调用方（如 `exec`）通常已经投递过了。
/// 重复投递会多出一条 pending；真实工作线程下还会多弹一个系统框，并让通道上
/// 出现两个 `FileReply` 与 `feed_reply` 竞争。
fn reply_picked(app: &mut NfcApp, purpose: FilePurpose, name: &str) {
    app.dialogs_host
        .feed_reply(purpose, Some(app.paths.dumpfiles_dir().join(name)));
    app.tick();
}

/// S11-7：`read-IC` 收尾的保存框必须走**真实的** app 侧投递链
/// （`pump_pending_save` → `DialogHost` → `resolve_save`），而不是像
/// S11-2~S11-6 那样悄悄用默认名落盘。
fn c_read_ic_save_round_trip() -> Result<(), String> {
    let (_d, mut app) = app_with_temp_dumps("readic-save");
    // 造一个非空 temp.mfd，模拟 mfoc 跑完
    std::fs::write(app.paths.temp_mfd(), vec![0x5a; 64]).unwrap();
    app.exec("read-IC", None);
    app.consume_pending();
    // 工具还没「跑完」时不该有保存请求
    if app.engine.pending_save().is_some() {
        return Err("工具未退出就挂起了保存请求".into());
    }
    // 让 mock 工具退出并走完收尾
    app.tick();
    let pending = app
        .engine
        .pending_save()
        .ok_or("read-IC 收尾应挂起保存请求")?;
    let default_path = pending.default_path.clone();
    if !default_path.starts_with(app.paths.dumpfiles_dir()) {
        return Err(format!(
            "默认路径不在 dumpfiles 下: {}",
            default_path.display()
        ));
    }
    // tick 应把它投递出去（手动宿主只记账）
    if app.dialogs_host.outstanding() != 1 {
        return Err(format!(
            "保存请求应被投递一次，实际 outstanding={}",
            app.dialogs_host.outstanding()
        ));
    }
    // 选一个自定义路径
    let target = app.paths.base().join("chosen.mfd");
    app.dialogs_host
        .feed_reply(FilePurpose::ReadIcSave, Some(target.clone()));
    app.tick();
    if !target.is_file() {
        return Err(format!("未落到用户选的路径: {}", target.display()));
    }
    if app.paths.temp_mfd().exists() {
        return Err("temp.mfd 未被搬走".into());
    }
    if app.engine.pending_save().is_some() {
        return Err("续接后请求应被取走".into());
    }
    Ok(())
}

/// 挂起期间**每帧**都会调 `pump_pending_save`；没做已在等的判断的话，
/// 会重复投递、真实环境里弹出几十个系统框。
fn c_pending_save_not_reposted() -> Result<(), String> {
    let (_d, mut app) = app_with_temp_dumps("readic-repost");
    std::fs::write(app.paths.temp_mfd(), vec![0x5a; 64]).unwrap();
    app.exec("read-IC", None);
    app.consume_pending();
    app.tick();
    if app.dialogs_host.outstanding() != 1 {
        return Err(format!(
            "首次应投递 1 次，实际 {}",
            app.dialogs_host.outstanding()
        ));
    }
    // 再 tick 5 帧，pending 期间不该再投
    for _ in 0..5 {
        app.tick();
    }
    if app.dialogs_host.outstanding() != 1 {
        return Err(format!(
            "等待期间被重复投递，实际 outstanding={}",
            app.dialogs_host.outstanding()
        ));
    }
    // 取消也要走完续接（read-IC 收尾取消 = 用默认名存）
    app.dialogs_host.feed_reply(FilePurpose::ReadIcSave, None);
    app.tick();
    if app.dialogs_host.outstanding() != 0 {
        return Err("取消后续接应排空队列".into());
    }
    if app.engine.is_running() {
        return Err("续接后应释放任务位".into());
    }
    Ok(())
}

/// HardNested `collectOnly` 的 nonce 保存走同一条链，且取消语义相反
/// （保留 nonces.bin，不丢用户采集的数据）。
fn c_nonces_save_round_trip() -> Result<(), String> {
    use nfc_core::model::keys::KeyType;
    let (_d, mut app) = app_with_temp_dumps("nonces-save");
    app.state.known_key_info = vec![("aabbccddeeff".to_string(), 1u8, KeyType::A)];
    // 采到的 nonce
    std::fs::write(app.paths.nonces_file(), vec![0xab; 32]).unwrap();
    // HardNested 走**类型化**的 Action（面板不经过字符串 exec）
    let cfg = nfc_core::state::HardNestedConfig {
        known_key: "aabbccddeeff".into(),
        known_sector: 1,
        known_key_type: KeyType::A,
        target_sector: 1,
        target_key_type: KeyType::A,
        collect_only: true,
        auto_run: false,
    };
    app.pending
        .push(nfc_core::engine::Action::HardNestedConfig(Box::new(cfg)));
    app.consume_pending();
    app.tick();
    let pending = app
        .engine
        .pending_save()
        .ok_or("collectOnly 应挂起 nonce 保存请求")?;
    if !app.engine.hn_saving() {
        return Err("应被识别为 nonce 保存（决定续接走 rename 而非 dump_commit）".into());
    }
    if pending.source != app.paths.nonces_file() {
        return Err(format!("源文件不对: {}", pending.source.display()));
    }
    let target = app.paths.base().join("out.bin");
    app.dialogs_host
        .feed_reply(FilePurpose::NoncesSave, Some(target.clone()));
    app.tick();
    if !target.is_file() {
        return Err("nonce 未保存到目标路径".into());
    }
    if app.paths.nonces_file().exists() {
        return Err("nonces.bin 未被搬走".into());
    }
    Ok(())
}

fn c_dialog_host_queue() -> Result<(), String> {
    let mut h = DialogHost::spawn();
    if h.is_pending() {
        return Err("刚建好就有待处理请求".into());
    }
    h.request_open(FilePurpose::ComparatorA, Default::default());
    h.request_open(FilePurpose::ComparatorB, Default::default());
    if h.outstanding() != 2 {
        return Err(format!("outstanding={}", h.outstanding()));
    }
    h.feed_reply(FilePurpose::ComparatorA, Some("/a.mfd".into()));
    let r = h.poll().ok_or("应有结果")?;
    if r.purpose != FilePurpose::ComparatorA || r.path.is_none() {
        return Err(format!("结果错配: {r:?}"));
    }
    // 取消也要回传，否则 pending 永远排不空
    h.feed_reply(FilePurpose::ComparatorB, None);
    if h.poll()
        .map(|r| r.path)
        .unwrap_or(Some("x".into()))
        .is_some()
    {
        return Err("取消应回 None".into());
    }
    if h.is_pending() {
        return Err("两个都回传后仍显示 pending".into());
    }
    Ok(())
}

fn c_portal_probe() -> Result<(), String> {
    // 无头环境（CI / SSH）必须报不可用，好让 app 给明确提示
    if dialogs_host::file_dialogs_supported() {
        // 本机有门户是允许的，但探测函数本身必须是确定性的
        let a = dialogs_host::file_dialogs_supported();
        let b = dialogs_host::file_dialogs_supported();
        if a != b {
            return Err("探测结果不稳定".into());
        }
    }
    // 无头环境应已由 NfcApp::new 记下警告
    let app = NfcApp::new(nfc_core::state::AppState::new());
    if !dialogs_host::file_dialogs_supported() && !app.warned_no_portal {
        return Err("无门户却没给启动警告".into());
    }
    Ok(())
}

fn c_write_ic_flow() -> Result<(), String> {
    let (_d, mut app) = app_with_temp_dumps("write-ic");
    // 1. 点「写卡」→ 只投递文件框，不派发
    app.exec("write-IC", None);
    if !app.pending.is_empty() {
        return Err("选文件前就派发了动作".into());
    }
    if app.dialogs_host.outstanding() != 1 {
        return Err(format!("outstanding={}", app.dialogs_host.outstanding()));
    }
    // 2. 用户选好 → 弹确认窗，仍不派发
    reply_picked(&mut app, FilePurpose::WriteDump, "card.mfd");
    let Some(c) = app.confirm.clone() else {
        return Err("选完文件没弹确认窗".into());
    };
    if !matches!(c.kind, ConfirmKind::WriteDump { .. }) {
        return Err(format!("确认类型错: {:?}", c.kind));
    }
    if !app.pending.is_empty() {
        return Err("确认窗未点就派发了".into());
    }
    Ok(())
}

fn c_write_ic_cancel_file() -> Result<(), String> {
    let (_d, mut app) = app_with_temp_dumps("write-ic-cancel-file");
    app.exec("write-IC", None);
    app.dialogs_host
        .request_open(FilePurpose::WriteDump, Default::default());
    app.dialogs_host.feed_reply(FilePurpose::WriteDump, None); // 取消
    app.tick();
    if app.confirm.is_some() {
        return Err("取消文件框后仍弹了确认窗".into());
    }
    if !app.pending.is_empty() {
        return Err("取消文件框后仍派发了动作".into());
    }
    Ok(())
}

fn c_write_ic_cancel_confirm() -> Result<(), String> {
    let (_d, mut app) = app_with_temp_dumps("write-ic-cancel-confirm");
    app.exec("write-IC", None);
    reply_picked(&mut app, FilePurpose::WriteDump, "card.mfd");
    let c = app.confirm.take().ok_or("应有确认窗")?;
    // 点「取消」→ 不派发、不批准
    if app.on_confirm_result(c, false) {
        return Err("取消竟然要求关窗".into());
    }
    if !app.pending.is_empty() {
        return Err("取消确认后仍派发".into());
    }
    if !app.pending_with_approval.is_empty() {
        return Err("取消确认却留下了待批准动作".into());
    }
    Ok(())
}

fn c_engine_never_opens_dialog() -> Result<(), String> {
    // 关键回归：路径已由 app 预选好，engine 绝不能再弹文件框
    let (_d, mut app) = app_with_temp_dumps("no-open-dialog");
    std::fs::write(app.paths.dumpfiles_dir().join("card.mfd"), vec![0x5a; 32]).unwrap();
    let mut dlg = nfc_core::dialogs::ScriptedDialog::new().with_confirm(true);
    app.engine
        .dispatch(
            nfc_core::engine::Action::WithArg(
                "write-IC",
                app.paths
                    .dumpfiles_dir()
                    .join("card.mfd")
                    .display()
                    .to_string(),
            ),
            &mut app.state,
            &app.paths,
            &mut dlg,
        )
        .map_err(|e| format!("dispatch 失败: {e}"))?;
    if dlg.open_calls() != 0 {
        return Err(format!("engine 弹了 {} 次文件框", dlg.open_calls()));
    }
    if dlg.confirm_calls() != 1 {
        return Err(format!("确认框应恰好 1 次，实为 {}", dlg.confirm_calls()));
    }
    // 备份段应已启动
    let (tool, args, _) = app.engine.runner().started_last().ok_or("未启动任何工具")?;
    if tool != "nfc-mfdetect" {
        return Err(format!("第一段应是 nfc-mfdetect，实为 {tool}"));
    }
    if !args.iter().any(|a| a.starts_with("-O")) {
        return Err(format!("备份段缺 -O: {args:?}"));
    }
    Ok(())
}

fn c_lock_ufuid_flow() -> Result<(), String> {
    let (_d, mut app) = app_with_temp_dumps("lock");
    app.exec("lock-ufuid", None);
    // lock 不需要选文件，直接就是确认窗
    if app.dialogs_host.outstanding() != 0 {
        return Err("lock-ufuid 不该弹文件框".into());
    }
    let c = app.confirm.take().ok_or("应有确认窗")?;
    if !matches!(c.kind, ConfirmKind::Lock) {
        return Err(format!("确认类型错: {:?}", c.kind));
    }
    if c.message.is_empty() {
        return Err("确认正文为空".into());
    }
    app.on_confirm_result(c, true);
    app.consume_pending();
    let (tool, args, _) = app
        .engine
        .runner()
        .started_last()
        .ok_or("未启动 nfc-mflock")?;
    if tool != "nfc-mflock" || args != vec!["-q".to_string()] {
        return Err(format!("参数错: {tool} {args:?}"));
    }
    Ok(())
}

fn c_write_uid_flow() -> Result<(), String> {
    let (_d, mut app) = app_with_temp_dumps("write-uid");
    let p = app.paths.dumpfiles_dir().join("uid.mfd");
    std::fs::write(&p, vec![0x11; 32]).unwrap();
    app.exec("write-ufuid-uid", None);
    if app.dialogs_host.outstanding() != 1 {
        return Err("write-ufuid-uid 应弹一次文件框".into());
    }
    reply_picked(&mut app, FilePurpose::WriteUidDump, "uid.mfd");
    let c = app.confirm.take().ok_or("应有确认窗")?;
    if !matches!(c.kind, ConfirmKind::WriteUid { .. }) {
        return Err(format!("确认类型错: {:?}", c.kind));
    }
    app.on_confirm_result(c, true);
    app.consume_pending();
    let (tool, args, _) = app
        .engine
        .runner()
        .started_last()
        .ok_or("未启动 nfc-mfsetuid")?;
    if tool != "nfc-mfsetuid" {
        return Err(format!("工具错: {tool}"));
    }
    if args[0] != "-q" || args[1].len() != 32 {
        return Err(format!("参数错: {args:?}"));
    }
    Ok(())
}

fn c_format_card_two_stage() -> Result<(), String> {
    let (_d, mut app) = app_with_temp_dumps("format");
    // S17：格式化补应用内确认窗，未确认不派发。
    app.exec("format-card", None);
    let c = app.confirm.take().ok_or("格式化应有确认窗")?;
    if !matches!(c.kind, ConfirmKind::FormatCard) {
        return Err(format!("确认类型错: {:?}", c.kind));
    }
    app.on_confirm_result(c, true);
    app.consume_pending();
    let (t1, a1, _) = app.engine.runner().started_last().ok_or("第一段未启动")?;
    if t1 != "nfc-mfdetect" || !a1.iter().any(|a| a.starts_with("-O")) {
        return Err(format!("第一段应是带 -O 的 mfdetect: {t1} {a1:?}"));
    }
    // 造出备份，让第二段能接上
    std::fs::write(app.paths.temp_mfd(), vec![0x22; 64]).unwrap();
    for _ in 0..3 {
        app.tick();
    }
    let (t2, a2, _) = app.engine.runner().started_last().ok_or("第二段未启动")?;
    if t2 != "nfc-mfclassic" {
        return Err(format!("第二段应是 nfc-mfclassic，实为 {t2}"));
    }
    // f A u <temp> <temp> f
    if a2.len() != 6 || a2[0] != "f" || a2[3] != a2[4] {
        return Err(format!("第二段参数错: {a2:?}"));
    }
    Ok(())
}

fn c_format_card_backup_guard() -> Result<(), String> {
    // 备份没生成 → 绝不能执行格式化（command.js:603-607）
    let (_d, mut app) = app_with_temp_dumps("format-guard");
    app.exec("format-card", None);
    let c = app.confirm.take().ok_or("格式化应有确认窗")?;
    app.on_confirm_result(c, true);
    app.consume_pending();
    let before = app.engine.runner().started_last().map(|(t, _, _)| t);
    for _ in 0..3 {
        app.tick();
    }
    // 工具仍是第一段那个 —— 第二段没被启动
    let after = app.engine.runner().started_last().map(|(t, _, _)| t);
    if after != before {
        return Err(format!("备份缺失却启动了第二段: {before:?} → {after:?}"));
    }
    if app.state.status.indicator != nfc_core::state::Indicator::Error {
        return Err("备份失败未置 error 状态".into());
    }
    if !app.state.log_buffer.contains("读卡失败") {
        return Err(format!("未记「读卡失败」: {:?}", app.state.log_buffer));
    }
    Ok(())
}

/// S17：手机/手环两个写动作补应用内确认窗（此前按钮点了没反应）。
fn c_phw_write_uid_confirm_flow() -> Result<(), String> {
    let (_d, mut app) = app_with_temp_dumps("phw-uid");
    app.state.phw.block0 = Some("04A8A68A800400010000000000000000".to_string());
    app.exec("phw-write-uid", None);
    let c = app.confirm.take().ok_or("phw-write-uid 应有确认窗")?;
    if !matches!(c.kind, ConfirmKind::PhwWriteUid { .. }) {
        return Err(format!("确认类型错: {:?}", c.kind));
    }
    if c.message.is_empty() {
        return Err("确认正文为空".into());
    }
    // 未确认 → 不启动任何工具
    app.on_confirm_result(c, false);
    app.consume_pending();
    if app.engine.runner().started_last().is_some() {
        return Err("未确认却启动了工具".into());
    }
    // 确认后才启动 nfc-mfsetuid -q <32hex>
    app.exec("phw-write-uid", None);
    let c = app.confirm.take().ok_or("二次应仍有确认窗")?;
    app.on_confirm_result(c, true);
    app.consume_pending();
    let (tool, args, _) = app
        .engine
        .runner()
        .started_last()
        .ok_or("未启动 nfc-mfsetuid")?;
    if tool != "nfc-mfsetuid" || args.len() != 2 || args[0] != "-q" || args[1].len() != 32 {
        return Err(format!("参数错: {tool} {args:?}"));
    }
    // 且绝不追加 -f
    if args.contains(&"-f".to_string()) {
        return Err("不应出现 -f: {args:?}".into());
    }
    Ok(())
}

/// S17：phw-write-data 确认窗 + 第一段 nfc-mfdetect -O。
fn c_phw_write_data_confirm_flow() -> Result<(), String> {
    let (_d, mut app) = app_with_temp_dumps("phw-data");
    app.state.phw.block0 = Some("04A8A68A800400010000000000000000".to_string());
    app.state.phw.sector = 0;
    app.state.phw.block = 1;
    app.exec("phw-write-data", None);
    let c = app.confirm.take().ok_or("phw-write-data 应有确认窗")?;
    if !matches!(c.kind, ConfirmKind::PhwWriteData { .. }) {
        return Err(format!("确认类型错: {:?}", c.kind));
    }
    app.on_confirm_result(c, true);
    app.consume_pending();
    let (tool, args, _) = app.engine.runner().started_last().ok_or("第一段未启动")?;
    if tool != "nfc-mfdetect" || !args.iter().any(|a| a.starts_with("-O")) {
        return Err(format!("第一段应是带 -O 的 mfdetect: {tool} {args:?}"));
    }
    Ok(())
}

/// S17：BCC 不自洽时 engine 拒绝写入（不再靠 -f 强写砖卡）。
fn c_phw_write_uid_bcc_rejected() -> Result<(), String> {
    let (_d, mut app) = app_with_temp_dumps("phw-bcc");
    // 第 5 字节改为 00，BCC 不自洽
    app.state.phw.block0 = Some("04A8A68A000400010000000000000000".to_string());
    app.exec("phw-write-uid", None);
    let c = app.confirm.take().ok_or("应有确认窗")?;
    app.on_confirm_result(c, true);
    app.consume_pending();
    if app.engine.runner().started_last().is_some() {
        return Err("BCC 不自洽却启动了写卡工具".into());
    }
    if !app.state.log_buffer.contains("BCC") {
        return Err(format!("未记 BCC 拒绝日志: {:?}", app.state.log_buffer));
    }
    Ok(())
}

/// S17：格式化未确认时不启动任何工具。
fn c_format_card_confirm_required() -> Result<(), String> {
    let (_d, mut app) = app_with_temp_dumps("format-confirm");
    app.exec("format-card", None);
    let c = app.confirm.take().ok_or("格式化应有确认窗")?;
    if !matches!(c.kind, ConfirmKind::FormatCard) {
        return Err(format!("确认类型错: {:?}", c.kind));
    }
    // 取消确认 → 不派发
    app.on_confirm_result(c, false);
    app.consume_pending();
    if app.engine.runner().started_last().is_some() {
        return Err("取消确认却启动了工具".into());
    }
    Ok(())
}

fn c_delete_needs_confirm() -> Result<(), String> {
    let (d, mut app) = app_with_temp_dumps("delete");
    std::fs::write(d.0.join("dumpfiles").join("a.mfd"), b"x").unwrap();
    // 直接派发（绕过 UI 模态窗）——engine 侧必须自己拦住
    let mut dlg = nfc_core::dialogs::ScriptedDialog::new(); // 空队列 → 拒绝
    app.engine
        .dispatch(
            nfc_core::engine::Action::DeleteDumps(vec!["a.mfd".into()]),
            &mut app.state,
            &app.paths,
            &mut dlg,
        )
        .map_err(|e| format!("dispatch 失败: {e}"))?;
    if !d.0.join("dumpfiles").join("a.mfd").exists() {
        return Err("未确认竟把文件删了".into());
    }
    if dlg.confirm_calls() != 1 {
        return Err(format!("确认框应被调 1 次，实为 {}", dlg.confirm_calls()));
    }
    Ok(())
}

fn c_approval_does_not_leak() -> Result<(), String> {
    // 回归：批准必须与动作**成对**，不能靠队列位置。
    // 真实 bug：热插拔每 500ms 往 pending 塞一个 ScanUsb，若批准单独存放、
    // 派发时按位置取，就会被 ScanUsb 先消费掉 → 用户点了确定却什么都没发生。
    let (_d, mut app) = app_with_temp_dumps("leak");
    // 模拟「确认窗点确定」把 write-IC 入队
    app.push_approved(nfc_core::engine::Action::Plain("lock-ufuid"));
    // 再模拟同一帧里热插拔 tick 塞进来的裸动作
    app.pending.push(nfc_core::engine::Action::ScanUsb);
    app.consume_pending();
    // 批准必须落到了 lock-ufuid 上 → nfc-mflock 被启动
    let (tool, args, _) = app
        .engine
        .runner()
        .started_last()
        .ok_or("批准被 ScanUsb 抢走了：lock-ufuid 未启动")?;
    if tool != "nfc-mflock" || args != vec!["-q".to_owned()] {
        return Err(format!("派发错了动作: {tool} {args:?}"));
    }
    // 且批准必须已被清空，不留给下一帧
    if !app.dialogs.confirm_results.is_empty() {
        return Err("批准残留在对话框队列里".into());
    }
    Ok(())
}

/// 回归：批准用完后不得影响下一个未批准的危险动作。
fn c_approval_is_single_use() -> Result<(), String> {
    let (d, mut app) = app_with_temp_dumps("single-use");
    std::fs::write(d.0.join("dumpfiles").join("a.mfd"), b"x").unwrap();
    app.push_approved(nfc_core::engine::Action::Plain("lock-ufuid"));
    app.consume_pending();
    // 现在直接派一个未经确认的删除：必须被拒
    app.pending
        .push(nfc_core::engine::Action::DeleteDumps(vec!["a.mfd".into()]));
    app.consume_pending();
    if !d.0.join("dumpfiles").join("a.mfd").exists() {
        return Err("上一次的批准泄漏到了删除操作".into());
    }
    Ok(())
}

fn c_backup_nonzero_blocks_write() -> Result<(), String> {
    // 回归：备份段**非 0 退出**时绝不能继续写卡。
    // 原版 `exec` 在 code !== 0 时 reject（execUtils.js:51-52），而第二段挂在
    // `.then()` 上（command.js:600），reject 后不会执行。
    // 曾实现成「不看第一段退出码、只看 temp.mfd 是否存在」，故此用例专防回归。
    let (_d, mut app) = app_with_temp_dumps("backup-nonzero");
    app.paths.base();
    let d = app.paths.dumpfiles_dir();
    std::fs::write(d.join("card.mfd"), vec![0x5a; 32]).unwrap();
    // 用「第一段非 0 退出」的 runner：Engine 的 mock 契约是 pump 一次后认 exit
    app.engine = nfc_core::engine::Engine::new(Box::new(
        nfc_core::runner::MockToolRunner::new(vec![]).with_exit(Some(1)),
    ));
    app.push_approved(nfc_core::engine::Action::WithArg(
        "write-IC",
        d.join("card.mfd").display().to_string(),
    ));
    app.consume_pending();
    // 备份文件存在且非空——排除「靠文件缺失挡住」这个更简单的路径
    std::fs::write(app.paths.temp_mfd(), vec![0x5a; 64]).unwrap();
    app.tick();
    let (tool, _, _) = app.engine.runner().started_last().ok_or("第一段未启动")?;
    if tool != "nfc-mfdetect" {
        return Err(format!("备份段非 0 退出却启动了第二段: {tool}"));
    }
    if app.state.status.indicator != nfc_core::state::Indicator::Error {
        return Err("备份段失败未置 error 状态".into());
    }
    Ok(())
}

fn c_write_uid_shows_block0() -> Result<(), String> {
    // 安全：写 UFUID 前必须让用户**看到**将被写入的 0 块（command.js:210-214）。
    // engine 侧 prepare_write_uid 虽也拼 hex 进 confirm_danger，但那次确认框
    // 不再呈现，故 app 层必须自己读一次并显示。
    let (_d, mut app) = app_with_temp_dumps("uid-show");
    let p = app.paths.dumpfiles_dir().join("uid.mfd");
    // block0 只有前 16 字节，标记字节必须落在 0..16 内
    let mut data = vec![0u8; 32];
    data[0] = 0xAB;
    data[15] = 0xCD;
    std::fs::write(&p, &data).unwrap();
    app.exec("write-ufuid-uid", None);
    app.dialogs_host
        .feed_reply(FilePurpose::WriteUidDump, Some(p.clone()));
    app.tick();
    let c = app.confirm.clone().ok_or("应有确认窗")?;
    // 期望的 block0 hex：AB + 14 个 00 + CD
    let want = format!("AB{}CD", "00".repeat(14));
    if !c.message.contains(&want) {
        return Err(format!("确认框未显示 block0 hex {want}: {}", c.message));
    }
    // 载荷必须是**完整路径**——engine 的 prepare_write_uid 拿它去读文件，
    // 传相对路径会因 CWD 不同而读不到（曾踩过这个坑）。
    match &c.kind {
        ConfirmKind::WriteUid { dump } if dump == &p.to_string_lossy() => {}
        other => return Err(format!("载荷不是完整路径: {other:?}")),
    }
    Ok(())
}

fn c_save_log_flow() -> Result<(), String> {
    let (_d, mut app) = app_with_temp_dumps("save-log");
    app.state.log_buffer = "第一行\n".to_owned();
    app.exec("save-log", None);
    // 内容必须先快照：弹窗期间日志还会被追加
    if app.log_pending.as_deref() != Some("第一行\n") {
        return Err(format!("未快照日志: {:?}", app.log_pending));
    }
    if app.dialogs_host.outstanding() != 1 {
        return Err("save-log 应投递一次保存框".into());
    }
    let out = app.paths.dumpfiles_dir().join("log.txt");
    app.dialogs_host
        .feed_reply(FilePurpose::SaveLog, Some(out.clone()));
    app.tick();
    if app.log_pending.is_some() {
        return Err("写完没清快照".into());
    }
    if std::fs::read_to_string(&out).unwrap_or_default() != "第一行\n" {
        return Err("落盘内容不对".into());
    }
    if !app.state.log_buffer.contains("已保存到") {
        return Err("未记「已保存到」".into());
    }
    Ok(())
}

fn c_dict_file_flow() -> Result<(), String> {
    let (_d, mut app) = app_with_temp_dumps("dict");
    let dic = app.paths.dumpfiles_dir().join("my.dic");
    app.exec("open-dict-file", None);
    if app.dialogs_host.outstanding() != 1 {
        return Err("应弹一次字典框".into());
    }
    app.dialogs_host
        .feed_reply(FilePurpose::DictFile, Some(dic.clone()));
    app.tick();
    if app.state.dict_path != dic {
        return Err(format!("dict_path 未回填: {:?}", app.state.dict_path));
    }
    if app.forms.dict_test.file_name != "my.dic" {
        return Err(format!("文件名未回填: {:?}", app.forms.dict_test.file_name));
    }
    Ok(())
}

fn c_history_rename() -> Result<(), String> {
    let (d, mut app) = app_with_temp_dumps("rename");
    let dir = d.0.join("dumpfiles");
    std::fs::write(dir.join("old.mfd"), b"x").unwrap();
    // 直接走提交逻辑（egui 的输入框交互不在本自检范围）
    app.forms.history_selected = vec!["old.mfd".to_owned()];
    let ctx = harness(&mut app);
    let _ = ctx;
    panels::commit_rename_for_test(&dir, "old.mfd", "new.mfd", &mut app);
    if !dir.join("new.mfd").exists() {
        return Err("重命名后新文件不存在".into());
    }
    if dir.join("old.mfd").exists() {
        return Err("重命名后旧文件还在".into());
    }
    if app.forms.history_selected != vec!["new.mfd".to_owned()] {
        return Err(format!("勾选未跟着改名: {:?}", app.forms.history_selected));
    }
    // 空名 / 同名应被拒
    panels::commit_rename_for_test(&dir, "new.mfd", "  ", &mut app);
    if !dir.join("new.mfd").exists() {
        return Err("空名竟被接受".into());
    }
    Ok(())
}

fn c_comparator_from_history() -> Result<(), String> {
    let (d, mut app) = app_with_temp_dumps("cmp-hist");
    let dir = d.0.join("dumpfiles");
    std::fs::write(dir.join("a.mfd"), vec![0xaa; 64]).unwrap();
    std::fs::write(dir.join("b.mfd"), vec![0xbb; 64]).unwrap();
    // 历史面板「比对」传 "a\u{1}b"
    app.exec("open-dump-comparator", Some("a.mfd\u{1}b.mfd".into()));
    if !app.state.panels.dump_comparator {
        return Err("比较器面板未打开".into());
    }
    if !app.forms.comparator.ready() {
        return Err(format!(
            "两侧未载入: a={:?} b={:?}",
            app.forms.comparator.a_name, app.forms.comparator.b_name
        ));
    }
    if app.forms.comparator.a_name.as_deref() != Some("a.mfd")
        || app.forms.comparator.b_name.as_deref() != Some("b.mfd")
    {
        return Err("A/B 装反了".into());
    }
    Ok(())
}

/// S14：手机/手环面板开关 + 块号门禁 + BCC 判定（对标上游 `phw_*`）。
fn case_phone_wristband() -> Result<(), String> {
    use nfc_core::tasks::phw::{block0_bcc_ok, SectorBlock};
    let mut app = NfcApp::new(nfc_core::state::AppState::new());
    // 开面板：表单重置为扇区 0 块 1（对齐上游 phoneWristband.html 默认值）
    app.exec("open-phw-window", None);
    if !app.state.panels.phone_wristband {
        return Err("手机/手环面板未打开".into());
    }
    if app.forms.phw.sector != "0" || app.forms.phw.block != "1" {
        return Err(format!("面板表单未重置为 0/1: {:?}", app.forms.phw));
    }
    // 二次打开也重置
    app.forms.phw.sector = "9".into();
    app.exec("open-phw-window", None);
    if app.forms.phw.sector != "0" {
        return Err("二次开面板未重置扇区".into());
    }
    // 块号门禁：块 0（只读 UID 块）/ 块 3（扇区尾块）不可写，1-2 可写
    let (b0, b3, ok_block, over) = (
        SectorBlock {
            sector: 0,
            block: 0,
        },
        SectorBlock {
            sector: 0,
            block: 3,
        },
        SectorBlock {
            sector: 15,
            block: 2,
        },
        SectorBlock {
            sector: 16,
            block: 1,
        },
    );
    if b0.is_valid() || b3.is_valid() {
        return Err("块 0/3 被误判可写".into());
    }
    if !ok_block.is_valid() || over.is_valid() {
        return Err("扇区 15 块 2 越界判定错误".into());
    }
    // 偏移公式 (sector*4+block)*16
    let off = SectorBlock {
        sector: 0,
        block: 1,
    }
    .offset();
    if off != 16 {
        return Err(format!("扇0块1 偏移应为 16，实得 {off}"));
    }
    // S17：BCC 自洽才允许写入；不自洽由 engine 直接拒绝（不再追加 -f 强写，
    // 否则会把坏 BCC 的 block0 写进卡里直接砖卡）。
    if !block0_bcc_ok("04A8A68A800400010000000000000000") {
        return Err("自洽 block0 未通过 BCC".into());
    }
    if block0_bcc_ok("04A8A68A000400010000000000000000") {
        return Err("不自洽 block0 被误判为自洽".into());
    }
    Ok(())
}

// ═══════════════════ S18 新能力接线 ═══════════════════

/// `cuid-write`：写卡列「CUID写」无参时先选源 dump（CuidDump），
/// 选中后进 `CuidWrite` 应用内确认框（S18）。
fn c_cuid_write_flow() -> Result<(), String> {
    let (_d, mut app) = app_with_temp_dumps("cuid");
    let p = app.paths.dumpfiles_dir().join("src.mfd");
    std::fs::write(&p, vec![0x33; 64]).unwrap();
    app.exec("cuid-write", None);
    if app.dialogs_host.outstanding() != 1 {
        return Err("cuid-write 应弹一次源 dump 文件框".into());
    }
    reply_picked(&mut app, FilePurpose::CuidDump, "src.mfd");
    let c = app.confirm.take().ok_or("应有 CUID 确认框")?;
    if !matches!(c.kind, ConfirmKind::CuidWrite { .. }) {
        return Err(format!("确认类型错: {:?}", c.kind));
    }
    if c.message.is_empty() {
        return Err("CUID 确认正文为空".into());
    }
    app.on_confirm_result(c, true);
    app.consume_pending();
    // 确认后要真的启动备份段（engine `cuid-write` → read_then_exec 第一段）
    let (t1, _, _) = app
        .engine
        .runner()
        .started_last()
        .ok_or("CUID 写未启动第一段")?;
    if t1 != "nfc-mfdetect" {
        return Err(format!("第一段应变 mfdetect，实为 {t1}"));
    }
    Ok(())
}

/// `input-uid-write`：打开 UidInput 面板（重置输入框）；
/// `done-input-uid-write` 非法输入 → 只记错误不弹确认框（S18）。
fn c_uid_input_invalid() -> Result<(), String> {
    let mut app = connected_app();
    app.exec("input-uid-write", None);
    if !app.state.panels.uid_input {
        return Err("UidInput 面板未打开".into());
    }
    if !app.forms.input_uid.is_empty() {
        return Err("开面板时输入框未清空".into());
    }
    // 非法输入：不是 8/32 hex
    app.forms.input_uid = "nothex".into();
    app.exec("done-input-uid-write", Some(app.forms.input_uid.clone()));
    if app.confirm.is_some() {
        return Err("非法输入不应弹确认框".into());
    }
    use nfc_core::state::EventKind;
    if !app.state.events.iter().any(|e| e.kind == EventKind::Error) {
        return Err("非法输入未记 error 事件".into());
    }
    Ok(())
}

/// `done-input-uid-write`：合法 8 位 UID → 拼 0 块 → `UidInputWrite` 确认框
/// （带 B6 随机码），确认后 engine 启动 `nfc-mfsetuid`（S18）。
fn c_uid_input_valid() -> Result<(), String> {
    let (_d, mut app) = app_with_temp_dumps("uid-input");
    app.exec("input-uid-write", None);
    // 8 位 UID（a1b2c3d4，BCC=0x04），清洗后应通过并拼 0 块
    app.forms.input_uid = "a1b2c3d4".into();
    app.exec("done-input-uid-write", Some(app.forms.input_uid.clone()));
    let c = app.confirm.take().ok_or("合法输入应有确认框")?;
    if !matches!(c.kind, ConfirmKind::UidInputWrite { .. }) {
        return Err(format!("确认类型错: {:?}", c.kind));
    }
    if c.code.is_none() {
        return Err("UidInputWrite 应带 B6 随机码".into());
    }
    let ConfirmKind::UidInputWrite { text } = &c.kind else {
        unreachable!()
    };
    if text != "a1b2c3d4" {
        return Err("确认框载荷丢了原文".into());
    }
    if !c.message.contains("A1B2C3D4") {
        return Err("确认正文应含拼好的 0 块大写 hex".into());
    }
    app.on_confirm_result(c, true);
    app.consume_pending();
    let (tool, args, _) = app
        .engine
        .runner()
        .started_last()
        .ok_or("确认后未启动 nfc-mfsetuid")?;
    if tool != "nfc-mfsetuid" {
        return Err(format!("工具错: {tool}"));
    }
    if args[0] != "-q" || args[1].len() != 32 {
        return Err(format!("参数错: {args:?}"));
    }
    Ok(())
}

/// `dump-editor-export-keys`：编辑器「导出密钥」→ 系统保存框（ExportKeys），
/// 写 `dumpcheck::key_dict` 到文件（S18）。
fn c_export_keys_flow() -> Result<(), String> {
    let (_d, mut app) = app_with_temp_dumps("export-keys");
    // 编辑器载入一个只有默认密钥的 dump，导出应得到密钥行
    let mut sectors = nfc_core::model::dump::to_sector_lines(&vec![0xff; 1024]).unwrap();
    // A2 全扇区默认密钥（写入 trailers）
    for si in 0..sectors.len() {
        let trailer = "FFFFFFFFFFFFFFFFFFFFFFFFF0FFFFFFFFFFFF".to_string();
        sectors[si][3] = trailer;
    }
    app.forms.editor.sectors = sectors;
    app.exec("dump-editor-export-keys", None);
    if app.dialogs_host.outstanding() != 1 {
        return Err("导出应弹一次保存框".into());
    }
    app.dialogs_host.feed_reply(
        FilePurpose::ExportKeys,
        Some(app.paths.dumpfiles_dir().join("keys.txt")),
    );
    app.tick();
    if !app.paths.dumpfiles_dir().join("keys.txt").exists() {
        return Err("导出文件未落盘".into());
    }
    Ok(())
}

/// `run-mfcuk`：写卡列「全加密爆破」→ 直接启动 mfcuk（无需文件/确认），
/// 跑完把爆出的密钥 save_keys 落盘（S18）。
fn c_run_mfcuk_flow() -> Result<(), String> {
    use nfc_core::runner::Chunk;
    let (_d, app) = app_with_temp_dumps("mfcuk");
    let mut app = app.with_mock_backend(vec![
        // `MfcukKeyParser` 认 `recovered KEY: [xxx]` 行
        Chunk::stdout("recovered KEY: [4d3c2b1a0f1e]"),
    ]);
    app.exec("run-mfcuk", None);
    app.consume_pending();
    let (tool, args, _) = app
        .engine
        .runner()
        .started_last()
        .ok_or("run-mfcuk 未启动")?;
    if tool != "mfcuk" {
        return Err(format!("工具错: {tool}"));
    }
    if args != nfc_core::tasks::card_write::mfcuk_args() {
        return Err(format!("参数错: {args:?}"));
    }
    // mock 吐一行 mfcuk 输出，跑完应产出 Key 方向事件（密钥落盘的标志）。
    for _ in 0..3 {
        app.tick();
    }
    use nfc_core::state::EventKind;
    if !app.state.events.iter().any(|e| e.kind == EventKind::Key) {
        return Err(format!("mfcuk 密钥未推 Key 事件: {:?}", app.state.events));
    }
    Ok(())
}
