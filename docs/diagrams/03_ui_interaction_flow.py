"""prisma-desktop — UI interaction sequences.

Run: .venv/bin/python docs/diagrams/03_ui_interaction_flow.py

Produces three separate HTML files — one per flow — because SequenceMap
does not support save_collection (SystemMap-only limitation in sysatlas).

  03a_open_stream.html  — user opens a research stream
  03b_vault_search.html — fast + deep vault search
  03c_dev_hot_reload.html — dev edit → auto-rebuild → browser reload
"""
from pathlib import Path
from sysatlas import SequenceMap

BASE = Path(__file__).parent

# ── Flow 1: open a research stream ────────────────────────────────────────────
s1 = SequenceMap(title="UI flow — open research stream")

s1.actor("user",       kind="actor",    label="User")
s1.actor("svelte",     kind="boundary", label="+page.svelte")
s1.actor("api",        kind="system",   label="FastAPI :8765")
s1.actor("stream_mgr", kind="control",  label="StreamManager")
s1.actor("zotero_svc", kind="system",   label="ZoteroService")

s1.send("user",       "svelte",      label="click stream in tree",          order=1)
s1.send("svelte",     "api",         label="GET /streams/{slug}/view",      order=2)
s1.send("api",        "stream_mgr",  label="get_stream(slug)",              order=3)
s1.send("stream_mgr", "api",         label="RenderedNode (HTML + collection_key)", order=4, kind="reply")
s1.send("api",        "svelte",      label="RenderedNode JSON",             order=5, kind="reply")
s1.send("svelte",     "svelte",      label="render HTML + show zoteroLoading overlay", order=6, kind="async")
s1.send("svelte",     "api",         label="GET /zotero/collections",       order=7)
s1.send("api",        "zotero_svc",  label="get_collections()",             order=8)
s1.send("zotero_svc", "api",         label="ZoteroCollection[]",            order=9, kind="reply")
s1.send("api",        "svelte",      label="collections JSON",              order=10, kind="reply")
s1.send("svelte",     "api",         label="GET /zotero/collections/{key}/items", order=11)
s1.send("api",        "zotero_svc",  label="get_items(collection_key)",     order=12)
s1.send("zotero_svc", "api",         label="ZoteroItem[]",                  order=13, kind="reply")
s1.send("api",        "svelte",      label="items JSON",                    order=14, kind="reply")
s1.send("svelte",     "user",        label="stream content + Zotero sidebar", order=15, kind="reply")

s1.save(str(BASE / "03a_open_stream.html"))
print("[sysatlas] wrote 03a_open_stream.html")

# ── Flow 2: vault search ──────────────────────────────────────────────────────
s2 = SequenceMap(title="UI flow — vault search")

s2.actor("user",     kind="actor",    label="User")
s2.actor("svelte",   kind="boundary", label="+page.svelte")
s2.actor("api",      kind="system",   label="FastAPI :8765")
s2.actor("chroma",   kind="system",   label="ChromaDB")
s2.actor("graphify", kind="system",   label="Graphify index")

s2.send("user",    "svelte",   label="type query (debounced 300ms)",    order=1)
s2.frame("opt", start_order=2, end_order=5, label="fast search (instant)")
s2.send("svelte",  "api",      label="GET /search?q=...",               order=2)
s2.send("api",     "svelte",   label="SearchResult[] (in-memory idx)",  order=3, kind="reply")
s2.frame("opt", start_order=4, end_order=9, label="deep search (semantic)")
s2.send("svelte",  "api",      label="GET /search/deep?q=...",          order=4)
s2.send("api",     "chroma",   label="query(q, top_k=60)",              order=5, kind="async")
s2.send("chroma",  "api",      label="chunk matches",                    order=6, kind="reply")
s2.send("api",     "graphify", label="node_titles(chunk_files)",        order=7)
s2.send("graphify","api",      label="title boosts",                     order=8, kind="reply")
s2.send("api",     "svelte",   label="SearchResult[] (re-ranked top 20)", order=9, kind="reply")
s2.send("svelte",  "user",     label="results list",                    order=10, kind="reply")

s2.save(str(BASE / "03b_vault_search.html"))
print("[sysatlas] wrote 03b_vault_search.html")

# ── Flow 3: dev hot-reload ────────────────────────────────────────────────────
s3 = SequenceMap(title="UI flow — dev hot-reload")

s3.actor("dev",      kind="actor",    label="Developer")
s3.actor("editor",   kind="boundary", label="File editor")
s3.actor("watcher",  kind="control",  label="UI watcher thread")
s3.actor("npm",      kind="system",   label="npm run build")
s3.actor("svelte",   kind="boundary", label="Browser / Tauri WebView")
s3.actor("api",      kind="system",   label="GET /ui/dev/version")

s3.send("dev",     "editor",   label="edit ui/src/*.svelte",             order=1)
s3.send("editor",  "watcher",  label="mtime change detected (1s poll)",  order=2, kind="async")
s3.send("watcher", "npm",      label="npm run build (cwd: ui/)",         order=3)
s3.send("npm",     "watcher",  label="exit 0",                            order=4, kind="reply")
s3.send("watcher", "api",      label="version++ (state dict)",            order=5, kind="async")
s3.frame("loop", start_order=6, end_order=8, label="every 2s")
s3.send("svelte",  "api",      label="GET /ui/dev/version",              order=6)
s3.send("api",     "svelte",   label="{version: N}",                     order=7, kind="reply")
s3.send("svelte",  "svelte",   label="version changed → window.location.reload()", order=8, kind="async")

s3.save(str(BASE / "03c_dev_hot_reload.html"))
print("[sysatlas] wrote 03c_dev_hot_reload.html")
