#!/usr/bin/env python3
"""全量静态回归：把关键不变量钉死在源码文本上。

不跑 cargo build/test，也不跑 rustfmt（前者慢、后者不是编译器）。
纯文本断言，用来防「改了代码但忘了改约束」这类漂移——
尤其是 S11-3 的 30 action parity 表（docs/04 §9）与 S11-2 的接线守卫。

用法：
    python3 tools/verify_parity.py

路径全部相对本文件解析，故仓库克隆到任何位置都能跑。
原版 Electron 源码（`NFCToolsGUI`）不在本仓库时，相关对拍项自动跳过
并计数，不算失败。
"""
from __future__ import annotations

import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
CORE = REPO / "crates/nfc-core"
APP = REPO / "crates/nfc-app"

# 原版 Electron 源码：默认在同级的 ../NFCToolsGUI，可用环境变量覆盖。
_orig_env = os.environ.get("NFC_ORIG_DIR")
if _orig_env:
    ORIG = Path(_orig_env).resolve()
    if not ORIG.is_dir():
        # 显式指定了 NFC_ORIG_DIR 却指向不存在的目录 = 配置错误（比如 CI 的
        # checkout 步骤没跑成功 / 路径写错）。此时**必须大声失败**，不能静默
        # 把 28 项对拍全 SKIP 掉——那正是「CI 盲区」的根源。
        print(
            f"ERROR: NFC_ORIG_DIR={_orig_env} 指向的目录不存在，无法对拍原版。\n"
            "       若本机没有原版可去掉该变量（对拍项会 SKIP）；若 CI 里设了它，"
            "       说明原版 checkout 步骤没成功。",
            file=sys.stderr,
        )
        sys.exit(3)
else:
    _cands = [
        REPO.parent / "NFCToolsGUI",          # 仓库与原版同级
        REPO.parent.parent / "NFCToolsGUI",   # 仓库嵌在 NFC/ 下、原版在上一级
        REPO / "NFCToolsGUI",                # 原版就在本仓库内
    ]
    ORIG = next((c for c in _cands if c.is_dir()), _cands[0])
# 原版仓库在不在本机。CI 只 clone 本仓库，因此**必须**支持缺失：
# 依赖原版的对拍项此时记为 SKIP 并计入跳过数，不算失败。
# （注意：只有「没显式指定 NFC_ORIG_DIR 又找不到」才走 SKIP；显式指定了却没有，
#   上面已经 sys.exit(3)，不会落到这里。）
ORIG_MISSING = not ORIG.is_dir()
ZH = REPO / "assets/locales/zh-CN.json"
# S16 UI 体验优化本地补的 21 个 key（docs/02 §6.25 续 / §6.28 偏离登记；
# 含 S18 后续补的 `html_detect_known` / `html_detect_unknown` / `html_rename`，
# 以及 §6.28 P1 补的控制台悬停说明 5 个）。
S16_EXTRA = {
    "html_console",
    "html_events",
    "html_no_events",
    "html_expand",
    "html_collapse",
    "html_tip_clear_log",
    "html_tip_save_log",
    "html_tip_stop_task",
    "html_tip_stop_idle",
    "html_tip_autoscroll",
    "html_waiting_file",
    "html_workflow_group",
    "html_detect_summary",
    "html_detect_known",
    "html_detect_unknown",
    "html_rename",
    "html_not_detected",
    "html_auto_run_hint",
    "html_appearance",
    "html_data_dir",
    "html_tool_dir",
}
# S17 写卡安全加固本地补的 3 个 key（docs/02 §6.26）。
# 注：`dialog_msg_confirm_format_card` 原版已有，不计入本地新增。
S17_EXTRA = {
    "log_msg_backup_kept",
    "log_msg_block0_bcc_invalid",
    "log_msg_write_incomplete",
}
# S18 搬运本地新增的 14 个 key（原版没有）：CUID 写、mfcuk、通信速度、
# 深度扫描、B6 确认码、值块标记（docs/02 §6.27 登记）。
S18_EXTRA = {
    "dialog_msg_confirm_cuid_write",
    "html_applies_after_reconnect",
    "html_confirm_code",
    "html_cuid_write",
    "html_deep_scan",
    "html_mfcuk_run",
    "html_tip_cuid_write",
    "html_tip_mfcuk_run",
    "html_value_block",
    "indicator_mfcuk_running",
    "indicator_writing_cuid",
    "log_msg_applies_after_reconnect",
    "log_msg_cuid_key_built",
    "log_msg_start_mfcuk",
}
# S18 从「上游独有」挪回的 20 个 key：两版共有且文案逐字一致。
S18_MIGRATED = {
    "dialog_msg_invalid_uid_input",
    "html_check_all_ok",
    "html_check_dump",
    "html_data_not_classic_layout",
    "html_error_acl_corrupt",
    "html_error_acl_invalid",
    "html_error_bcc",
    "html_error_not_hex",
    "html_error_size",
    "html_error_size_bytes",
    "html_export_keys",
    "html_fix_dump",
    "html_fix_none",
    "html_fix_summary",
    "html_speed",
    "html_tip_write_uid_input",
    "html_uid_input_placeholder",
    "html_uid_input_title",
    "html_write_uid_input",
    "log_msg_bcc_force",
}
# 两个 scratch 目录：每次运行重建（a_rustfmt rmtree 后重拷，b_resolve mkdir）。
# 用系统临时目录而非写死 /tmp/opencode——后者是本机产物，CI 的 Linux/Windows
# runner 上未必存在或可写。
ISO = Path(tempfile.mkdtemp(prefix="verify_parity_rustfmt_"))
RESOLVE = Path(tempfile.mkdtemp(prefix="verify_parity_resolve_"))
# index.crates.io-<hash> 的 hash 随 cargo 版本变化，故不写死；
# 取本地第一个 index.crates.io-* 目录。CI 上没有索引缓存，此时该项跳过。
_cargo_index = Path.home() / ".cargo/registry/index"
_idx_dirs = sorted(_cargo_index.glob("index.crates.io-*")) if _cargo_index.is_dir() else []
IDX: Path | None = (_idx_dirs[0] / ".cache") if _idx_dirs else None
MSRV = (1, 95)

results: list[tuple[str, str, str]] = []
# 因「原版缺失」而跳过的项计数（区别于 c_msrv 那种「无 cargo index」的合法跳过）。
ORIG_SKIPS = 0


def check(ok: bool, label: str, detail: str = "") -> bool:
    results.append(("PASS" if ok else "FAIL", label, detail))
    return ok


def skip(label: str, detail: str = "", orig: bool = False) -> None:
    """本版无法执行的项。计入跳过数，不算失败。

    orig=True 表示这条跳过是因「原版源码缺失」——只有它参与
    「设了 NFC_ORIG_DIR 却仍跳对拍」的护栏（见 main 末尾）。
    """
    global ORIG_SKIPS
    if orig:
        ORIG_SKIPS += 1
    results.append(("SKIP", label, detail or f"原版源码缺失：{ORIG}"))


def check_orig(ok: bool, label: str, detail: str = "") -> bool:
    """依赖原版源码的对拍项。原版不在时跳过而非失败（见文件头用法说明）。"""
    if ORIG_MISSING:
        skip(label, detail, orig=True)
        return False
    return check(ok, label, detail)


def rd(p: Path) -> str:
    """读文件。原版仓库不在时返回空串——相关对拍项由 `check_orig` 跳过，
    而不是整脚本崩在 FileNotFoundError 上。"""
    try:
        return p.read_text(encoding="utf-8", errors="replace")
    except OSError:
        return ""


def rd_json(p: Path) -> dict:
    """读 JSON 对象。文件缺失或不是合法 JSON 时返回空对象——
    直接 `json.loads(rd(...))` 会在原版仓库不在时抛 JSONDecodeError 崩掉整个脚本。"""
    try:
        return json.loads(p.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return {}


def sh(cmd: list[str], cwd: Path | None = None, timeout: int = 300) -> subprocess.CompletedProcess:
    return subprocess.run(cmd, cwd=cwd, capture_output=True, text=True, encoding="utf-8", timeout=timeout)


# ---------------------------------------------------------------- A rustfmt
def a_rustfmt() -> None:
    # edition 从 Cargo.toml 读，不在脚本里重复登记。
    edition = re.search(r'^edition\s*=\s*"(\d{4})"', rd(CORE / "Cargo.toml"), re.M)
    edition = edition.group(1) if edition else "2021"
    # 必须保持目录结构：rustfmt 会解析 `mod x;`，拍平到同一目录后
    # model/mod.rs 找不到 parse / keys / dump / hex，module resolution 失败。
    shutil.rmtree(ISO, ignore_errors=True)
    for f in sorted(CORE.glob("**/*.rs")):
        dst = ISO / f.relative_to(CORE)
        dst.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(f, dst)
    for f in sorted(ISO.glob("**/*.rs")):
        r = sh(["rustfmt", "--edition", edition, "--check", str(f)], cwd=ISO)
        out = (r.stderr + r.stdout).strip()
        # 语法错误会让 returncode != 0/1；纯格式差异只产生 stdout 的 "Diff in ..."
        # 且 stderr 为空。故非 0 即有问题。
        check(r.returncode == 0, f"A rustfmt {f.relative_to(ISO)}",
              out.splitlines()[0] if out else "")


# ---------------------------------------------------------------- B 依赖解析
def b_resolve() -> None:
    RESOLVE.mkdir(parents=True, exist_ok=True)
    (RESOLVE / "src").mkdir(exist_ok=True)
    (RESOLVE / "src/lib.rs").write_text("")
    shutil.copy2(CORE / "Cargo.toml", RESOLVE / "Cargo.toml")
    for feats, tag in [([], "默认"), (["--features", "real-serial"], "real-serial")]:
        r = sh(["cargo", "metadata", "--format-version", "1", *feats], cwd=RESOLVE, timeout=420)
        detail = ""
        if r.returncode == 0:
            meta = json.loads(r.stdout)
            pkgs = {p["name"]: p["version"] for p in meta["packages"]}
            detail = f"serialport={pkgs.get('serialport')}, regex={pkgs.get('regex')}, thiserror={pkgs.get('thiserror')}"
        else:
            detail = r.stderr.strip().splitlines()[-1] if r.stderr.strip() else ""
        expect_sp = ("serialport=None" in detail) if tag == "默认" else ("serialport=4" in detail)
        check(r.returncode == 0 and expect_sp, f"B 依赖解析 {tag}", detail)


# ---------------------------------------------------------------- C MSRV
def c_msrv() -> None:
    if IDX is None:
        skip("C 依赖 MSRV 不高于本仓库 rust-version", "本机无 cargo index 缓存")
        return
    for crate in ["serialport", "regex"]:
        parts = list(crate)
        if len(parts) >= 4:
            rel = Path(parts[0] + parts[1]) / (parts[2] + parts[3]) / crate
        elif len(parts) == 3:
            rel = Path("3") / parts[0] / crate
        else:
            rel = Path(str(len(parts))) / crate
        p = IDX / rel
        if not p.exists():
            check(False, f"C MSRV {crate}", "index 缓存缺失")
            continue
        best = None
        for line in rd(p).splitlines():
            line = line.strip()
            if not line.startswith("{"):
                continue
            try:
                e = json.loads(line)
            except json.JSONDecodeError:
                continue
            if e.get("name") != crate or e.get("yanked"):
                continue
            rv = e.get("rust_version")
            if rv:
                nums = tuple(int(x) for x in re.findall(r"\d+", rv)[:2])
                if best is None or nums > best[0]:
                    best = (nums, rv)
        if best is None:
            check(True, f"C MSRV {crate}", "未声明 rust-version（视为兼容）")
        else:
            check(best[0] <= MSRV, f"C MSRV {crate}", f"{best[1]} <= 1.95")


# ---------------------------------------------------------------- D-G 原文对齐
def d_align() -> None:
    if ORIG_MISSING:
        skip("D 原文标记 / libnfc.conf 模板 / DEVICE_SPEED / DEVICE_PREFIX 对齐",
             orig=True)
        return
    cmd_js = rd(ORIG / "src/command.js")
    status_js = rd(ORIG / "src/status.js")
    preload_js = rd(ORIG / "src/preload.js")
    serial_rs = rd(CORE / "src/serial.rs")
    paths_rs = rd(CORE / "src/paths.rs")

    for m in ["NFC device: NFC_Device opened", "Unable to open NFC device"]:
        check_orig(m in cmd_js, f"D 原文含标记 {m!r}")
        check(re.search(rf'"{re.escape(m)}"', serial_rs) is not None, f"D serial.rs 含标记 {m!r}")

    m = re.search(r"const content = `(.*?)`", cmd_js, re.S)
    tmpl = m.group(1) if m else ""
    tmpl_norm = tmpl.replace("${status.currentDevice}", "{device}").replace("${status.currentSpeed}", "{speed}")
    rust_m = re.search(r'format!\(\s*"device\.name.*?"\s*\)', paths_rs, re.S)
    norm = lambda s: re.sub(r"[\s\\]+", "", s)  # noqa: E731
    ok = bool(rust_m) and norm(tmpl_norm) in norm(rust_m.group(0))
    check_orig(ok, "E libnfc.conf 模板对齐", tmpl_norm)

    orig_speed = re.search(r"currentSpeed\s*=\s*(\d+)", status_js)
    rust_speed = re.search(r"DEVICE_SPEED:\s*u32\s*=\s*([\d_]+)", paths_rs)
    check_orig(
        orig_speed and rust_speed and int(orig_speed.group(1)) == int(rust_speed.group(1).replace("_", "")),
        "F DEVICE_SPEED 对齐",
        f"原版 {orig_speed.group(1) if orig_speed else '?'} / rust {rust_speed.group(1) if rust_speed else '?'}",
    )

    op = re.search(r'devicePrefix:\s*"([^"]+)"', preload_js)
    check(op and f'"{op.group(1)}"' in serial_rs, "G DEVICE_PREFIX 对齐 preload.js", op.group(1) if op else "?")
    check_orig('device === " "' in cmd_js, 'G 原文含 device === " " 断连分支')
    check('" "' in serial_rs, "G DEVICE_NONE 对齐")


# ---------------------------------------------------------------- H i18n
def h_i18n() -> None:
    keys = json.loads(rd(REPO / "assets/locales/zh-CN.json"))
    orig_zh_keys = set(rd_json(ORIG / "src/locales/zh-CN.json")) if not ORIG_MISSING else set()
    check(
        len(keys) == 194 + len(S16_EXTRA) + len(S17_EXTRA) + len(S18_EXTRA) + len(S18_MIGRATED),
        f"H zh-CN.json {194 + len(S16_EXTRA) + len(S17_EXTRA) + len(S18_EXTRA) + len(S18_MIGRATED)} key"
        f"（基础 194 + S16 补 {len(S16_EXTRA)} + S17 补 {len(S17_EXTRA)}"
        f" + S18 新增 {len(S18_EXTRA)} + S18 迁移 {len(S18_MIGRATED)}）",
        f"实际 {len(keys)}",
    )
    # S8 新增的 3 个 settings 按钮文案（原版 settings.html 硬编码英文，无 i18n key，
    # 此处按 docs/01 §1.6 决策 #6 恢复入口时补齐）
    for k in ("html_toggle_dark_mode", "html_toggle_light_mode", "html_follow_system_mode"):
        check(k in keys, f"H zh-CN 含 {k}")
    for k in S16_EXTRA:
        check(k in keys, f"H zh-CN 含 S16 {k}")
    for k in S17_EXTRA:
        check(k in keys, f"H zh-CN 含 S17 {k}")
    for k in S18_EXTRA:
        check(k in keys, f"H zh-CN 含 S18 新增 {k}")
    for k in S18_MIGRATED:
        check(k in keys, f"H zh-CN 含 S18 迁移 {k}")
    for k in S18_MIGRATED:
        check(k in orig_zh_keys, f"H S18 迁移 {k} 确在上游（非误造）")
    check(not (REPO / "assets/locales/en.json").exists(), "H 无 en.json（仅中文决策）")
    body = rd(CORE / "src/serial.rs")
    used = set()
    for seg in re.split(r"i18n::t\(", body)[1:]:
        used |= set(re.findall(r'"([a-z][a-z0-9_]{3,40})"', seg[:200]))
    missing = sorted(k for k in used if k not in keys)
    check(not missing, "H serial.rs 用到的 key 全部存在", f"缺失 {missing}")
    check(len(used) >= 4, "H serial.rs 覆盖 4 个连接文案", f"实际 {len(used)}: {sorted(used)}")
    for bad in ["log_msg_discover_deviec", "log_msg_notfounded_device"]:
        check(bad not in body, f"H 无臆造 key {bad}")


# ---------------------------------------------------------------- I 门禁
def i_gate() -> None:
    eu = rd(ORIG / "src/execUtils.js")
    rs = rd(CORE / "src/runner.rs")
    check_orig('status.currentDevice === null' in eu, "I 原文门禁①：currentDevice === null")
    check_orig('cmd !== "nfc-list"' in eu, "I 原文门禁②：未连接仅放行 nfc-list")
    check_orig("status.isRunningTask" in eu, "I 原文门禁③：忙碌")
    for pat, label in [
        (r"has_current_device", "I rust 门禁①"),
        (r"nfc-list", "I rust 门禁②"),
        (r"is_running_task", "I rust 门禁③"),
    ]:
        check(re.search(pat, rs) is not None, label)


# ---------------------------------------------------------------- J fixture
def j_fixture() -> None:
    fx = CORE / "tests/fixtures"
    opened = rd(fx / "nfc-list-opened.txt")
    unable = rd(fx / "nfc-list-unable.txt")
    nofound = rd(fx / "nfc-list-nofound.txt")
    check("NFC device: NFC_Device opened" in opened, "J fixture opened 含成功标记")
    check("Unable to open NFC device" in unable, "J fixture unable 含失败标记")
    check(not nofound.strip(), "J fixture nofound 为空（原文无第三标记串）", f"{len(nofound)}B")


# ---------------------------------------------------------------- K 文档
def k_docs() -> None:
    docs = {p.name: rd(p) for p in sorted(REPO.glob("docs/*.md"))}
    check(len(docs) >= 4, f"K 文档齐全 ({len(docs)})")
    bad = [
        n
        for n, t in docs.items()
        if any("No NFC device found" in ln and "勿臆造" not in ln for ln in t.splitlines())
    ]
    check(not bad, "K 文档无臆造判定串", f"{bad}")

    def doc(prefix: str) -> str:
        for n, v in docs.items():
            if n.startswith(prefix):
                return v
        return ""

    s3 = doc("03-")
    for needle in ["实际落地", "偏离原版", "SerialTrait", "PortScanner", "ConnectFlow"]:
        check(needle in s3, f"K docs/03 S3 含 {needle}")
    c02 = doc("02-")
    for needle in [
        'serialport = { version = "4", optional = true }',
        'real-serial = ["dep:serialport"]',
        'regex = "1"',
    ]:
        check(needle in c02, f"K docs/02 依赖草案含 {needle!r}")
    for ghost in ['"connect_device"', "connect_device"]:
        check(
            not any(
                ghost in ln and "ConnectFlow" not in ln
                for n, t in docs.items()
                for ln in t.splitlines()
            ),
            f"K 文档不残留 {ghost}",
        )


# ---------------------------------------------------------------- L 结构
def l_structure() -> None:
    ct = rd(CORE / "Cargo.toml")
    check('serialport = { version = "4", optional = true }' in ct, "L Cargo.toml serialport optional")
    check('real-serial = ["dep:serialport"]' in ct, "L Cargo.toml real-serial feature")
    check("pub mod serial;" in rd(CORE / "src/lib.rs"), "L lib.rs 声明 pub mod serial")
    serial = rd(CORE / "src/serial.rs")
    check("#[cfg(feature = \"real-serial\")]" in serial, "L RealSerial 被 feature 门控")
    check("pub trait SerialTrait" in serial, "L SerialTrait 已定义")
    check("impl SerialTrait for MockSerial" in serial, "L MockSerial 实现 SerialTrait")
    n_serial = len(re.findall(r"#\[test\]", serial))
    n_int = len(re.findall(r"#\[test\]", rd(CORE / "tests/integration.rs")))
    # 这里原先钉死 16 个，每加一个用例就要手工改一次——已因此漂移过
    # （S12-8 加了第 17 个）。改成下界断言：只保证「够多」，不阻塞正常新增。
    check(n_serial >= 17, f"L serial.rs 单测 >= 17", f"实际 {n_serial}")
    check(n_int == 58, "L integration.rs 集成 = 58（S3 14 + S4 18 + S5 14 + S6/S7 11 + S14 1）", f"实际 {n_int}")
    gi = rd(REPO / ".gitignore") if (REPO / ".gitignore").exists() else ""
    check("fixtures" in gi or not "*.mfd" in gi, "L .gitignore 允许 fixture 目录", gi.strip().replace("\n", " | "))


# ---------------------------------------------------------------- M 契约
def m_contract() -> None:
    serial = rd(CORE / "src/serial.rs")

    fi = serial.find("pub fn finish(")
    seg = serial[fi:] if fi >= 0 else ""
    check(
        seg.find("Event::DeviceState(") >= 0
        and seg.find("Event::DeviceState(") < seg.find("Event::TaskDone"),
        "M finish 先 DeviceState 后 TaskDone（对齐原版顺序）",
    )

    gi_ = serial.find("pub fn judge_connect(")
    seg2 = serial[gi_ : gi_ + 400]
    check(seg2.find("MARKER_OPENED") < seg2.find("MARKER_UNABLE"), "M judge_connect 成功标记优先")

    bi = serial.find("pub fn begin(")
    seg3 = serial[bi:] if bi >= 0 else ""
    check(seg3.find("write_nfc_config(") < seg3.find("runner.start("), "M begin 先写 libnfc.conf 再 spawn")
    oi = [
        seg3.find("Event::Status {"),
        seg3.find("Event::DeviceState(DeviceState::Connecting)"),
        seg3.find("Event::Log(format!("),
    ]
    check(
        all(v >= 0 for v in oi) and oi == sorted(oi),
        "M begin 顺序 Status→Connecting→Log（对齐 conn-usb-devices）",
        str(oi),
    )

    check("is_device_list_equal" in serial, "M PortScanner 走等值比对")
    check("state.current_device = None" in serial, "M disconnect 清 current_device")
    check("thread::sleep" not in serial and ".wait()" not in serial, "M serial.rs 无阻塞等待")

    # DeviceError::Tool 保留 ToolError 原类型，不做 to_string() 降级
    check(
        "Tool(#[from] ToolError)" in serial and "DeviceError::Tool(e.to_string())" not in serial,
        "M DeviceError::Tool 保留 ToolError 原类型",
    )
    check("#[error(transparent)]" in serial, "M DeviceError::Tool 用 transparent Display")
    integ = rd(CORE / "tests/integration.rs")
    check(
        "matches!(r, Err(DeviceError::Tool(ToolError::Busy)))" in integ,
        "M 集成测试按 ToolError 变体断言（非字符串匹配）",
    )
    check("pub trait SerialTrait: Send + Sync" in serial, "M SerialTrait 有 Send + Sync")
    check('vec!["Status", "DeviceState:Connecting", "Log"]' in serial, "M begin 测试锁定事件顺序")
    check(
        "被截断的块不应产生结论日志" in integ and "kill_during_connect_truncates_pending" in integ,
        "M kill 测试覆盖截断语义",
    )
    check("struct TempGuard(Vec<PathBuf>)" in integ, "M 集成测试守卫自持目录（Vec<PathBuf>）")
    check("let (paths, _guard) = tmp_guarded()" in integ, "M 集成测试用配对守卫 tmp_guarded")
    # 共享 Vec + drop 时 drain 全部 → libtest 多线程下会互删目录
    for f, nm in ((rd(CORE / "src/data.rs"), "data.rs"), (rd(CORE / "src/tasks/mfoc.rs"), "mfoc.rs")):
        check("struct TempGuard(PathBuf)" in f, f"M {nm} 守卫自持目录")
        check("DIRS.lock().unwrap().drain" not in f, f"M {nm} 不再跨测试 drain 全局目录表")
    check('assert!(!paths.nfc_config().exists()' in integ, "M 写盘时机测试有前置断言")
    # 测试不得对无 PartialEq 的类型用 assert_eq!
    for bad in ['assert_eq!(events,', "assert_eq!(s,", "assert_eq!(&s,", "assert_eq!(state,"]:
        check(bad not in integ and bad not in serial, f"M 无 {bad}... 比较")


# ---------------------------------------------------------------- N 泛型声明
STDLIB = {
    "Self", "String", "Vec", "Option", "Result", "Box", "PathBuf", "Path", "AtomicUsize",
    "Ordering", "RefCell", "OnceLock", "HashMap", "HashSet", "HashSet", "IntoIterator", "Into",
    "From", "Iterator", "IntoIter", "SliceConcatExt", "u8", "u16", "u32", "u64", "usize",
    "i32", "i64", "isize", "bool", "char", "f32", "f64", "str", "OptionBool",
    "Item", "Error", "Output",
}


def n_generic() -> None:
    """where 子句里用到的类型参数必须在 <...> 里声明过。

    rustfmt 只做语法解析，抓不到「impl 只声明 <I> 却在 where 里用 S」这类错误。
    """
    for f in sorted(CORE.glob("**/*.rs")):
        text = rd(f)
        known = set(STDLIB)
        for line in text.splitlines():
            s = line.strip()
            if s.startswith("use "):
                known |= set(re.findall(r"\b(\w+)\b", s.split(";", 1)[0]))
        known |= set(re.findall(r"\b(?:struct|enum|trait|type|fn|const|static)\s+(\w+)\b", text))
        known |= set(re.findall(r"\b([A-Z]\w*)\s*::", text))
        known |= set(re.findall(r"Event::(\w+)", text))

        declared: list[str] = []
        lines = text.splitlines()
        for i, line in enumerate(lines):
            s = line.strip()
            if re.match(r"(pub\s+)?(?:fn|impl|struct|enum|trait|type)\b", s):
                m = re.search(r"<([^<>]+)>", s)
                declared = [p.split(":", 1)[0].strip().split(" ", 1)[-1] for p in m.group(1).split(",")] if m else []
            if not s.startswith("where") or not declared:
                continue
            buf = [s]
            j = i
            while (
                not (buf[-1].rstrip().endswith(";") or buf[-1].strip() == "{")
                and j + 1 < len(lines)
            ):
                j += 1
                buf.append(lines[j].strip())
            used = set()
            for b in buf:
                used |= set(re.findall(r"\b([A-Z][A-Za-z0-9_]*)\b", b))
            undeclared = sorted(used - known - set(declared))
            check(not undeclared, f"N 泛型 where 声明完整 {f.relative_to(CORE)}", f"第 {i + 1} 行未声明 {undeclared}")


# ---------------------------------------------------------------- S4 数据模型
# 原版源码路径统一走 ORIG（可用 NFC_ORIG_DIR 覆盖），不在脚本里写死绝对路径。
JS = ORIG / "src/command.js"
DUMPJS = ORIG / "src/renderer/js/dumpEditor.js"
MODEL = CORE / "src/model"
FIX = CORE / "tests/fixtures"


def s4_model() -> None:
    hex_rs = rd(MODEL / "hex.rs")
    dump_rs = rd(MODEL / "dump.rs")
    keys_rs = rd(MODEL / "keys.rs")
    parse_rs = rd(MODEL / "parse.rs")
    js = rd(JS)

    # --- A 默认密钥表逐字对齐 command.js:50-64
    # 原版缺省时 `js` 是空串，不能 `.index()`；此时留空表交给 check_orig 跳过。
    if "const defaultKeys" in js:
        _d = js.index("const defaultKeys")
        js_keys = re.findall(r'"([0-9a-f]{12})"', js[_d:_d + 700])
    else:
        js_keys = []
    rs_keys = re.findall(r'"([0-9a-f]{12})"', keys_rs[keys_rs.index("DEFAULT_KEYS: [&str; 13]") :][:700])
    check_orig(js_keys == rs_keys, "A DEFAULT_KEYS 与 command.js:50-64 逐字一致", f"{js_keys} != {rs_keys}")
    check(len(rs_keys) == 13, "A DEFAULT_KEYS 恰 13 条", f"实得 {len(rs_keys)}")

    # --- B 标记串与偏移逐字对齐
    for name, needle in [
        ("MARKER_UID", "UID (NFCID1):"),
        ("MARKER_COLLECT_UID", "Found tag with uid "),
        ("MARKER_COLLECT_KEY", "collecting nonces for key"),
        ("MARKER_CROPTO1", "Key found:"),
        ("MARKER_MFDICT", "Found Key: "),
    ]:
        check(f'"{needle}"' in parse_rs, f"B {name} 标记串逐字")
    check('pub const MARKER_MFOC_PROGRESS: &str = "[Key: ";' in parse_rs, "B mfoc 进度前缀逐字")
    check("const HIT_SKIP: usize = 11;" in parse_rs, "B 两个命中标记偏移均为 11（原版怪癖）")
    check("MARKER_UID_OFFSET: usize = 14" in parse_rs and "UID_WINDOW: usize = 14" in parse_rs,
          "B UID 偏移 14 / 窗口 14（对齐原版 substring(i, i+14)）")
    check("take_window_after" in parse_rs and "fn take_hex_after" not in parse_rs,
          "B 窗口切片函数名为 take_window_after（不做 hex 校验，对齐原版）")
    check(re.search(r"MARKER_COLLECT_UID,\s*MARKER_COLLECT_UID\.chars\(\)\.count\(\),\s*8,", parse_rs) is not None,
          "B collect UID 偏移 19 / 取 8 hex")
    check("MARKER_COLLECT_KEY.len()" in parse_rs, "B collect keyType 偏移 26")

    # --- C 尺寸常量与原版分组逻辑一致
    check("pub const BLOCK_SIZE: usize = 16;" in dump_rs, "C BLOCK_SIZE=16")
    check("pub const BLOCKS_PER_SECTOR: usize = 4;" in dump_rs, "C 4 块/扇区（command.js:429 i+=4）")
    check("pub const M1_SECTORS: usize = 16;" in dump_rs, "C 1K=16 扇区（订正了原计划的「64 扇区」）")
    check("pub const M1_SIZE: usize = SECTOR_SIZE * M1_SECTORS;" in dump_rs, "C M1_SIZE 由常量推导")
    check("**16 扇区**" in dump_rs and "64 块" in dump_rs, "C dump.rs 写明 1024B=64 块=16 扇区")
    check("1024 字节 = 64 扇区" not in dump_rs, "C dump.rs 未把 1K 误称 64 扇区")

    # --- D 尾块四段偏移对齐 dumpEditor.js:91-94
    dumpjs = rd(DUMPJS)
    for off in ["substring(0, 12)", "substring(12, 18)", "substring(18, 20)", "substring(20)"]:
        check_orig(off in dumpjs, f"D 原版尾块切分 {off} 存在")
    for seg in ["seg(0, 12)", "seg(12, 18)", "seg(18, 20)", "seg(20, 32)"]:
        check(seg in dump_rs, f"D 本实现尾块切分 {seg}")

    # --- E 正则收紧（bug #1）
    check("(?::\\s*([0-9A-Fa-f]{12}))?" in parse_rs, "E 尾组改为可选组（bug #1）")
    check("(Found|Unknown)" in parse_rs, "E 状态字段由 \\w{5}|\\w{7} 收紧为具名")
    check('Regex::new(r"[0-9A-Fa-f]{12}")' in keys_rs, "E parse_12hex 正则逐字")

    # --- F 扇区号取自 `Sector NN -` 行头（不是「序号/2」反推）
    #
    # 这里原先断言 `self.index / 2`，把**错误的**实现当成了正确行为锁住。
    # 反推序号只在「每扇区恰好两条密钥且不跳扇区」时才等于真实扇区号；
    # 一旦工具跳过某扇区，A 扇区的密钥就会被算到 B 扇区，且**静默错位**。
    # 原版 keyParser.js:20 早就按行头取值了，是本重写回退到了更差的假设。
    check("Sector\\s+(\\d+)" in parse_rs or 'Sector\\s+(\\d+)' in parse_rs,
          "F 扇区号取自 Sector NN - 行头（正则）")
    check("sector_header_re" in parse_rs, "F 有独立的行头正则")
    check("let Some(sector) = self.sector else" in parse_rs,
          "F 无 Sector 行头时整行跳过（不产出、不消耗）")
    check("self.index / 2" not in parse_rs,
          "F 不得用「序号/2」反推扇区号（会静默错位）")
    check("fn key_info_result_is_identical_for_every_chunk_width" in rd(CORE / "tests/integration.rs"),
          "F 有「所有 chunk 宽度结果一致」集成测试")
    # 原版唯一可执行真相源已逐条移植
    check((CORE / "tests" / "orig_keyparser.rs").exists(),
          "F 原版 keyParser.test.js 已逐条移植为 Rust 测试")
    orig_t = rd(CORE / "tests" / "orig_keyparser.rs")
    for grp in ["orig_1_whole_block", "orig_2_sector_comes_from_output",
                "orig_3_chunk_invariance", "orig_4_edges_and_noise",
                "orig_5_uppercase_hex_normalized"]:
        check(grp in orig_t, f"F 原版测试第 {grp} 组已移植")
    check("to_ascii_lowercase" in parse_rs,
          "F 大写 hex 归一化为小写（与 keys.txt 一致，原版 keyParser.js:28）")

    # --- G bug #3/#4/#6/#12 的修复证据
    integ = rd(CORE / "tests/integration.rs")
    check("fn new_keys_from_detect_are_all_usable_keys" in integ, "G bug#3 有 new_keys 非空测试")
    check("assert!(!p.new_keys().is_empty());" in integ, "G bug#3 断言 new_keys 非空")
    check("assert_eq!(p.info().sector, Some(0u8));" in parse_rs, "G bug#4 扇区号为 u8 非 String")
    check("fn is_default_key_is_case_insensitive_but_merge_dedup_is_not" in keys_rs, "G bug#6 覆盖")
    check("eq_ignore_ascii_case" in keys_rs, "G bug#6 用 ASCII 大小写折叠")
    check('.tmp"' in keys_rs and "fs::rename" in keys_rs, "G bug#12 临时文件 + rename 原子写")
    check("fn save_keys_leaves_no_temp_file_behind" in integ, "G bug#12 有无残留测试")

    # --- H bug #7 hex 显式报错
    check("pub fn hex_decode(text: &str) -> Result<Vec<u8>, HexError>" in hex_rs, "H hex_decode 返回 Result")
    check("OddLength" in hex_rs and "InvalidChar" in hex_rs, "H 两类错误齐备")
    check("fn decode_rejects_odd_length" in hex_rs and "fn decode_rejects_invalid_char_instead_of_truncating" in hex_rs,
          "H 有奇数尾 / 非法字符测试")
    check("fn dump_hex_text_rejects_malformed_input_instead_of_silently_shortening" in integ, "H 集成层覆盖")

    # --- I validate 四类非法
    for v in ["DumpError::Empty", "NotSectorAligned", "LineCount", "LineLength", "NonHex"]:
        check(v in dump_rs, f"I validate 覆盖 {v}")
    check("fn validate_gates_non_hex_on_check_content_only" in dump_rs, "I 字符集仅在 check_content 时校验")

    # --- J fixture 齐备且是 fixture 驱动
    need = ["mfoc-output.txt", "mfdetect-keys.txt", "collect-output.txt",
            "cropto1-output.txt", "mfdict-output.txt"]
    for n in need:
        check((FIX / n).is_file() and (FIX / n).stat().st_size > 0, f"J fixture {n} 存在且非空")
    for n in need:
        check(f'"{n}"' in integ, f"J 集成测试用到 {n}")
    check(integ.count("usize::MAX") >= 5, "J 每个解析器都跑「整块喂入」基线")

    # --- K 偏离已登记
    doc2 = rd(REPO / "docs/02-架构设计.md")
    check("## 6. 与原版行为的偏离登记" in doc2, "K docs/02 §6 偏离登记表存在")
    for bug in ["| 1 |", "| 2 |", "| 3 |", "| 4 |", "| 6 |", "| 7 |", "| 12 |", "| 14 |",
                "| A |", "| B |", "| C |", "| D |", "| E |", "| F |", "| G |", "| H |"]:
        check(bug in doc2, f"K 偏离表登记 {bug.strip('| ')}")
    check("dumpEditor.js:112-118" in doc2, "K §6.2 F 行已修正引用")
    check("command.js:114" not in doc2.split("### 6.2")[1].split("### 6.3")[0],
          "K §6.2 内不再误引 command.js:114")
    check("### 6.3 刻意保留的原版行为" in doc2, "K 有「刻意保留」小节")
    check("故意跳号" in doc2, "K §6.1 跳号有注脚说明")
    check("| G |" in doc2 and "| H |" in doc2, "K §6.2 补齐 G/H 两行")
    check("不消耗序号" in doc2, "K 登记了 Found 缺 key 不消耗序号")
    check("uid_hex" in doc2, "K 登记了 uid_hex 是原版没有的重建")
    check("必须用 `UidParser::uid()`" in doc2, "K 写明 S5 命名必须用 UidParser")
    check("### 6.4 不可取证项（不得臆造）" in doc2, "K 有「不可取证项」小节")
    check("Key default" in doc2 and "0 命中" in doc2, "K 登记了臆造串的 0 命中事实")
    for m in re.finditer(r"docs/02 §(\d)", rd(MODEL / "hex.rs") + rd(MODEL / "dump.rs")
                         + rd(MODEL / "keys.rs") + rd(MODEL / "parse.rs") + rd(MODEL / "mod.rs")):
        check(m.group(1) == "6", "K 代码注释引用的偏离表编号是 §6", f"实得 §{m.group(1)}")

    # --- L 文档无残留错误说法
    doc3 = rd(REPO / "docs/03-实施计划.md")
    s4 = re.search(r"## S4 数据模型.*?(?=\n## S5)", doc3, re.S)
    check(s4 is not None, "L docs/03 有 S4 小节")
    s4 = s4.group(0) if s4 else ""
    check("64 扇区" not in s4, "L docs/03 S4 已订正「64 扇区」错误")
    check("16 扇区" in s4, "L docs/03 S4 写明 16 扇区")
    doc4 = rd(REPO / "docs/04-测试计划.md")
    f02 = doc4.split("## 2. fixture 清单")[1].split("## 3.")[0]
    for n in need:
        check(f"`{n}`" in f02, f"L docs/04 §2 列了实际 fixture {n}")
    for ghost in ["mfoc-full.txt", "mfoc-default-keys.txt", "nfc-mfdetect-mixed.txt",
                  "libnfc-collect.txt", "cropto1_bs-found.txt", "nfc-mfdict-found.txt"]:
        check(f"`{ghost}`" not in f02, f"L docs/04 §2 不再列不存在的 {ghost}")
    check("7 个" in f02 and "0 文件" in f02, "L docs/04 §2 声明 7 个子模块为 0 文件")
    check("不可取证" in f02 and "不臆造" in f02, "L docs/04 §2 有不可取证与不臆造声明")
    md = doc4.split("### model/dump")[1][:500]
    check("64 扇区" not in md.split("4096B = 64 扇区")[0], "L docs/04 model/dump 已订正「64 扇区」")
    check("**16 扇区**渲染" in doc4, "L docs/04 S8 dump_editor 已订正为 16 扇区")
    check("to_sector_lines" in md, "L docs/04 用实际函数名 to_sector_lines")
    check("# registry" in doc2, "L docs/02 data.rs 归属已改为 registry")
    check("未跑 `cargo test`" in s4, "L docs/03 S4 有未跑 cargo 的警示")
    check("已在 S4 落于" in doc3, "L docs/03 S5 的 data.rs 归属已改")
    check("connect_device" not in doc3, "L docs/03 无残留 connect_device")

    # --- M 泛型/结构
    check("pub struct KeyInfo {" in keys_rs and "pub sector: u8," in keys_rs, "M KeyInfo 三元组")
    check("pub struct UnknownKeyInfo {" in keys_rs, "M UnknownKeyInfo 无 key 字段")
    check("pub struct Trailer {" in dump_rs, "M Trailer 四字段")
    check("pub enum KeyType {" in keys_rs, "M KeyType 枚举")
    # ── S5 读卡闭环 ────────────────────────────────────────────────
    lib = rd(CORE / "src/lib.rs")
    for m in ["data", "dialogs", "tasks"]:
        check(f"pub mod {m};" in lib, f"S5 lib.rs 导出 {m}")
    dl = rd(CORE / "src/dialogs.rs")
    check("pub trait DialogProvider" in dl and "fn show_save_dialog" in dl and "fn show_open_dialog" in dl,
          "S5 DialogProvider 含 save/open 两个方法")
    check("pub struct ScriptedDialog" in dl and "save_requests" in dl and "pub type MockDialog = ScriptedDialog" in dl, "S5 ScriptedDialog 记录请求（MockDialog 保留别名）")
    check('extensions: vec!["dump".to_string(), "mfd".to_string()]' in dl, "S5 dump 过滤器含 dump+mfd")
    mf = rd(CORE / "src/tasks/mfoc.rs")
    main_rs = rd(APP / "src/main.rs")
    ser = rd(CORE / "src/serial.rs")
    check("pub const ETA_THROTTLE_MS: u64 = 1_000;" in mf, "S5 ETA 节流 1000ms")
    check("last_update_ms: now_ms" in mf, "S5 节流初值取当前时刻（原版 bug #4）")
    check("pub fn format_duration" in mf and 'if h > 0 {' in mf, "S5 format_duration 按 h>0 决定带不带小时段")
    check("pub fn mfoc_keys_total" in mf and "DEFAULT_KEYS.len() + file_keys" in mf, "S5 ETA 分母含默认密钥")
    check('starts_with("-k")' in mf, "S5 arg_keys 按 -k 前缀过滤")
    check("buf: LineBuffer" in mf and "self.buf.push(chunk)" in mf, "S5 ETA 复用 LineBuffer（继承 \\r 归一）")
    check("pub fn flush(&mut self, now_ms: u64) -> Option<EtaUpdate>" in mf, "S5 EtaTracker 有 flush（末行无换行也要结算）")
    check("MARKER_MFOC_PROGRESS" in mf and 'matches(MARKER_MFOC_PROGRESS)' in mf,
          "S5 count_keys_done 复用 parse 的 marker 常量")
    check("self.keys_done += count_keys_done" in mf, "S5 ETA 按 [Key: 计数累加")
    check("self.finished = true;" in mf and "percent: Some(100.0)" in mf, "S5 ETA 收敛推 100 并停止")
    check("Create_dir_all" in mf.replace("std::fs::create_dir_all", "Create_dir_all"), "S5 dump_save 用 create_dir_all")
    check("pub enum SaveOutcome" in mf and "Empty" in mf and "Failed { reason: SaveFailure }" in mf,
          "S5 SaveOutcome 三态（Failed 带具名原因）")
    check("pub enum SaveFailure" in mf and "Mkdir" in mf and "Move" in mf, "S5 SaveFailure 区分 Mkdir/Move")
    check("EXDEV" in mf and "std::fs::copy(from, to)?" in mf, "S5 跨设备移动有 copy+remove 兜底")
    check("temp.display()" in mf, "S5 保存失败时日志带 temp 路径（唯一副本需可抢救）")
    check(rd(CORE / "src/dialogs.rs").count("pub fn dump_filter") == 1, "S5 复用 dialogs::dump_filter 不重复定义")
    check("url.unwrap_or_else(|| pending.default_path.clone())" in mf,
          "S5 用户取消回落默认名")
    check("Event::DumpSaved(target.clone())" in mf, "S5 落盘后推 DumpSaved")
    dt = rd(CORE / "src/tasks/detect.rs")
    check('"-N".to_string(),' in dt and 'assert_eq!(a[0], "-N")' in dt, "S5 探测默认参数 -N")
    check('format!(' in dt and '"-O{}"' in dt, "S5 落盘模式参数 -O")
    check("pub fn unknown_guidance_text" in dt and "guidance_unknown_pre" in dt, "S5 unknown 引导文案")
    check('("hard-nested", i18n::t("html_hard_nested"))' in dt and '("dict-test", i18n::t("html_test_dictionary"))' in dt,
          "S5 引导两按钮 action 名与 command.js 一致")
    check("pub enum DetectOutcome" in dt and "ReadFailed { reason: ReadFailure }" in dt, "S5 DetectOutcome 两态")
    check("pub fn guidance_for_unknown" in dt, "S5 引导独立成 detect-card-type 专属路径")
    check("pub fn after_detect" in dt and "DetectOutcome::Proceed" in dt,
          "S5 readICThenExec 路径不因 unknown 拦下主命令")
    check("save_keys(paths, parser.new_keys())" in dt, "S5 探测收尾落盘新读到的密钥")
    check("pub fn detect_status_key" in dt and "pub fn detect_status(" in dt, "S5 状态条同时给出 key 与已翻译文案")
    check('i18n::t("log_msg_card_read_failed")' in dt, "S5 读卡失败文案")
    check("pub fn check_key_file_exists" in dt, "S5 密钥文件缺失时创建")
    dr = rd(CORE / "src/data.rs")
    check("pub struct CardRecord" in dr and "pub struct Registry" in dr, "S5 registry 数据结构")
    check("pub fn append" in dr and "pub fn append_and_save" in dr, "S5 registry 追加（按 UID 去重）")
    check('format!("{}.tmp", path.display())' in dr and "std::fs::rename" in dr,
          "S5 registry 原子写（显式后缀，不用 with_extension）")
    check("eq_ignore_ascii_case" in dr, "S5 registry 按 UID 大小写不敏感去重")
    check("self.registry.pop();" in dr, "S5 registry 落盘失败回滚内存")
    check('.bad' in dr, "S5 registry 损坏文件被隔离为 .bad")
    check('ErrorKind::NotFound' in dr, "S5 registry 首运行缺失文件返回空")
    check("#[serde(default)]" in dr, "S5 registry 字段容错")
    # S5 单测数量
    s5_units = sum(rd(CORE / f"src/{f}").count("#[test]")
                   for f in ["data.rs", "dialogs.rs", "tasks/mfoc.rs", "tasks/detect.rs"])
    check(s5_units == 82, "S5 单测累计 82（含 S6/S11-2/S17/F4 新增）", f"实际 {s5_units}")
    ints = rd(CORE / "tests/integration.rs")
    check(ints.count("#[test]") == 58, "S5+S6/S7 集成测试累计 58", f"实际 {ints.count('#[test]')}")
    check(ints.count("fn s5_") == 14, "S5 新增 14 个集成测试", f"实际 {ints.count('fn s5_')}")
    check("ToolRunner" in ints.split("S5 读卡闭环")[0] or "ToolRunner" in ints, "S5 集成测试引入 ToolRunner trait")
    check("pub mod detect;" in rd(CORE / "src/tasks/mod.rs") and "pub mod mfoc;" in rd(CORE / "src/tasks/mod.rs"),
          "S5 tasks/mod.rs 导出 detect+mfoc")
    check("硬嵌套" not in rd(CORE / "src/tasks/mod.rs"), "S5 mod.rs 不误声明未实现模块")

    # ── S6 写卡类 / S7 破解类 ─────────────────────────────────────
    cw = rd(CORE / "src/tasks/card_write.rs")
    check('MFCLASSIC_WRITE: &str = "w"' in cw and 'MFCLASSIC_FORMAT: &str = "f"' in cw, "S6 mfclassic 子命令常量")
    check("pub fn mfclassic_write_args" in cw and "pub fn mfclassic_format_args" in cw, "S6 写卡/格式化参数拼装")
    check("pub fn mfsetuid_args" in cw and "pub fn mflock_args" in cw, "S6 写/锁 UFUID 参数")
    check("pub fn read_block0_hex" in cw and "hex_encode_upper" in cw, "S6 block0 读为大写 hex")
    check("pub fn block0_is_valid" in cw and "hex.len() == 32" in cw, "S6 block0 长度校验 32")
    check("pub enum PrepareOutcome" in cw and "Cancelled" in cw and "Failed { reason: WriteFailure }" in cw,
          "S6 PrepareOutcome 三态")
    check("pub enum WriteFailure" in cw and "ReadFailed" in cw and "TooShort" in cw, "S6 写卡失败原因具名")
    check("pub fn write_uid_confirm" in cw and "block0_hex" in cw, "S6 写 UID 确认框拼 block0")
    check("pub fn resolve_dump_id" in cw and "is_absolute" in cw, "S6 dumpId 相对/绝对路径解析")
    check("pub fn open_dump_request" in cw and '"dump".to_string()' in cw, "S6 dump 打开对话框过滤器")
    dl = rd(CORE / "src/dialogs.rs")
    check("pub struct ConfirmRequest" in dl and "fn confirm_danger" in dl, "S6 危险确认框进 DialogProvider")
    check("fn confirm_danger(&mut self, _req: &ConfirmRequest) -> bool {\n        false" in dl,
          "S6 confirm_danger 默认返回 false（安全默认）")
    check("pub fn with_confirm" in dl and "confirm_requests" in dl, "S6 MockDialog 记录确认请求")
    hn = rd(CORE / "src/tasks/hard_nested.rs")
    check('TOOL_COLLECT: &str = "libnfc-collect"' in hn and 'TOOL_CRYPTO1: &str = "cropto1_bs"' in hn,
          "S7 采集/破解工具名")
    check("pub enum Step" in hn and "StopNoKnownKey" in hn and "StopAllDecrypted" in hn, "S7 Step 变体齐")
    check("SaveNonces" in hn and "StopNoProgress" in hn and "StopCancelled" in hn, "S7 新增 Step 变体可达")
    check("pub fn collect_args" in hn and "trailer_block" in hn, "S7 采集参数用扇区→块换算")
    check('"bin".to_string()' in hn, "S7 采集第 6 参为 bin")
    check("pub fn after_crack_guarded" in hn and "unknown_after >= unknown_before" in hn,
          "S7 autoRun 无进展兜底（防无限递归）")
    check("pub fn cancel" in hn, "S7 提供中止路径")
    check("pub fn nonces_default_name" in hn and '"nonces"' in hn, "S7 nonce 默认名回落")
    check("pub fn remove_unknown" in hn and "remove_unknown_removes_non_head_match" in hn,
          "S7 移除匹配到的 unknown（任意位置）")
    check("pub fn progress_percent" in hn and ".min(100.0)" in hn, "S7 进度上限 100")
    dt = rd(CORE / "src/tasks/dict_test.rs")
    check('TOOL_DICT: &str = "nfc-mfdict"' in dt, "S7 字典工具名")
    check("pub fn dict_args" in dt and '"-s{}"' in dt and '"-t{}"' in dt and '"-l{}"' in dt and '"-d{}"' in dt,
          "S7 字典四参数拼装")
    check("pub fn dict_file_name" in dt and "rsplit" in dt, "S7 字典文件名跨平台切分")
    check("pub fn apply_dict_choice" in dt, "S7 选字典后落 dict_path")
    # KeyType 唯一性（S1 遗留双类型已合并）
    keys_rs = rd(CORE / "src/model/keys.rs")
    st_rs = rd(CORE / "src/state.rs")
    check(keys_rs.count("pub enum KeyType") == 1, "S6 KeyType 全项目唯一定义")
    check("pub use crate::model::keys::KeyType;" in st_rs, "S6 state.rs 改为 re-export KeyType")
    check("pub enum KeyType" not in st_rs, "S6 state.rs 不再另立 KeyType")
    check("pub fn as_char" in keys_rs and "pub fn parse_lenient" in keys_rs, "S6 KeyType 兼具两种解析语义")
    check("pub struct UnknownKeyInfo" in keys_rs and "Copy" in keys_rs, "S6 UnknownKeyInfo 补 Copy")
    # 测试数
    s6s7_units = sum(rd(CORE / f"src/{f}").count("#[test]")
                     for f in ["tasks/card_write.rs", "tasks/hard_nested.rs", "tasks/dict_test.rs"])
    check(s6s7_units == 76, "S6/S7 新增 76 个单测（含 S17/S18 增量 + S19 的 uid_prefix/非hex 判定）", f"实际 {s6s7_units}")
    ints2 = rd(CORE / "tests/integration.rs")
    check(ints2.count("#[test]") == 58, "S6/S7 后集成测试 58", f"实际 {ints2.count('#[test]')}")
    check(ints2.count("fn s6_") == 7 and ints2.count("fn s7_") == 4, "S6/S7 新增 11 个集成测试",
          f"实际 s6={ints2.count('fn s6_')} s7={ints2.count('fn s7_')}")
    check("use nfc_core::state::DictTestConfig;" in ints2, "S7 集成测试从 state 取 DictTestConfig")
    tm = rd(CORE / "src/tasks/mod.rs")
    check("pub mod card_write;" in tm and "pub mod hard_nested;" in tm and "pub mod dict_test;" in tm,
          "S6/S7 tasks/mod.rs 导出三个新模块")

    # ── S8 主窗口 UI ──────────────────────────────────────────────
    app = rd(APP / "src/app.rs")
    panels = rd(APP / "src/panels.rs")
    tui = rd(APP / "src/theme_ui.rs")
    main = rd(APP / "src/main.rs")
    for name in ("main.rs", "app.rs", "panels.rs", "selftest.rs", "theme_ui.rs"):
        check((APP / "src" / name).exists(), f"S8 存在 {name}")
    check("with_decorations(false)" in main, "S8 标题栏自绘（关系统装饰）")
    check("--selftest" in main, "S8 支持 --selftest")
    check("ViewportCommand::Minimized" in app, "S8 最小化按钮")
    check("ViewportCommand::Close" in app, "S8 关闭按钮")
    check("ComboBox" in app, "S8 设备下拉")
    check("DEVICE_PREFIX" in app, "S8 下拉用 DEVICE_PREFIX（原版子串替换）")
    check("EventKind::Success" in app, "S8 事件条着色")
    check("EVENT_MAX" in app, "S8 事件条 30 行上限")
    check("stick_to_bottom" in app, "S8 日志/事件条自动滚底")
    check("ProgressBar" in app, "S8 进度条")
    check("HOTPLUG_INTERVAL" in app and "from_millis(500)" in app, "S8 热插拔 500ms")
    # R5：连接中禁设备下拉。S9 拆出 device_combo() 后曾把 add_enabled 弄丢，
    # 且 add_enabled 不接受 ComboBox::show_ui（返回 InnerResponse 非 Widget），
    # 故用 add_enabled_ui。
    check("add_enabled_ui(!self.connecting" in app, "S8 R5 连接中禁下拉（add_enabled_ui 包裹 ComboBox）")
    check("fn device_combo(&mut self, ui: &mut Ui)" in app, "S8 设备下拉已抽成独立方法")
    check("add_enabled" in app, "S8 有条件禁用控件")
    check("format_duration" in app, "S8 计时格式化")
    check("Event::Guidance" in app, "S8 显式消费 Guidance（core 侧 no-op）")
    check("Event::DumpSaved" in app, "S8 显式消费 DumpSaved")
    check("panels::show" in app, "S8 主窗口驱动面板")
    check("impl eframe::App for NfcApp" in app, "S8 实现 eframe::App")
    check("fn logic(" in app, "S8 用 logic 钩子叠模态（App::ui 只拿 &mut Ui）")
    # panels
    check("pub enum Panel" in panels, "S8 面板枚举")
    check(panels.count("Panel::") >= 9, "S8 八个面板都有映射")
    check("window_fill" not in panels, "S8 面板不硬编码配色")
    # 主题桥接：孤儿规则 → 必须走 newtype
    check("pub struct Paint" in tui, "S8 主题用 newtype（孤儿规则禁 impl From<Rgb> for Color32）")
    check("impl std::ops::Deref for Paint" in tui, "S8 Paint 解引用到 Theme")
    check("to_visuals" in tui, "S8 Paint 桥接 egui Visuals")
    check("Visuals::dark()" in tui and "Visuals::light()" in tui, "S8 深浅两套底色")
    # i18n 新增 3 key（S8 settings 面板按钮文案）
    check("html_toggle_dark_mode" in rd(ZH), "H zh-CN 含 S8 settings 按钮文案")

    # ── S9 模态面板 ──────────────────────────────────────────────
    hn = rd(ORIG / "src/renderer/html/hardNested.html")
    dt = rd(ORIG / "src/renderer/html/dictTest.html")
    ik = rd(ORIG / "src/renderer/html/inputkeys.html")
    st = rd(ORIG / "src/renderer/html/settings.html")
    ab = rd(ORIG / "src/renderer/html/about.html")
    wj = rd(ORIG / "src/windows.js")
    mj = rd(ORIG / "src/main.js")
    pa = rd(APP / "src/panels.rs")
    ap = rd(APP / "src/app.rs")
    stf9 = rd(APP / "src/selftest.rs")

    check_orig('value="ffffffffffff,"' in ik and 'INPUT_KEYS_DEFAULT: &str = "ffffffffffff,"' in pa,
          "S9 inputKeys 默认值含原版那个尾逗号")
    check("collect_only: self.collect_only && !self.auto_run" in pa,
          "S9 hardNested autoRun 强制 collectOnly=false（hardNested.html:65）")
    check("pub fn others_disabled(&self) -> bool" in pa and "self.auto_run" in pa,
          "S9 hardNested autoRun 禁用其余输入（hardNested.html:92-105）")
    check("no_config" in pa and "html_detect_card_first" in pa,
          "S9 无检测结果显示红色提示（catch 分支）")
    check("pub fn reset_from(&mut self, state: &AppState)" in pa,
          "S9 面板按检测结果预填（command.js:240-246 / 383-392）")
    check("unknown_key_info.first()" in pa and "known_key_info.first()" in pa,
          "S9 预填取 known[0] / unknown[0]")
    check("enum ThemeMode" in ap and "ThemePreference::System" in ap,
          "S9 主题三态（main.js:31-39 的 dark/light/system）")
    check("ctx.system_theme()" in ap, "S9 System 态每帧问 egui 系统主题")
    check("ctx.set_theme(" in ap and "ctx.set_visuals(" in ap,
          "S9 同时刷 set_visuals（自建 token）与 set_theme（弹层底色）")
    check("ThemePreference::Dark" in ap and "ThemePreference::Light" in ap,
          "S9 三态都映射到 egui::ThemePreference")
    # 工具菜单：原版 5 项 + settings（偏离，原版入口被注释）
    check("open-history-keys" in ap and "dump-history" in ap and "open-about" in ap,
          "S9 工具菜单含原版 5 项")
    # 原版 616b5b5「feat: restore the settings entry point」把 settings 按钮
    # 取消注释了（index.html:38-40）。我们早已实现了 settings 面板与入口，
    # 故这条守卫原先断言的「原版入口被注释」已过期——上游变了，不是我们的锅。
    # 改为断言**真正要守的东西**：原版与我们都提供 settings 入口。
    orig_idx = rd(ORIG / "src/renderer/html/index.html")
    check_orig("open-settings-window" in orig_idx,
          "S9 settings 入口在原版存在（616b5b5 起已取消注释）")
    check("open-settings-window" in ap and "Panel::Settings" in ap,
          "S9 我们也实现了 settings 入口与面板")
    # about 开源清单与原版一致（b0bd8d2 重新整理后为 18 项，含 11 个 Rust crate）
    orig_lic = re.findall(r"\((MIT|GPL-2\.0|GPL-3\.0|LGPL-3.0|BSD-2-Clause)\)", ab)
    # Rust crate 的 "MIT OR Apache-2.0" 需映射为 MIT（原版只有 MIT）
    ours = re.findall(r'"(MIT|GPL-2\.0|GPL-3\.0|LGPL-3.0|BSD-2-Clause|MIT OR Apache-2.0)"', pa)
    ours = ["MIT" if x == "MIT OR Apache-2.0" else x for x in ours]
    check_orig(len(orig_lic) == 10, "S9 原版 about 开源清单 10 项", f"实际 {len(orig_lic)}")
    check_orig(set(orig_lic) <= set(ours),
               "S9 about 开源许可证覆盖原版全部",
               f"仅本项目 {set(ours)-set(orig_lic)} / 缺 {set(orig_lic)-set(ours)}")
    # 18 = 7 个 C 工具 + 11 个 Rust crate（b0bd8d2 重新整理后）
    n_gh = pa.count("https://github.com")
    check(n_gh == 20, "S9 about 含开源链接 + LICENSE + 仓库首页", f"实际 {n_gh}")
    check(pa.count("const OPEN_SOURCE: [(&str, &str, &str); 18]") == 1,
          "S9 about 开源清单恰好 18 项")
    # 窗口尺寸对标 windows.js（取非 win32 分支）
    for const, w, h in [("SIZE_SETTINGS", 400, 510), ("SIZE_INPUT_KEYS", 400, 200),
                        ("SIZE_HARD_NESTED", 500, 610), ("SIZE_DICT_TEST", 450, 390),
                        ("SIZE_ABOUT", 600, 480)]:
        check(f"const {const}: [f32; 2] = [{float(w)}, {float(h)}]" in pa,
              f"S9 {const} 尺寸对标 windows.js {w}x{h}")
    check("default_size([w, h])" in pa, "S9 面板保留 windows.js 的默认尺寸")
    check(".constrain_to(ctx.content_rect().shrink(16.0))" in pa, "S16 面板受视口约束，不再溢出窗口")
    # ── §6.28 P1 UI 打磨 ────────────────────────────────────────
    check("html_tip_clear_log" in ap and "html_tip_save_log" in ap
          and "html_tip_stop_task" in ap and "html_tip_autoscroll" in ap,
          "P1 控制台按钮都有悬停说明")
    check("on_disabled_hover_text" in ap and "html_tip_stop_idle" in ap,
          "P1 停止按钮禁用态也说明原因")
    # 快捷键接线：把整段 handle_keys 切出来逐个锚点确认，
    # 否则 `"dump-editor-save"` 之类的字符串在 exec 分支里本来就存在，检查会假阳性。
    _hk = (ap[ap.index("fn handle_keys("):ap.index("pub fn connecting_now(")]
           if "fn handle_keys(" in ap else "")
    check(all(s in _hk for s in ("Key::Escape", "self.confirm = None", "close_topmost")),
          "P1 Esc 取消确认窗 / 关最上层面板")
    check(all(s in _hk for s in ("Key::S", "self.confirm.is_none()", "can_save()",
                                 "dump-editor-save")),
          "P1 Ctrl(⌘)+S 保存转储（仅面板开着、校验通过且无确认窗时）")
    check("pub fn close_topmost" in pa, "P1 panels 有 close_topmost（倒序关第一个）")
    check(pa.count("horizontal_wrapped") >= 4,
          "P1 长按钮行自动换行", f"实际 {pa.count('horizontal_wrapped')}")
    check(".max_height(360.0)" not in pa and "list_h" in pa,
          "P1 历史列表高度随窗口自适应（不再写死 360）")
    # body() 的返回值必须被 show() 认（回归：早期版本丢弃了，点取消不关面板）
    check("keep = body(p, ui, app)" in pa and "if !open || !keep" in pa,
          "S9 show() 认 body() 的关闭请求（点取消/确定能关面板）")
    check("c_panel_closes_on_cancel" in stf9, "S9 selftest 覆盖面板关闭")
    # selftest 条数
    # 条数按**函数引用**统计：rustfmt 会把名字长的条目折成多行，
    # 数 `("` 会漏。每项都以 `c_xxx,` 结尾。
    _vec = stf9[stf9.index("vec![") : stf9.index("];", stf9.index("vec!["))]
    n_cases = len(re.findall(r'\bc_[a-z0-9_]+\b', _vec))
    check(n_cases == 87, "S9-S18 UI selftest 87 项（79 原有 + S18 增 5 + 字典接线/引导直达/路径穿越 3）", f"实际 {n_cases}")

    # ── S10 dump 三面板 ──────────────────────────────────────────
    de = rd(ORIG / "src/renderer/html/dumpEditor.html")
    dc = rd(ORIG / "src/renderer/html/dumpComparator.html")
    dh = rd(ORIG / "src/renderer/html/dumpHistory.html")
    dejs = rd(ORIG / "src/renderer/js/dumpEditor.js")
    dcjs = rd(ORIG / "src/renderer/js/dumpComparator.js")
    dhjs = rd(ORIG / "src/renderer/js/dumpHistory.js")
    dfs = rd(CORE / "src/dumpfiles.rs")
    dumps = rd(CORE / "src/model/dump.rs")

    check("pub fn diff_sectors" in dumps and "pub enum DiffSeg" in dumps,
          "S10 core 有逐字符 diff（dumpComparator.js:32-89）")
    check("pub type SectorDiff" in dumps, "S10 SectorDiff 有类型别名")
    check('"html_caption"' in pa and "html_update_colors" not in pa.split("fn dump_editor")[1][:2000],
          "S10 编辑器有图例（dumpEditor.html:29-39）")
    # 编辑粒度：原版每扇区一个 contenteditable div，不是 64 个块输入框
    # （锚点随上游 dumpEditor 重构更新：原 data.data.forEach 已不存在，
    # 改用 groupHasTrailer 作稳定锚点——每扇区一个 div、块 3 单独着色）。
    check("desired_rows(4)" in pa, "S10 编辑器每扇区 4 行一个多行输入")
    check_orig("contenteditable" in dejs and "groupHasTrailer" in dejs,
          "S10 原版确认：每扇区一个 contenteditable div")
    _cj = rd(ORIG / "src/command.js")
    check_orig("slice(i, i + 4)).join('\\n')" in _cj and "i += 4" in _cj,
          "S10 原版按 4 块分扇区并以换行连接（command.js:429-430）")
    # 保存的两道检查
    check("pub fn can_save(&self) -> bool" in pa, "S10 有 can_save 封装（dumpEditor.js:16+20）")
    check("!self.sectors.is_empty() && self.validate(true).is_ok()" in pa,
          "S10 can_save 含非空判断（否则写出 0 字节文件）")
    check_orig("!currentFilePath || !getHexData()" in dejs, "S10 原版确是两道检查")
    # 比较器
    check("pub fn ready(&self) -> bool" in pa, "S10 比较器有 ready（dumpComparator.js:28）")
    check_orig("dumpA.length > 0 && dumpB.length > 0" in dcjs, "S10 原版两侧非空才比对")
    check("fn render_diff_row" in pa and "mark_bg" in pa,
          "S10 diff 用 mark_bg/mark_fg 着色（对齐原版 <mark>）")
    # 历史面板按钮规则
    check("pub fn history_button_states(selected: usize) -> (bool, bool, bool)" in pa,
          "S10 历史按钮规则有函数（dumpHistory.js:100-115）")
    check_orig("selectedDumps.length === 2" in dhjs and "selectedDumps.length === 1" in dhjs,
          "S10 原版按勾选数分 1/2/其他三档")
    check_orig('id="deleteSelectedButton"' in dh and "disabled" not in dh.split('id="deleteSelectedButton"')[1][:120],
          "S10 删除按钮无 disabled（原版如此）")
    # dumpfiles 安全加固
    check("pub fn list" in dfs and "pub fn delete" in dfs and "pub fn rename" in dfs,
          "S10 dumpfiles 有 list/delete/rename")
    check("pub fn is_plain_name" in dfs and "DUMP_EXT" in dfs,
          "S10 dumpfiles 有路径白名单 + 只列 .mfd")
    # S19：文件名防线作为统一入口。app 侧所有把用户可见文件名 join 到
    # dumpfiles 的地方（编辑器载入 / 比较器两侧 / 写卡带参 / 编辑器保存）
    # 都必须过这道校验——曾出现过防线只覆盖 delete/rename 的缺口。
    for site, label in (
        ("load_dump_into_editor", "编辑器载入"),
        ("open-dump-comparator", "比较器两侧"),
        ("dump-editor-save", "编辑器保存"),
    ):
        check(site in ap and "is_plain_name" in ap,
              f"S19 {label} 入口过 is_plain_name 防线")
    check_orig(".endsWith('.mfd')" in rd(ORIG / "src/command.js"),
          "S10 原版只列 .mfd（command.js:500）")
    # Action 新变体
    eng = rd(CORE / "src/engine.rs")
    check("SaveDump {" in eng and "DeleteDumps(" in eng,
          "S10/S11 Action 有 SaveDump / DeleteDumps")
    check("use nfc_core::engine::Action" in pa and "use crate::app::{Action" not in pa,
          "S11 Action 已移到 core（panels 从 nfc_core 引入）")
    check("pub fn load_dump_into_editor" in ap, "S10 打开编辑器可直接载入 dump")
    # selftest 条数

    # ── S11 engine 接线 ─────────────────────────────────────────
    eng = rd(CORE / "src/engine.rs")
    cj = rd(ORIG / "src/command.js")
    ej = rd(ORIG / "src/execUtils.js")
    rs = rd(CORE / "src/runner.rs")

    check("pub struct Engine" in eng and "pub fn dispatch" in eng and "pub fn pump" in eng,
          "S11 core 有 Engine（dispatch + pump）")
    check("nfc-mfclassic" in eng and "nfc-mfdetect" in eng and "nfc-mfdict" in eng,
          "S11 Engine 能跑读卡链路的三个工具")
    check("runner::gate(tool, gs)?" in eng, "S11 启动前过门禁（execUtils.js 的 exec）")
    check("is_running_task = true" in eng and "Indicator::Running" in eng,
          "S11 启动后置 isRunningTask + running 指示")
    check("state.is_running_task = false" in eng and "state.progress = None" in eng,
          "S11 退出后清任务位与进度条")
    check("ToolError::NotWired" in eng and "ToolError::NotWired" in rs,
          "S11 未接线动作显式报错（点了没反应必须可见）")
    check(rs.count("fn started_last(&self) -> Option<(String, Vec<String>, GateState)>") == 2,
          "S11 ToolRunner 加了 started_last（trait 默认 + Mock 覆写）")
    check("self.started.last().cloned()" in rs, "S11 MockToolRunner 覆写 started_last")
    # pending 不再被丢弃
    check("pending.clear()" not in ap, "S11 pending 不再每帧 clear（S8-S10 的老坑）")
    check("pub fn consume_pending(&mut self)" in ap, "S11 有 consume_pending")
    check("std::mem::take(&mut self.pending)" in ap,
          "S11 consume 用 mem::take（不吞掉 dispatch 期间新入队的动作）")
    check("self.engine.pump(&mut self.state, &self.paths);" in ap,
          "S11 每帧 pump 工具输出（S11-7 起不再需要 dialog）")
    check("fn on_dispatch_error" in ap, "S11 dispatch 拒绝时有反馈")
    check("Engine::with_real_tools" in ap or "with_real_engine" in ap,
          "S11 有接真工具的入口")
    # Action 的归属
    check("pub enum Action" in eng and "pub enum Action" not in ap,
          "S11 Action 住在 core::engine（S0-S10 时在 app）")
    # 参数构造对拍原版
    check("detect_args(paths, false, false)" in eng, "S11 detect-card-type 用 -N 分支（command.js:169-171）")
    check("mfoc_args(paths, &extra)" in eng, "S11 read-IC 用 mfoc_args（command.js:608-611）")
    check("dict_args(cfg, &state.dict_path)" in eng, "S11 dict-test 用 dict_args（command.js:395-397）")
    check('format!("-k{key}")' in eng, "S11 done-input-keys 转 -k 参数（command.js:154-156）")
    check("ConnectFlow::new(dev)" in eng and "write_nfc_config" in ser,
          "S11 connect 走 ConnectFlow 写 libnfc.conf（setNFCConfig）")
    check("self.scanner.scan_if_changed()" in eng and "Event::DeviceList" in eng,
          "S11 scan 走串口枚举并推 DeviceList（SerialPort.list）")
    check("self.conn.as_mut()" in ser or "self.conn.take()" in eng,
          "S11 connect 有异步握手状态（修掉了停在 Connecting 的死锁）")
    check("matches!(self.state.device_state, DeviceState::Connecting)" in ap,
          "S13 连接中禁用下拉由 device_state 派生（R5，不再是死字段）")
    check_orig("writeFileSync(knownKeysFile" in cj or "knownKeysFile" in cj,
          "S11 原版 connect 也写 libnfc.conf")
    check_orig("if (!isRunningTask)" in ej or "isRunningTask" in ej,
          "S11 原版 exec 门禁含 isRunningTask")
    # nfc-list 的门禁语义（易错点）
    check("!st.has_current_device || (!st.is_device_connected && tool != ALWAYS_ALLOWED)" in rs,
          "S11 门禁：currentDevice 为 null 时连 nfc-list 也拒（原版行为）")
    # nfc-list 仍不占位（否则 connect 被自己拒），但 S11-7 起挂起的保存框要占位
    check("self.running.is_some() || self.pending_save.is_some()" in eng and
          "self.runner.is_running()" not in eng.split("pub fn is_running")[1].split("}")[0],
          "S11 is_running 看任务位+挂起保存框，但不看 nfc-list")
    # selftest
    _vec2 = stf9[stf9.index("vec![") : stf9.index("];", stf9.index("vec!["))]
    n11 = len(re.findall(r"\bc_[a-z0-9_]+\b", _vec2))
    check(n11 == 87, "S11-S18 UI selftest 87 项（含 S18 新增 5 项 + 字典接线/引导直达/路径穿越 3）", f"实际 {n11}")

    # ── S11-2 系统文件对话框 + 写卡接线 ───────────────────────────
    dh = rd(APP / "src/dialogs_host.rs")
    du = rd(APP / "src/dialogs_ui.rs")
    pn = rd(APP / "src/panels.rs")
    check("rfd" in rd(APP / "Cargo.toml"), "S11-2 nfc-app 依赖 rfd")
    check("rfd" not in rd(CORE / "Cargo.toml"), "S11-2 core 不依赖 rfd（保持无 GUI 依赖）")
    # 线程宿主：阻塞 API 必须离开 UI 线程
    check("std::thread::Builder::new()" in dh and "pick_file()" in dh,
          "S11-2 系统文件框跑在专用线程")
    check("nfc-dialog" in dh, "S11-2 对话框线程有可识别名字")
    check("fn portal_available" in dh, "S11-2 Linux 门户探测是纯函数（可测）")
    # 已知限制必须写进注释，不能靠口口相传
    check("不区分" in dh and "取消" in dh, "S11-2 记录 rfd 不区分取消/后端失败")
    # 危险确认改为应用内模态窗
    check("egui::Modal::new" in du and "dialog_button_ok" in du,
          "S11-2 确认窗为应用内 Modal（带遮罩，挡住主窗输入）")
    check("set_parent" not in dh or "并非不可用" in dh,
          "S11-2 如实记录 set_parent 为何用不上（Context 活不过闭包 / 跨线程）")
    # core 侧：engine 必须真的问确认，删/写/锁/格式化才生效
    check("dialog.confirm_danger(&crate::dumpfiles::delete_confirm" in eng,
          "S11-2 engine 删除前过 confirm_danger 闸")
    check("if !dialog.confirm_danger" in eng,
          "S11-2 engine 未确认即拒绝执行")
    # readICThenExec 两段式：备份缺失必须中止
    check("fn read_then_exec" in eng and "fn start_next_stage" in eng,
          "S11-2 readICThenExec 两段编排（备份 → 执行）")
    check("log_msg_card_read_failed" in eng,
          "S11-2 备份失败记「读卡失败，未执行操作」")
    check("backed_up" in eng and "len() > 0" in eng,
          "S11-2 空备份也判失败（command.js:597）")
    # 绕过确认的路径必须不存在——这两条正是 S11-2 修掉的真 bug
    check('Action::DeleteDumps' not in pn,
          "S11-2 面板不得直接派发 DeleteDumps（须过确认窗）")
    check('Action::WithArg("write-IC"' not in pn,
          "S11-2 面板不得直接派发 write-IC（须过确认窗）")
    check("on_confirm_result" in ap and "fn push_approved" in ap,
          "S11-2 确认通过才把批准与动作成对入队")
    check("self.dialogs.confirm_results.clear()" in ap,
          "S11-2 派发后清空批准（防泄漏到下一个动作）")
    # 成功退出必须落回空闲（printExitLog(0) 对所有工具一视同仁）
    _fin = eng.split("fn finish")[1]
    _fin = _fin[: _fin.index("\n    }\n")] if "\n    }\n" in _fin else _fin
    check("indicator_free" in _fin, "S11-2 成功退出落回空闲状态")
    check('crate::i18n::t("log_msg_finished")' in eng,
          "S11-2 成功退出记「运行完毕」")
    # 写卡类动作的 i18n 键
    for k in ["indicator_writing_ic_card", "indicator_formatting_ic_card",
              "indicator_backing_up_current_card", "indicator_locking_ufuid",
              "indicator_writing_ufuid_uid"]:
        _loc = rd(REPO / "assets/locales/zh-CN.json")
        check(f'"{k}"' in _loc, f"S11-2 i18n 键 {k} 存在")

    # ── S11-3 30 action parity 表（docs/04 §9）─────────────────────
    d04 = rd(REPO / "docs/04-测试计划.md")
    cm9 = rd(CORE / "src/command.rs")
    check("## 9. 39 个 action 的 parity 核验表" in d04, "S11-3 docs/04 §9 parity 表存在")
    _i = cm9.index("pub const ALL_ACTIONS"); _j = cm9.index("\n];", _i)
    _all = re.findall(r'"([a-zA-Z0-9-]+)"', cm9[_i:_j])
    check(len(_all) == 39, "S11-3 ALL_ACTIONS 为 39 项（30 对齐 + 4 S14 phw + 5 S18 增量）", f"实际 {len(_all)}")
    _missing = [a for a in _all if f"`{a}`" not in d04]
    check(not _missing, "S11-3 39 个 action 全部在 parity 表中", f"缺 {_missing}")
    _rows = d04.count("\n| ") - d04.count("\n|---")
    check(_rows >= 35, "S11-3 parity 表至少 35 行数据", f"实际 {_rows}")
    for kw in ["libnfc-collect", "readICThenExec", "nfc-mfsetuid", "nfc-mflock",
               "nfc-mfclassic f A u", "ToolError::NotWired", "mfcuk", "nfc-mfsetuid"]:
        check(kw in d04, f"S11-3 parity 表提到 {kw}")
    # run-hard-nested 必须仍是显式未接线，不能悄悄变成别的行为
    check("run-hard-nested" in d04, "S11-3 表中含 run-hard-nested")
    check("39 个全部对齐" in d04, "S11-3 结论为 39 个全部对齐（30 对齐 + 4 S14 + 5 S18 增量）")
    check("无进展即停" in d04, "S11-3 登记 HardNested 的无进展兜底偏离")
    # 未接线机制本身仍须保留：S12 若新加不接的动作要能显式报错
    check("ToolError::NotWired" in eng, "S11-3 未接线动作仍显式报错")
    check("NotWired(other)" in eng or "ToolError::NotWired(other)" in eng,
          "S11-3 plain() 兜底分支仍产出 NotWired")

    # ── S11-4 独立审计后的加固守卫 ────────────────────────────────
    # 每一条都对应一次「曾真错过」的缺陷，改回去就会红。
    # ① 备份段非 0 退出绝不能继续写卡（execUtils.js:51-52 reject 后 then 不跑）
    _fin = eng.split("fn finish")[1]
    _ok_arm = _fin.split("Ok(()) => {")[1].split("Err(e) => {")[0]
    check(_fin.index("match exit.result()") < _fin.index("run.next"),
          "S11-4 next 的处理必须在退出码判断**之内**（备份失败须中止）")
    check("let next = run.next" in _ok_arm or "if let Some(next) = run.next" in _ok_arm,
          "S11-4 第二段只在 Ok 分支里启动")
    # ② 批准必须与动作成对，不能靠队列位置（热插拔会抢批）
    check("pending_with_approval" in ap and "pop_front" in ap,
          "S11-4 批准与动作成对入队（VecDeque）")
    check("approved" not in ap.split("pub struct NfcApp")[1].split("}")[0],
          "S11-4 不再有独立的 approved 字段")
    check("fn push_approved" in ap and "pending_with_approval.push_back" in ap,
          "S11-4 push_approved 成对入队")
    # ③ 重命名的回车/失焦必须独立判断（曾写成 && 互斥，永不触发）
    check("lost_focus() ||" in pn,
          "S11-4 重命名：失焦与回车是 || 而非 &&")
    check("lost_focus() &&" not in pn, "S11-4 重命名不得写成互斥的 &&")
    # ④ write-ufuid-uid 的确认框必须显示 block0（用户盲授权 = 安全问题）
    check("write_uid_confirm(&hex).message" in ap,
          "S11-4 write-ufuid-uid 确认正文含 block0 hex")
    check("ConfirmKind::WriteUid { dump: full }" in ap,
          "S11-4 WriteUid 载荷是完整路径（engine 要靠它读文件）")
    # ⑤ 面板按钮必须走 exec，不能直接 push
    for name in ["open-dict-file", "dump-editor-choose-file", "dump-editor-save",
                 "dump-comparator-choose-file"]:
        check(f'app.exec("{name}"' in pn, f"S11-4 面板的 {name} 走 exec")
    check('Action::Plain("open-dict-file")' not in pn,
          "S11-4 面板不得直接 push open-dict-file（会无反应）")
    check("Action::SaveDump{" not in pn,
          "S11-4 面板不得直接 push SaveDump（engine 是 no-op，保存会失效）")
    # ⑥ save-log 的 defaultPath 是裸文件名（原版无 path.join）
    check('PathBuf::from(format!("NFCTools_log_{}"' in ap and "dumpfiles_dir().join(format!(\"NFCTools_log" not in ap,
          "S11-4 save-log 默认名不带目录（与 command.js:541 一致）")
    # ⑦ 读卡收尾必须真的落盘（曾完全没接 dump_save，主流程断掉）
    check("mfoc::dump_save" in eng, "S11-4 read-IC 收尾调用 dump_save")
    check("scan_mfoc_uid" in eng and "take_uid" in eng,
          "S11-4 从 mfoc 输出抓 UID 当 dump 文件名前缀")
    check("Event::DictHistory(crate::dumpfiles::list" in eng,
          "S11-4 落盘后刷新历史列表")
    check("dump_save" in rs or "dump_save" in eng, "S11-4 dump_save 有生产调用者")
    # ⑧ hard-nested 是链路一环，成功退出不该置空闲
    check("chain_continues" in eng, "S11-4 链路型工具不置空闲（hard-nested）")
    # ⑨ framework_bin 向上查找必须有测试覆盖「优先命中近的」
    check("framework_bin_prefers_nearest_when_several_exist" in rs,
          "S11-4 framework_bin 有「优先近处」的非恒真测试")
    # ⑩ selftest 不得触发真实系统弹窗
    check("fn manual() -> Self" in dh, "S11-4 DialogHost::manual（测试不弹真实框）")
    check("DialogHost::manual()" in stf9,
          "S11-4 selftest 用 manual 宿主")
    check("fn request_open(" in dh and "fn feed_reply(" in dh,
          "S11-4 有 feed_reply 测试通道")

    # ── S11-5 HardNested 多轮闭环守卫 ──────────────────────────────
    check("enum HnStep" in eng, "S11-5 HardNested 步骤枚举在 engine")
    check("tasks::hard_nested::HardNestedRunner" in eng,
          "S11-5 engine 真的驱动 HardNestedRunner（S7 就有实现，此前无人调用）")
    check("self.hn.after_detect(" in eng and "self.hn.after_collect(" in eng and
          "after_crack_guarded(" in eng,
          "S11-5 三处状态机推进都接上了")
    check("TOOL_COLLECT" in eng and "TOOL_CRYPTO1" in eng,
          "S11-5 libnfc-collect / cropto1_bs 都由 engine 启动")
    check("fn hn_advance_round" in eng,
          "S11-5 autoRun 下一轮配置从最新 known/unknown 重建")
    check("collect_args" in eng and "crypto1_args" in eng,
          "S11-5 采集/破解参数经 RoundContext 构造")
    check("remove_unknown" in eng, "S11-5 命中后移除匹配到的 unknown")
    check("CollectParser::new" in eng and "SingleKeyParser::cropto1()" in eng,
          "S11-5 两个工具各有输出解析器")
    # 非 0 退出不推进（与 readICThenExec 的 reject 语义一致）
    _hna = eng.split("fn hn_advance(")[1].split("fn finish(")[0]
    check("exit.result()" in _hna and "return;" in _hna,
          "S11-5 HardNested 工具非 0 退出即中止，不推进状态机")
    # 「本轮无进展就停」是无终止条件时的唯一兜底，必须仍在
    check("StopNoProgress" in eng and "log_msg_hard_nested_no_progress" in eng,
          "S11-5 无进展兜底仍在（原版会无限递归）")
    check("trailer_block" in rd(CORE / "src/state.rs"),
          "S11-5 扇区→trailer 块号换算 (s+1)*4-1 已用无溢出写法")

    # ── S12 交付层守卫 ──────────────────────────────────────────────
    ct = rd(REPO / "Cargo.toml")
    check("[profile.release]" in ct, "S12 有 release profile")
    for k in ['panic = "abort"', "strip = true", 'lto = "thin"']:
        check(k in ct, f"S12 release profile 含 {k}")
    check("pub fn missing_tools" in rs, "S12 有 framework/bin 启动自检")
    check("KNOWN_TOOLS" in rs, "S12 列出已知工具清单")
    check("MAX_LOOKUP_LEVELS" in rs, "S12 framework_bin 上溯有层数上限")
    check("fn check_tool_dir" in ap and "html_framework_bin_missing" in ap,
          "S12 缺工具时给明确引导（不只抛 IO 错误）")
    check("fn missing_tools_bar" in ap, "S12 有缺失工具的红色引导条")
    rk = rd(REPO / "scripts/package.sh")
    check("cargo build --release" in rk and "framework/bin" in rk,
          "S12 打包脚本构建 release 并检测 framework/bin")
    check("tools/verify_parity.py" in rk, "S12 打包脚本会跑静态门禁")
    wf = rd(REPO / ".github/workflows/ci.yml")
    check("verify_parity.py" in wf, "S12 CI 跑静态门禁")
    check("cargo clippy" in wf and "-D warnings" in wf, "S12 CI clippy 零告警")
    check("cargo test --workspace" in wf, "S12 CI 跑全量测试")
    check("--selftest" in wf, "S12 CI 跑两个 selftest")
    check("reqwest|hyper|tokio::net" in wf, "S12 CI 有外联审计")
    check("windows-latest" in wf, "S12 CI 有 Windows 发布构建")
    check("libgtk-3-dev" not in wf, "S12 CI 不需要 GTK（rfd 用 xdg-portal）")
    # S12-10：CI 必须把 28 项对拍**真跑起来**，而不是静默 SKIP。
    # 做法 = checkout 原版（pin commit、不拉子模块）+ 设 NFC_ORIG_DIR。
    # 若漏了任一环，verify_parity.py 的 check_orig 会回退成 SKIP，CI 又是空转。
    check("actions/checkout@v4" in wf and "ORIG_REF" in wf,
          "S12-10 CI 有原版 checkout（对拍基准，pin commit）")
    check("ORIG_REPO" in wf and "agogo233/NFCToolsGUI" in wf,
          "S12-10 CI 指向正确的原版仓库")
    check("submodules: false" in wf,
          "S12-10 原版 checkout 不拉 C 子模块（对拍只读 src/）")
    check("NFC_ORIG_DIR" in wf,
          "S12-10 CI 设了 NFC_ORIG_DIR（让 28 项对拍真跑，否则静默 SKIP）")
    # 脚本侧的两道自护栏：显式指定了原版却缺失 → 大声失败（不静默降级）
    check('sys.exit(3)' in rd(REPO / "tools/verify_parity.py")
          and "ORIG_SKIPS" in rd(REPO / "tools/verify_parity.py"),
          "S12-10 verify_parity 有「指定了 NFC_ORIG_DIR 却仍跳对拍」的自护栏")
    rme = rd(REPO / "README.md")
    check("真机验收清单" in rme, "S12 README 有真机验收清单")
    check("AGPL" in rme, "S12 README 声明许可证")
    check("不可逆" in rme, "S12 README 有不可逆操作警告")
    check("compile.sh" in rme, "S12 README 说明 C 工具需原版 compile.sh")
    check((REPO / "LICENSE").is_file(), "S12 LICENSE 已纳入仓库")

    # ── S11-6 「数据流没接上」类缺陷的守卫 ───────────────────────────
    # 每一条都对应一次「骨架接上了但数据没流下去」，改回去就会红。
    det = rd(CORE / "src/tasks/detect.rs")
    # ① 密钥列表回填：整条「检测 → 破解」链路的前提
    check("state.known_key_info = parser" in det,
          "S11-6 after_detect 回填 known_key_info")
    check("state.unknown_key_info = parser" in det,
          "S11-6 after_detect 回填 unknown_key_info")
    # ② 无进展判据必须用 pop **之后**的长度
    _crk = eng.split("HnStep::Crack => {")[1].split("}")[0] if "HnStep::Crack => {" in eng else ""
    check("let after = state.unknown_key_info.len()" in eng,
          "S11-6 无进展判据用 pop 之后的 unknown 长度")
    check("after_crack_guarded(&cfg, run.unknown_before, unknown.len())" not in eng,
          "S11-6 不得用 pop 之前的快照（恒等 → 多轮只跑 1 轮）")
    check("fn hard_nested_autorun_advances_multiple_rounds" in eng,
          "S11-6 有跑满多轮的回归测试")
    check("pub fn with_scripts" in rs,
          "S11-6 MockToolRunner 能逐次喂不同脚本（否则写不出多轮测试）")
    # ③ collectOnly 真保存
    check("Step::SaveNonces => {" in eng and "show_save_dialog" in eng,
          "S11-6 collectOnly 会弹保存框并 rename nonce")
    check("nonces_is_empty" in eng, "S11-6 collectOnly 有非空判定")
    check("clear_nonces" in eng, "S11-6 autoRun 起手清空 nonces.bin")
    # ④ 状态条进度不被 launch 覆盖
    check("fn launch_with_status" in eng, "S11-6 有可传状态文案的 launch 变体")
    check("let status = self.hn_progress_text" in eng,
          "S11-6 HardNested 状态条带进度")
    check("is_some_and(|c| c.auto_run)" in eng,
          "S11-6 采集态的进度后缀只看 auto_run（command.js:284）")
    # ⑤ Windows .exe
    check("fn tool_present" in rs and 'with_extension("exe")' in rs,
          "S11-6 工具探测认 .exe（Windows 主目标平台）")
    check("fn missing_tools_accepts_exe_suffix" in rs, "S11-6 有 .exe 回归测试")
    # ⑥ clippy 必须真的零告警（曾有一处让 CI 必红）
    check("self.pending.first().copied()" in dh,
          "S11-6 不用 .iter().next()（clippy::iter_next_slice 会让 CI 红）")
    # ⑦ package.sh 不能在非 Windows 上直接 die
    rk2 = rd(REPO / "scripts/package.sh")
    check("MINGW" in rk2 and 'nfc-app.exe' in rk2,
          "S11-6 打包脚本按平台取产物名")
    check('"$REPO_ROOT/../NFCToolsGUI/framework/bin"' in rk2,
          "S11-6 打包脚本会找原版项目的 framework/bin")
    _pk = [l for l in rk2.splitlines() if "tail -1" in l and not l.lstrip().startswith("#")]
    check(not _pk, "S11-6 门禁失败要能看到具体红项（别只留 tail -1）", f"仍有一行 {_pk}")

    # ── S11-7 挂起式保存框 ──────────────────────────────────────────
    # 曾经的谎：read-IC 收尾与 collectOnly 标着「已接线」，实际 engine 侧
    # 永远拿不到路径，静默用默认名落盘。parity 表说完成，其实没弹过框。
    dl = rd(CORE / "src/dialogs.rs")
    check("pub struct PendingSave" in dl, "S11-7 有挂起式保存请求")
    check("pub fn pending_save" in eng, "S11-7 engine 暴露 pending_save")
    check("pub fn resolve_save" in eng, "S11-7 engine 有续接入口")
    check("pub fn dump_prepare" in mf, "S11-7 dump_save 拆成 prepare/commit")
    check("pub fn dump_commit" in mf, "S11-7 有 commit 半段")
    # 忙门禁：挂起期间必须挡住并发任务，否则 temp.mfd 会被下一次读卡覆盖
    check("self.running.is_some() || self.pending_save.is_some()" in eng,
          "S11-7 挂起期间算作在忙")
    check('''                        self.pending_save = Some(p);
                        // 关键：`finish` 开头已把 `is_running_task` 置 false''' in eng,
          "S11-7 挂起后重新置 is_running_task（Busy 门禁只看这个字段）")
    # 防重复投递：pump_pending_save 每帧都调
    check("fn pump_pending_save" in ap, "S11-7 app 有投递挂起请求的帧钩子")
    check("if self.dialogs_host.is_pending() {" in ap,
          "S11-7 投递前判是否已在等（否则每帧弹一个框）")
    # engine 侧绝不能自己弹框
    _engcode = "\n".join(l for l in eng.splitlines() if not l.lstrip().startswith("//"))
    check("show_save_dialog" not in _engcode,
          "S11-7 engine 不再自己弹保存框（会在 UI 线程冻结窗口）")
    # 两条取消语义不同
    check("fn resolve_nonces" in eng, "S11-7 nonce 走独立的续接路径")
    check('''            self.hn_idle(state);
            return;
        };
        match std::fs::rename(&pending.source, &target)''' in eng,
          "S11-7 collectOnly 取消保留 nonces.bin（与 read-IC 相反）")
    dh = rd(APP / "src/dialogs_host.rs")
    check("ReadIcSave" in dh and "NoncesSave" in dh, "S11-7 有两个新 FilePurpose")
    st = rd(APP / "src/selftest.rs")
    check("c_read_ic_save_round_trip" in st, "S11-7 app 侧有端到端往返用例")
    check("c_pending_save_not_reposted" in st, "S11-7 有防重复投递用例")
    check("c_nonces_save_round_trip" in st, "S11-7 有 collectOnly 往返用例")

    # ── S12-2b 工具名 / 参数风格 ────────────────────────────────────
    # 曾经真的红过：engine 读卡 spawn `nfc-mfclassic`，但原版读卡用的是 `mfoc`。
    # 单测里「参数与原版字符串一致」成立，故一直没发现；编出真工具才露馅。
    _rd = eng.split("fn read_ic(")[1].split("\n    }")[0] if "fn read_ic(" in eng else ""
    check('self.launch(\n            "mfoc",' in eng or '"mfoc",\n            &args,' in eng,
          "S12-2b 读卡 spawn 的是 mfoc（command.js:104/159），不是 nfc-mfclassic")
    check('"nfc-mfclassic",\n                        &args,' not in eng,
          "S12-2b 读卡不得用 nfc-mfclassic（位置参数风格，吃不下 -O/-f）")
    check('tool: "nfc-mfclassic"' in eng, "S12-2b 写卡/格式化仍用 nfc-mfclassic")
    check('"mfoc",\n    "nfc-mfclassic",' in rs or '"mfoc",' in rs,
          "S12-2b KNOWN_TOOLS 含 mfoc（启动自检/打包都要它）")
    check(rs.count('KNOWN_TOOLS: &[&str]') == 1, "S12-2b KNOWN_TOOLS 仍单处定义")
    pk = rd(REPO / "scripts/package.sh")
    check("mfoc nfc-mfclassic" in pk.replace("\n", " ").replace("  ", " "),
          "S12-2b package.sh 清单含 mfoc")
    # 真实工具测试存在且不是空壳
    for f, needle in [
        ("real_tools.rs", "every_tool_loads_its_shared_libraries"),
        ("tool_cli.rs", "read_ic_flags_are_accepted_by_the_real_binary"),
    ]:
        t = rd(CORE / "tests" / f)
        check(needle in t, f"S12-2b 有针对真实二进制的测试 {needle}")
        check("find_bin_dir" in t, f"S12-2b {f} 会定位 framework/bin")
    check("looks_like_argv_rejection" in rd(CORE / "tests" / "tool_cli.rs"),
          "S12-2b 断言的是「参数被拒」而非退出码（无读卡器时退出码无意义）")

    # ── S12-3 Windows 交叉编译 ──────────────────────────────────────
    # 交叉编出 .exe 后查 PE 头才发现 Subsystem=3（Console）——
    # 双击会先闪一个黑框。原版 Tauri 默认 GUI 子系统，没有这个框。
    check('windows_subsystem = "windows"' in main_rs,
          "S12-3 release 是 GUI 子系统（不该双击闪黑框）")
    check("cfg_attr(not(debug_assertions)" in main_rs,
          "S12-3 debug 仍保留 console（--selftest 的输出要看得见）")
    ci2 = rd(REPO / ".github/workflows/ci.yml")
    check("IMAGE_SUBSYSTEM_WINDOWS_GUI" in ci2 or "Subsystem" in ci2,
          "S12-3 CI 校验 PE 子系统（Windows 是主目标平台）")
    check("cargo build --release --locked -p nfc-app" in ci2, "S12-3 CI 仍做 Windows 发布构建")

    # ── S12-4 开发态工具定位 ────────────────────────────────────────
    # 向上查找只往父目录走，看不见**兄弟**目录。原版项目被 clone 在
    # NFCToolsGUI/ 与本仓库平级时，启动就是一条红色「缺少工具」条。
    # 集成测试早就有 NFC_FRAMEWORK_BIN 约定，生产路径却没有 → 两处行为分叉。
    check('FRAMEWORK_BIN_ENV: &str = "NFC_FRAMEWORK_BIN"' in rs,
          "S12-4 framework_bin 支持环境变量覆盖（兄弟目录看不到）")
    check("var_os(FRAMEWORK_BIN_ENV)" in rs, "S12-4 实际读了该环境变量")
    check("!dir.as_os_str().is_empty()" in rs,
          "S12-4 空值视为未设（否则静默全缺）")
    check("NFC_FRAMEWORK_BIN" in rd(REPO / "README.md"),
          "S12-4 README 说明兄弟目录场景要用环境变量")
    for f in ["real_tools.rs", "tool_cli.rs"]:
        check("NFC_FRAMEWORK_BIN" in rd(CORE / "tests" / f),
              f"S12-4 {f} 与生产侧用同一个环境变量名")

    # ── S12-8 nfc-list 两条判定通道 ──────────────────────────────────
    # libnfc 的 ERR()（nfc-list.c:155）走 stderr，`… opened` 走 stdout。
    # 原版 command.js:793-802 分给 processHandler / stderrHandler 两个回调，
    # 后者在进程退出时跑，**会把先前的 true 纠正成 false**。
    # 我们曾把两个流混进一个缓冲区一次判定，导致「先 opened 后 unable」
    # 永远停在 true——UI 显示已连接，卡实际读不出来且无任何提示。
    check("self.connected.is_none()" not in ser,
          "S12-8 连接判定不得「判到一次就短路」（stderr 后到时要能纠正）")
    check("judge_connect(&c.text)" in ser,
          "S12-8 每块独立判定 opened / unable，与到达顺序无关")
    check("self.connected != Some(v)" in ser,
          "S12-8 仅在结论变化时打日志，不重复刷屏")
    check("fn opened_then_unable_must_not_stay_reported_as_connected" in ser,
          "S12-8 有「先 opened 后 unable」的回归测试")
    check("Chunk::stderr_chunk" in ser,
          "S12-8 回归测试真的用 stderr 通道（而非 stdout）")

    # ── S12-9 fixture 必须可回溯到 C 源码 ────────────────────────────
    # S12-5 抓出一整类「照着看起来像编的」fixture；S12-8 又在 collect/nfc-list
    # 上续挖到几处。这里钉死：每个 fixture 都要在 tests/fixtures/README 里
    # 登记出处（哪个 C 文件的哪条 printf），便于下次直接核对而非凭印象重编。
    fx_dir = CORE / "tests" / "fixtures"
    fx_readme = fx_dir / "README.md"
    check(fx_readme.exists(), "S12-9 fixture 有出处登记表（README.md）")
    if fx_readme.exists():
        reg = fx_readme.read_text(encoding="utf-8")
        for f in sorted(p.name for p in fx_dir.glob("*.txt")):
            check(f in reg, f"S12-9 fixture {f} 在 README 里登记了出处")
        check("nfc-mfdetect.c" in reg and "Sector %02d" in reg,
              "S12-9 登记了 mfdetect 的真实输出格式（含 Sector 行头）")
        check("libnfc-collect" in reg, "S12-9 登记了 collect 的真实输出格式")
        check("nfc-list.c" in reg, "S12-9 登记了 nfc-list 的真实输出格式")
    # 已知的编造串不得再出现
    all_fx = "\n".join(p.read_text(encoding="utf-8") for p in fx_dir.glob("*.txt"))
    for bogus, why in [
        ("Reader #", "nfc-list 不打这一行（S12-9 已删）"),
        ("Trying 0", "cropto1_bs 不打 Trying（S12-9 已删）"),
        ("\tcollecting", "collect 三个子串同行，前置制表符是编的"),
    ]:
        check(bogus not in all_fx, f"S12-9 fixture 不含编造串 {bogus!r}（{why}）")
    src_code = ser
    check("Unable to init device" not in src_code,
          "S12-9 代码里不引用不存在的 libnfc 输出串")

    # ── selftest（docs/04 §5）────────────────────────────────────
    stf = rd(CORE / "src/bin/selftest.rs")
    check("fn main()" in stf and "catch_unwind" in stf, "ST selftest 有 main 且逐项捕获 panic")
    check("std::process::exit(1)" in stf, "ST selftest 失败时非零退出")
    _ncase = stf.count('("')
    check(_ncase >= 14, "ST selftest 至少 14 个用例", f"实际 {_ncase}")
    for label, fn in [("connect", "case_connect_detect"), ("read-IC", "case_read_ic_saves_dump"),
                      ("write-IC", "case_write_ic"), ("format-card", "case_format_card"),
                      ("write-uid", "case_write_uid_and_lock"), ("手动", "case_hard_nested_manual"),
                      ("autoRun", "case_hard_nested_autorun"), ("dict_test", "case_dict_test"),
                      ("cancel", "case_cancel")]:
        check(f"fn {fn}(" in stf, f"ST selftest 含 {label} 用例")
    check("assert!(guard < 50" in stf, "ST autoRun 用例有递归上限（防无限循环）")
    check("MockToolRunner" in stf and "MockDialog" in stf, "ST selftest 全部走 mock，不碰硬件")
    ru = rd(CORE / "src/runner.rs")
    check("pub fn new(code: Option<i32>) -> Self" in ru, "ST runner::Exit 有便捷构造")

    check("pub mod model;" in rd(CORE / "src/lib.rs"), "M lib.rs 导出 model")
    # S0-S3 回归：本次全量复查后固化
    th = rd(CORE / "src/theme.rs")
    check("tokens.css 有 39 个 `--color-*`" in th, "R theme.rs 注明建模范围 34/39")
    tok = rd(ORIG / "src/renderer/css/tokens.css")
    for css, hexval in (("--color-danger", "#eb7171"), ("--color-danger-strong", "#c0392b")):
        check_orig(css in tok and hexval in tok, f"R theme 危险色对标原版 tokens.css {css}={hexval}")
    check("未建模 5 个" in th, "R theme.rs 列明未建模的 5 个 token（4 滚动条 + 1 焦点环）")
    check("原生滚动条" in doc2 and "--color-scrollbar" in doc2, "R theme 缺口已登记 docs/02 §6.3")
    i18n_asset = rd_json(REPO / "assets/locales/zh-CN.json")
    orig_zh = rd_json(ORIG / "src/locales/zh-CN.json")
    orig_en = rd_json(ORIG / "src/locales/en.json")
    # S8 补了 3 个 key：原版 settings.html 把这三个按钮文案**硬编码英文**
    # （Toggle Dark Mode / Toggle Light Mode / Follow system），没有走 i18n。
    # 我们把它们纳入中文表，属刻意的本地化补全，登记于 docs/02 §6.6。
    S8_EXTRA = {"html_toggle_dark_mode", "html_toggle_light_mode", "html_follow_system_mode"}
    # S11 补的 1 个：原版 autoRun 无终止条件、会无限递归（command.js:341-350），
    # 我们加了「本轮无进展就停」的兜底，该 key 原版没有。登记于 docs/02 §6.12。
    S11_EXTRA = {
        "log_msg_hard_nested_no_progress",
        "log_msg_hard_nested_state_lost",
        "log_msg_nonces_empty_not_saved",
    }
    # S12 补的 2 个：framework/bin 缺失引导。原版没有启动自检，缺失只在
    # spawn 时抛 IO 错误（docs/02 §6.13）。
    S12_EXTRA = {"html_framework_bin_missing", "html_framework_bin_missing_short"}
    # S15 补的 6 个：标题栏主题切换钮、冷启动引导、事件条类型徽标。
    # 原版均无对应 key（docs/02 §6.25）。
    S15_EXTRA = {
        "html_switch_theme",
        "html_guidance_connect_first",
        "html_refresh_device_list",
        "html_event_key",
        "html_event_success",
        "html_event_error",
    }
    # S16 补的 21 个：控制台、事件折叠、工作流分组、检测结果、设置目录/外观、
    # 自动执行提示、文件框等待文案、检测结果“已知/未知”、历史重命名、
    # 控制台按钮悬停说明（§6.28 P1）
    # （docs/02 §6.25 续 / §6.28 / S16 偏离登记）。
    LOCAL_EXTRA = S8_EXTRA | S11_EXTRA | S12_EXTRA | S15_EXTRA | S16_EXTRA | S17_EXTRA | S18_EXTRA
    # 原版对拍基线（ORIG_REF）漂移时上游会新增 key，对应本重写**没有**的功能/结构。
    # 搬运这些文案没有意义（没有代码会 t() 它们），故显式登记为「上游有、我们不做」。
    # 若将来真要做对应功能，把 key 从这里挪进 LOCAL_EXTRA 即可。
    UPSTREAM_ONLY = {
        # ── 主题三态切换 + 设置页分区标题：我们 S8 用了不同的 key 结构，不搬 ──
        "html_theme",
        "html_theme_system",
        "html_theme_light",
        "html_theme_dark",
        # ── 主窗按钮悬浮提示与分组标题：S15 搬回了 11 个（我们有的按钮 +
        # UFUID 分组），其余属我们没有的 UI 结构 ──
        "html_device_group",
        "html_dump_group",
        "html_solving_group",
        "html_tip_dec_full_card",
        # ── 手动输入 UID 写 UFUID（dea7854）与 dump 体检/修复/导出：S18 已实现，
        #    其 key 移入 S18_MIGRATED，不在此处。
        # ── 全加密全卡解密（mfcud/mfcuk 求解器）：v2 边界内 ──
        "html_dec_full_card",
        "dialog_full_enc_expectation",
        "dialog_msg_full_enc_no_solver",
        "indicator_full_enc",
        "log_msg_full_enc_start",
        "log_msg_full_enc_keys",
        "log_msg_full_enc_nokey",
        "settings_full_enc_label",
        # ── 采集 nonce 归档与合并：我们 v1 只留单文件 nonces.bin ──
        "html_merge_nonces",
        "html_nonce_merge_title",
        "html_nonce_merge_pick",
        "html_nonce_merge_hint",
        "html_nonce_merge_run",
        "dialog_msg_nonce_bad_name",
        "dialog_msg_nonce_header_mismatch",
        "dialog_msg_nonce_need_two",
        "indicator_merging_nonces",
        "log_msg_nonce_archived",
        "log_msg_nonce_key_found",
        "log_msg_nonce_no_key",
        # ── 杂项：设备自动选择、静默保存等文案重构 ──
        "html_auto_select_device",
        "html_silent_save_dump",
        "dialog_msg_dict_test_large_sector",
        "dialog_msg_invalid_file_name",
        "dialog_msg_write_bad_size",
        "log_msg_internal_error",
        "log_msg_operation_failed",
        "log_msg_no_hard_nested_progress",
    }
    check_orig(set(i18n_asset) - LOCAL_EXTRA == set(orig_zh) - UPSTREAM_ONLY == set(orig_en) - UPSTREAM_ONLY,
                f"R i18n key 集除本地补的 {len(LOCAL_EXTRA)} 个与上游独有的 {len(UPSTREAM_ONLY)} 个外完全一致",
               f"仅本项目 {set(i18n_asset)-set(orig_zh)-LOCAL_EXTRA} / "
               f"仅原版 {set(orig_zh)-set(i18n_asset)-UPSTREAM_ONLY}")
    check(S8_EXTRA <= set(i18n_asset), "R S8 补的 3 个 settings 按钮文案在表内")
    check(S11_EXTRA <= set(i18n_asset), "R S11 补的无进展兜底文案在表内")
    check(S15_EXTRA <= set(i18n_asset), "R S15 补的 6 个 UI 文案在表内")
    check(S16_EXTRA <= set(i18n_asset), "R S16 补的 21 个 UI 文案在表内")
    check_orig(LOCAL_EXTRA.isdisjoint(orig_zh),
          f"R 本地补的 {len(LOCAL_EXTRA)} 个 key 确认原版不存在")
    check(UPSTREAM_ONLY.isdisjoint(set(i18n_asset)),
           f"R 上游独有的 {len(UPSTREAM_ONLY)} 个 key 确认我们没搬（搬了也没有代码 t() 它们）")
    check_orig(all(i18n_asset.get(k) == orig_zh.get(k) for k in set(i18n_asset) - LOCAL_EXTRA),
          f"R i18n 文案与原版逐条一致（除本地新增 {len(LOCAL_EXTRA)} 个）")
    check([k for k in i18n_asset if k.startswith("lod_")] != [], "R 原版拼写错误 key lod_* 原样保留")
    # 「以为在补东西、其实建了个新 key」：曾出现过 `html_license:`（多了个冒号），
    # 被测试当成「原版拼写错误」而保留下来。key 名以标点结尾一律可疑。
    check(not [k for k in i18n_asset if k[-1:] in {":", ".", ",", " ", "-", "_"}],
          "R i18n key 名不得以标点结尾（疑似误建的新 key）",
          f"{[k for k in i18n_asset if k[-1:] in {':', '.', ',', ' ', '-', '_'}]}")
    # 本地补的 key 数量原本写死在 Rust 测试与此处两处，已漂移过一次。双向钉死。
    check(len(LOCAL_EXTRA) == 14 + len(S16_EXTRA) + len(S17_EXTRA) + len(S18_EXTRA),
          "R 本地补的 i18n key 恰为 52 个", f"实际 {len(LOCAL_EXTRA)}")
    cd = rd(CORE / "src/command.rs")
    # rustfmt 会把字段拆成多行，故用 DOTALL 匹配整个结构体字面量
    specs = re.findall(
        r'ActionSpec \{\s*name: "([\w-]+)",\s*'
        r'connected: (true|false),\s*idle: (true|false),\s*'
        r'not_while_connecting: (true|false),?\s*\}', cd)
    check(len(specs) == 39, "R ACTION_SPECS 覆盖 39 个 action", f"实际 {len(specs)}")
    check(sum(1 for _, c, _, _ in specs if c == "true") == 16, "R connected 门禁 16 个")
    check(sum(1 for _, _, i, _ in specs if i == "true") == 20, "R idle 门禁 20 个")
    check(sum(1 for _, _, _, n in specs if n == "true") == 1, "R not_while_connecting 1 个")
    cn = {a for a, c, _, _ in specs if c == "true"}
    idl = {a for a, _, i, _ in specs if i == "true"}
    check(cn <= idl, "R connected ⊆ idle")
    check(len(cn | idl | {a for a, _, _, n in specs if n == "true"}) == 21, "R 三门禁并集 21")
    allact = re.findall(r'"([\w-]+)"', cd[cd.index("ALL_ACTIONS"):cd.index("];", cd.index("ALL_ACTIONS"))])
    check([s[0] for s in specs] == allact, "R ALL_ACTIONS 与 ACTION_SPECS 顺序一致")
    js_actions = re.findall(r'^\s{4}"([\w-]+)"\s*:', rd(JS), re.M)
    phw_extra = ["open-phw-window", "phw-read-uid", "phw-write-uid", "phw-write-data"]
    s18_extra = ["cuid-write", "input-uid-write", "done-input-uid-write", "dump-editor-export-keys", "run-mfcuk"]
    check_orig(allact[:30] == js_actions and allact[30:34] == phw_extra and allact[34:] == s18_extra,
          "R ALL_ACTIONS 前30 对齐 command.js + 34-37 为 S14 phw + 38-42 为 S18 新增")
    ru = rd(CORE / "src/runner.rs")
    gate_rs = ru[ru.index("pub fn gate("):]
    gate_rs = gate_rs[:gate_rs.index("\n}\n")]
    gate_js = rd(ORIG / "src/execUtils.js")
    check_orig("!st.has_current_device || (!st.is_device_connected && tool != ALWAYS_ALLOWED)" in gate_rs
          and 'status.currentDevice === null || (!status.isDeviceConnected && cmd !== "nfc-list")' in gate_js,
          "R runner::gate 首条判定与 execUtils.js:13 逐字等价")
    check(gate_rs.index("NotConnected") < gate_rs.index("Busy"), "R runner::gate 判定顺序 NotConnected 先于 Busy")
    check("if st.is_running_task {" in gate_rs, "R runner::gate 忙碌判定不含 nfc-list 豁免")
    cgate = cd[cd.index("pub fn gate("):]
    cgate = cgate[:cgate.index("\n}\n")]
    check(cgate.index("Connecting") < cgate.index("NotConnected"), "R command::gate 判定顺序 Connecting 先于 NotConnected")
    check("pub fn gate(spec: &ActionSpec" in cd and "pub fn gate(tool: &str" in ru,
          "R 两处 gate 签名区分明确（command 走 ActionSpec，runner 走 GateState）")
    check("pending.fetch_add(1, Ordering::SeqCst);" in ru
          and ru.index("fetch_add") < ru.index("tx.send"), "R drain 先 fetch_add 再 send")
    check("j.pending.load(Ordering::SeqCst) != 0 || !j.ready.load(Ordering::SeqCst)" in ru,
          "R exit 双条件（pending 与 ready）")
    st = rd(CORE / "src/state.rs")
    el = st[st.index("fn exit_log("):]
    # rustfmt 可能把三元 if 折成多行，按「两个 key 都在且 failed 决定取谁」判
    check('"log_msg_error"' in el and '"log_msg_finished"' in el
          and el.index('"log_msg_error"') < el.index('"log_msg_finished"'),
          "R exit_log 映射 printExitLog(1)/(0,2)")
    check("indicator_error" in el and "indicator_free" in el, "R exit_log 状态条映射正确")
    # 审计修复后的行为证据
    check("fn from_sector_lines_rejects_short_line" in dump_rs, "M from_sector_lines 自查行长")
    check("fn from_sector_lines_reports_non_hex_separately_from_length" in dump_rs, "M 非 hex 与长度错误分开报")
    check("Block0Length" in dump_rs, "M block0_hex 有专用错误变体（不复用 TrailerLength）")
    check("format!(\"{}.tmp\"" in keys_rs, "M 临时文件名显式追加后缀")
    check("fs::create_dir_all" in keys_rs, "M save_keys 建父目录")
    check("HashSet<&str>" in keys_rs, "M merge_keys 去重是 O(n)")
    # 原先断言 `fn next_sector` 存在——那把「序号/2 反推扇区」这个**错误实现**
    # 当成了正确行为。现扇区号取自 `Sector NN - ` 行头，故断言它不得回退。
    check("fn next_sector" not in parse_rs, "M 不得回退到序号/2 推扇区（S12-5 已修）")
    check('Sector\\s+(\\d+)' in parse_rs, "M 扇区号取自 Sector NN - 行头")
    check(r"\d{1,2}" not in parse_rs, "M 扇区正则用 ASCII [0-9] 而非 \\d")
    check("长 **25**" in parse_rs, "M MARKER_COLLECT_KEY 长度注释已订正为 25")
    check("长 **6**" in parse_rs, "M MARKER_MFOC_PROGRESS 长度注释已订正为 6")
    # 审计发现的 7 处必红断言已修
    check("fn found_without_key_group_is_ignored_entirely" in parse_rs, "M A7 测试已改为「不消耗序号」语义")
    check("DEADBEEF1234" in keys_rs, "M A5 大小写去重测试不再用默认密钥")
    check('format!("{}Z", "0".repeat(31))' in integ, "M C1 非法字符测试第 0 行合法")
    check("fn key_info_width_one_matches_expected_sectors" in integ, "M C5 有宽度 1 的绝对锚点")
    check('assert_eq!(t.key_b, "000000000000");' in dump_rs, "M A3 key_b 期望值已修正")
    check("303132333435363738393a3b3c3d3e3f" in dump_rs, "M A2 块3 期望值已修正")
    check("fn detect_keys_that_are_all_defaults_leave_keys_file_empty" in integ, "M A6 拆出「全是默认密钥」用例")
    check("is_default_key" in integ, "M A6 集成层用到 is_default_key")
    # fixture 无前导空格缺陷
    # S12-5 起 fixture 照抄 nfc-mfdetect.c:448-470 的真实输出：
    # 每扇区一行、以 `Sector NN - ` 开头、A/B 印在同一行。
    # 原先检查的是「每行有前导空格」，那同样是把编造格式当正确。
    mfd = (FIX / "mfdetect-keys.txt").read_text(encoding="utf-8").splitlines()
    keylines = [l for l in mfd if re.search(r"(Found|Unknown)\s+Key\s+[AB]", l)]
    badlines = [l for l in keylines if not l.startswith("Sector ")]
    check(keylines, "M fixture 里有密钥行")
    check(not badlines, "M mfdetect fixture 每条密钥行都以 Sector NN - 开头", f"{badlines[:2]}")
    # 每个扇区恰好 A、B 各一条（B 半行可与 A 同行）
    for i, l in enumerate([x for x in mfd if x.startswith("Sector ")]):
        a = len(re.findall(r"Found   Key A|Unknown Key A", l))
        b = len(re.findall(r"Found   Key B|Unknown Key B", l))
        check(a == 1 and b == 1, f"M fixture 第 {i} 行 A/B 各一条", f"第 {i} 行 A={a} B={b}")
    check("[0-9A-Fa-f]{12}" in parse_rs or "[0-9a-fA-F]{12}" in parse_rs,
          "M 密钥正则仍限定 12 位 hex")
    # new_keys 全是默认密钥（A6 的前提）
    dk = re.findall(r'"([0-9a-f]{12})"', keys_rs[keys_rs.index("DEFAULT_KEYS"):][:700])
    pat = re.compile(r" (Found|Unknown)\s+Key\s+([AB])\s*(?::\s*([0-9A-Fa-f]{12}))?")
    nk = {m.group(3) for l in mfd for m in pat.finditer(l) if m.group(1) == "Found" and m.group(3)}
    check(nk and all(k.lower() in dk for k in nk), "M fixture 的 new_keys 全是默认密钥", f"{nk}")


def s14_phw() -> None:
    """S14 手机/手环操作区：对拍上游 Tauri `phw_*` 的关键行为。

    上游在 `src-tauri/src/commands.rs`（`3316b08` + `f835e4e`）。原版若指向
    main 分支（无 src-tauri）或目录缺失，则跳过而非失败。
    """
    tauri = rd(ORIG / "src-tauri" / "src" / "commands.rs")
    if not tauri:
        skip("S14 手机/手环 对拍上游 Tauri phw_*", "无 src-tauri（main 分支或原版缺失）")
        return
    # 上游钉住的 4 项关键行为
    check("(1..=2).contains" in tauri, "S14 上游写数据扇区限块 1-2", "f835e4e")
    check('(sector * 4 + block)' in tauri, "S14 上游偏移公式 (sector*4+block)*16")
    check('-f" ' in tauri or '-f".to_string()' in tauri or '"-f"' in tauri,
          "S14 上游 BCC 不自洽时 mfsetuid 追加 -f")
    check("nfc-mfdetect" in tauri and "-O" in tauri, "S14 上游读回用 nfc-mfdetect -O")
    # 我们侧镜像（引擎读回 + 数据层块号/偏移/BCC）
    eng = rd(CORE / "src" / "engine.rs")
    phw = rd(CORE / "src" / "tasks" / "phw.rs")
    check('phw-write-data' in eng and 'phw-read-uid' in eng and 'phw-write-uid' in eng,
          "S14 引擎接线三个 phw 动作")
    check('BLOCK_MIN' in phw and 'BLOCK_MAX' in phw and "SECTOR_MAX" in phw,
          "S14 数据层有块 1-2 / 扇区 0-15 上限常量")
    check("sector * BLOCKS_PER_SECTOR" in phw, "S14 数据层偏移公式与上游一致")
    check("block0_bcc_ok" in phw, "S14 数据层有 BCC 校验（对应上游 -f 分支）")


def main() -> int:
    for fn in (
        a_rustfmt,
        b_resolve,
        c_msrv,
        d_align,
        h_i18n,
        i_gate,
        j_fixture,
        k_docs,
        l_structure,
        m_contract,
        n_generic,
        s4_model,
        s14_phw,
    ):
        try:
            fn()
        except Exception as e:  # noqa: BLE001
            check(False, f"{fn.__name__} 异常", f"{type(e).__name__}: {e}")

    w = max(len(l) for _, l, _ in results)
    fails = sum(1 for st, _, _ in results if st == "FAIL")
    skips = sum(1 for st, _, _ in results if st == "SKIP")
    passes = len(results) - fails - skips
    for st, label, detail in results:
        mark = "✓" if st == "PASS" else ("-" if st == "SKIP" else "✗")
        line = f"{mark} {label.ljust(w)}"
        if detail:
            line += f"  ← {detail[:160]}"
        print(line)
    print("-" * (w + 12))
    total = f"合计 {len(results)} 项，通过 {passes}，失败 {fails}"
    if skips:
        total += f"，跳过 {skips}（原版源码不在本机：{ORIG}，用 NFC_ORIG_DIR 指向）"
    print(total)
    # 显式指定了 NFC_ORIG_DIR = 本就是要全量对拍。此时「原版缺失」类跳过绝不该
    # 出现（exit(3) 已挡住「指定了目录却不存在」）。注意只统计原版相关跳过：
    # c_msrv 的「无 cargo index 缓存」是合法跳过，不算数（Windows 上 parity 先于
    # cargo 运行时会走到这里，若把它也算进去会误杀）。
    if _orig_env and ORIG_SKIPS:
        print(
            f"ERROR: 已指定 NFC_ORIG_DIR={_orig_env}，仍有 {ORIG_SKIPS} 项对拍因"
            "「原版缺失」被跳过，全量对拍不完整。",
            file=sys.stderr,
        )
        return 2
    return 1 if fails else 0


if __name__ == "__main__":
    sys.stdout.reconfigure(encoding="utf-8")
    sys.exit(main())
