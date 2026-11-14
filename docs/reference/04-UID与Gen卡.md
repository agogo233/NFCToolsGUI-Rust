# UID / BCC 结构与 Gen1a / Gen2 可改 UID 卡（魔卡）

> 依据：ISO/IEC 14443-3（UID/BCC 定义）、NXP `MF1S50YYX_V1`（厂商块布局）、
> vendored `nfc-mfsetuid` / `nfc-mflock` 源码。
> 本文档是本项目 `write-ufuid-uid`、`lock-ufuid`、`nfc-mfsetuid`/`nfc-mflock`
> 动作的参考，以及 UID 写入的正确性与安全性依据。

---

## 1. UID 与 BCC 结构

### 1.1 单尺寸 UID（4 字节）

```
字节:  0      1      2      3
      SN0    SN1    SN2    SN3     (无 BCC，防碰撞 1 级完成)
```

### 1.2 双尺寸 UID（7 字节）

```
字节:  0      1      2      3      4      5      6
      SN0    SN1    SN2    BCC0   SN3    SN4    SN5    SN6
      └── 第1级防碰撞 ──┘        └── 第2级防碰撞 ──┘
                                (SN6 之后还有 BCC1，但不在 UID 串内)
```

- `SN0`：厂商 ID（NXP = `04h`）。
- **BCC0 = CT ⊕ SN0 ⊕ SN1 ⊕ SN2**（CT = `88h` 级联标记）。
- **BCC1 = SN3 ⊕ SN4 ⊕ SN5 ⊕ SN6**。

> **项目 `block0_bcc_ok` 的适用范围**：该校验假定 block0 是
> **4 字节 UID** 布局（`[UID 4B][BCC][08 04 00][8×00]`，即 `uid_to_block0`
> 生成的格式），计算 `BCC = block0[0]^[1]^[2]^[3]` 并与 block0[4] 比较。
> 该布局只适用于 UFUID 卡（Gen1a/Gen2）的 UID 写入场景，不用于校验
> 7 字节 UID 卡的原生 block0（其布局不同）。

---

## 2. 厂商块（Block 0 / Sector 0）布局

厂商块是**只读**的（生产时写保护），普通卡无法改写。其 16 字节：

### 2.1 4 字节 NUID 版本

```
字节:  0      1      2      3      |  4    ...   15
      NUID (4B)                    |  Manufacturer Data (12B)
```

### 2.2 7 字节 UID 版本

```
字节:  0      1      2      3      4      5      6      |  7    ...   15
      SN0    SN1    SN2    BCC0   SN3    SN4    SN5    SN6   | Manufacturer Data
```

> 本项目的 `uid_to_block0`（`card_write.rs`）把 **8 位 UID** 组装成 32 位 block0：
>
> ```
> [UID 4B][BCC 1B][08 04 00][8×00]
> ```
>
> 其中 `08 04 00` 是 **SAK=08h（MIFARE Classic 1K）+ ATQA=04 00**（4 字节 NUID），
> 后 8 字节补零。这与原版 Tauri `uid_to_block0` 一致。

---

## 3. Gen1a / Gen2（可改 UID 卡，俗称「魔卡」）

### 3.1 什么是 UFUID / 魔卡

普通 MIFARE Classic 的 block0 生产时写保护，不可改。但部分**中国克隆卡**
（Chinese clones）支持改写 block0，从而改变 UID。这类卡俗称 **UFUID 卡**。

| 类型 | UID 可写 | 特点 |
|---|---|---|
| **Gen1a**（旧） | 是 | 通过「后门命令」写 block0，可无限次改写 |
| **Gen2**（新） | 是 | 用 **`nfc-mfclassic W A u`** 写 block0（大写 `W` 触发后门 + `u` 只写 UID），写入后需重新上电才生效 |
| **普通卡** | 否 | block0 生产写保护 |

> 本项目 `write-ufuid-uid` 用 `nfc-mfsetuid`（Gen1a 后门），
> `cuid-write`（S17 起）用 `nfc-mfclassic W A u <src> <keyfile> f`（Gen2 风格，
> 大写 `W` + `u`）。备份段用 `nfc-mfdetect -S`（严格模式，缺块不落盘）。

### 3.2 Gen1a 后门命令（nfc-mfsetuid）

Gen1a 卡通过特殊命令序列进入后门状态，无需认证即可写 block0：

```
magic_wakeup():
  Halt (50 00)
  Unlock1 (40h)      ← 7-bit 命令
  Unlock2 (43h)

backdoor_write_block0():
  Write (A0 00 00 00) + CRC
  16 字节 block0 数据 + CRC
```

命令码：
- `40h`：Unlock1（7 位帧）
- `43h`：Unlock2
- `A0h` + 地址 `00 00 00`：写 block0（带 CRC）

### 3.3 Gen1a 锁卡（nfc-mflock）

锁卡使 block0 **永久只读**（不可再改写），命令序列：

```
magic_wakeup():
  Halt (50 00)
  Unlock1 (40h)      ← 7-bit 命令
  Unlock2 (43h)

lock 写入:
  Write (E1 00 00 00) + CRC   ← E1h 是「锁」命令
  16 字节数据: 85 00 00 00 00 00 00 00 00 00 00 00 00 00 00 08
```

> `E1h` 写入到 block0 的厂商块，置位相应位使 block0 永久只读。
> 本项目 `lock-ufuid` 动作即调 `nfc-mflock -q`。

### 3.4 BCC 不自洽的强制写入

若 block0 的 BCC 不自洽，写入后卡可能**软砖**（无法完成防碰撞）。
`nfc-mfsetuid` 提供 `-f` 强制写入（跳过 BCC 校验），`-n` 跳过回读验证。

> 本项目 `input-uid-write` 在 BCC 不自洽时**追加 `-f` 参数**并警示（对标上游 `-f` 分支）。
> `phw-write-uid` 在 BCC 不自洽时直接报错（不 spawn），因为这是危险操作。

---

## 4. 与项目源码的对应

| 资料库概念 | 项目源码/工具 | 说明 |
|---|---|---|
| UID→block0 组装 | `card_write.rs::uid_to_block0` | `[UID 4B][BCC][08 04 00][8×00]` |
| BCC 校验 | `phw.rs::block0_bcc_ok` | `block0[0..4]` 异或 == block0[4] |
| UID 输入清洗 | `card_write.rs::clean_uid_input` | 8 或 32 位 hex，去分隔符 |
| Gen1a 写 UID | `nfc-mfsetuid`（`40h`/`43h`/`A0h`） | 后门写 block0 |
| Gen1a 锁 UID | `nfc-mflock`（`40h`/`43h`/`E1h`） | 永久锁 block0 |
| Gen2 写 UID | `nfc-mfclassic W A u` | 大写 `W` + `u` 写 block0（Gen2 后门） |
| 厂商块布局 | [01-MIFARE-Classic.md](01-MIFARE-Classic.md) §6 | block0 只读 |
