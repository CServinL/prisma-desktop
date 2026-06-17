# prisma-desktop

Tauri v2 + SvelteKit desktop UI for the [Prisma](https://github.com/CServinL/prisma) research assistant.

The Python backend (`prisma serve`) runs as a local HTTP server on `localhost:7799`. The frontend calls it directly — no Rust proxy, no sidecar.

---

## Prerequisites

- [Tauri v2 prerequisites](https://tauri.app/start/prerequisites/) (Rust, WebKit2GTK on Linux)
- Node.js ≥ 20
- The `prisma` Python package installed and `prisma serve` running

---

## Running locally

```bash
# terminal 1 — backend
cd ../prisma && .venv/bin/prisma serve

# terminal 2 — desktop (WSLg: ensure DISPLAY=:0)
PATH="$HOME/.cargo/bin:$PATH" PKG_CONFIG_PATH=/usr/lib/x86_64-linux-gnu/pkgconfig npm run tauri dev
```

First Rust compile takes ~10 minutes. Subsequent builds are incremental.

---

## Architecture

| Layer | Technology | Role |
|-------|------------|------|
| UI | SvelteKit + TypeScript | All user-facing views |
| Shell | Tauri v2 (Rust) | Window, tray, OS integration |
| Backend | Python FastAPI (`prisma serve`) | Vault, search, Graphify, ChromaDB, Zotero |

The server URL is user-configurable in the toolbar and persisted in `localStorage`.

---

## Status Indicators

The toolbar status popover shows live health from `GET /status`:

- **Config** — whether `config.yaml` loads without errors
- **Graphify** — knowledge graph index state and last indexed time
- **Chroma** — ChromaDB chunk count, files indexed, embedding model
- **Vault** — note/source/chat/stream counts and vault root
- **Zotero** — connection mode and availability

---

## Domain Ontology

All entities (Source, Note, Stream, ZoteroItem, …) are defined in the shared ontology:

```
../prisma/docs/ontologia.md
../prisma/docs/concepts/<entity>.md
```

Key invariant: Zotero is the bookmark layer (stream runs write here). The vault is the second brain — deliberate import only.
