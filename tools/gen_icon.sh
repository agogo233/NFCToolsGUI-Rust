#!/usr/bin/env bash
# 从 assets/icons/icon.svg 生成 PNG / ICO 图标资产。
#
# 依赖（生成一次即可，产物提交进仓库，运行时不需要）：
#   librsvg2-bin（rsvg-convert）  +  imagemagick（convert）
#   apt-get install -y librsvg2-bin imagemagick
#
# 用法：
#   ./tools/gen_icon.sh

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

command -v rsvg-convert >/dev/null || {
  echo "缺 rsvg-convert，请先: apt-get install -y librsvg2-bin" >&2; exit 1; }
command -v convert >/dev/null || {
  echo "缺 convert，请先: apt-get install -y imagemagick" >&2; exit 1; }

# 窗口图标（egui with_icon，宽高须为 4 的倍数）
rsvg-convert -w 32  -h 32  assets/icons/icon.svg -o assets/icons/app_icon.png
# 自绘标题栏左上角 logo（运行时缩放到 20px 显示）
rsvg-convert -w 128 -h 128 assets/icons/icon.svg -o assets/icons/title_icon.png
# Windows exe 嵌入图标（多尺寸）
rsvg-convert -w 512 -h 512 assets/icons/icon.svg -o /tmp/nfc-icon-512.png
convert /tmp/nfc-icon-512.png \
  -define icon:auto-resize=16,24,32,48,64,128,256 \
  assets/icons/icon.ico

echo "已生成：assets/icons/app_icon.png / title_icon.png / icon.ico"
