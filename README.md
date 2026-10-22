# Shorts Circuit

Local-first desktop app that republishes your TikTok-origin short videos to YouTube Shorts,
Instagram Reels, Snapchat Spotlight, and LinkedIn. Connect your accounts once, choose the folder
your videos arrive in, and let them flow: each **workflow** ("TikTok → YouTube Shorts") renders,
captions (from the TikTok post or a caption file, fitted to each platform) and publishes every
new video automatically. Review is an optional per-workflow setting.

Status: YouTube publishing is verified with a real upload; the Instagram, LinkedIn and Snapchat
connectors are built against their docs and local fakes and await a first real post (Snapchat
also awaits Snap's allowlisting). An optional cloud runner posts while the PC is off. See
[docs/architecture.md](docs/architecture.md) for the design (including "Workflows UX") and
[docs/human-scope.md](docs/human-scope.md) for the one-time setup and how to test it.

## Using it

1. Start the app. The setup guide asks for the **source** (your watch folder, and optionally
   TikTok for captions), then each **destination** (a card per platform with the console steps),
   then suggests one **workflow** per connected destination with Auto Publish on.
2. Save videos to the watch folder. **Workflows** shows each workflow's status and item count;
   open one for its content list and settings. **Activity** lists every video with each
   destination's status. **Connections** holds the accounts; **Settings** the rarely used
   options and, under Advanced, the library, collections and job monitor.

To preview the UI in a browser with made-up data: `cd app && pnpm dev`, then open
`http://localhost:1420/?mock` (or `?mock=empty` for the first-run guide).

## Layout

```
crates/
  domain/      entities, publication state machine, revisions and approvals
  store/       SQLite persistence (SQLx, migrations, WAL)
  media/       FFprobe inspection, render recipes, FFmpeg rendering and cache
  scheduler/   durable jobs: leases, heartbeats, retries, checkpoints, recovery (over a JobQueue trait)
  adapters/    platform adapter contract, manual-handoff exporter, TikTok Display API, sign-in and broker clients
  publish/     the publish pipeline over the adapter contract, shared by the desktop and the runner
  desktop/     application services behind the UI: workflows, import, library, review, scheduling, job handlers
  cloud/       desktop ↔ cloud runner sync protocol and HTTPS client
  runner/      optional cloud runner: PostgreSQL, encrypted credentials, sync API (docs/cloud-runner.md)
  backend/     Axum OAuth broker (TikTok code exchange and refresh)
app/           Svelte + TypeScript frontend (pnpm)
app/src-tauri/ Tauri 2 shell (separate Cargo workspace; needs the platform WebView)
```

## Development

Requires Rust stable. The desktop app additionally needs Node.js 22, pnpm 10, and FFmpeg/FFprobe.

```bash
# Rust core (tests that need FFmpeg skip when it is not on PATH)
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace

# Frontend
cd app
pnpm install
pnpm check && pnpm test && pnpm build

# Desktop app (Windows: WebView2; Linux: webkit2gtk-4.1 dev packages)
pnpm tauri dev
```

FFmpeg is found in this order: `SHORTS_FFMPEG` / `SHORTS_FFPROBE`, `ffmpeg(.exe)` / `ffprobe(.exe)`
next to the app executable (where a packaged build should ship them), then `PATH`. When it is
missing the app still starts and shows a banner.

Optional TikTok new-post detection (captions from your posts) uses your own TikTok developer app:
paste its client key and secret in Connections → Source; the app signs in through a loopback
redirect (`http://127.0.0.1:47616/callback/`). The OAuth broker (`cargo run -p shorts-backend`
with `TIKTOK_CLIENT_KEY`, `TIKTOK_CLIENT_SECRET`, `TIKTOK_REDIRECT_URI`, and the app started with
`SHORTS_BROKER_URL`) remains an alternative. See [docs/human-scope.md](docs/human-scope.md) for
registering the TikTok developer app.

The optional cloud runner (`crates/runner`, posting while the PC is off) needs PostgreSQL; its
tests run when `DATABASE_URL` points at one (e.g.
`postgres://postgres:postgres@localhost/shorts_runner_test`) and skip otherwise. Hosting it is
described in [docs/cloud-runner.md](docs/cloud-runner.md).

### Building the Windows installer

The release build bundles `ffmpeg.exe` and `ffprobe.exe` next to the app (Tauri sidecars) and
produces an NSIS installer that installs for the current user without admin rights.

```powershell
# FFmpeg for the bundle: download a checked Gyan.dev build, or copy a local one
./scripts/fetch-ffmpeg.ps1
./scripts/fetch-ffmpeg.ps1 -FromDir "C:\ffmpeg\bin"

cd app
pnpm tauri build --config src-tauri/tauri.release.conf.json
# → app/src-tauri/target/release/bundle/nsis/Shorts Circuit_<version>_x64-setup.exe
```

The **Release** GitHub workflow does the same on `windows-latest`: run it by hand for a test
build (the installer is a workflow artifact), or push a tag `v<version>` matching
`tauri.conf.json` to attach the installer to a draft GitHub release. Builds are unsigned for
now; see [docs/human-scope.md](docs/human-scope.md) ("Releases").

Keep `@tauri-apps/api` and `@tauri-apps/plugin-dialog` on the same minor version as the `tauri`
and `tauri-plugin-dialog` crates in `app/src-tauri/Cargo.lock`; `tauri build` refuses to run
when they differ.

App data (library database, managed originals, render cache, default export folder) lives in the
platform's local app-data folder, e.g. `%LOCALAPPDATA%\io.github.claymstamper.shorts-circuit`.

## License

MIT
