---
name: run-re-nndd
description: Build, launch, screenshot, and drive the Re:NNDD desktop app (Tauri 2 + Rust + Svelte 5). Use when asked to run / start / build / screenshot Re:NNDD, drive its UI, click through its pages, or verify a change in the real running app (not just tests). Also covers cargo test (Rust backend) and npm test (Svelte frontend).
---

Re:NNDD is a **Tauri 2 desktop app** (Rust backend in `src-tauri/`, Svelte 5
frontend in `src/`, bundled SQLite). It opens a WebKitGTK window, so a headless
agent drives it through **`.claude/skills/run-re-nndd/driver.mjs`** — a
zero-dependency WebDriver client that spawns `tauri-driver`, launches the real
app binary under `xvfb-run`, and screenshots / clicks / evals the live UI.
Backend logic is exercised with `cargo test`; the frontend with `npm test`.

All paths below are relative to the repo root (the app == the repo).

## Prerequisites

Ubuntu/Debian. The WebKitGTK dev stack + the WebDriver + xvfb + a screenshot tool:

```bash
sudo apt-get update
sudo apt-get install -y --no-install-recommends \
  libwebkit2gtk-4.1-dev libsoup-3.0-dev libjavascriptcoregtk-4.1-dev \
  libayatana-appindicator3-dev librsvg2-dev libgtk-3-dev libssl-dev \
  build-essential pkg-config \
  webkit2gtk-driver xvfb imagemagick
```

Runtimes: Node 20+ and Rust **stable ≥ 1.96** (see Setup — 1.94 will not build).
`tauri-driver` is provided by `WebKitWebDriver` from `webkit2gtk-driver` above.

## Setup

```bash
npm install

# Tauri's externalBin requires the sidecar files to physically exist or the
# build script aborts (even for --no-bundle / clippy / test). Stub them:
TRIPLE=$(rustc -vV | awk '/^host:/{print $2}')   # x86_64-unknown-linux-gnu
mkdir -p src-tauri/binaries
for n in yt-dlp ffmpeg; do f="src-tauri/binaries/$n-$TRIPLE"; : > "$f"; chmod +x "$f"; done

# rusqlite 0.40 pulls libsqlite3-sys 0.38, whose build.rs uses the `cfg_select`
# macro — unstable on Rust 1.94, stable on 1.96. Upgrade or the build dies with
# E0658. (This box shipped 1.94; `rustup update stable` brought 1.96.)
rustup update stable

# The WebDriver intermediary the driver spawns:
cargo install tauri-driver --locked
```

`yt-dlp` / `ffmpeg` are optional — the app resolves them to "not found" and runs
fine (only downloads are disabled). For real downloads: `bash scripts/fetch-binaries.sh`.

## Build

```bash
npx tauri build --debug --no-bundle
```

Runs `npm run build` then a debug `cargo build`, emitting the binary at
**`target/debug/nndd-next`** (workspace root `target/`, NOT `src-tauri/target/`).
`--no-bundle` skips packaging (no real sidecars needed); `--debug` skips release LTO.
First build is ~5–10 min cold.

## Run (agent path) — drive the live app

The driver launches the real app under Xvfb and screenshots all nine sidebar
routes. The Xvfb default screen (1280x1024x24) is fine; the WebKitGTK
software-render env vars are baked into the driver.

```bash
xvfb-run -a node .claude/skills/run-re-nndd/driver.mjs tour
```

Output: PNGs in `/tmp/re-nndd-shots/` (`00-home.png` … `08-settings.png`). Then
**look at them** (Read the PNG) — a real render shows the dark sidebar + cards +
"アプリバージョン 0.1.0" (proves the Tauri IPC reached the live Rust backend).

| command                               | what it does                                                          |
| ------------------------------------- | --------------------------------------------------------------------- |
| `node driver.mjs tour [outDir]`       | screenshot every route → `outDir` (default `/tmp/re-nndd-shots`)      |
| `node driver.mjs shot <route> <file>` | navigate to one route, write one PNG (e.g. `shot /search /tmp/s.png`) |
| `node driver.mjs eval <route> '<js>'` | navigate, run JS in the webview, print the JSON result                |

Example — read state out of the live DOM (returns `"0.1.0"`):

```bash
xvfb-run -a node .claude/skills/run-re-nndd/driver.mjs eval / 'document.querySelector(".env dd")?.textContent'
```

Env knobs: `APP_BIN` (binary path), `OUT_DIR`, `PORT` (tauri-driver, default 4444),
`NO_VITE=1` (skip the defensive Vite launch — see Gotchas), `REUSE_VITE=1` (reuse a
server already on :1420 instead of failing closed).

## Test

```bash
cargo test --workspace        # Rust backend — 233 tests (api, library/SQLite, downloader, plugins)
npm test                      # Svelte frontend (vitest) — 135 tests
```

⚠ Run `cargo test`/`cargo build` **before** the final `npx tauri build` — see Gotchas.

## Run (human path)

```bash
npm run tauri:dev   # Vite + a real window. Useless headless; Ctrl-C to stop.
```

## Gotchas

- **A debug `cargo build`/`cargo test` clobbers the app binary into dev mode.**
  Tauri debug builds default to loading `devUrl` (`http://localhost:1420`); only
  `npx tauri build` bakes the frontend in. So after `cargo test --workspace`
  rebuilds `nndd-next`, launching it shows a blank page reading **"Could not
  connect to localhost: Connection refused."** The driver defends against this by
  starting its own Vite (from this repo) on :1420 before launching — a prod binary
  ignores it, a dev binary loads it, both render. If :1420 is already occupied it
  fails closed (it can't prove a stranger's server is this checkout); free it,
  `REUSE_VITE=1` to use it anyway, or `NO_VITE=1` for a prod binary. For the pure
  prod path, rebuild with `npx tauri build --debug --no-bundle` and pass `NO_VITE=1`.
- **WebKitGTK is blank without software-render env vars under Xvfb.** The driver
  exports `WEBKIT_DISABLE_COMPOSITING_MODE=1`, `WEBKIT_DISABLE_DMABUF_RENDERER=1`,
  `LIBGL_ALWAYS_SOFTWARE=1`, `GDK_BACKEND=x11`. If you launch the binary yourself
  (not via the driver), set them or you get a 4KB all-black screenshot.
- **The binary is at the workspace root `target/`, not `src-tauri/target/`** — it's
  a cargo workspace.
- **`externalBin` blocks every cargo invocation, not just bundling.** Without the
  stub files (Setup) even `cargo test` / `cargo clippy` abort.
- **One WebDriver session at a time.** The driver launches its own app instance;
  don't have another `nndd-next` running against the same `library.db`.

## Troubleshooting

- **`error[E0658]: use of unstable library feature 'cfg_select'` in
  `libsqlite3-sys-0.38.0/build.rs`**: toolchain too old. `rustup update stable`
  (needs ≥ 1.96). `RUSTC_BOOTSTRAP=1` does NOT fix it.
- **`failed to build app` / `externalBin ... not found`**: missing
  `src-tauri/binaries/{yt-dlp,ffmpeg}-<triple>` stubs — see Setup.
- **Driver error `selector not found in 45000ms: .brand`**: the webview never
  rendered. Usually the dev-mode-binary issue above (check `/tmp/re-nndd-vite.log`
  and that the driver printed "Vite up on :1420"), or the software-render env vars
  are missing.
- **`tauri-driver: command not found`**: `cargo install tauri-driver --locked`
  and ensure `~/.cargo/bin` is on `PATH`.
- **Orphaned `vite` / `nndd-next` after a crash**: `pkill -f node_modules/.bin/vite; pkill -f target/debug/nndd-next`.
