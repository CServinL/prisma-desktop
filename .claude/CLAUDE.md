# prisma-desktop

Tauri v2 + SvelteKit desktop UI for the Prisma research assistant.
The Python backend lives in the sibling `prisma/` repo (`prisma serve`).

## Before writing code

Read the domain ontology first — it is the shared contract for both repos:

```
../prisma/docs/ontologia.md          ← entity map, mechanics, axioms
../prisma/docs/concepts/<entity>.md  ← one file per concept
```

Key invariant: Zotero is the bookmark layer (stream runs write here).
The vault is the second brain (deliberate import only, via POST /zotero/import/{key}).
Do not auto-populate the vault from any automated pipeline.

## Architecture

- No sidecar, no Rust proxy. User runs `prisma serve`; Svelte calls the HTTP API directly.
- Server URL is user-configurable in the toolbar, persisted in `localStorage`.
- All domain entities (Source, Note, Stream, ZoteroItem, …) are defined in the ontology above.

## Running locally

```bash
# terminal 1 — backend
cd ../prisma && .venv/bin/prisma serve

# terminal 2 — desktop
PATH="$HOME/.cargo/bin:$PATH" PKG_CONFIG_PATH=/usr/lib/x86_64-linux-gnu/pkgconfig npm run tauri dev
```

WSLg must be enabled (`DISPLAY=:0`). First Rust compile takes ~10 min.
