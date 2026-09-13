# syntax=docker/dockerfile:1
#
# Mirrors the image built yesterday by hand (docker run --sleep-infinity,
# docker cp, docker commit), but as a reproducible Dockerfile.
#
# The Python venv (torch/ctranslate2/faster-whisper/piper-tts, ~9.2GB) is NOT
# baked into the image. `pip install torch` inside a build would push its
# CUDA wheel through the same flaky local SOCKS proxy that broke on long
# transfers yesterday -- and a first attempt to COPY the pre-built venv in
# via an additional build context OOM'd this host outright, so it's not
# baked in at all. Build the venv once on the host at
# /home/user/data/python-envs/langteacher (already done) and bind-mount it
# read-only at `docker run` time instead, same as the Claude credentials:
#
#   docker build -t langteacher:latest .
#   docker run -d --name langteacher --hostname "$(hostname)" \
#     -v ~/.claude:/home/user/.claude:ro \
#     -v /home/user/data/python-envs/langteacher:/home/user/data/python-envs/langteacher:ro \
#     -v /home/user/.pyenv/versions/3.13.9:/home/user/.pyenv/versions/3.13.9:ro \
#     -e SOCKS5_PROXY=host:port \
#     --device /dev/snd --group-add 29 \
#     -v /run/user/1000/pipewire-0:/run/user/1000/pipewire-0 -e XDG_RUNTIME_DIR=/run/user/1000 \
#     -v /tmp/.X11-unix:/tmp/.X11-unix:ro -e DISPLAY="$DISPLAY" \
#     -v "$XAUTHORITY:$XAUTHORITY:ro" -e XAUTHORITY="$XAUTHORITY" \
#     langteacher:latest sleep infinity
#
#   docker exec -it langteacher tutor   # mic/push-to-talk (Right Shift) run
#
# Two non-obvious flags above:
# - the pyenv mount: the venv's bin/python3 is a symlink to the pyenv
#   interpreter that built it (/home/user/.pyenv/versions/3.13.9/...), not
#   to the apt-installed python3.13 in this image -- without it "python3" in
#   the activated venv is a dangling symlink.
# - --hostname "$(hostname)": python-xlib (pynput's backend) looks up the
#   Xauthority cookie by the *connecting* hostname for local/unix-socket
#   connections. A container's default hostname is its container ID, which
#   won't match any entry in the mounted Xauthority file, and pynput fails
#   with "Authorization required, but no authorization protocol specified"
#   even though the socket and file are both mounted correctly.
#
# OmniVoice is intentionally not included (dropped -- too heavy for the GPU
# it has to run on); the image ships Piper (TTS_ENGINE=piper) instead.

########## Stage 1: build the Rust proxy ##########
FROM debian:trixie-slim AS proxy-builder

RUN apt-get update && apt-get install -y --no-install-recommends \
      ca-certificates curl build-essential pkg-config \
    && rm -rf /var/lib/apt/lists/*

ENV RUSTUP_HOME=/opt/rustup CARGO_HOME=/opt/cargo PATH=/opt/cargo/bin:$PATH
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal

WORKDIR /build
COPY claude-llama-proxy/Cargo.toml claude-llama-proxy/Cargo.lock ./
COPY claude-llama-proxy/src ./src
RUN cargo build --release

########## Stage 2: runtime image ##########
FROM debian:trixie-slim

# Matches yesterday's `apt-mark showmanual` on the hand-built image, plus
# pipewire-alsa so PortAudio's ALSA "default" device resolves to the host's
# PipeWire server over the mounted /run/user/1000/pipewire-0 socket (mic
# capture for push-to-talk -- see `tutor` below).
RUN apt-get update && apt-get install -y --no-install-recommends \
      ca-certificates curl espeak-ng libportaudio2 privoxy \
      python3.13 python3.13-venv pipewire-alsa \
    && rm -rf /var/lib/apt/lists/*

# uid/gid 1000 to match the host user; also join `audio` (Debian's stock
# gid 29, same as the host) so /dev/snd is usable when --device /dev/snd is
# passed at `docker run`.
RUN useradd --uid 1000 --user-group --create-home --shell /bin/bash user \
    && usermod -aG audio user \
    && chown -R user:user /etc/privoxy

WORKDIR /home/user/langteacher

# The venv is bind-mounted at runtime (see note above) -- just make sure the
# mount point exists and is owned by `user`.
RUN mkdir -p /home/user/data/python-envs/langteacher \
    && chown -R user:user /home/user/data

COPY --chown=user:user . /home/user/langteacher
COPY --chown=user:user --from=proxy-builder /build/target/release/claude-llama-proxy \
     claude-llama-proxy/target/release/claude-llama-proxy
COPY entrypoint.sh /entrypoint.sh
COPY tutor.sh /usr/local/bin/tutor
RUN chmod +x /entrypoint.sh /usr/local/bin/tutor claude-llama-proxy/target/release/claude-llama-proxy

USER user
ENV HOME=/home/user

EXPOSE 8080
ENTRYPOINT ["/entrypoint.sh"]
