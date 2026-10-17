# Shorts Circuit

Local-first desktop app for curating short-form videos and publishing them to YouTube Shorts,
Instagram Reels, Snapchat Spotlight, and LinkedIn. One source video becomes several independently
managed publications, each with its own variant, caption, schedule, approval, and upload state.

Status: early development (Increment 2, desktop workflow). Every destination currently publishes
through an export package; platform API publishing arrives in Increment 3. See
[docs/architecture.md](docs/architecture.md) for the design and
[docs/human-scope.md](docs/human-scope.md) for setup steps that need a person.

## Layout

```
crates/
  domain/      entities, publication state machine, revisions and approvals
  store/       SQLite persistence (SQLx, migrations, WAL)
  media/       FFprobe inspection, render recipes, FFmpeg rendering and cache
  scheduler/   durable jobs: leases, heartbeats, retries, checkpoints, recovery
  adapters/    platform adapter contract, manual-handoff exporter, TikTok Display API and broker clients
  desktop/     application services behind the UI: import, library, review, scheduling, job handlers
  backend/     Axum OAuth broker (TikTok code exchange and refresh) and (later) cloud runner
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

Optional TikTok new-post detection needs the OAuth broker (`cargo run -p shorts-backend`, configured
with `TIKTOK_CLIENT_KEY`, `TIKTOK_CLIENT_SECRET` and `TIKTOK_REDIRECT_URI`) and the desktop app
started with `SHORTS_BROKER_URL` pointing at it. Without them the feature shows as unavailable;
see [docs/human-scope.md](docs/human-scope.md) for registering the TikTok developer app.

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
