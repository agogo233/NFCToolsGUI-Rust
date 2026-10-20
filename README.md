# NFCToolsGUI (Rust 重写版)

与 <b>PN532</b> 交互的跨平台工具，支持 Windows / Linux / macOS。

> **这是 Rust 重写版。** UI 与全部 34 个动作的行为对齐原版（30 个对齐 Electron
> 实现，4 个手机/手环增量对齐 Tauri 端口；`NFCToolsGUI`，AGPL-3.0）。逐项核验表见 [docs/04 §9](docs/04-测试计划.md)。

## 功能

| | |
|---|---|
| MFOC 读卡 | 写卡 / 格式化 / 写 UFUID UID / 锁 UFUID |
| HardNested 破解 | 字典测试 |
| 手机/手环 | 读卡号 / 写 UID 位 / 写数据扇区 |
| 转储编辑 | 转储比较 |
| 历史记录 | 密钥管理 |

34 个 action 的「触发点 → 对话框/确认 → 工具与参数 → 收尾 → 原版行号」对照表见
[docs/04 §9](docs/04-测试计划.md)。

## 驱动安装

以 CH341 为例，按实际情况选对应驱动。

- **Windows**：[CH341SER](http://www.wch-ic.com/downloads/CH341SER_ZIP.html)
- **macOS**：[CH341SER_MAC](http://www.wch-ic.com/downloads/CH341SER_MAC_ZIP.html)
- **Linux**：内核一般已含 CH341 驱动。若缺失可试
  [CH341SER_LINUX](http://www.wch-ic.com/downloads/CH341SER_LINUX_ZIP.html)

## 构建

### 1. C 工具（`framework/bin`）

9 个 C 工具源码已**纳入本仓库**（`framework/source/`，原为 `agogo233/NFCToolsGUI`
的 git 子模块、是 `GSWXXN/*` 的 fork，现已 vendoring 进来），编译脚本 `compile.sh` 也在仓库根目录。
工具需要 autoconf / libtool 等工具链，且 Linux 产物不能用于 Windows。

Debian/Ubuntu 安装依赖（`compile.sh` 不会自动装）：

```bash
apt-get install -y autoconf automake libtool pkg-config cmake \
                   patchelf libusb-1.0-0-dev libreadline-dev \
                   zlib1g-dev liblzma-dev
```

Windows（MSYS2）还需先装 mingw 工具链，见 `compile.sh` 开头的 `pacman` 段。

然后在**本仓库根目录**执行：

```bash
sh ./compile.sh
```

产物落在本仓库的 `framework/bin/`。找到它有两种方式：

- **放在 exe 旁边**（或其上溯 5 层内的任一目录）—— 打包后的标准做法
- **设 `NFC_FRAMEWORK_BIN` 环境变量**指向它

```bash
export NFC_FRAMEWORK_BIN=./framework/bin
cargo run -p nfc-app
```

> 向上查找只往**父目录**走，看不见兄弟目录。若启动后顶部出现红色「缺少工具」
> 条并打印出查找路径，照上面任一方式指一下即可。

编出来后可以跑针对**真实二进制**的集成测试（找不到 `framework/bin` 时
自动跳过，可用 `NFC_FRAMEWORK_BIN` 指定目录）：

```bash
cargo test -p nfc-core --test real_tools --test tool_cli
```

这批测试抓到过一个「单测全绿但主流程跑不起来」的缺陷：
读卡该用 `mfoc` 而不是 `nfc-mfclassic`，两者参数风格不同
（docs/02 §6.17）。

> **缺失时的表现**：程序启动时顶部显示红色引导条并列出缺哪些工具。
> 这是相对原版的**刻意改进**（docs/02 §6.13）：原版要等用户点下按钮，
> 才在 `spawn` 处抛一条看不懂的 IO 错误。

### 2. 本程序

```bash
cd NFCToolsGUI-Rust
cargo build --release          # 产物 target/release/nfc-app(.exe)
```

或直接打包：

```bash
cd NFCToolsGUI-Rust
./scripts/package.sh           # 构建 + 收成 out/NFCToolsGUI-win-x64.zip
./scripts/package.sh --check   # 只跑门禁与检测，不产出 zip
```

最低 Rust 版本 1.95（`rust-version` 字段）。

## 运行

```bash
cargo run -p nfc-app --release
```

**首次使用**：选设备 → 等待连接成功 → 即可读卡。

自检模式（不开窗口）：

```bash
cargo run -p nfc-core -- --selftest   # 14 项核心行为
cargo run -p nfc-app  -- --selftest   # 73 项界面 + 接线（S8 主窗口 + S9 面板 + S10 三面板 + S11 接线 + S13 设备层）
```

### 平台依赖

- **Windows / macOS**：文件对话框走各自的原生弹窗，**零系统依赖**。
- **Linux**：文件对话框走 XDG 桌面门户（`xdg-portal` + `wayland`），
  **不需要 GTK**（`rfd` 的 `gtk3` 特性是可选且非默认）。但需要有门户服务的
  桌面会话；无头 / 纯 SSH 环境下启动会给出明确提示。

## 开发

```bash
cargo test --workspace          # 476 项（含 15 项真实 C 工具测试 + 6 项原版解析套件）
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
python3 tools/verify_parity.py  # 693 项静态门禁（parity 表 / 接线守卫）
```

`tools/verify_parity.py` 把关键不变量钉在源码文本上——它会核对
`command::ALL_ACTIONS` 那 34 个 action 与 docs/04 §9 的表是否一一对应，
并守住一批「曾经真错过」的缺陷。路径相对脚本解析，仓库克隆到任何位置都能跑；
原版 Electron 源码不在时相关对拍项自动跳过。

### 文档

| | |
|---|---|
| [docs/01](docs/01-功能规格.md) | 功能规格 |
| [docs/02](docs/02-架构设计.md) | 架构设计 + **§6 与原版行为的偏离登记** |
| [docs/03](docs/03-实施计划.md) | 实施计划（S0–S12） |
| [docs/04](docs/04-测试计划.md) | 测试计划 + **§9 30 action parity 表** |

**改动前先看 docs/02 §6。** 那里登记了所有刻意偏离原版的地方及原因 ——
包括危险确认框改为应用内、提示条用 UTC、文件名排序等。

## ⚠️ 安全警告

- **写卡 / 格式化 / 锁卡都是不可逆的。** 锁卡尤其：UFUID 一旦写死不可恢复。
- 程序对写卡链路有两道闸：应用内危险确认窗，以及 engine 侧独立的
  `confirm_danger` 校验（绕过 UI 直接派发也会被拒）。覆盖
  `write-IC` / `format-card` / `write-ufuid-uid` / `lock-ufuid` /
  `phw-write-uid` / `phw-write-data` 六个动作。
- 写卡/格式化前会**先备份当前卡**到 `temp.mfd`；备份失败则**中止**，
  绝不继续（对标原版 `readICThenExec` 的 reject 语义）。
- 写卡/格式化**不会**只看退出码：会核对工具报的「已写 X / 共 Y 块」，
  全部写完才报成功并删备份；部分写入或工具中途失败则**保留** `temp.mfd`
  供恢复，并报失败。锁卡（`nfc-mflock`）每一步都要收到卡片回应，任一步
  无回应即报失败——不会把「没锁成」显示成「完成」。
- 请用**可弃的测试卡**验收。

## 真机验收清单

以下均**未在本仓库的 CI 中验证**（无硬件）。请在有读卡器与可弃测试卡的环境逐项过一遍。

### 基础

- [ ] 驱动装好后，设备出现在左上角下拉里
- [ ] 选中设备 → 状态变为「已连接」
- [ ] 拔掉设备 → 状态回落为「未连接设备」

### 读卡

- [ ] `读卡` → 日志出现 `### 运行完毕 ###`，状态回到「空闲」
- [ ] `dumpfiles/` 里出现 `<UID>_<时间戳>.mfd`
- [ ] 引导条出现，点「写这张卡」→ 弹确认窗
- [ ] `检测卡片` → 能解析出已知/未知密钥，未知密钥给出 HardNested / 字典引导

### 系统对话框（本仓库环境无图形界面，**必须人工验证**）

- [ ] `写卡` → 弹出**系统**文件选择框，默认目录是 `dumpfiles/`
- [ ] 取消文件框 → 什么都不发生，且**无错误提示**（对标原版）
- [ ] 选好文件 → 弹应用内确认窗 → 确定后才真的写卡
- [ ] `写UFUID的UID` → 确认窗正文**显示了将要写入的 0 块 hex**
- [ ] `保存日志` → 弹保存框，默认名 `NFCTools_log_<时间戳>`，过滤器 txt
- [ ] 编辑器「另存为」→ 过滤器是 **mfd / dump**（不是 txt）
- [ ] 确认窗弹出时**主窗口不可点**（egui::Modal 有遮罩）

### 写卡链路（用可弃测试卡！）

- [ ] `写卡` → 先跑备份段（mfdetect）再跑写入；日志能看到两段命令
- [ ] **拔掉读卡器**再点`写卡` → 应记「读卡失败，未执行操作」并**中止**，
      **不**继续格式化（这是原版的安全闸，本重写曾一度漏掉）
- [ ] `格式化` → 同上有备份闸，且**有应用内确认窗**（原版没有）
- [ ] `锁UFUID` → 确认窗提示不可恢复；确定后跑 `nfc-mflock -q`
- [ ] 转储文件不足 16 字节时选`写UFUID的UID` → 报「转储文件太短」，
      **不**弹确认框
- [ ] **写卡成功后** `temp.mfd` 被删除；**写卡失败 / 部分写入**时
      `temp.mfd` 保留，日志里能看到「原卡备份已保留：<完整路径>」
- [ ] **手机/手环**「写 UID 位」「写数据扇区」→ 各有应用内确认窗，
      确认后才跑工具；卡号 BCC 不自洽时拒绝写入并落日志（**不**强写）
- [ ] 用一张**有 ACL 保护块**的卡试「写数据扇区」→ 应中止而不破坏数据
      （严格备份闸）

### 破解

- [ ] `HardNested` 面板 → 跑通 `libnfc-collect` → `cropto1_bs`
- [ ] 命中时密钥落盘、`unknown` 减少
- [ ] autoRun 多轮：连续解出多个扇区，进度 `1/N`、`2/N` 正确推进
- [ ] autoRun 某轮解不出 → 停住并记「本轮未解出任何密钥，已停止自动重试」
      （原版在此**无限递归**，我们加了兜底）
- [ ] 勾选「仅采集」→ 只跑 collect，不跑 cropto1_bs
- [ ] `字典测试` → 进度条与 ETA 正常

### 文件与历史

- [ ] 历史面板 → 重命名（回车或失焦提交）→ 文件名与历史刷新一致
- [ ] 历史面板 → 勾 2 个「比对」→ 比较器两侧都载入
- [ ] 历史面板 → 删除 → 确认窗 detail 列出待删文件；取消则一个都不删
- [ ] 工具菜单「编辑历史密钥」→ 系统文本编辑器打开 `keys.txt`

## License

**AGPL-3.0**（与原版一致，见 [LICENSE](LICENSE)）。

本仓库是原版 NFCToolsGUI 的 Rust 重写，其行为与源码结构大量对应原实现，
故沿用原版许可证。`framework/bin` 下的 C 工具各有独立许可证，
随其上游 fork（`GSWXXN/*`）分发。

程序不含任何网络功能，CI 有外联审计强制保证这一点。
