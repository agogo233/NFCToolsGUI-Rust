# NFC 技术参考资料库

> 本目录为 NFCToolsGUI (Rust) 项目提供**逻辑理论基础**，覆盖本仓库各工具与协议所依据的
> ISO/IEC、NXP 数据手册、以及破解算法的第一性原理。所有结论均从**权威一手来源**核对
> （NXP 官方数据手册 PDF、ISO/IEC 14443 系列、libnfc/mfoc/hardnested 源码），
> 并尽可能与项目源码（`crates/nfc-core`）中的常量、命令码、内存布局一一对齐。

## 资料库主题

| 文件 | 主题 | 对应项目中的角色 |
|---|---|---|
| [00-ISO14443.md](00-ISO14443.md) | ISO/IEC 14443 物理层、卡片激活、防碰撞、UID/SAK/ATQA | 所有卡交互的协议基础；`nfc-mfdetect` 类型识别 |
| [01-MIFARE-Classic.md](01-MIFARE-Classic.md) | MIFARE Classic 1K/4K 内存布局、访问位、值块、命令集 | `mfoc` 读卡、写卡、格式化、转储编辑/比较 |
| [02-Crypto1.md](02-Crypto1.md) | Crypto1 流密码、三遍认证、密钥破解（nested/hardnested/dark） | `hard-nested`、`dict-test`、`mfcuk`、`libnfc-collect` |
| [03-Ultralight-NTAG.md](03-Ultralight-NTAG.md) | MIFARE Ultralight / NTAG 页布局、UID、锁定、认证 | 手机/手环「写数据扇区」、Ultralight 卡识别 |
| [04-UID与Gen卡.md](04-UID与Gen卡.md) | UID/BCC 结构、Gen1a/Gen2 可改 UID 卡（魔卡） | `write-ufuid-uid`、`lock-ufuid`、`nfc-mfsetuid`/`nfc-mflock` |
| [05-NDEF.md](05-NDEF.md) | NDEF 消息/TLV 记录、URI/文本记录格式 | 手机/手环操作区、NFC 数据交换 |
| [06-PN532.md](06-PN532.md) | PN532 主机接口（UART/SPI/I²C）、帧格式、命令集 | 硬件层：PN532 与 PC 的串口/SPI 通信 |

## 来源与核对原则

1. **NXP 官方数据手册（PDF）** 为最高优先级来源，涵盖：
   - `MF1S50YYX_V1`（MIFARE Classic EV1 1K 数据手册）
   - `MF1S70YYX_V1`（MIFARE Classic EV1 4K 数据手册）
   - `MF0ICU1`（MIFARE Ultralight 数据手册）
   - `MF0ICU2`（MIFARE Ultralight C 数据手册）
   - `NTAG213_215_216`（NTAG 数据手册）
   - `AN10833`（MIFARE type identification procedure 应用笔记，含 ATQA/SAK/GetVersion 对照表）
2. **ISO/IEC 14443-2/-3/-4** 为协议层基准（载波、调制、防碰撞、T=CL）。
3. **开源工具源码**（已 vendored 于 `framework/source/`）用于核对工具实际行为：
   - `libnfc`（底层库：`nfc-list`/`nfc-mfdetect` 等）
   - `mfoc`（普通密钥恢复）
   - `cropto1_bs` + `crapto1`（HardNested 攻击）
   - `libnfc_collect`（nonce 收集）
   - `nfc-mfsetuid` / `nfc-mflock`（Gen1a UID 改写）
   - `mfcuk`（MIFARE Classic 工具集）

## 与本仓库源码的对应

| 资料库主题 | 项目源码位置 |
|---|---|
| MIFARE Classic 内存布局（扇区/块/尾块） | `crates/nfc-core/src/model/dump.rs`（`BLOCK_SIZE`/`BLOCKS_PER_SECTOR`/`split_trailer`） |
| 密钥模型（KnownKey/UnknownKey） | `crates/nfc-core/src/model/keys.rs` |
| UID/BCC0/BCC1 计算 | `crates/nfc-core/src/model/hex.rs`、`engine.rs`（`uid_to_block0`/`block0_bcc_ok`） |
| 卡类型识别（ATQA/SAK 解析） | `crates/nfc-core/src/tasks/detect.rs`（`parse_key_info`） |
| 破解流程 | `crates/nfc-core/src/tasks/mfoc.rs`、`hard_nested.rs`、`dict_test.rs` |
| 写卡/格式化 | `crates/nfc-core/src/tasks/card_write.rs` |
| 手机/手环操作区 | `crates/nfc-core/src/tasks/phw.rs` |

> 核对说明：各文档标注的「NXP 数据手册来源」指对应的 Rev./日期；与源码不一致处
> 以项目源码为准并显式标注偏离。
