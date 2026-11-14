# Crypto1 流密码与密钥破解

> 依据：NXP MIFARE Classic 数据手册（CRYPTO1 认证）、vendored 源码
> `mfoc`（普通恢复）、`cropto1_bs`（HardNested，基于 Meijer & Verdult 2015）、
> `libnfc_collect`（nonce 收集）、`mfcuk`。
> 本文档是本项目 `hard-nested`、`dict-test`、`mfoc` 破解动作的逻辑基础。

---

## 1. Crypto1 概述

**Crypto1** 是 MIFARE Classic 1K/4K 专用的**私有流密码**（Keystream 生成器）。
不是分组密码，而是基于 **48 位 LFSR**（线性反馈移位寄存器）的密钥流生成器，
配合一个非线性 **filter 函数** 输出每比特密钥流。

| 参数 | 值 |
|---|---|
| 密钥长度 | 48 位（6 字节） |
| LFSR 状态 | 48 位 |
| Filter 函数 | 非线性布尔函数，从 LFSR 抽 5 个位点 |
| 认证 | 3 遍相互认证（ISO/IEC DIS 9798-2） |
| 密钥流输出 | 每时钟 1 位 |

### 1.1 LFSR 反馈多项式

Crypto1 的 LFSR 反馈多项式为：

```
x^48 + x^43 + x^40 + x^37 + x^36 + x^33 + x^28 + x^21 + x^19 + x^14 + x^9 + x^6 + x^3 + x^1 + 1
```

> 具体实现：`crapto1.h` 的 `LF_POLY_ODD (0x29CE5C)` / `LF_POLY_EVEN (0x870804)`
> 按奇偶位拆分 LFSR 反馈，`crapto1.c` 的 `filter()` 用 5 组 4 位（共 20 位
> LFSR 抽头）经非线性布尔函数压缩为 1 bit 密钥流。filter 的 5 个子函数
> 常量见 `filter()` 内的 `0xf22c0/0x6c9c0/0x3c8b0/0x1e458/0x0d938` 与
> 最终 `BIT(0xEC57E80A, f)`。

---

## 2. 三遍相互认证（3-Pass Authentication）

MIFARE Classic 的认证流程（ISO/IEC DIS 9798-2 风格的相互认证）：

```
PCD                                     PICC
 │── Auth(60h/61h) + BlockAddr ────────▶│
 │◀── Nt（随机数，明文）────────────────│   (第 1 遍：卡→读卡器，明文 nonce)
 │── {Nr}（加密的读卡器随机数 + 奇偶位）▶│   (第 2 遍：读卡器→卡，加密 nr)
 │◀── {Nt'} = suc3(Nt)（加密，含奇偶位）─│   (第 3 遍：卡→读卡器，加密的 tag-nonce 后继)
```

### 2.1 各步的 Crypto1 状态更新

1. **初始化**：Crypto1 密钥装入 LFSR（`key` 48 位）。
2. **UID 加载**：`crypto1_word(pcs, uid ^ Nt, 0)` —— 用 `UID ⊕ Nt` 的 32 位加载到状态位 48–79。
   （libnfc 的 `mf_enhanced_auth` 中 `crypto1_word(pcs, bytes_to_num(Rx,4) ^ t.authuid, 0)`）
3. **第 2 遍**：读卡器生成随机 `Nr`，加密 `ArEnc[i] = crypto1_byte(pcs, Nr[i], 0) ^ Nr[i]`，
   奇偶位 `ArEncPar[i] = filter(pcs) ^ oddparity(Nr[i])`。
4. **第 3 遍**：卡用 `suc3(Nt)`（PRNG 后继 3 次）生成 `At` 并加密回传。
5. **验证**：读卡器 `crypto1_word(pcs, 0x00, 0) ^ Rx == (Nt 的 suc3)` 判定认证成功。

### 2.2 密钥流的作用

认证成功后，Crypto1 状态继续滚动。**读写命令的数据与奇偶位均用密钥流异或加密**：
`密文 = 明文 ⊕ 密钥流`，`奇偶位密 = 奇偶位明 ⊕ filter(状态)`。

---

## 3. PRNG（伪随机数生成器）

卡内随机数生成器（Nt）基于 16 位 LFSR。mfoc 的 `prng_successor` 实现
（`crypto1.c`）：

```
x = x >> 1 | (x>>16 ^ x>>18 ^ x>>19 ^ x>>21) << 31   （SWAPENDIAN 后）
```

等价递推（低 16 位有效）：**`x_{n+16} = x_n ⊕ x_{n+2} ⊕ x_{n+3} ⊕ x_{n+5}`**。

`suc(n)` 表示 PRNG 的下一个 16 位状态（`prng_successor`）。

> 项目工具 `mfoc`/`libnfc-collect` 中的 `prng_successor(Nt, 32)` 计算 `suc32(Nt)`，
> `suc3(Nt)` 即 `suc(suc(suc(Nt)))`。卡生成的 Nt 只有 16 位熵，**不是 32 位独立随机**——
> 这是所有 Crypto1 攻击（nested/dark/hardnested）可利用的核心弱点。

---

## 4. 密钥破解原理总览

MIFARE Classic 密钥恢复依赖**认证协议的密码学弱点**，分三类：

| 攻击 | 原理 | 需要的交互 | 工具 |
|---|---|---|---|
| **Nested**（嵌套） | 已知一个扇区密钥 → 用其认证获得目标扇区密钥流的**部分信息**，暴力恢复 | 需 1 个已知密钥 | `mfoc`、`nfc-mfdict` |
| **Dark**（暗） | 利用认证失败时 **Nt 仅 16 位熵** 且奇偶位泄漏 | 无需已知密钥 | `crapto1`、`mfcuk` |
| **HardNested**（强化嵌套） | Ciphertext-only 分析，对**硬化卡**仍有效 | 收集 nonce（密文） | `libnfc-collect` + `cropto1_bs` |

> **nfc-mfdict**：纯字典攻击，逐个密钥尝试认证，不利用协议弱点。适合密钥在字典内。

### 4.1 Nested 攻击（mfoc）

- **前提**：至少知道 1 个扇区的密钥（默认密钥可破解首个）。
- **原理**：用已知密钥认证已知扇区 → 获得一个"已知状态" → 对该状态发起对目标扇区的认证，
  通过观察**认证失败时的加密响应**，可对目标密钥做 **2^16 级**的暴力搜索
  （PRNG 16 位熵限制），而非全 2^48。
- **流程**（mfoc.c）：
  1. 用默认密钥表尝试认证所有扇区（`defaultKeys`）。
  2. 对已知密钥扇区做 `mf_enhanced_auth` 收集 nonce 距离（`denonce.distances`）。
  3. 对未知密钥扇区做嵌套认证，收集足够 nonce。
  4. 用 `crypto1_word`/`prng_successor` 暴力恢复密钥（`distribute` 阶段）。

### 4.2 HardNested 攻击（cropto1_bs）

- **依据**：Meijer & Verdult, *"Ciphertext-only Cryptanalysis on Hardened Mifare Classic Cards"*
  (CCS 2015)。对**硬化卡**（修复了 PRNG 弱点）仍有效的攻击。
- **原理**：只利用嵌套认证中收到的**密文 nonce**（ciphertext-only），不依赖实现缺陷，
  而是 Crypto1 自身的结构弱点（LFSR + filter 的代数结构）。
- **流程**：
  1. `libnfc-collect` 用已知密钥对目标扇区做嵌套认证，收集大量 `(Nt, Nr, Ar)` 密文三元组，
     写入 `.bin` / `.txt`。
  2. `cropto1_bs`（`crypto1_bs_crack`）用 **bitsliced 暴力搜索** 在密钥空间中筛选，
     利用 filter 函数子表达式复用加速。
  3. 输出候选密钥（可能多个，需 `solve_bs`/`solve_piwi` 去重）。
- **与 mfoc 的区别**：HardNested 对硬化卡有效（mfoc 的 nested 攻击对硬化卡失效）。

> 本项目 `hard-nested` 动作即走「`libnfc-collect` 收集 → `cropto1_bs` 破解」链路。

### 4.3 Dark 攻击（crapto1）

- 利用认证失败时**奇偶位泄漏**与 **Nt 仅 16 位** 的特性，无需已知密钥即可恢复密钥。
- 是早期 `crapto1`（Nohl 2009）与 `mfcuk` 的基础。

---

## 5. 密钥文件格式与字典

### 5.1 mfoc 密钥文件

`mfoc -f keys.txt` 读入的密钥文件，每行 12 个 hex 字符（6 字节），可含注释。

```
FFFFFFFFFFFF
A0A1A2A3A4A5
D3F7D3F7D3F7
000000000000
AABBCCDDEEFF   ← 本项目 keys.rs DEFAULT_KEYS 中的条目
```

> 本项目 `parse_12hex` 校验 12 位 hex，`DEFAULT_KEYS` 含上述默认密钥
> （见 `crates/nfc-core/src/model/keys.rs`）。

### 5.2 KnownKey / UnknownKey 模型

本项目对标原版 `knownKeyInfo` / `unknownKeyInfo`：

- **KnownKey**：`(key, sector, key_type)` —— 已破解的扇区密钥三元组。
- **UnknownKey**：`(sector, key_type)` —— 未破解的扇区（key_type: A/B）。

> 见 `crates/nfc-core/src/model/keys.rs`。检测动作（`nfc-mfdetect -N`）输出
> `known`/`unknown` 集合，驱动 `hard-nested` 与 `dict-test` 的可用性。

---

## 6. 与项目源码的对应

| 资料库概念 | 项目源码/工具 | 说明 |
|---|---|---|
| 嵌套认证 + nonce 收集 | `libnfc-collect`（`nested_auth`） | 收集 `(Nt,Nr,Ar)` 密文 |
| HardNested 破解 | `cropto1_bs`（`crypto1_bs_crack`） | bitsliced 暴力 |
| 普通恢复 | `mfoc`（`mf_enhanced_auth` + 暴力） | nested 攻击 |
| 字典测试 | `nfc-mfdict` | 逐密钥认证尝试 |
| PRNG | `prng_successor` | 16 位 LFSR |
| Crypto1 核心 | `crapto1/crapto1.c` | 48 位 LFSR + filter |
| 密钥模型 | `keys.rs` KnownKey/UnknownKey | 已/未破解扇区 |
| 认证命令 | `60h`/`61h` | KeyA/KeyB 认证 |
