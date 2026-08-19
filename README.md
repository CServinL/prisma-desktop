# prisma-desktop

Tauri v2 + SvelteKit desktop UI for the [Prisma](https://github.com/CServinL/prisma) research assistant.

The Python backend (`prisma serve`) is a small supervisor that runs the API, Web UI, ChromaDB,
and knowledge-graph module as independent processes (ADR-012 in the `prisma` repo) —
by default: API at `localhost:8765`, Web (the built SvelteKit UI, what this window actually
loads) at `:8766`, ChromaDB at `:8767`. The SvelteKit frontend calls the API directly for all
data (notes, chat, search, streams) — still no Rust proxy in that path. The Rust shell does now
own one background piece: an optional vault-sync engine (see below) that keeps a local `.md`
copy of the vault in sync with a remote server over the LAN, for when `prisma serve` runs on a
separate machine.

---

## Prerequisites

- [Tauri v2 prerequisites](https://tauri.app/start/prerequisites/) (Rust, WebKit2GTK on Linux)
- `tauri-cli` — `cargo install tauri-cli --version "^2" --locked` (no Node/npm needed:
  this repo has no JS of its own — the SvelteKit UI lives in `prisma/ui/` and ships
  already built into `prisma serve`'s Web process; `prisma-desktop` is Rust only)
- The `prisma` Python package installed and `prisma serve` running

---

## Running locally

```bash
# terminal 1 — backend
cd ../prisma && .venv/bin/prisma serve

# terminal 2 — desktop (PATH prefix only needed if cargo came from rustup
# rather than a system package)
PATH="$HOME/.cargo/bin:$PATH" cargo tauri dev
```

First Rust compile takes ~10 minutes. Subsequent builds are incremental.

---

## Architecture

| Layer | Technology | Role |
|-------|------------|------|
| UI | SvelteKit + TypeScript | All user-facing views |
| Shell | Tauri v2 (Rust) | Window, tray, OS integration, vault-sync engine |
| Backend | Python FastAPI, supervised multi-process (`prisma serve`, ADR-012) | Vault, search, native knowledge graph (Kùzu-backed, no third-party dependency), ChromaDB, Zotero, auth |

The server URL is user-configurable in the toolbar and persisted in `localStorage`
(browser/PWA) or `settings.json` (Tauri).

---

## Vault Sync & Auth

When `prisma serve` runs on a separate machine (the "LAN server" deployment —
see `docs/wiki/deployment-models.md` in the `prisma` repo), the desktop shell
can keep a local, editable `.md` copy of the vault instead of only reaching
the server through the UI's own network calls:

- A filesystem watcher (`src-tauri/src/sync/push.rs`) pushes local
  create/edit/delete of `.md` files to the server via `PUT`/`DELETE
  /sync/file`, whole-file (not diffs).
- A persistent WebSocket connection (`sync/pull.rs`) receives `vault_change`
  push notifications from the server (Zotero auto-saves, edits from other
  clients, streams) and pulls the changed file.
- Conflicts are resolved last-write-wins by mtime; the losing version is
  kept as a `<path>.conflict-<ts>.md` sibling rather than silently discarded.
- KG (Kùzu) and ChromaDB stay 100% server-side — the desktop shell never
  runs its own; this only mirrors the plain `.md` files.

This only applies to the Tauri-wrapped desktop build — the pure browser/PWA
client (Android, iOS, or a desktop browser tab with no Tauri shell) has no
persistent local filesystem to sync into, and continues to talk to the
server directly, same as before.

Reaching a server across the LAN is gated by the server's own ADR-011
password-mode auth (`server.auth.mode = "password"` in the server's
`config.toml`) — the login screen shown when a 401 is hit calls the
`sync_login` Tauri command, which stores the session for both the sync
engine and the SvelteKit UI's own API calls from one password prompt.

---

## Status Indicators

The toolbar status popover shows live health from `GET /status`:

- **Config** — whether `config.toml` loads without errors
- **Knowledge graph** — native (Kùzu-backed) index state and last indexed time
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
