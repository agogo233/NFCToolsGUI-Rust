# MIFARE Classic 1K / 4K 完整参考

> 依据：NXP `MF1S50YYX_V1`《MIFARE Classic EV1 1K》Rev. 3.2（2018-05-23）、
> `MF1S70YYX_V1`《MIFARE Classic EV1 4K》Rev. 3.2（2017-11-23）。
> 本文件是本项目 `mfoc` 读卡、写卡、格式化、转储编辑/比较、密钥管理的数据模型基础。

---

## 1. 总体结构

| 型号 | EEPROM | 扇区 | 块/扇区 | 数据块 | 块大小 |
|---|---|---|---|---|---|
| **1K** (MF1S50) | 1 kB = 1024 B | **16 扇区**（0–15） | 4 | 每扇区 3 数据块 + 1 尾块 | 16 B |
| **4K** (MF1S70) | 4 kB = 4096 B | **40 扇区**（0–39） | 前 32 扇区 4 块；后 8 扇区 16 块 | 尾块 1 个/扇区 | 16 B |

> 项目源码常量：`crates/nfc-core/src/model/dump.rs`
> `BLOCK_SIZE = 16`、`BLOCKS_PER_SECTOR = 4`、`M1_SECTORS = 16`、`M1_SIZE = 1024`。
> 4K 支持由 `group_has_trailer` / `group_to_sector`（见 §1.1）处理大扇区。

### 1.1 扇区/块地址映射（4K 的关键）

4K 的扇区 0–31 与 1K 相同（每扇区 4 块，块号 = 扇区×4 + 块内偏移）。
4K 的扇区 32–39 为**大扇区**（16 块/扇区），其块号布局：

```
扇区 32: 块 128–143（尾块 = 143）
扇区 33: 块 144–159（尾块 = 159）
...
扇区 39: 块 240–255（尾块 = 255）
```

> 本项目 `dump.rs` 的 `to_sector_lines` 把字节流按 **64 字节（4 块）一组**切成
> 编辑器组；`group_to_sector` 把编辑器组映射到**逻辑扇区**（4K 大扇区由 4 组构成）：
>
> - `groups <= 32 || group < 32`：组即扇区（1K 或 4K 前 32 个 4 块扇区）
> - 否则：`32 + (group - 32) / 4`（4K 大扇区，每组 4 块）
> - `group_has_trailer`：4K 大扇区仅每 4 组的最后一组含尾块

**4K 块号 → (扇区, 块内号) 换算**：

```
扇区 s ∈ [0,32):  block = s×4 + b   (b ∈ 0..4)
扇区 s ∈ [32,40): block = 128 + (s-32)×16 + b   (b ∈ 0..16)
尾块 = 扇区最后一块（1K 是 b=3；4K 大扇区是 b=15）
```

> 编辑器按 4 块/组展示 4K：组 32 起每 4 组 = 1 个大扇区，
> 组首块号 = `128 + (group-32)×4`（`group_first_block`）。

---

## 2. 内存组织（Memory organization）

### 2.1 1K 扇区布局

每个扇区（64 字节）由 4 个 16 字节块组成：

```
扇区 S:  [ 块 0 数据 ][ 块 1 数据 ][ 块 2 数据 ][ 块 3 尾块 ]
```

- 扇区 0 的块 0 是**厂商块（Manufacturer Block）**，生产时编程并**写保护**（只读）。
- 扇区 0 只有 2 个数据块（块 1、块 2），块 0 是厂商块。
- 其余扇区有 3 个数据块 + 1 个尾块。

### 2.2 扇区尾块（Sector Trailer，块 3 或大扇区最后一块）

16 字节：

```
字节:   0     1     2     3     4     5     6     7     8     9     10    11    12    13    14    15
      └─────── Key A ───────┘└─── Access Bits ───┘└─GPD─┘└──────── Key B ────────┘
        [0,6)                [6,9)              [9,10)            [10,16)
```

| 字段 | 字节偏移 | 长度 | 说明 |
|---|---|---|---|
| **Key A** | 0–5 | 6 B | 必选密钥，读时返回全 0 |
| **Access Bits** | 6–8 | 3 B | 访问条件（见 §3） |
| **GPD**（通用数据字节） | 9 | 1 B | 用户数据 |
| **Key B** | 10–15 | 6 B | 可选密钥；若配置为可读则读时返回明文 |

> 本项目 `dump.rs::split_trailer` 的字段切分（`key_a=[0,12)`、`access=[12,18)`、
> `gpd=[18,20)`、`key_b=[20,32)`）对应**hex 字符串**切片（每字节 2 hex），
> 与上表字节偏移完全一致。

---

## 3. 访问条件（Access Bits）

访问条件由 **3 位（C1, C2, C3）** 定义，存储在尾块字节 6–8，**非反转 + 反转**各存一份。
每个数据块（块 0–2）与尾块各有 3 位。

### 3.1 位分布（字节 6–8 内的排列）

| 字节 | bit7 | bit6 | bit5 | bit4 | bit3 | bit2 | bit1 | bit0 |
|---|---|---|---|---|---|---|---|---|
| 字节 6 | C23 | C22 | C21 | C20 | C13 | C12 | C11 | C10 |
| 字节 7 | C13 | C12 | C11 | C10 | C33 | C32 | C31 | C30 |
| 字节 8 | C33 | C32 | C31 | C30 | C23 | C22 | C21 | C20 |

其中 `Cx0/Cx1/Cx2/Cx3` 分别对应**块 0/块 1/块 2/尾块**的 `C1/C2/C3` 位。
字节 9 为 GPD（用户数据），与字节 6–8 同等访问权。

> **解码（libfreefare `get_block_access_bits` 权威实现）**：
> - 非反转位：`sector = ((b7 & 0xF0) >> 4) | (b8 << 4)`（12 位）
> - 反转位：`sector_ = b6 | ((b7 & 0x0F) << 8) | 0xF000`
> - 校验：`sector ^ ~sector_` 应为 0，否则扇区被锁死
> - 每块用掩码 `0x0111 << shift`（shift：块0→0，块1→1，块2→2，尾块→3）
>   提取 C1（低半字节）、C2（次半字节）、C3（高半字节）
> - **块 0（厂商块）不适用**访问位判定（libfreefare 对 block 0 返回 EINVAL，
>   因其为只读厂商块）
> - 例：`FF 07 80` 解码为数据块 `000`（传输配置），见 §8。
> 更完整的权限位表见 libfreefare `mifare_data_access_permissions` /
> `mifare_trailer_access_permissions`。

### 3.2 尾块访问条件（表 7）

| C1 | C2 | C3 | KeyA 读 | KeyA 写 | Access 读 | Access 写 | KeyB 读 | KeyB 写 | 备注 |
|---|---|---|---|---|---|---|---|---|---|
| 0 | 0 | 0 | never | KeyA | KeyA | never | KeyA | KeyA | KeyB 可读（[1]） |
| 0 | 1 | 0 | never | never | KeyA | never | KeyA | never | KeyB 可读（[1]） |
| 1 | 0 | 0 | never | KeyB | KeyA\|B | never | never | KeyB | |
| 1 | 1 | 0 | never | never | KeyA\|B | never | never | never | |
| 0 | 0 | 1 | never | KeyA | KeyA | KeyA | KeyA | KeyA | **出厂传输配置**（[1]） |
| 0 | 1 | 1 | never | KeyB | KeyA\|B | KeyB | never | KeyB | |
| 1 | 0 | 1 | never | never | KeyA\|B | KeyB | never | never | |
| 1 | 1 | 1 | never | never | KeyA\|B | never | never | never | |

> [1] 此配置下 KeyB 可读，因此 **KeyB 不能用于认证**。若读取器用 KeyB 认证了此扇区，
> 卡会拒绝后续所有内存访问（见 NXP 数据手册 §8.7.2）。

### 3.3 数据块访问条件（表 8）

| C1 | C2 | C3 | 读 | 写 | 增/减 | 减/转/恢 | 块类型 |
|---|---|---|---|---|---|---|---|
| 0 | 0 | 0 | KeyA\|B | KeyA\|B | KeyA\|B | KeyA\|B | **传输配置**（读/写块） |
| 0 | 1 | 0 | KeyA\|B | never | never | never | 读/写块 |
| 1 | 0 | 0 | KeyA\|B | KeyB | never | never | 读/写块 |
| 1 | 1 | 0 | KeyA\|B | KeyB | KeyB | KeyA\|B | **值块** |
| 0 | 0 | 1 | KeyA\|B | never | never | KeyA\|B | **值块**（只读/递减，不可充值） |
| 0 | 1 | 1 | KeyB | KeyB | never | never | 读/写块 |
| 1 | 0 | 1 | KeyB | never | never | never | 读/写块 |
| 1 | 1 | 1 | never | never | never | never | 读/写块 |

> **厂商块**：只读状态不受访问位影响（生产时写保护）。
> **传输配置**（`0,0,0`）：KeyA 必须用于认证（出厂时 KeyB 可读）。

---

## 4. 值块（Value Block）

值块是**特殊数据块**，用于电子钱包类应用，支持 increment/decrement/restore/transfer。
固定 16 字节格式，带冗余校验与备份管理：

```
字节:   0      1      2      3      4      5      6      7      8      9      10     11     12     13     14     15
      └──────── value ────────┘└───── value 反码 ─────┘└──────── value ────────┘└─adr─┘└─adr反─┘└─adr─┘└─adr反─┘
```

| 字段 | 字节 | 说明 |
|---|---|---|
| **value** | 0–3 | 有符号 4 字节值，**LSB 先**（小端），负数用 2 的补码 |
| **value 反码** | 4–7 | `~value`（按位取反） |
| **value** | 8–11 | 再次 `value` |
| **adr** | 12 | 块地址（用于备份管理），存 4 次 |
| **adr 反码** | 13 | `~adr` |
| **adr** | 14 | 再次 `adr` |
| **adr 反码** | 15 | `~adr` |

> 有效值块判据：`value == ~value' == value''` 且 `adr == ~adr' == adr''`。
> 例：十进制 1234567（`0x0012D687`），LSB 先存 `87 D6 12 00`，反码 `78 29 ED FF`。

---

## 5. 命令集（Command Overview）

所有 MIFARE Classic 命令在认证后使用 **Crypto1** 加密（REQA/WUPA/防碰撞除外）。

| 命令 | ISO/IEC 14443 | 命令码 | 说明 |
|---|---|---|---|
| Request | REQA | `26h`（7 位） | 请求应答 |
| Wake-up | WUPA | `52h`（7 位） | 唤醒 |
| Anticollision CL1 | Anticollision CL1 | `93h 20h` | 防碰撞级 1 |
| Select CL1 | Select CL1 | `93h 70h` | 选择级 1 |
| Anticollision CL2 | Anticollision CL2 | `95h 20h` | 防碰撞级 2 |
| Select CL2 | Select CL2 | `95h 70h` | 选择级 2 |
| Halt | Halt | `50h 00h` | 挂起 |
| **Authentication with Key A** | — | `60h` | 密钥 A 认证 |
| **Authentication with Key B** | — | `61h` | 密钥 B 认证 |
| Personalize UID Usage | — | `40h` | UID 功能个性化 |
| SET_MOD_TYPE | — | `43h` | 负载调制类型设置 |
| **MIFARE Read** | — | `30h` | 读 16 字节块 |
| **MIFARE Write** | — | `A0h` | 写 16 字节块 |
| MIFARE Decrement | — | `C0h` | 递减值块 |
| MIFARE Increment | — | `C1h` | 递增值块 |
| MIFARE Restore | — | `C2h` | 值块→传输缓冲 |
| MIFARE Transfer | — | `B0h` | 传输缓冲→值块 |

### 5.1 ACK / NAK（4 位响应）

| 代码 | 含义 |
|---|---|
| `Ah` | ACK（成功） |
| `0h` | 无效操作（transfer buffer 有效） |
| `1h` | 奇偶或 CRC 错误（transfer buffer 有效） |
| `4h` | 无效操作（transfer buffer 无效） |
| `5h` | 奇偶或 CRC 错误（transfer buffer 无效） |

> libnfc 的 `nfc_initiator_mifare_cmd` 即按此 4 位 ACK 判定读写结果。

---

## 6. UID 与厂商块（Block 0 / Sector 0）

厂商块（块 0 扇区 0）生产时编程并写保护，内容：

### 6.1 4 字节 NUID 版本（MF1S503yX）

```
字节:   0      1      2      3      |  4                     ...         15
      └────── NUID ──────┘          └────────── Manufacturer Data ────────┘
```

### 6.2 7 字节 UID 版本（MF1S500yX）

```
字节:   0      1      2      3      4      5      6      |  7    ...      15
      SN0    SN1    SN2    BCC0   SN3    SN4    SN5    SN6   | Manufacturer Data
      └──────── UID ────────┘                                └─────────────────┘
```

> UID/BCC 结构详见 [00-ISO14443.md](00-ISO14443.md) §5、[04-UID与Gen卡.md](04-UID与Gen卡.md)。

---

## 7. ATQA / SAK 识别

| 卡型 | ATQA | SAK | UID 尺寸 |
|---|---|---|---|
| MIFARE Classic EV1 1K | `00 04h`（4B NUID）/ `00 44h`（7B UID） | `08h` | 单/双 |
| MIFARE Classic EV1 4K | `00 02h`（4B NUID）/ `00 42h`（7B UID） | `18h` | 单/双 |

> **SAK 判定**：SAK bit4=1 → Classic（1K 或 4K）；bit5=1 → 4K。
> 本项目 `detect.rs` 读 SAK 判断 1K/4K。SAK 是位映射，不要用完整字节值。

---

## 8. 出厂默认密钥（Transport Key）

出厂时所有扇区 KeyA = `FF FF FF FF FF FF`，KeyB = `FF FF FF FF FF FF`，
访问位字节 6–8 = **`FF 07 80`**（标准传输配置）。

用 libfreefare 权威解码（`get_block_access_bits`）验证 `FF 07 80`：
- 非反转位 = `(b7&0xF0)>>4 | (b8<<4)` = `0x0800`
- 数据块 `C1C2C3 = 000`（Table 8 第一行：传输配置，KeyA|B 可读写/增减）
- 尾块 `C1C2C3 = 001`（Table 7 第五行：**出厂传输配置**，KeyA 认证，KeyB 可读）

> 非反转与反转互补校验通过（`sa ^ ~sa_ == 0`），说明该配置是合法传输配置。
> 数据块传输配置为 **`000`**，尾块传输配置为 **`001`**。

> 本项目 `keys.rs` 的 `DEFAULT_KEYS`（含 `AABBCCDDEEFF` 等）用于字典测试与默认密钥列表。

---

## 9. 与项目源码的对应

| 资料库概念 | 项目源码/工具 | 说明 |
|---|---|---|
| 扇区/块常量 | `dump.rs` `BLOCK_SIZE`/`BLOCKS_PER_SECTOR`/`M1_SECTORS` | 1K 结构 |
| 尾块字段切分 | `dump.rs` `split_trailer` | KeyA/Access/GPD/KeyB |
| 转储→扇区行 | `dump.rs` `to_sector_lines` | 32 hex/行，4 行/扇区 |
| 扇区分组（4K） | `dump.rs` `group_to_sector`/`group_has_trailer` | 4K 大扇区 |
| 读卡 | `mfoc`（`-O dump`） | Crypto1 恢复后读全卡 |
| 写卡/格式化 | `nfc-mfclassic`（`nfc-mfclassic W A`） | 写密钥/数据 |
| 密钥模型 | `keys.rs` KnownKey/UnknownKey | 已破解/未破解扇区 |
| 转储比较 | `dump.rs` `diff_sectors` | 逐字符 diff |
