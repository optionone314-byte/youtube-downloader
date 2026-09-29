# Video Downloader

A desktop video downloader built with **Rust + Tauri 2**, powered by
[yt-dlp](https://github.com/yt-dlp/yt-dlp) — so it supports **1800+ sites**,
not just YouTube.

![Tauri](https://img.shields.io/badge/Tauri-2-blue) ![Rust](https://img.shields.io/badge/Rust-stable-orange)

## Features

- **Any site** — YouTube, Instagram, TikTok, X/Twitter, Facebook, Vimeo, Twitch,
  SoundCloud, and ~1800 more.
- **Every format listed** — resolution, container, codecs, FPS and size, with
  filter tabs and sorting.
- **Video + audio** — video-only streams are merged with the best audio track
  into a single MP4 automatically (via ffmpeg).
- **Playlists** — expand the list, pick which videos you want, or take all of
  them. Downloads run as one sequential batch.
- **Live progress** — percentage, speed, ETA, per-download cancel.
- **Automatic retry** — YouTube regularly 403s individual stream URLs; the app
  silently retries with an alternate source.
- **Browser cookies** — optional, for sites that hide media unless you're
  logged in (Instagram, X, Facebook). Read locally, never uploaded.

## Requirements

- Windows / macOS / Linux desktop
- [ffmpeg](https://ffmpeg.org/) on your `PATH` (needed to merge streams and
  extract audio). Windows: `winget install Gyan.FFmpeg`
- The `yt-dlp` engine (~17 MB) is downloaded automatically on first run.

## Build from source

```bash
# install deps
npm install          # only needed if you add a frontend toolchain

# development
cargo tauri dev

# release installer (Windows/macOS/Linux)
cargo tauri build
```

Layout:

```
src/                 frontend (plain HTML/CSS/JS, no bundler)
src-tauri/src/lib.rs backend: commands, download engine, playlist logic
src-tauri/tauri.conf.json
```

## Android

```bash
cargo tauri android init      # once
cargo tauri android build --apk
```

GitHub Actions builds the APK on every push (`.github/workflows/android.yml`)
and Windows installers in `windows.yml`.

**Android status — read this.** The UI is responsive and the APK builds, but
the *download engine is a desktop binary*: `yt-dlp` cannot execute on Android
without a Termux/rooted environment. So on Android you get a working shell of
the app; actual downloading needs either a Termux setup or a native Rust
re-implementation of the extractor/merger. That port is the honest next step if
you want a real SnapTube replacement on the phone.

## Legal

Downloads only work for content you have the right to download. Respect
copyright and each site's terms of service. DRM-protected and paid
members-only content is not supported.
