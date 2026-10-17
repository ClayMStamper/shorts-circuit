# Shorts Circuit

Local-first desktop app for curating short-form videos and publishing them to YouTube Shorts,
Instagram Reels, Snapchat Spotlight, and LinkedIn. One source video becomes several independently
managed publications, each with its own variant, caption, schedule, approval, and upload state.

Status: early development. See [docs/architecture.md](docs/architecture.md) for the design and
[docs/human-scope.md](docs/human-scope.md) for setup steps that need a person.

## Layout

```
crates/
  domain/      entities, publication state machine, revisions and approvals
  store/       SQLite persistence (SQLx, migrations, WAL)
  media/       FFprobe inspection, render recipes, FFmpeg rendering and cache
  scheduler/   durable jobs: leases, heartbeats, retries, checkpoints, recovery
  adapters/    platform adapter contract and per-provider implementations
  backend/     Axum OAuth broker and (later) cloud runner
app/           Tauri 2 + Svelte desktop shell (not yet created)
```

## Development

Requires Rust stable. The desktop app additionally needs Node.js, pnpm, and FFmpeg/FFprobe on PATH.

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## License

MIT
