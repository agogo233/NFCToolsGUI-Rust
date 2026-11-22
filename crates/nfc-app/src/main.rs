//! nfc-app 主程序入口。
//!
//! 两种模式：
//! - 默认：开 GUI 主窗口
//! - `--selftest`：不开窗口，只跑 UI 冒烟（docs/03 S8 验证项）
//!
//! 字体加载：优先 SourceCodePro（等宽，日志区用），再探测系统 CJK 字体。
//! 探测顺序见 `CJK_FONT_CANDIDATES`，覆盖 Linux/macOS/Windows 常见路径。
//!
//! # Windows 下不要弹控制台窗口
//!
//! S12-3 交叉编译出 `.exe` 后查 PE 头发现 Subsystem=3（Console），
//! 即双击会先闪一个黑框再开窗口。原版 Tauri 默认就是 GUI 子系统，没有这个框。
//! 故加 `#![windows_subsystem = "windows"]` 对齐。
//!
//! 代价：`--version` / `--selftest` 的 `println!` 在 Windows 下不再有可见输出
//! （stdout 没地方去）。这是刻意的取舍——GUI 程序的常规形态优先；
//! 需要看输出时用 `--selftest` 的退出码，或在终端里手动跑。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod dialogs_host;
mod dialogs_ui;
mod panels;
mod selftest;
mod single_instance;
mod theme_ui;
mod ui;

use app::NfcApp;

use nfc_core::paths::Paths;
use nfc_core::state::AppState;

const CJK_FONT_CANDIDATES: &[&str] = &[
    "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
    "/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttc",
    "/usr/share/fonts/opentype/noto/NotoSansCJKsc-Regular.otf",
    "/usr/share/fonts/truetype/noto/NotoSansCJKsc-Regular.ttc",
    "/usr/share/fonts/truetype/wqy/wqy-microhei.ttc",
    "/usr/share/fonts/truetype/droid/DroidSansFallbackFull.ttf",
    "/System/Library/Fonts/PingFang.ttc",
    "/System/Library/Fonts/Hiragino Sans GB.ttc",
    "C:/Windows/Fonts/msyh.ttc",
    "C:/Windows/Fonts/simhei.ttf",
    "C:/Windows/Fonts/simsun.ttc",
];

/// 原版窗口尺寸（`main.js` BrowserWindow）。
// 默认窗口 1366×800：≥1280 逻辑宽的首屏直接呈现三栏工作台
// （响应式四档见 docs/design/ui-responsive-tiers-proposal.html）。
const WIN_W: f32 = 1366.0;
const WIN_H: f32 = 800.0;
/// S15：可缩放后的最小尺寸（挡住「挤成一条」的极端尺寸）。
const WIN_MIN_W: f32 = 820.0;
const WIN_MIN_H: f32 = 640.0;

fn main() -> eframe::Result {
    let args: Vec<String> = std::env::args().skip(1).collect();

    if args.iter().any(|a| a == "--selftest") {
        return selftest::run();
    }
    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!(
            "NFCToolsGUI {} (git {}, builder {})",
            env!("APP_VERSION"),
            env!("GIT_HASH"),
            env!("BUILDER")
        );
        return Ok(());
    }

    // Paths::from_app_data 只算路径，不建目录；此处补上。
    //
    // 必须建 `base` 本身（= data_dir/NFCToolsGUI），而不是它的父目录：
    // keys.txt / registry.json 都直接落在 `base` 下，只建父目录再靠
    // `dumpfiles_dir()` 的递归创建去"顺带"建出 base 是隐式耦合——
    // 一旦 dumpfiles 目录改到别处，这些文件就没地方写了。
    let paths = Paths::from_app_data();
    let _ = std::fs::create_dir_all(paths.base());
    // 目录收紧为 0700：目录里的文件名含卡片 UID，可列目录 = 泄露读卡记录
    // （Windows no-op，%APPDATA% ACL 本就限本人）。
    nfc_core::paths::restrict_owner_dir(paths.base());
    let _ = std::fs::create_dir_all(paths.dumpfiles_dir());
    nfc_core::paths::restrict_owner_dir(&paths.dumpfiles_dir());

    // 单实例锁（§6.2 #34）：双开实例会互踩共享数据目录里的固定文件
    // （libnfc.conf 每次连接整文件覆盖、temp.mfd 是写卡备份闸的唯一载体）。
    // 锁随句柄存活、进程退出即释放。拿不到 = 已有实例在跑，提示后退出。
    // 命名绑定（而非 `let _`）：后者语句结束即 drop，锁形同虚设。
    let _instance_lock =
        match single_instance::InstanceLock::acquire(&paths.base().join("instance.lock")) {
            // 锁文件打不开（数据目录不可写等）属基础设施故障，与「已有实例」
            // 是两回事：告警后继续启动，后续文件操作会给出更具体的错误。
            Err(e) => {
                eprintln!("instance lock unavailable: {e}");
                None
            }
            Ok(Some(lock)) => Some(lock),
            Ok(None) => {
                single_instance::notify_already_running();
                return Ok(());
            }
        };

    // 窗口 / 任务栏图标：解码内嵌 32×32 PNG（IconData 宽高须为 4 的倍数）。
    // 资源损坏时退回默认图标，不让程序直接 panic。
    let window_icon = include_bytes!("../../../assets/icons/app_icon.png");
    let window_icon = image::load_from_memory(window_icon)
        .ok()
        .map(|img| img.to_rgba8())
        .map(|img| {
            let (w, h) = img.dimensions();
            egui::IconData {
                rgba: img.into_raw(),
                width: w,
                height: h,
            }
        })
        .unwrap_or_default();

    let options = eframe::NativeOptions {
        // eframe 的 `Renderer::default()` 在 glow + wgpu 双特性并存时选 wgpu，
        // 故这里显式钉 glow，避免将来误加 `wgpu` 特性后静默换回重后端。
        renderer: eframe::Renderer::Glow,
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([WIN_W, WIN_H])
            // S15：允许缩放，内容随之弹性排布；下限挡住「挤成一条」的极端尺寸
            .with_min_inner_size([WIN_MIN_W, WIN_MIN_H])
            .with_resizable(true)
            .with_decorations(false) // R2：自绘标题栏
            .with_title("NFCToolsGUI")
            .with_icon(window_icon),
        ..Default::default()
    };

    eframe::run_native(
        "NFCToolsGUI",
        options,
        Box::new(|cc| {
            setup_fonts(&cc.egui_ctx);
            // 首帧可能不触发 `sync_theme` 的守卫（`app.theme` 初值与 resolve
            // 值可能相等），故这里无条件先写一遍两桶，否则未被覆盖的那个
            // 桶会留 egui 出厂色。
            theme_ui::apply_visuals(&cc.egui_ctx);

            // S11：接上真工具。`framework/bin` 的定位规则见
            // `runner::framework_bin`（打包时 = exe 旁，开发时 = 仓库根下），
            // 与原版 `app.isPackaged ? resourcesPath : __dirname/../..` 一致。
            let paths = nfc_core::paths::Paths::from_app_data();
            // 工具目录按 **exe 相对**定位（打包 = exe 旁，开发 = 仓库根），
            // 规则见 `runner::framework_bin`。
            let root = std::env::current_exe()
                .ok()
                .and_then(|exe| exe.parent().map(|p| p.to_path_buf()))
                .unwrap_or_else(|| std::path::PathBuf::from("."));
            let bin_dir = nfc_core::runner::framework_bin(&root);
            let engine = nfc_core::engine::Engine::with_real_tools(
                bin_dir.clone(),
                paths.base().to_path_buf(),
            );

            let app = NfcApp::new(AppState::new())
                .with_logo(load_title_logo(&cc.egui_ctx))
                .with_real_engine(engine, bin_dir);
            Ok(Box::new(app))
        }),
    )
}

/// 加载自绘标题栏左上角的 logo 纹理。失败时返回 `None`
/// （只是少了左上角标识，不影响任何功能）。
fn load_title_logo(ctx: &egui::Context) -> Option<egui::TextureHandle> {
    let bytes = include_bytes!("../../../assets/icons/title_icon.png");
    let img = image::load_from_memory(bytes).ok()?.to_rgba8();
    let size = [img.width() as usize, img.height() as usize];
    let color = egui::ColorImage::from_rgba_unmultiplied(size, img.as_raw());
    Some(ctx.load_texture("app_title_logo", color, egui::TextureOptions::LINEAR))
}

fn setup_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();

    // 编译期内嵌（与 zh-CN.json / 图标同一策略）：此前按 CWD 相对路径读
    // 文件，从快捷方式 / 文件管理器启动（工作目录任意）时静默失败、日志区
    // 丢等宽，且打包脚本不含 assets/fonts，发布版必然缺失。
    fonts.font_data.insert(
        "SourceCodePro".to_owned(),
        std::sync::Arc::new(egui::FontData::from_static(include_bytes!(
            "../../../assets/fonts/SourceCodePro-Regular.ttf"
        ))),
    );
    fonts
        .families
        .entry(egui::FontFamily::Monospace)
        .or_default()
        .insert(0, "SourceCodePro".to_owned());

    // 缺 CJK 字体时中文会显示为方块。找不到不算致命——英文界面仍可用。
    if let Some((name, bytes)) = probe_cjk_font() {
        fonts.font_data.insert(
            name.clone(),
            std::sync::Arc::new(egui::FontData::from_owned(bytes)),
        );
        fonts
            .families
            .entry(egui::FontFamily::Proportional)
            .or_default()
            .insert(0, name.clone());
        // 日志区强制 Monospace（SourceCodePro 不含 CJK 字形），
        // 必须把 CJK 字体也追加到 Monospace 家族，否则中文回落时仍显方块。
        fonts
            .families
            .entry(egui::FontFamily::Monospace)
            .or_default()
            .push(name);
    }

    ctx.set_fonts(fonts);
}

fn probe_cjk_font() -> Option<(String, Vec<u8>)> {
    CJK_FONT_CANDIDATES.iter().find_map(|path| {
        let bytes = std::fs::read(path).ok()?;
        let name = std::path::Path::new(path)
            .file_name()?
            .to_string_lossy()
            .to_string();
        Some((name, bytes))
    })
}
