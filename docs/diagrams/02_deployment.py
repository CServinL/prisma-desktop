"""prisma-desktop — deployment view (multi-view collection).

Run: .venv/bin/python docs/diagrams/02_deployment.py

Two views:
  - Deployment  : physical processes and hosts (WSL2, Windows, internet)
  - Network     : network boundaries, ports, and protocols
"""
from pathlib import Path
from sysatlas import SystemMap

OUT = Path(__file__).with_suffix(".html")

# ── View 1: Deployment ────────────────────────────────────────────────────────
dep = SystemMap(title="prisma-desktop — deployment")

dep.group("WSL2",     color="#6366f1", label="WSL2 (Linux processes)")
dep.group("Windows",  color="#0ea5e9", label="Windows host")
dep.group("Internet", color="#64748b", label="Internet")

dep.add_component("tauri_proc",    label="prisma-desktop",      layer="wsl2",  group="WSL2",     tech="Tauri v2 process (Rust)")
dep.add_component("prisma_proc",   label="prisma serve",        layer="wsl2",  group="WSL2",     tech="uvicorn :8765 (Python)")
dep.add_component("vault_fs",      label="Vault files",         layer="wsl2",  group="WSL2",     tech="~/prisma-vault/ (Markdown)")
dep.add_component("chromadb_fs",   label="ChromaDB",            layer="wsl2",  group="WSL2",     tech="vault/chromadb/")
dep.add_component("graphify_fs",   label="Graphify index",      layer="wsl2",  group="WSL2",     tech="vault/graphify-out/")
dep.add_component("ui_build_fs",   label="UI build artifacts",  layer="wsl2",  group="WSL2",     tech="~/Repos/prisma/ui/build/")

dep.add_component("ollama_win",    label="Ollama",              layer="win",   group="Windows",  tech=":11434 — GPU-accelerated LLM")
dep.add_component("zotero_win",    label="Zotero Desktop",      layer="win",   group="Windows",  tech=":23119 — local read-only HTTP")
dep.add_component("explorer_win",  label="Windows Explorer",    layer="win",   group="Windows",  tech="URL / file handler")

dep.add_component("zotero_cloud",  label="Zotero Web API",      layer="inet",  group="Internet", tech="api.zotero.org — read + write")
dep.add_component("arxiv",         label="arXiv API",           layer="inet",  group="Internet", tech="export.arxiv.org")
dep.add_component("s2",            label="Semantic Scholar",    layer="inet",  group="Internet", tech="api.semanticscholar.org")

dep.connect("tauri_proc",   "prisma_proc",   label=":8765/app")
dep.connect("prisma_proc",  "vault_fs",      label="r/w")
dep.connect("prisma_proc",  "chromadb_fs",   label="upsert")
dep.connect("prisma_proc",  "graphify_fs",   label="r/w")
dep.connect("prisma_proc",  "ui_build_fs",   label="/app")
dep.connect("prisma_proc",  "ollama_win",    label="HTTP")
dep.connect("prisma_proc",  "zotero_win",    label="HTTP")
dep.connect("tauri_proc",   "explorer_win",  label="open_url")
dep.connect("prisma_proc",  "zotero_cloud",  label="HTTPS")
dep.connect("prisma_proc",  "arxiv",         label="HTTPS")
dep.connect("prisma_proc",  "s2",            label="HTTPS")

# ── View 2: Network boundaries ────────────────────────────────────────────────
net = SystemMap(title="prisma-desktop — network boundaries")

net.group("LoopbackWSL2", color="#6366f1", label="127.0.0.1 (WSL2 loopback)")
net.group("ETH0Bridge",   color="#10b981", label="eth0 bridge (WSL2 ↔ Windows)")
net.group("Internet",     color="#64748b", label="Internet (HTTPS)")

net.add_component("tauri_webview", label="Tauri WebView",      layer="loopback", group="LoopbackWSL2", tech="http://127.0.0.1:8765/app")
net.add_component("pwa_client",    label="Browser PWA",        layer="loopback", group="LoopbackWSL2", tech="http://<host>:8765/app")
net.add_component("prisma_api",    label="prisma serve",       layer="loopback", group="LoopbackWSL2", tech=":8765 (FastAPI)")
net.add_component("dev_reload",    label="/ui/dev/version",    layer="loopback", group="LoopbackWSL2", tech="polled every 2s")

net.add_component("ollama_ep",     label="Ollama endpoint",    layer="bridge",   group="ETH0Bridge",   tech="<windows_ip>:11434")
net.add_component("zotero_ep",     label="Zotero local API",   layer="bridge",   group="ETH0Bridge",   tech="<windows_ip>:23119")
net.add_component("url_open_ep",   label="explorer.exe",       layer="bridge",   group="ETH0Bridge",   tech="WSL2 interop")

net.add_component("zotero_web_ep", label="api.zotero.org",     layer="inet",     group="Internet",     tech="HTTPS / REST + API key")
net.add_component("paper_apis",    label="Paper search APIs",  layer="inet",     group="Internet",     tech="arXiv / S2 / OpenLibrary")

net.connect("tauri_webview", "prisma_api",    label="GET /app")
net.connect("pwa_client",    "prisma_api",    label="GET /app")
net.connect("tauri_webview", "dev_reload",    label="poll", style="dashed")
net.connect("pwa_client",    "dev_reload",    label="poll", style="dashed")
net.connect("prisma_api",    "ollama_ep",     label="HTTP")
net.connect("prisma_api",    "zotero_ep",     label="HTTP")
net.connect("tauri_webview", "url_open_ep",   label="open_url")
net.connect("prisma_api",    "zotero_web_ep", label="HTTPS")
net.connect("prisma_api",    "paper_apis",    label="HTTPS")

SystemMap.save_collection(
    {"Deployment": dep, "Network": net},
    str(OUT),
    title="prisma-desktop — deployment",
)
print(f"[sysatlas] wrote {OUT}")
