#!/usr/bin/env bash
# 打包 Windows 发布版。
#
# 产物布局对标 docs/02 §8：
#
#   NFCToolsGUI-win-x64.zip
#   ├── nfctoolsgui.exe        # cargo build --release
#   ├── framework/bin/         # 9 个 C 工具 —— 由用户的 compile.sh 生成
#   ├── dict.dic
#   ├── README.md / LICENSE
#
# **本脚本不构建 framework/bin**。C 工具是 C 子模块（GSWXXN/* 的 fork），
# 需要 autoconf/libtool 等工具链，且 Linux/macOS 产物不能用在 Windows 上。
# 原始 Electron 项目的 compile.sh 负责这件事；本脚本只负责检测它是否就位。
#
# 用法：
#   ./scripts/package.sh              # 构建 + 打包
#   ./scripts/package.sh --check      # 只做检测与静态门禁，不产出 zip
#
# 退出码：0 成功；1 缺前置或门禁未过。

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

OUT_DIR="$REPO_ROOT/out"
ZIP_NAME="NFCToolsGUI-win-x64.zip"

CHECK_ONLY=0
[[ "${1:-}" == "--check" ]] && CHECK_ONLY=1

say()  { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
ok()   { printf '\033[1;32m  ok\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33m  !!\033[0m %s\n' "$*"; }
die()  { printf '\033[1;31m ERR\033[0m %s\n' "$*" >&2; exit 1; }

# ── 1. 前置检查 ────────────────────────────────────────────────────────────
command -v cargo >/dev/null || die "未找到 cargo"
ok "cargo: $(cargo --version)"

# ── 2. 静态门禁（与 CI 跑同一套）────────────────────────────────────────────
if [[ -f tools/verify_parity.py ]]; then
  say "静态回归 tools/verify_parity.py"
  if command -v python3 >/dev/null; then
    # 原版 Electron 源码不在本仓库时，脚本会自动跳过相关对拍项
    # 不用 `| tail -1`：门禁失败时要能看到具体哪一项红了
    python3 tools/verify_parity.py
    ok "静态门禁通过"
  else
    warn "无 python3，跳过静态门禁"
  fi
else
  warn "tools/verify_parity.py 不存在，跳过静态门禁"
fi

# ── 3. framework/bin 检测 ───────────────────────────────────────────────────
# 定位规则与 `runner::framework_bin`（crates/nfc-core/src/runner.rs）一致：
# exe 同级命中，否则向上最多 5 层。这里从**真实的 exe 目录**起步，
# 复现生产环境的查找路径。
detect_bin_dir() {
  local start="$1"
  [[ -d "$start/framework/bin" ]] && { echo "$start/framework/bin"; return 0; }
  local cur="$start"
  for _ in 1 2 3 4 5; do
    cur="$(dirname "$cur")"
    [[ "$cur" == "/" || "$cur" == "." ]] && break
    [[ -d "$cur/framework/bin" ]] && { echo "$cur/framework/bin"; return 0; }
  done
  echo "$start/framework/bin"
}

# 候选顺序：先按运行时规则（exe 同级 → 向上 5 层），再试原版项目目录
# （`compile.sh` 的产物在那儿，但它不是本仓库的祖先目录，运行时规则找不到）。
BIN_DIR=""
for cand in \
  "$(detect_bin_dir "$REPO_ROOT/target/release")" \
  "$REPO_ROOT/framework/bin" \
  "$REPO_ROOT/../NFCToolsGUI/framework/bin" \
  "$REPO_ROOT/../NFCToolsGUI/rust/framework/bin" ; do
  if [[ -d "$cand" ]]; then BIN_DIR="$cand"; break; fi
done
BIN_DIR="${BIN_DIR:-$REPO_ROOT/target/release/framework/bin}"
if [[ -d "$BIN_DIR" ]]; then
  ok "framework/bin: $BIN_DIR"
  missing=0
  # 清单与 crates/nfc-core/src/runner.rs 的 KNOWN_TOOLS 保持一致
  # `mfoc`（读卡）与 `nfc-mfclassic`（写卡/格式化）都要——两者参数风格不同，
  # 见 crates/nfc-core/src/runner.rs 的 KNOWN_TOOLS 注释。
  for t in nfc-list mfoc nfc-mfclassic nfc-mfdetect nfc-mfdict \
           nfc-mfsetuid nfc-mflock libnfc-collect cropto1_bs; do
    if [[ -f "$BIN_DIR/$t" || -f "$BIN_DIR/$t.exe" ]]; then
      printf '      %s\n' "$t"
    else
      printf '\033[1;33m      缺 %s\033[0m\n' "$t"
      missing=1
    fi
  done
  [[ $missing -eq 1 ]] && warn "framework/bin 不完整，打出的包在用户机上会缺功能"
else
  warn "未找到 framework/bin（$BIN_DIR）"
  warn "  C 工具需在原版 NFCToolsGUI 目录跑 compile.sh 生成（需 autoconf/libtool）"
  warn "  缺失时程序启动会显示红色引导条，而不是在点按钮后才报错"
fi

# ── 4. dict.dic ────────────────────────────────────────────────────────────
DICT_SRC=""
for cand in "$REPO_ROOT/../NFCToolsGUI/dict.dic" "$REPO_ROOT/dict.dic"; do
  [[ -f "$cand" ]] && { DICT_SRC="$cand"; break; }
done

if [[ $CHECK_ONLY -eq 1 ]]; then
  [[ -n "$DICT_SRC" ]] && ok "dict.dic: $DICT_SRC" || warn "未找到 dict.dic（发布包需自带）"
  for doc in README.md LICENSE; do
    [[ -f "$doc" ]] && ok "$doc" || warn "缺少 $doc"
  done
  say "--check 模式，不产出 zip"
  exit 0
fi

# ── 5. 构建 ────────────────────────────────────────────────────────────────
say "cargo build --release"
cargo build --release --locked
# 产物名按平台取：Windows 带 .exe，Unix 不带。
case "$(uname -s)" in
  MINGW*|MSYS*|CYGWIN*) EXE="target/release/nfc-app.exe" ;;
  *)                    EXE="target/release/nfc-app" ;;
esac
[[ -f "$EXE" ]] || die "未找到构建产物 $EXE（当前平台 $(uname -s)）"
ok "$EXE ($(du -h "$EXE" | cut -f1))"

# ── 6. 收集 ────────────────────────────────────────────────────────────────
say "收集产物到 $OUT_DIR"
STAGE="$OUT_DIR/stage"
rm -rf "$STAGE"
mkdir -p "$STAGE"

cp "$EXE" "$STAGE/nfctoolsgui.exe"
[[ -d "$BIN_DIR" ]] && cp -r "$BIN_DIR" "$STAGE/framework-bin-tmp" \
  && mkdir -p "$STAGE/framework" && mv "$STAGE/framework-bin-tmp" "$STAGE/framework/bin"
if [[ -n "$DICT_SRC" ]]; then
  cp "$DICT_SRC" "$STAGE/dict.dic"      # 失败要让 set -e 生效，不能被 || 吞掉
else
  warn "未找到 dict.dic，打出的包会缺内置字典"
fi
for doc in README.md LICENSE; do
  if [[ -f "$doc" ]]; then
    cp "$doc" "$STAGE/"
  else
    warn "缺少 $doc"
  fi
done
ok "已收集 $(find "$STAGE" -type f | wc -l) 个文件"

# ── 7. 压缩 ────────────────────────────────────────────────────────────────
command -v zip >/dev/null || die "未找到 zip 命令"
say "打包 $ZIP_NAME"
rm -f "$OUT_DIR/$ZIP_NAME"
( cd "$STAGE" && zip -qr "$OUT_DIR/$ZIP_NAME" . )
ok "$OUT_DIR/$ZIP_NAME ($(du -h "$OUT_DIR/$ZIP_NAME" | cut -f1))"

say "完成。framework/bin 若为空，用户需自行跑 compile.sh。"
