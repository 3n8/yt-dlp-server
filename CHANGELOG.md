# Changelog

All notable changes to this project are documented here.

## [0.1.1] - 2026-08-29

### Changed

- Download directory is `/downloads` (was `/data`).
- Set `DOWNLOADS` to change that path at runtime; default config `output` paths under `/downloads` or `/data` follow it.

## [0.1.0] - 2026-08-29

### Added

- Initial rewrite of a yt-dlp web/REST server in Rust.
- Alpine image based on `ghcr.io/3n8/alpine-base-image`, intended to run with docker-compose `user: "${PUID}:${PGID}"`.
- Official musllinux yt-dlp binary (not apk/pip), seeded into `/config/bin` so in-container updates persist.
- Web UI: download form, formats, profiles, aliases, inspect/metadata, job logs, finished-file browser, ffmpeg cut, bookmarklet.
- Footer **Update yt-dlp** button with progress bar and success/failure confirmation.
- Nightly update channel by default (`ydl_server.update_channel`).
- SQLite job queue shared by downloads, ffmpeg cuts, and yt-dlp updates.
- Scheduled upcoming live/premiere jobs.

UI and API shape follow [nbr23/youtube-dl-server](https://github.com/nbr23/youtube-dl-server) (MIT).
