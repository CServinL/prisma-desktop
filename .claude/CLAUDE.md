# prisma-desktop

Thin Tauri v2 shell for Prisma on Linux and Windows/WSL2.

Read before modularization/refactoring work:
- `docs/software-engineering-quality-aspects.md` — rubric to evaluate against (also loaded globally, see `~/.claude/CLAUDE.md`)

The UI source lives in the sibling `prisma/ui/` directory.
The Python backend lives in the sibling `prisma/` repo (`prisma serve`).

Tauri is the "PWA runtime" for Linux/WSL2 — platforms that don't support native PWA install.
It opens a native window pointed at `http://127.0.0.1:8766/app` — the Web process
(served by `prisma serve`'s supervisor; see ADR-012 in the `prisma` repo). REST/WebSocket
calls go to the separate API process at `:8765` (configurable as "Server URL" in Settings).
On Android, iOS, and macOS the same URL is used directly in the browser as a PWA.

## What lives here

- `src-tauri/src/settings.rs` / `window.rs` — window management, settings persistence, WSL2-aware URL opener
- `src-tauri/src/auth/` — ADR-011 password-mode login (`sync_login`/`sync_logout`/`sync_status` commands), `auth.json` session store
- `src-tauri/src/sync/` — vault-sync engine: fs-watcher push (`push.rs`), WebSocket pull (`pull.rs`), initial-reconciliation diff (`manifest.rs`) — see README's "Vault Sync & Auth" section for the design
- `src-tauri/tauri.conf.json` — window config, CSP, icons
- No SvelteKit source — that is in `prisma/ui/`

## Running locally

```bash
# terminal 1 — backend + UI
cd ../prisma
.venv/bin/prisma serve        # supervisor: API :8765, Web/UI :8766, ChromaDB :8767

# terminal 2 — Tauri shell (tauri-cli via `cargo install tauri-cli --version "^2"`,
# no Node/npm needed here — this repo is Rust only)
PATH="$HOME/.cargo/bin:$PATH" cargo tauri dev
```

The Tauri shell loads `http://127.0.0.1:8766/app` — no Vite dev server needed.

## Building the UI

```bash
cd ../prisma/ui
npm install
npm run build      # output → prisma/ui/build/
```

Then restart `prisma serve` — it mounts `ui/build/` at `/app` automatically.

## Before opening a PR

Regenerate all diagrams:

```bash
bash docs/diagrams/gen.sh
```

Diagrams live in `docs/diagrams/`. Include updated HTML files in the PR — reviewing them is part of the PR checklist:

| File | Views | What it shows |
|------|-------|---------------|
| `01_system_topology.html` | System topology, UI pipeline | Clients, Tauri internals, server, UI build pipeline |
| `02_deployment.html` | Deployment, Network | Physical processes (WSL2/Windows) + port/protocol map |
| `03a_open_stream.html` | — | User opens a research stream (sequence) |
| `03b_vault_search.html` | — | Fast + deep vault search flows (sequence) |
| `03c_dev_hot_reload.html` | — | Edit → rebuild → browser reload (sequence) |

Note: `SequenceMap` uses a separate renderer from `SystemMap` and cannot be combined into a multi-view file. The three sequence diagrams remain separate files — tracked as a sysatlas 0.4.0 improvement.
