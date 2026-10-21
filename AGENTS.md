# AGENTS.md

NFCToolsGUI 的 Rust 重写版（AGPL-3.0）。与 PN532 交互的跨平台桌面工具，Windows / Linux / macOS。

## Workspace 结构（2 个 crate）

- `crates/nfc-core` — 纯逻辑，**零 GUI 依赖**，不编译 serialport 即可 `cargo test`。`real-serial` 特性默认**关闭**。
- `crates/nfc-app` — eframe/egui 0.36 GUI 壳（bin），默认开 `real-serial`。`rfd` 只开 `xdg-portal + wayland`，**不含 gtk3**。

`resolver = "2"`，MSRV 1.95（`rust-version`）。release profile：`panic = "abort"`、`strip = true`、`lto = "thin"`。

## 必备前置：C 工具 `framework/bin`

9 个 C 工具（`nfc-list mfoc nfc-mfclassic nfc-mfdetect nfc-mfdict nfc-mflock nfc-mfsetuid libnfc-collect cropto1_bs`）由仓库内 vendored 源码编译，**不在 CI 里自动构建**（Linux 产物不能用于 Windows）。

- 编译：`sh ./compile.sh`（Linux 需 `autoconf automake libtool pkg-config cmake patchelf libusb-1.0-0-dev libreadline-dev zlib1g-dev liblzma-dev`；Windows 走 MSYS2 MINGW64 shell）
- 运行时定位：exe 同级的 `framework/bin` → 否则向上最多 5 层父目录 → 否则读 `NFC_FRAMEWORK_BIN` 环境变量
- **缺失时的表现**：启动时顶部红色引导条列出缺哪些工具（这是刻意改进，原版要等到点按钮才报 IO 错误）。不要当成 bug。

## 开发命令

```bash
cargo build --release              # → target/release/nfc-app(.exe)
cargo run -p nfc-app --release     # 运行 GUI
cargo run -p nfc-core -- --selftest   # 14 项核心行为（不开窗口）
cargo run -p nfc-app  -- --selftest   # 73 项界面 + 接线

cargo test --workspace             # 476 项
cargo test -p nfc-core --test real_tools --test tool_cli   # 需要 framework/bin（否则自动跳过）
cargo clippy --workspace --all-targets -- -D warnings
cargo clippy --workspace --all-targets --features real-serial -- -D warnings   # CI 额外跑一次：RealSerial 平时不参与编译
cargo fmt --all --check
python3 tools/verify_parity.py     # 693 项静态门禁（34 action parity 表 + 接线守卫）
./scripts/package.sh               # 构建 + 打包 out/NFCToolsGUI-win-x64.zip
./scripts/package.sh --check       # 只跑门禁与检测
```

**顺序约定**：`fmt → clippy → test → verify_parity`。CI 的 ubuntu job 就是按这个顺序跑的。

## 测试要点

- `nfc-core` 默认不编译 `serialport` → 单测零系统库依赖，`cargo test --workspace` 在干净环境可跑。
- `real_tools` / `tool_cli` 是针对**真实 C 二进制**的集成测试，找不到 `framework/bin` 时自动跳过；用 `NFC_FRAMEWORK_BIN=./framework/bin` 指定目录即可跑。
- 抓过一个「单测全绿但主流程跑不起来」的缺陷：读卡该用 `mfoc` 而不是 `nfc-mfclassic`，两者参数风格不同（`crates/nfc-core/src/runner.rs` 的 `KNOWN_TOOLS` 注释）。
- `egui_kittest` 是 dev-dependency，CI 直接 `cargo test -p nfc-app --tests`，**不要**加 ui-test feature。
- Linux GUI 测试（app selftest）不需要显示服务器，但需要 `libxkbcommon-dev libx11-dev libxrandr-dev libxi-dev libxcursor-dev libwayland-dev libudev-dev libxcb1-dev pkg-config`。

## verify_parity.py 门禁

把 `command::ALL_ACTIONS`（34 个 action）钉在 `docs/04 §9` 的 parity 表上，并守住一批「曾经真错过」的缺陷。路径相对脚本解析，克隆到任何位置都能跑。

- 依赖原版 Electron 源码的 28 项 `check_orig` 对拍：原版不在时记 SKIP（**不算失败**）。
- 显式设了 `NFC_ORIG_DIR` 却指向不存在的目录 → 退出码 3（防静默降级）。
- CI 里 pin 了原版 commit（`ORIG_REF`），设 `NFC_ORIG_DIR` 后 SKIP 会直接失败。
- 原版源码默认在 `../NFCToolsGUI`（仓库同级）。

## 关键约定与陷阱

- **改动前先看 `docs/02 §6`**：登记了所有刻意偏离原版的地方（危险确认框改应用内、提示条用 UTC、文件名排序、S16 UI 补的 13 个 i18n key 等）。
- `dict.dic` 在仓库根，运行时资源，**不内嵌**；打包时放 exe 旁。
- 资产（`assets/locales/zh-CN.json`、字体、图标）走 `include_str!` / `include_bytes!` 内嵌。
- `crates/nfc-app/build.rs` 注入 `GIT_HASH` / `APP_VERSION=v1.0.0` / `BUILDER=GSWXXN`；Windows 下把 `assets/icons/icon.ico` 嵌入 exe 资源。
- **无网络功能**：CI 有外联审计，grep `reqwest|hyper|tokio::net|tokio::dns|ureq|isahc|surf|curl` 命中即失败。别引入任何 HTTP/DNS 依赖。
- Linux 文件对话框走 XDG 桌面门户（`xdg-portal` + wayland），**不需要 GTK**；无头/纯 SSH 环境启动会给出明确提示。
- Windows 打包后必须确认 PE 子系统是 `IMAGE_SUBSYSTEM_WINDOWS_GUI`（2），否则双击闪黑框（最近一次 fix 就是消除启动外部工具时的控制台黑框）。
- 提交风格：`feat:` / `fix:` 中文 Conventional Commits。

## 文档索引

- `docs/01-功能规格.md` — 功能规格
- `docs/02-架构设计.md` — 架构 + §6 偏离登记
- `docs/03-实施计划.md` — S0–S12 实施计划
- `docs/04-测试计划.md` — 测试计划 + §9 34 action parity 表
- `PLAN.md` — 总计划

## 真机验收

CI 不验证硬件（无读卡器）。真机验收清单在 README「真机验收清单」一节，覆盖驱动、读卡、系统对话框、写卡链路（含拔掉读卡器应中止的安全闸）、破解、文件与历史。