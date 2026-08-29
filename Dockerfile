# syntax=docker/dockerfile:1

FROM rust:1-alpine AS backend
RUN apk add --no-cache musl-dev gcc
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release

FROM node:22-alpine AS frontend
WORKDIR /app
COPY web/package.json web/package-lock.json* ./
RUN npm ci
COPY web ./
RUN npm run build

FROM ghcr.io/3n8/alpine-base-image:latest
LABEL maintainer="3n8"
LABEL org.opencontainers.image.source="https://github.com/3n8/yt-dlp-server"
LABEL org.opencontainers.image.title="yt-dlp-server"
LABEL org.opencontainers.image.description="Web and REST interface for yt-dlp"

ARG APPNAME=yt-dlp-server
ARG RELEASETAG=local
ARG TARGETARCH=amd64
ARG YDLS_VERSION=dev
ARG YDLS_RELEASE_DATE

ENV HOME=/config \
    TERM=xterm \
    LANG=en_GB.UTF-8 \
    YTDLP_BIN=/config/bin/yt-dlp \
    YDL_CONFIG_PATH=/config \
    YDL_DEFAULT_CONFIG=/usr/lib/yt-dlp-server/default_config.yml \
    YDL_STATIC_DIR=/usr/lib/yt-dlp-server/static \
    YDLS_VERSION=${YDLS_VERSION} \
    YDLS_RELEASE_DATE=${YDLS_RELEASE_DATE} \
    DOWNLOADS=/downloads \
    XDG_CONFIG_HOME=/config \
    XDG_CACHE_HOME=/config/cache \
    PATH=/config/bin:/usr/local/bin/system/scripts/docker:/usr/local/bin/run:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin

RUN apk add --no-cache ffmpeg deno ca-certificates curl wget

RUN mkdir -p /usr/lib/yt-dlp-server /config/bin /config/cache /downloads && \
    if [ "$TARGETARCH" = "arm64" ] || [ "$TARGETARCH" = "aarch64" ]; then \
      curl -fsSL -o /usr/lib/yt-dlp-server/yt-dlp \
        https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp_musllinux_aarch64; \
    else \
      curl -fsSL -o /usr/lib/yt-dlp-server/yt-dlp \
        https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp_musllinux; \
    fi && \
    chmod 755 /usr/lib/yt-dlp-server/yt-dlp && \
    chmod 777 /config /downloads /usr/lib/yt-dlp-server

COPY --from=backend /src/target/release/yt-dlp-server /usr/local/bin/yt-dlp-server
COPY --from=frontend /app/dist /usr/lib/yt-dlp-server/static
COPY config.yml /usr/lib/yt-dlp-server/default_config.yml
COPY build/common/root/init.sh /usr/bin/init.sh
COPY build/common/root/supervisord.conf /etc/supervisord.conf
COPY build/common/root/yt-dlp-server.conf /etc/supervisor/conf.d/yt-dlp-server.conf

RUN chmod 755 /usr/local/bin/yt-dlp-server /usr/bin/init.sh && \
    echo "export BASE_RELEASE_TAG=${RELEASETAG}" >> /etc/image-build-info && \
    echo "export TARGETARCH=${TARGETARCH}" >> /etc/image-build-info && \
    echo "export APPNAME=${APPNAME}" >> /etc/image-build-info && \
    echo "export YDLS_VERSION=${YDLS_VERSION}" >> /etc/image-build-info

EXPOSE 8080
VOLUME ["/config", "/downloads"]

ENTRYPOINT ["/usr/bin/dumb-init", "--"]
CMD ["/usr/bin/init.sh"]
