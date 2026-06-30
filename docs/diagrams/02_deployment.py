"""prisma-desktop — deployment view (what runs where).

Run: .venv/bin/python docs/diagrams/02_deployment.py

Physical/process boundary view:
- WSL2 (Linux): prisma Python process, Tauri shell process
- Windows host: Ollama (GPU), Zotero Desktop
- Network: WSL2 ↔ Windows eth0 bridge
"""
from pathlib import Path
from sysatlas import SystemMap

OUT = Path(__file__).with_suffix(".html")

m = SystemMap(title="prisma-desktop — deployment view")

# WSL2 Linux environment
m.add_component("tauri_proc",   label="Tauri process",        layer="wsl2",  group="WSL2 (Linux)", tech="prisma-desktop (Rust)")
m.add_component("prisma_proc",  label="prisma serve",         layer="wsl2",  group="WSL2 (Linux)", tech="uvicorn :8765")
m.add_component("chromadb_fs",  label="ChromaDB",             layer="wsl2",  group="WSL2 (Linux)", tech="~/prisma-vault/chromadb/")
m.add_component("vault_fs",     label="Vault files",          layer="wsl2",  group="WSL2 (Linux)", tech="~/prisma-vault/ (Markdown)")
m.add_component("graphify_fs",  label="Graphify index",       layer="wsl2",  group="WSL2 (Linux)", tech="~/prisma-vault/graphify-out/")
m.add_component("ui_build_fs",  label="UI build artifacts",   layer="wsl2",  group="WSL2 (Linux)", tech="~/Repos/prisma/ui/build/")

# Windows host
m.add_component("ollama_win",   label="Ollama",               layer="win",   group="Windows host", tech=":11434 (GPU)")
m.add_component("zotero_win",   label="Zotero Desktop",       layer="win",   group="Windows host", tech=":23119 (local HTTP API)")
m.add_component("explorer",     label="Windows Explorer",     layer="win",   group="Windows host", tech="URL handler")

# External internet
m.add_component("zotero_web",   label="Zotero Web API",       layer="inet",  group="Internet",     tech="api.zotero.org")
m.add_component("search_apis",  label="Search APIs",          layer="inet",  group="Internet",     tech="arXiv, S2, OpenLibrary")

# WSL2 internal wiring
m.connect("tauri_proc",  "prisma_proc",  label="WebView :8765/app")
m.connect("prisma_proc", "chromadb_fs",  label="read / upsert")
m.connect("prisma_proc", "vault_fs",     label="read / write")
m.connect("prisma_proc", "graphify_fs",  label="read / write")
m.connect("prisma_proc", "ui_build_fs",  label="serves /app")

# WSL2 → Windows (eth0 bridge)
m.connect("prisma_proc", "ollama_win",   label="HTTP (eth0)")
m.connect("prisma_proc", "zotero_win",   label="HTTP (eth0)")
m.connect("tauri_proc",  "explorer",     label="open_url → explorer.exe")

# WSL2 → Internet
m.connect("prisma_proc", "zotero_web",   label="HTTPS (writes + reads)")
m.connect("prisma_proc", "search_apis",  label="HTTPS")

m.save(str(OUT))
print(f"[sysatlas] wrote {OUT}")
