# yt-dlp-server

Web UI and REST API for downloading videos onto a server with [yt-dlp](https://github.com/yt-dlp/yt-dlp).

This is a Rust rewrite of the idea behind [nbr23/youtube-dl-server](https://github.com/nbr23/youtube-dl-server) (MIT), aimed at:

- running as a non-root user via docker-compose `user: "${PUID}:${PGID}"`
- Alpine Linux (`ghcr.io/3n8/alpine-base-image`)
- updating yt-dlp from the webpage without rebuilding the image

The container **refuses to start as root**. Use the `user:` directive.

## Run

Pull `ghcr.io/3n8/yt-dlp-server:latest`. There is no image `build:` — same as the other 3n8 containers. UID/GID come from compose `user:`, not `PUID`/`PGID` env vars.

```yaml
services:
  youtube-dl:
    image: ghcr.io/3n8/yt-dlp-server:latest
    hostname: youtube-dl-server.docker
    container_name: yt-dlp-server
    restart: unless-stopped
    user: "${PUID}:${PGID}"
    networks:
      - frontend
    ports:
      - ${HOST_IP}:8085:8080
    cpus: 8
    mem_limit: 4G
    environment:
      - TZ=${TZ}
      - DOWNLOADS=/downloads
    volumes:
      - ${DOCKER_HOME}/youtube-dl:/config
      - /mnt/mergerfs/media/youtube-dl:/downloads
    dns:
      - ${DNS1}
      - ${DNS2}

networks:
  frontend:
    external: true
```

Open `http://${HOST_IP}:8085/`.

### Environment

| Variable | Default | Meaning |
|----------|---------|---------|
| `TZ` | UTC | Timezone |
| `DOWNLOADS` | `/downloads` | Directory **inside the container** where files are written |

`DOWNLOADS` is not a host path. The volume mount target must be the same path. Example that writes onto a media volume mounted at `/mnt/media`:

```yaml
environment:
  - TZ=${TZ}
  - DOWNLOADS=/mnt/media/youtube-dl
volumes:
  - ${DOCKER_HOME}/youtube-dl:/config
  - media:/mnt/media
```

If `DOWNLOADS` is unset or ignored, files land in `/downloads` (or `/data` on old config) and will not show up on the host unless that path is mounted.

### Volumes

| Path | Role |
|------|------|
| `/config` | `config.yml`, jobs database, yt-dlp binary, cache, logs |
| whatever `DOWNLOADS` is | downloaded files (default `/downloads`) |

### yt-dlp on Alpine

The image does **not** install yt-dlp from apk or pip. It uses the official musllinux release binary (`yt-dlp_musllinux` / `yt-dlp_musllinux_aarch64`), which supports `yt-dlp -U`.

On first start the seed binary is copied to `/config/bin/yt-dlp`. The footer **Update yt-dlp** button runs `yt-dlp --update-to <channel>` against that file, so updates survive container recreation. Default channel is `nightly` (see yt-dlp docs). ffmpeg and deno are included.

## Config

Copied to `/config/config.yml` on first run if missing. Same YAML shape as nbr23: `ydl_server`, `ydl_options` (flags without the leading `--`), `profiles`, `aliases`.

Set `ydl_server.update_channel` to `nightly`, `stable`, or `master`.

## API

Compatible with the nbr23 endpoints (`POST /api/downloads`, logs, jobs, finished files, metadata, formats, extractors). Extra:

- `POST /api/yt-dlp/update` — queue an update, returns `{ job_id }`
- `GET /api/jobs/{id}/events` — SSE (`log`, `done`) used by the footer progress bar

Bookmarklet (HTTPS):

```javascript
javascript:fetch("https://${host}/api/downloads",{body:JSON.stringify({url:window.location.href}),method:"POST",headers:{'Content-Type':'application/json'}});
```

## Build from source

```bash
cargo build --release
cd web && npm ci && npm run build
YDL_STATIC_DIR=web/dist YTDLP_BIN=$(which yt-dlp) YDL_CONFIG_PATH=./config.yml cargo run --release
```

## License

MIT. UI/API inspired by nbr23/youtube-dl-server (MIT). yt-dlp is Unlicense; the bundled musllinux binary includes third-party code (see yt-dlp release notes).
