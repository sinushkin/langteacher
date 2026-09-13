#!/usr/bin/env bash
set -euo pipefail

export HOME="/home/user"
VENV="/home/user/data/python-envs/langteacher"

# --- Outbound network: SOCKS5_PROXY, e.g. "192.168.3.1:1080", passed in at
# `docker run -e SOCKS5_PROXY=...`. We run our own local privoxy
# (127.0.0.1:8118) forwarding to it, so claude-llama-proxy (which has that
# address hardcoded) and the claude CLI it spawns just work unchanged,
# exactly like on the host this image was built from. ---
if [ -n "${SOCKS5_PROXY:-}" ]; then
  {
    echo "listen-address 127.0.0.1:8118"
    echo "forward-socks5t / ${SOCKS5_PROXY} ."
  } > /etc/privoxy/config
  privoxy --no-daemon /etc/privoxy/config &
  for _ in $(seq 1 20); do
    curl -s -o /dev/null -m 1 --proxy http://127.0.0.1:8118 http://127.0.0.1:8118 2>/dev/null && break
    sleep 0.2
  done
else
  echo "WARNING: SOCKS5_PROXY not set -- claude-llama-proxy needs outbound network" \
       "access via http://127.0.0.1:8118 and will fail without it. Run with" \
       "-e SOCKS5_PROXY=host:port." >&2
fi

# --- Auth: this image ships no credentials on purpose. Mount your Claude
# Code session at runtime: docker run -v ~/.claude:/home/user/.claude:ro ... ---
if [ ! -f /home/user/.claude/.credentials.json ]; then
  echo "WARNING: /home/user/.claude/.credentials.json not found -- mount your" \
       "Claude Code credentials with -v ~/.claude:/home/user/.claude:ro" >&2
fi

# --- ctranslate2 (faster-whisper) dlopens CUDA libs lazily at inference time
# and, unlike torch, has no RPATH into the venv's pip-installed nvidia/*/lib
# dirs -- point it there explicitly. ---
export LD_LIBRARY_PATH="$(find "$VENV/lib/python3.13/site-packages/nvidia" -maxdepth 2 -type d -name lib | tr '\n' ':')${LD_LIBRARY_PATH:-}"
export PATH="/home/user/.local/bin:$PATH"
# shellcheck disable=SC1091
source "$VENV/bin/activate"

# --- Start claude-llama-proxy (resident `claude -p` session) in the background. ---
(
  cd /home/user/langteacher/claude-llama-proxy
  ./target/release/claude-llama-proxy
) &

for _ in $(seq 1 50); do
  curl -s -o /dev/null -m 1 http://127.0.0.1:8080/health && break
  sleep 0.2
done

cd /home/user/langteacher
export HF_HUB_OFFLINE=1
export TTS_ENGINE=piper

if [ "$#" -eq 0 ]; then
  echo "claude-llama-proxy is up on 127.0.0.1:8080. Run: python3 main.py"
  echo "(needs -it, mic/speaker passthrough e.g. --device /dev/snd, or --output-method text --input-method text)"
  exec bash
fi
exec "$@"
