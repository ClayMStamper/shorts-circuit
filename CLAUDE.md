# Shorts Circuit — agent notes

Read `docs/architecture.md` before designing anything; it is the source of truth for entities, the
publication state machine, worker guarantees, and the adapter contract. `docs/human-scope.md` lists
what only the owner can do — don't plan work that silently depends on those items.

## Commands

- `cargo fmt --all --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace`
- Frontend, in `app/`: `pnpm check`, `pnpm test`, `pnpm build`
- Tauri shell (own workspace, needs WebView libraries): `cargo clippy --manifest-path app/src-tauri/Cargo.toml --all-targets -- -D warnings`

CI runs the Rust checks on Ubuntu and Windows, the frontend checks on Ubuntu, and the Tauri shell
check on Windows. Keep all of them green.

## Rules

- Crate boundaries are deliberate: `domain` has no I/O; `store` owns SQL; `adapters` own provider
  payloads and checkpoints; `scheduler` drives jobs through traits, not concrete adapters;
  `desktop` holds application services; the Tauri shell only translates commands.
- Every state transition goes through the domain state machine; never write a status column
  directly.
- Approvals bind to an immutable publication revision. Editing media, copy, or destination creates a
  new revision and drops approval.
- A lost publish response becomes `OutcomeUnknown`, never an automatic retry.
- Store instants in UTC; keep the user's IANA timezone for recurring schedules.
- No secrets, tokens, or real account identifiers in the repo, tests, or logs. Redact credentials in
  `tracing` output.
- Provider integrations are tested against local fake servers (e.g. `wiremock`); tests never call
  real platform APIs.
- Original media files are immutable; renders and thumbnails are regenerable cache.

## Environment notes

- Cloud sessions run on Linux: build and test the Rust core, backend, and Svelte code there. Tauri
  window behavior, Windows packaging, and FFmpeg bundling for Windows are verified on the owner's
  Windows machine.
- Tests that need FFmpeg should skip with a clear message when `ffmpeg`/`ffprobe` are not on PATH.
