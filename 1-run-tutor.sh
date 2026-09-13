#!/usr/bin/env bash
set -euo pipefail

VENV="/home/user/data/python-envs/langteacher"
cd "$(dirname "$0")"

# LangTeacher talks to plain http://127.0.0.1:8080 like a normal llama.cpp
# server -- it has no idea the other end is actually claude-llama-proxy.
# Make sure no proxy env vars leak in here: they'd make `requests` try to
# reach 127.0.0.1:8080 *through* the outbound proxy, which can't route there.
unset HTTP_PROXY HTTPS_PROXY http_proxy https_proxy

if ! curl -s -o /dev/null -m 2 http://127.0.0.1:8080/health; then
  echo "claude-llama-proxy не отвечает на 127.0.0.1:8080."
  echo "Сначала запусти его:  cd ~/claude-llama-proxy && ./run.sh"
  exit 1
fi

source "$VENV/bin/activate"

# ctranslate2 (faster-whisper's backend) dlopen's CUDA libs like libcublas.so.12
# at inference time rather than import time, and -- unlike torch, which has
# RPATH baked in -- doesn't know to look inside the venv's nvidia/*/lib pip
# package dirs. Point it there explicitly, or transcription fails with
# "Library libcublas.so.12 is not found" on the first recording.
SITE_PACKAGES="$VENV/lib/python3.13/site-packages"
export LD_LIBRARY_PATH="$(find "$SITE_PACKAGES/nvidia" -maxdepth 2 -type d -name lib | tr '\n' ':')${LD_LIBRARY_PATH:-}"

# OmniVoice weights were pre-downloaded straight to this dir (HF Hub download
# kept stalling locally under memory pressure) -- point at them directly so
# no network fetch is needed at startup.
export OMNIVOICE_MODEL="$(pwd)/omnivoice-model"
export HF_HUB_OFFLINE=1

# OmniVoice (diffusion-based voice cloning) is too slow on this GPU (Pascal,
# no fast fp16 -- ~10s per short reply even at NUM_STEP=4). Piper is
# CPU-only, no diffusion steps, near-instant -- trades voice cloning for a
# fixed pretrained voice.
export TTS_ENGINE=piper

python3 main.py "$@"
