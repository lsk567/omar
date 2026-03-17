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
        procps \
        python3 \
        tmux \
        tini \
        xauth \
        xdotool \
    && rm -rf /var/lib/apt/lists/*

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
        procps \
        python3 \
        tmux \
        tini \
        xauth \
        xdotool \
    && rm -rf /var/lib/apt/lists/*

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
