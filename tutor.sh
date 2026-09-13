#!/usr/bin/env bash
# Entry point for `docker exec -it langteacher tutor`: `docker exec` doesn't
# inherit the env `entrypoint.sh` exported into PID 1's shell, so this redoes
# the same venv/env setup before handing off to main.py.
set -euo pipefail

VENV="/home/user/data/python-envs/langteacher"
export LD_LIBRARY_PATH="$(find "$VENV/lib/python3.13/site-packages/nvidia" -maxdepth 2 -type d -name lib | tr '\n' ':')${LD_LIBRARY_PATH:-}"
export PATH="/home/user/.local/bin:$PATH"
# shellcheck disable=SC1091
source "$VENV/bin/activate"

export HF_HUB_OFFLINE=1
export TTS_ENGINE=piper

cd /home/user/langteacher
exec python3 main.py "$@"
