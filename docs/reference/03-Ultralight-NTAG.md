# MIFARE Ultralight / NTAG 完整参考

> 依据：NXP `MF0ICU1`《MIFARE Ultralight》Rev. 3.9（2014-07-23）、
> `MF0ICU2`《MIFARE Ultralight C》Rev. 3.5（2026-01-30）、
> `NTAG213_215_216`《NTAG213/215/216》Rev. 3.2（2015-06-02）。
> 本文档是手机/手环「写数据扇区」、Ultralight 类卡识别、以及 NDEF 写入的参考。

---

## 1. 总体对比

| 型号 | 总内存 | 页数 | 用户内存 | 页大小 | 命令集 |
|---|---|---|---|---|---|
| MIFARE Ultralight (MF0ICU1) | 512 bit = 64 B | 16 页 | 48 B（页 4–15） | 4 B | READ/WRITE |
| MIFARE Ultralight C (MF0ICU2) | 1536 bit = 192 B | 48 页 | 144 B（页 4–39） | 4 B | READ/WRITE/COMP_WRITE/AUTH |
| NTAG213 | 180 B | 45 页 | 144 B（页 4–39） | 4 B | READ/WRITE/FAST_READ/GET_VERSION/... |
| NTAG215 | 540 B | 135 页 | 504 B（页 4–129） | 4 B | 同上 |
| NTAG216 | 924 B | 231 页 | 888 B（页 4–225） | 4 B | 同上 |

> 所有 Ultra/NTAG 家族使用 **7 字节 UID**，ATQA=`00 44h`，SAK=`00h`。
> 因此 ATQA/SAK **无法区分** Ultralight 与 NTAG，需读页或 GetVersion（NTAG 支持）。

---

## 2. Ultralight（MF0ICU1）内存布局

16 页 × 4 字节：

| 页 | 内容 |
|---|---|
| 0–1 | **UID**（7 字节，含 BCC0/BCC1） |
| 2 | 第 9 字节 UID 续 + 内部 + **lock bytes** |
| 3 | **OTP**（一次性可编程） |
| 4–15 | **用户内存**（48 B） |

### 2.1 UID 结构（页 0–2）

```
页0: [SN0][SN1][SN2][SN3]   页1: [SN4][SN5][SN6][BCC1]   页2: [BCC0][内部][lock0][lock1]
```

- SN0 = 厂商码 `04h`（NXP）。
- **BCC0 = CT ⊕ SN0 ⊕ SN1 ⊕ SN2**（CT=`88h`）。
- **BCC1 = SN3 ⊕ SN4 ⊕ SN5 ⊕ SN6**。

> 与 NTAG 的 UID 结构完全一致（见 [00-ISO14443.md](00-ISO14443.md) §5）。

### 2.2 OTP 页（页 3）

- 出厂默认 `00 00 00 00h`。
- **只可置位**：WRITE 数据与当前内容**按位 OR** 后写入，不可逆。
- 可用于 32 次一次性计数器。

### 2.3 Lock 页（页 2 的字节 2–3）

**Ultralight（页 3 = OTP）** 与 **NTAG（页 3 = CC）** 的静态锁位布局相同（图 9）：

| lock byte | 位 | 功能 |
|---|---|---|
| byte 0 | bit 0 | `BL_OTP/CC`：冻结页 3 的锁定配置 |
| byte 0 | bit 1 | `BL9-4`：冻结页 4–9 的锁定配置 |
| byte 0 | bit 2 | `BL15-10`：冻结页 10–15 的锁定配置 |
| byte 0 | bit 3 | `L_OTP/CC`：锁页 3（OTP/CC）为只读 |
| byte 0 | bit 4–7 | `L4–L7`：分别锁页 4–7 |
| byte 1 | bit 0–7 | `L8–L15`：分别锁页 8–15 |

- **Lx**：置 1 后对应页永久只读。
- **BLx**：block-locking，置 1 后对应区域的锁定配置冻结（不可再改）。
- 写入方式：WRITE 到页 2，数据与现内容**按位 OR**，不可逆。
- **重要**：写入锁位后需发 REQA/WUPA 才生效（Ultralight 数据手册 §7.5.2）。

> Ultralight C（MF0ICU2）扩展为 48 页：页 0–3 同 Ultralight，页 4–39 用户内存，
> 页 40（28h）lock bytes 2/3，页 41（29h）16-bit 计数器，页 42–43 认证配置，
> 页 44–47 3DES 密钥。

---

## 3. NTAG21x 内存布局

### 3.1 NTAG213（45 页）

```
页 0-2:    UID(7B) + BCC + lock bytes 0/1      ← 厂商数据 + 静态锁
页 3:      Capability Container (CC)           ← 页 3
页 4-39:   用户内存 (144 B)                     ← 页 4-27h
页 40(28h): dynamic lock bytes + RFUI          ← 页 28h
页 41(29h): CFG 0
页 42(2Ah): CFG 1                              ← 配置页
页 43(2Bh): PWD
页 44(2Ch): PACK + RFUI
```

### 3.2 NTAG215（135 页）

- 用户内存页 4–129（504 B），dynamic lock 在页 130（82h），配置页 131–134（83h–86h）。

### 3.3 NTAG216（231 页）

- 用户内存页 4–225（888 B），dynamic lock 在页 226（E2h），配置页 227–230（E3h–E6h）。

### 3.4 各页功能

| 页字段 | 说明 |
|---|---|
| **UID**（页 0–2） | 7 字节 + BCC0/BCC1，生产时写保护 |
| **Static lock**（页 2 字节 2–3） | 锁页 3–15（单页粒度），block-locking 冻结 |
| **CC**（页 3） | Capability Container：`[NFC Forum 版本][内存大小][访问][数据区大小]` |
| **User memory** | 用户可读写区（NDEF 存这里） |
| **Dynamic lock**（页 28h/82h/E2h） | 锁页 16 之后（4 页粒度） |
| **CFG 0/1** | MIRROR 配置、NFC 计数器使能、AUTH0 等（见 §4） |
| **PWD**（4 B） | 32 位密码，默认 `FFFFFFFFh` |
| **PACK**（2 B） | 16 位密码确认，默认 `0000h` |

### 3.5 UID 结构（NTAG 与 Ultralight 相同）

```
页0: [SN0][SN1][SN2][SN3]   页1: [SN4][SN5][SN6][BCC1]   页2: [BCC0][内部][lock0][lock1]
```

- SN0 = `04h`（NXP），BCC0 = `88h ⊕ SN0 ⊕ SN1 ⊕ SN2`，BCC1 = `SN3⊕SN4⊕SN5⊕SN6`。

---

## 4. NTAG 特有功能

### 4.1 密码保护（PWD_AUTH）

- **PWD_AUTH**（`1Bh`）命令携带 4 字节 PWD，比对 CFG 区的 PWD。
- 成功后卡返回 **PACK**（2 字节）；失败返回 NAK。
- 认证成功后，受保护内存可访问（AUTH0 指定受保护起始页）。

### 4.2 NFC 计数器（README_CNT）

- **READ_CNT**（`39h`）读取 3 字节计数器。
- `NFC_CNT_EN` 使能后，每次 READ/FAST_READ 自动递增计数器。
- 计满 `FFFFFFh` 后停止递增（饱和）。
- 可选 **NFC 计数器 ASCII 镜像**：把计数值自动写入用户内存的指定页（MIRROR_PAGE）。

### 4.3 UID ASCII 镜像

- **MIRROR_CONF** 配置把 UID 以 ASCII 形式自动镜像到用户内存指定页。
- 用于 NDEF 消息的自动序列化（`MIRROR_CONF=10b/11b`）。

---

## 5. 命令集（NTAG21x / Ultralight）

### 5.1 NTAG21x 命令

| 命令 | 命令码 | NFC Forum | 说明 |
|---|---|---|---|
| Request | `26h` (7bit) | SENS_REQ | 请求 |
| Wake-up | `52h` (7bit) | ALL_REQ | 唤醒 |
| Anticollision CL1 | `93h 20h` | SDD_REQ CL1 | 防碰撞 1 |
| Select CL1 | `93h 70h` | SEL_REQ CL1 | 选择 1 |
| Anticollision CL2 | `95h 20h` | SDD_REQ CL2 | 防碰撞 2 |
| Select CL2 | `95h 70h` | SEL_REQ CL2 | 选择 2 |
| Halt | `50h 00h` | SLP_REQ | 挂起 |
| **GET_VERSION** | `60h` | — | 读版本（NTAG 新） |
| **READ** | `30h` | READ | 读 4 页（16 B） |
| **FAST_READ** | `3Ah` | — | 读多页（NTAG 新） |
| **WRITE** | `A2h` | WRITE | 写 1 页（4 B） |
| **COMPATIBILITY_WRITE** | `A0h` | — | 兼容写 16 B |
| **READ_CNT** | `39h` | — | 读计数器（NTAG 新） |
| **PWD_AUTH** | `1Bh` | — | 密码认证（NTAG 新） |
| **READ_SIG** | `3Ch` | — | 读 ECC 签名（NTAG 新） |

### 5.2 Ultralight C 命令（MF0ICU2）

| 命令 | 命令码 |
|---|---|
| READ | `30h`（16 B） |
| WRITE | `A2h`（4 B） |
| COMPATIBILITY WRITE | `A0h`（16 B） |
| AUTHENTICATE（3DES） | `1Ah` |

### 5.3 ACK / NAK

| 代码 | 含义 |
|---|---|
| `Ah` | ACK |
| `0h` | NAK（无效页地址） |
| `1h` | NAK（奇偶/CRC 错误） |
| `4h` | NAK（认证计数器溢出） |
| `5h` | NAK（EEPROM 写错误） |

---

## 6. ATQA / SAK / GetVersion 识别

| 卡型 | ATQA | SAK | GetVersion 第2字节 |
|---|---|---|---|
| MIFARE Ultralight | `00 44h` | `00h` | 不支持 |
| MIFARE Ultralight C | `00 44h` | `00h` | 不支持 |
| NTAG213/215/216 | `00 44h` | `00h` | `0xX4`（NTAG） |
| NTAG I²C | `00 44h` | `00h` | `0xX7` |

> **ATQA/SAK 相同**，无法区分。NTAG 用 GetVersion（`0xX4`），Ultralight 家族需读页内容。
> 本项目 `detect.rs` 以 libnfc 返回的卡型（`nfc_mfclassic` 读块 0 区分）为准。

---

## 7. 与项目源码的对应

| 资料库概念 | 项目源码/工具 | 说明 |
|---|---|---|
| 卡型识别（SAK/ATQA） | `detect.rs` `parse_key_info` | 读 SAK 判定 |
| 手机/手环写数据扇区 | `tasks/phw.rs` | Ultralight/NTAG 页写 |
| UID/BCC 计算 | `hex.rs` `uid_to_block0`/`block0_bcc_ok` | 用于 UID 写入 |
| NDEF 存于用户内存 | [05-NDEF.md](05-NDEF.md) | 页 4 之后的用户区 |
