# prisma-desktop

Thin Tauri v2 shell for Prisma on Linux and Windows/WSL2.

The UI source lives in the sibling `prisma/ui/` directory.
The Python backend lives in the sibling `prisma/` repo (`prisma serve`).

Tauri is the "PWA runtime" for Linux/WSL2 — platforms that don't support native PWA install.
It opens a native window pointed at `http://127.0.0.1:8765/app` (served by `prisma serve`).
On Android, iOS, and macOS the same URL is used directly in the browser as a PWA.

## What lives here

- `src-tauri/` — Rust code: window management, settings persistence, WSL2-aware URL opener
- `src-tauri/tauri.conf.json` — window config, CSP, icons
- No SvelteKit source — that is in `prisma/ui/`

## Running locally

```bash
# terminal 1 — backend + UI
cd ../prisma
.venv/bin/prisma serve        # serves API on :8765 and UI at :8765/app

# terminal 2 — Tauri shell
PATH="$HOME/.cargo/bin:$PATH" npm run tauri dev
```

The Tauri shell loads `http://127.0.0.1:8765/app` — no Vite dev server needed.

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

| File | Type | What it shows |
|------|------|---------------|
| `01_system_topology.html` | SystemMap | Clients, Tauri shell internals, server, UI pipeline |
| `02_deployment.html` | SystemMap | Physical processes: WSL2, Windows host, internet |
| `03a_open_stream.html` | SequenceMap | User opens a research stream in the UI |
| `03b_vault_search.html` | SequenceMap | Fast + deep vault search flows |
| `03c_dev_hot_reload.html` | SequenceMap | Dev workflow: edit → auto-rebuild → browser reload |
