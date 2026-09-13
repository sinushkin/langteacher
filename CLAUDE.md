# LangTeacher

Fork of [janosdios/langteacher](https://github.com/janosdios/langteacher);
`origin` points at the fork (`sinushkin/langteacher`), pushed to over SSH.

See `HOWTO.md` for how to run it (native and Docker) — read that first.

## Non-obvious project state

- **LLM backend**: not a local GGUF model or a paid API — `claude-llama-proxy/`
  (a small Rust server, vendored in-tree, no upstream repo of its own) makes
  the user's own Claude Code subscription look like a local `llama-server` on
  `127.0.0.1:8080`. LangTeacher's code has no idea; it thinks it's talking to
  llama.cpp.
- **OmniVoice was dropped.** It cloned the tutor's voice but needs more GPU
  than this machine has. TTS is Piper (`TTS_ENGINE=piper`) instead — no voice
  cloning, CPU-only. `requirements.txt` and `requirements-rpi.txt` still list
  `omnivoice`/`torch` as if it were live; that's stale, not a re-adopted
  dependency — don't take it as a reason to reintroduce OmniVoice wiring.
- **Docker image is reproducible, but the venv isn't baked in.** The Python
  venv (torch/ctranslate2/faster-whisper/piper-tts, ~9.2GB, built once at
  `/home/user/data/python-envs/langteacher`) is bind-mounted at `docker run`
  time rather than `COPY`'d into the image during build. Two things forced
  this: (1) `pip install torch` during a build would push the CUDA wheel
  through the same flaky local SOCKS proxy that used to break on long
  transfers, and (2) an attempt to `COPY` the pre-built venv in via an
  additional build context OOM'd the host outright. So the image just
  contains the app source + compiled Rust proxy (~267MB); the venv mount is
  required at runtime.
- **`/home/user/data` is a separate encrypted btrfs volume** (`docker_crypt`)
  that also holds Docker's storage and several unrelated production
  containers (postgres, VPN, a training server). It runs close to full and
  is prone to the classic btrfs trap where `df` shows free space but
  `Device unallocated` hits ~0 and metadata chunks can't grow, producing
  "no space left on device" on any build even with plenty of `df` headroom.
  Fix is `sudo btrfs balance start -dusage=0 -musage=0 /home/user/data`
  (reclaims empty chunks, doesn't touch data, safe to rerun) — check
  `sudo btrfs filesystem usage /home/user/data` before assuming a build
  failure here is really about disk space.
- **Mic + push-to-talk (Right Shift) in Docker needs host audio/X11
  passthrough**, not just the proxy: `stt_engine.py` uses `pynput` (X11
  backend) for the hotkey and `sounddevice`/PortAudio (ALSA, routed through
  PipeWire) for capture. The container needs `--device /dev/snd`,
  `--group-add 29` (host's `audio` gid), the PipeWire socket
  (`/run/user/1000/pipewire-0`), and the X11 socket + `DISPLAY` +
  `XAUTHORITY` from the host's actual session (this machine runs Xwayland,
  typically `DISPLAY=:1`) — see the `docker run` example at the top of
  `Dockerfile`. Two easy-to-miss extras baked into that example: the venv's
  `bin/python3` is a symlink into `/home/user/.pyenv/versions/3.13.9`
  (that's what actually built it, not the apt-installed python3.13 in the
  image), so that pyenv version dir needs mounting in too; and the container
  needs `--hostname "$(hostname)"` matching the host, because pynput's X11
  backend (python-xlib) looks up the Xauthority cookie by the connecting
  hostname for local/unix-socket connections, and a container's default
  hostname (its container ID) won't match any entry in the mounted
  Xauthority file. Run the app itself with `docker exec -it langteacher
  tutor` (not `python3 main.py` directly) — `tutor` (`/usr/local/bin/tutor`,
  from `tutor.sh`) redoes the venv/env setup that `entrypoint.sh` only
  exported into PID 1's own shell, which a fresh `docker exec` doesn't
  inherit.
- **The `claude` CLI itself isn't in the image.** `claude-llama-proxy` execs
  the `claude` binary directly; it's a self-contained native binary (glibc
  deps only, no Node.js needed) living at
  `~/.local/share/claude/versions/<ver>`, symlinked from `~/.local/bin/claude`
  — both need mounting into the container (see `Dockerfile`'s `docker run`
  example). It's tied to the host's own Claude Code install and credentials,
  so it's mounted rather than baked in, same reasoning as the venv.
- **faster-whisper needs its HF cache mounted too** (`~/.cache/whisper`) —
  `HF_HUB_OFFLINE=1` (set for the Claude backend's own sake) makes it refuse
  to download the model on a cache miss instead of falling back online.
- **GPU needs `--gpus all`** at `docker run`, or torch/ctranslate2 silently
  run on CPU. Separately: this host's GPU is a P106-100 (Pascal mining
  card, no fast fp16), so ctranslate2's "compute type inferred ... float16,
  but ... do not support efficient float16" warning is expected and not a
  passthrough bug.

## What's actually in the container vs. bind-mounted from the host

The image (~267MB) is just Debian trixie-slim + apt packages + the
LangTeacher source + the compiled Rust proxy. Everything else the app
needs is a host bind-mount at `docker run` time, not part of the image:

1. `~/.claude` — Claude Code credentials
2. `~/.local/share/claude` + `~/.local/bin` — the `claude` CLI binary itself
3. `/home/user/data/python-envs/langteacher` — the venv (torch/ctranslate2/
   faster-whisper/piper-tts, ~9.2GB)
4. `/home/user/.pyenv/versions/3.13.9` — the interpreter the venv symlinks to
5. `~/.cache/whisper` — the STT model weights
6. `/dev/snd` + `--group-add 29` (audio gid) — sound hardware
7. `/run/user/1000/pipewire-0` — the host's PipeWire socket
8. `/tmp/.X11-unix` + `DISPLAY` + `XAUTHORITY` + `--hostname` — X11, for
   pynput's global Right-Shift hotkey
9. `--gpus all` — GPU passthrough
10. `SOCKS5_PROXY` — this host's local outbound proxy address

**This means the container is not portable as-is** (e.g. to a Windows/WSL
box): items 2-5 are just files on this specific machine's disk that would
have to be rebuilt from scratch elsewhere (install Claude Code + log in,
`pip install` the whole venv again, redownload the whisper model), and
items 6-8 assume this host's audio/X11 stack (PipeWire + Xwayland) --
WSLg exposes audio/X11 through entirely different socket paths and would
need the `docker run` flags reworked, not just copied. Only the image
itself (and, as plain files, the venv/whisper-cache dirs) would carry over
directly, and only to another Linux/amd64 host.

**Plan (once the new disk is attached):** bake everything into the image
except item 1 (`~/.claude` credentials, which should never be baked into
an image) -- i.e. `COPY` the venv and whisper cache in at build time
instead of bind-mounting them, so the image is self-contained modulo
auth. The reason this wasn't done originally (a `COPY` of the venv via an
additional build context OOM'd this host, and there wasn't enough spare
disk) should no longer apply with the new disk -- but re-check available
RAM/disk before that build regardless. Items 2 and 4 (the `claude` CLI
binary and the pyenv interpreter) can likely be baked in too, since
they're not credentials, just binaries -- only item 1 is inherently
per-host. Items 6-9 (audio/X11/GPU) stay runtime flags no matter what;
those are host-integration points, not something an image can carry.
