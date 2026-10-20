# 测试 fixture 出处登记表

**每个 fixture 都必须能在下表找到它抄自哪条 `printf`。**

S12-5 抓出一整类「照着看起来像编的」fixture：它们形状恰好与**错误实现**自洽，
于是错误代码 + 编造数据 = 一片绿。S12-8、S12-9 又续挖到几处。

新加 fixture 前，**先在这里登记出处**，再写内容。

---

## `mfdetect-keys.txt` — nfc-mfdetect 的密钥行

出处：`source/nfc-mfdetect/src/nfc-mfdetect.c:448-470`

```c
fprintf(stdout, "Sector %02d - Found   Key A: %012llx ", i, ...);  // A 与 B 同行
fprintf(stdout, "Found   Key B: %012llx\n", ...);                  // 未知时 Unknown Key B
```

要点：**每扇区一行、以 `Sector NN - ` 开头、A/B 各一条**。
扇区号只能从行头取，不能用「第几条 ÷ 2」反推（原版 `keyParser.js:20`）。

## `mfoc-output.txt` — mfoc 的同一段输出

出处：`source/mfoc/src/mfoc.c:447`（`Sector %02d - ` 同款）+ `:641-646` 的 UID 行

```c
fprintf(stdout, "Sector %02d - Found   Key A: %012llx ", i, ...)
```

UID 行格式 `--------\nUID (NFCID1): <hex>\n--------`，
原版对它做固定 14 字符窗口切片（`command.js:643-645`）。

## `collect-output.txt` — libnfc-collect 的 tag 行

出处：`src/command.js:304-312`（三个子串必须**同一行**拿到）；
完整样例见 `test/parse/keyParser.test.js:162-164`

```
Found tag with uid 1a2b, collecting nonces for key B of block 15 (sector 3) using known key A ffffffffffff for block 0 (sector 0)
```

要点：三个子串（uid / `collecting nonces for key` / `(sector N)`）在**一行**里。
拆到多行、或加制表符前缀，都是编的。

## `cropto1-output.txt` — cropto1_bs 的命中行

出处：`source/cropto1_bs/hardnested/hardnested_bruteforce.c:168`

```c
sprintf(progress_text, "Key found: %012" PRIx64, key);
```

要点：小写 12 位 hex，偏移 +11。
**`Trying xxxx` 这个串不存在**（源码与真实二进制输出里都没有），S12-9 已删。

## `mfdict-output.txt` — nfc-mfdict 的命中行

出处：`src/command.js:409` 检索 `Found Key: `，偏移 +11。

## `nfc-list-opened.txt` — 连接成功

出处：`source/libnfc/utils/nfc-list.c:164` 及 `:172-186`

```c
printf("NFC device: %s opened\n", nfc_device_get_name(pnd));
printf("%d ISO14443A passive target(s) found%s\n", res, ...);
```

要点：**没有 `NFC reader(s) found:` 这一行**，S12-9 已删。

## `nfc-list-unable.txt` — 连接失败

出处：`source/libnfc/utils/nfc-list.c:155`

```c
ERR("Unable to open NFC device: %s", connstrings[i]);
```

要点：`ERR()` 走 **stderr**，且带设备串。
**没有 `Unable to init device` 这个串**，S12-9 已删。

## `nfc-list-nofound.txt` — 空输出

出处：`source/libnfc/libnfc/chips/pn53x.c` 走 ERR 退出；无设备时 stdout/stderr 均无
可判定内容，故保留为空文件，锁定 `judge_connect("") == None`。

---

## 三条判定的通道归属（别搞混）

| 标记 | 通道 | 原版回调 |
|---|---|---|
| `NFC device: … opened` | **stdout** | `processHandler` |
| `Unable to open NFC device` | **stderr** | `stderrHandler`（**进程退出时**才跑） |

两者**互不覆盖**——stderrHandler 的作用正是把先前的 true 纠正回 false。
把两条通道合成一条会让「打开成功但握手失败」永久停在 true，
UI 显示已连接而卡根本读不出来。回归测试见 `serial.rs` 的
`opened_then_unable_must_not_stay_reported_as_connected`。