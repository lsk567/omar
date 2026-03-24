# syntax=docker/dockerfile:1.7

FROM rust:1-bookworm AS builder

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates \
        libssl-dev \
        pkg-config \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY . .

RUN cargo build --release --workspace


FROM debian:bookworm-slim AS runtime

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        bash \
        ca-certificates \
        curl \
        git \
        imagemagick \
        less \
        libssl3 \
        nodejs \
        npm \
        procps \
        python3 \
        tmux \
        tini \
        xauth \
        xdotool \
    && npm install -g @anthropic-ai/claude-code @openai/codex \
    && curl -fsSL https://opencode.ai/install | bash -s -- --no-modify-path \
    && curl -fsSL https://cursor.com/install | bash \
    && install -m 0755 /root/.opencode/bin/opencode /usr/local/bin/opencode \
    && mkdir -p /opt/cursor-agent \
    && cp -a /root/.local/share/cursor-agent/versions/*/. /opt/cursor-agent/ \
    && ln -sf /opt/cursor-agent/cursor-agent /usr/local/bin/cursor-agent \
    && ln -sf /opt/cursor-agent/cursor-agent /usr/local/bin/agent \
    && rm -rf /var/lib/apt/lists/* /root/.npm

RUN cat <<'EOF' >/usr/local/bin/cursor
#!/usr/bin/env bash
set -euo pipefail
if [ "${1:-}" = "agent" ]; then
  shift
fi
exec /usr/local/bin/agent "$@"
EOF

RUN chmod +x /usr/local/bin/cursor

RUN useradd --create-home --home-dir /home/omar --shell /bin/bash --uid 1000 omar

COPY --from=builder /app/target/release/omar /usr/local/bin/omar
COPY --from=builder /app/target/release/omar-slack /usr/local/bin/omar-slack
COPY --from=builder /app/target/release/omar-computer /usr/local/bin/omar-computer
COPY docker/config.toml /etc/omar/config.toml
COPY docker/entrypoint.sh /usr/local/bin/omar-entrypoint

RUN chmod +x /usr/local/bin/omar-entrypoint \
    && chown -R omar:omar /etc/omar /home/omar

ENV HOME=/home/omar
ENV CARGO_HOME=/home/omar/.cargo
WORKDIR /workspace
USER omar

EXPOSE 9876 9877 9878

ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/omar-entrypoint"]
CMD ["sleep", "infinity"]


FROM rust:1-bookworm AS dev

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        bash \
        ca-certificates \
        curl \
        git \
        imagemagick \
        less \
        libssl-dev \
        nodejs \
        npm \
        procps \
        python3 \
        tmux \
        tini \
        xauth \
        xdotool \
    && npm install -g @anthropic-ai/claude-code @openai/codex \
    && curl -fsSL https://opencode.ai/install | bash -s -- --no-modify-path \
    && curl -fsSL https://cursor.com/install | bash \
    && install -m 0755 /root/.opencode/bin/opencode /usr/local/bin/opencode \
    && mkdir -p /opt/cursor-agent \
    && cp -a /root/.local/share/cursor-agent/versions/*/. /opt/cursor-agent/ \
    && ln -sf /opt/cursor-agent/cursor-agent /usr/local/bin/cursor-agent \
    && ln -sf /opt/cursor-agent/cursor-agent /usr/local/bin/agent \
    && rm -rf /var/lib/apt/lists/* /root/.npm

RUN cat <<'EOF' >/usr/local/bin/cursor
#!/usr/bin/env bash
set -euo pipefail
if [ "${1:-}" = "agent" ]; then
  shift
fi
exec /usr/local/bin/agent "$@"
EOF

RUN chmod +x /usr/local/bin/cursor

RUN useradd --create-home --home-dir /home/omar --shell /bin/bash --uid 1000 omar

COPY --from=builder /app/target/release/omar /usr/local/bin/omar
COPY --from=builder /app/target/release/omar-slack /usr/local/bin/omar-slack
COPY --from=builder /app/target/release/omar-computer /usr/local/bin/omar-computer
COPY docker/config.toml /etc/omar/config.toml
COPY docker/entrypoint.sh /usr/local/bin/omar-entrypoint
COPY . /workspace

RUN cat <<'EOF' >/etc/profile.d/omar-history.sh
export HISTFILE="${HISTFILE:-$HOME/.omar/bash_history}"
export HISTSIZE=50000
export HISTFILESIZE=50000
shopt -s histappend 2>/dev/null || true
if [ -f "${HISTFILE}" ]; then
  size=$(wc -c < "${HISTFILE}")
  if [ "${size}" -gt 52428800 ]; then
    tail -c 52428800 "${HISTFILE}" > "${HISTFILE}.tmp" && mv "${HISTFILE}.tmp" "${HISTFILE}"
  fi
fi
PROMPT_COMMAND="history -a${PROMPT_COMMAND:+;${PROMPT_COMMAND}}"
EOF

RUN for bin in /usr/local/cargo/bin/*; do \
      ln -sf "${bin}" "/usr/local/bin/$(basename "${bin}")"; \
    done

RUN chmod +x /usr/local/bin/omar-entrypoint \
    && chown -R omar:omar /etc/omar /home/omar /workspace

ENV HOME=/home/omar
ENV CARGO_HOME=/home/omar/.cargo
WORKDIR /workspace
USER omar

EXPOSE 9876 9877 9878

ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/omar-entrypoint"]
CMD ["sleep", "infinity"]
