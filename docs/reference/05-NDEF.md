# NDEF 消息与 TLV 记录格式

> 依据：NFC Forum《NFC Data Exchange Format (NDEF) Technical Specification》、
>《NFC Forum Type 2 Tag Operation Specification》（Ultralight/NTAG 的 NDEF 封装）、
> NXP `MF0ICU1`/`NTAG213_215_216` 数据手册（CC/用户内存布局）。
> 本文档是手机/手环「写数据扇区」及 NFC 数据交换的格式参考。

---

## 1. NDEF 概述

**NDEF（NFC Data Exchange Format）** 是 NFC Forum 定义的消息格式，用于在
NFC 设备与标签/卡之间交换数据。一个 NDEF 消息由**一个或多个 NDEF 记录**组成，
记录按类型（TNF）承载 URI、文本、MIME 等载荷。

```
NDEF 消息 = [记录0][记录1]...[记录N]
记录     = [头][类型][ID][载荷]
```

---

## 2. NDEF 记录格式

### 2.1 记录头（Record Header）

```
字节 0:  MB  ME  CF  SR  IL  TNF
         ↑   ↑   ↑   ↑   ↑   └── 类型名格式 (3 bit)
         │   │   │   │   └────── ID 长度字段存在 (1 bit)
         │   │   │   └────────── 短记录 (1 bit)：载荷长度 1 字节
         │   │   └────────────── 链式标志 (1 bit)：后续还有记录
         │   └────────────────── 消息结束 (1 bit)
         └────────────────────── 消息开始 (1 bit)
```

| 位 | 含义 |
|---|---|
| MB | 消息开始（第一条记录=1） |
| ME | 消息结束（最后一条记录=1） |
| CF | 链式（长记录分段） |
| SR | 短记录：载荷长度字段为 1 字节 |
| IL | ID 长度字段存在 |
| TNF | **类型名格式**（见 §2.2） |

### 2.2 TNF 类型名格式

| TNF | 值 | 含义 |
|---|---|---|
| Empty | `0x0` | 空记录 |
| Well Known | `0x1` | NFC Forum 明确定义的类型（URI/Text 等） |
| MIME Media | `0x2` | MIME 类型（如 `text/plain`） |
| Absolute URI | `0x3` | RFC 3986 URI |
| External | `0x4` | 外部类型 |
| Unknown | `0x5` | 未知类型 |
| Unchanged | `0x6` | 未改变（分段） |
| Reserved | `0x7` | 保留 |

### 2.3 记录字段

| 字段 | 长度 | 说明 |
|---|---|---|
| TYPE LENGTH | 1 B | 类型字段长度 |
| PAYLOAD LENGTH | 1 B（SR=1）/ 4 B（SR=0） | 载荷长度 |
| ID LENGTH | 1 B（IL=1，否则 0） | ID 长度 |
| TYPE | TYPE LENGTH 字节 | 类型（如 `U`=URI、`T`=Text） |
| ID | ID LENGTH 字节 | 记录 ID |
| PAYLOAD | PAYLOAD LENGTH 字节 | 载荷数据 |

---

## 3. 常用记录

### 3.1 URI 记录（TNF=Well Known, Type=`U`）

- **Type 字段**：`0x55`（`'U'`）。
- **载荷**：`[URI 标识码][URI 剩余部分]`。

**URI 标识码**（缩写）：

| 码 | URI 前缀 |
|---|---|
| `0x00` | 无前缀（URI 完整） |
| `0x01` | `http://www.` |
| `0x02` | `https://www.` |
| `0x03` | `http://` |
| `0x04` | `https://` |
| `0x05` | `tel:` |
| `0x06` | `mailto:` |

例：`https://www.example.com` 编码为 `[02]["example.com"]`（码 `0x02` = `https://www.` + 剩余部分），
或 `[04]["www.example.com"]`（码 `0x04` = `https://` + 剩余部分）。

### 3.2 文本记录（TNF=Well Known, Type=`T`）

- **Type 字段**：`0x54`（`'T'`）。
- **载荷**：`[语言码长度][语言码(ISO-639)][文本 UTF-8]`。

例：中文文本 `你好` → `[02]["zh"]["你好"]`。

---

## 4. Type 2 Tag 的 NDEF 封装（Ultralight / NTAG）

NFC Forum Type 2 Tag（Ultralight/NTAG）的 NDEF 数据存放在**用户内存**中，
用 **TLV（Type-Length-Value）** 结构封装。

### 4.1 内存布局（页 4 起）

```
页 3 (CC)   →  页 4 (NDEF TLV)  →  ...  →  Terminator TLV (0xFE)
```

- **CC（Capability Container，页 3）**：描述内存大小、可访问性、NDEF 支持。
- **TLV**：`[T][L][V]`，`T`=类型，`L`=长度，`V`=值。

### 4.2 TLV 类型

| TLV 类型 | 值 | 说明 |
|---|---|---|
| Lock Control TLV | `0x01` | 锁定控制 |
| Memory Control TLV | `0x02` | 内存控制 |
| **NDEF Message TLV** | `0x03` | 承载 NDEF 消息 |
| Proprietary TLV | `0xFD` | 厂商私有 |
| **Terminator TLV** | `0xFE` | 标记 NDEF 区结束 |

### 4.3 NDEF Message TLV

```
[0x03][长度][NDEF 消息...]
```

- `0x03`：NDEF Message TLV 类型。
- `长度`：后续 NDEF 消息字节数（1 字节，≤255；若为 `0xFF` 则长度扩展为 3 字节）。
- 之后是标准的 NDEF 消息（§2）。

### 4.4 例：写入 `https://example.com` 到 NTAG213

URI 记录载荷 = `[04]["example.com"]` = 1（URI 码）+ 11（"example.com"）= 12 字节。
NDEF 记录头 = `D1`（MB=1, ME=1, SR=1, TNF=1）。
NDEF 消息 = `[D1 01 0C 55 04 65 78 61 6D 70 6C 65 2E 63 6F 6D]`（16 字节）。

```
页 3 (CC):   E1 10 12 00      ← NFC Forum v1.0, 内存 12h, 访问 00h
页 4 起:     [03][10][D1 01 0C 55 04 65 78 61 6D 70 6C 65 2E 63 6F 6D][FE]...
              T  L   │  │  │  │  │   └── "example.com" (11 B)
              │  │   │  │  │  └── URI 码 04h = "https://"
              │  │   │  │  └── TYPE 'U' (55h)
              │  │   │  └── PAYLOAD LENGTH = 0Ch (12 B)
              │  │   └── TYPE LENGTH = 01h
              │  └── D1 = MB=1 ME=1 SR=1 TNF=1 (Well Known)
              └── 长度 10h (16 B)
```

> 之后跟 Terminator TLV `FE`。CC（页 3，出厂值 `E1 10 12 00`）：
> `E1`=NFC Forum 版本 1.0，`10`=内部字节（NTAG213 出厂 10h），
> `12`=NDEF 可用内存大小（144 字节），`00`=访问权限（可读写）。
> CC 字节 2（内存大小）按表：NTAG213=`12h`(144B)、NTAG215=`3Eh`(496B)、
> NTAG216=`6Dh`(872B)。

---

## 5. MIFARE Classic 中的 NDEF 应用

MIFARE Classic 本身不是 NFC Forum Type 2 Tag，但可承载 NDEF 数据：
- **MAD（MIFARE Application Directory）**：扇区 0 块 0–1（Sector 0 的 block 0/1）
  存储应用目录，标识 NDEF 应用所在扇区。
- **NDEF 存于用户数据扇区**：用 MIFARE Classic 的块写命令写入，密钥 A/B 控制访问。

> 本项目「写数据扇区」向 MIFARE Classic 扇区写入任意数据（`nfc-mfclassic W A u`），
> 可用于向 Classic 卡写入 NDEF 载荷，但不自动封装 TLV（由用户/上游决定）。

---

## 6. 与项目源码的对应

| 资料库概念 | 项目源码/工具 | 说明 |
|---|---|---|
| Type 2 Tag 内存布局 | [03-Ultralight-NTAG.md](03-Ultralight-NTAG.md) | 页 3 CC、页 4+ 用户区 |
| 写数据扇区（Classic） | `tasks/phw.rs` | 向 Classic 扇区写任意块 |
| 手机/手环操作区 | `docs/05-手机手环操作区方案.md` | 数据扇区默认扇区 0 块 1 |
| NDEF 封装 | NFC Forum 规范 | TLV 封装 |
