#!/usr/bin/env bash
#
# 下载第一版所需的本地语音模型资源到 ./models
#
# 组成：
#   silero_vad.onnx   —— Silero VAD 语音活动检测（1.7 MB）
#   model.int8.onnx   —— SenseVoice 多语言语音/情绪/事件模型 int8 量化版（228 MB）
#   tokens.txt        —— SenseVoice 符号表（0.3 MB）
#
# 说明：
#   * 模型文件不随仓库分发，也不嵌入主 EXE，必须单独部署（见 DEV.md 二.2）。
#   * 默认走 hf-mirror.com 镜像；若网络可直连 HuggingFace，可用 HF_ENDPOINT 覆盖。
#   * 支持断点续传，重复执行不会重新下载已完整的文件。
#
set -euo pipefail

HF_ENDPOINT="${HF_ENDPOINT:-https://hf-mirror.com}"
VAD_REPO="csukuangfj/vad"
ASR_REPO="csukuangfj/sherpa-onnx-sense-voice-zh-en-ja-ko-yue-2024-07-17"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(dirname "$SCRIPT_DIR")"
MODELS_DIR="$ROOT_DIR/models"
ASR_DIR="$MODELS_DIR/sense-voice"

mkdir -p "$ASR_DIR"

fetch() {
  local url="$1" dest="$2" label="$3"
  echo "==> $label"
  echo "    $url"
  curl -fL --retry 3 --retry-delay 2 --connect-timeout 20 \
    -C - --progress-bar -o "$dest" "$url"
  ls -lh "$dest" | awk '{print "    完成: " $5}'
}

fetch "$HF_ENDPOINT/$VAD_REPO/resolve/main/silero_vad.onnx" \
  "$MODELS_DIR/silero_vad.onnx" "Silero VAD"

fetch "$HF_ENDPOINT/$ASR_REPO/resolve/main/model.int8.onnx" \
  "$ASR_DIR/model.int8.onnx" "SenseVoice 模型 (int8)"

fetch "$HF_ENDPOINT/$ASR_REPO/resolve/main/tokens.txt" \
  "$ASR_DIR/tokens.txt" "SenseVoice 符号表"

echo
echo "全部模型已就绪:"
du -sh "$MODELS_DIR"
