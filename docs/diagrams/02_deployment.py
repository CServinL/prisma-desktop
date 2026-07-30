"""prisma-desktop — deployment view (multi-view collection).

Run: .venv/bin/python docs/diagrams/02_deployment.py

Two views:
  - Deployment  : physical processes and hosts (Linux desktop, forge, internet)
  - Network     : network boundaries, ports, and protocols
"""
from pathlib import Path
from sysatlas import SystemMap

OUT = Path(__file__).with_suffix(".html")

# ── View 1: Deployment ────────────────────────────────────────────────────────
dep = SystemMap(title="prisma-desktop — deployment")

dep.group("Linux",     color="#6366f1", label="Linux desktop (single machine)")
dep.group("Forge",     color="#f59e0b", label="forge.internal (LAN)")
dep.group("Internet",  color="#64748b", label="Internet")

dep.add_component("tauri_proc",    label="prisma-desktop",      layer="linux", group="Linux",     tech="Tauri v2 process (Rust)")
dep.add_component("prisma_proc",   label="prisma serve",        layer="linux", group="Linux",     tech="uvicorn :8765 (Python)")
dep.add_component("vault_fs",      label="Vault files",         layer="linux", group="Linux",     tech="~/prisma-vault/ (Markdown)")
dep.add_component("chromadb_fs",   label="ChromaDB",            layer="linux", group="Linux",     tech="vault/chromadb/")
dep.add_component("graphify_fs",   label="Graphify index",      layer="linux", group="Linux",     tech="vault/graphify-out/")
dep.add_component("ui_build_fs",   label="UI build artifacts",  layer="linux", group="Linux",     tech="~/Repos/prisma/ui/build/")

dep.add_component("llama_swap",    label="llama-swap",          layer="forge", group="Forge",     tech="KG extraction — small local model, bounded chunks")

dep.add_component("openrouter",    label="OpenRouter",          layer="inet",  group="Internet",  tech="cloud chat LLM — long-context reasoning")
dep.add_component("zotero_cloud",  label="Zotero Web API",      layer="inet",  group="Internet",  tech="api.zotero.org — read + write, only path (no local Desktop API)")
dep.add_component("arxiv",         label="arXiv API",           layer="inet",  group="Internet",  tech="export.arxiv.org")
dep.add_component("s2",            label="Semantic Scholar",    layer="inet",  group="Internet",  tech="api.semanticscholar.org")

dep.connect("tauri_proc",   "prisma_proc",   label=":8765/app")
dep.connect("prisma_proc",  "vault_fs",      label="r/w")
dep.connect("prisma_proc",  "chromadb_fs",   label="upsert")
dep.connect("prisma_proc",  "graphify_fs",   label="r/w")
dep.connect("prisma_proc",  "ui_build_fs",   label="/app")
dep.connect("prisma_proc",  "llama_swap",    label="HTTP — KG extraction")
dep.connect("prisma_proc",  "openrouter",    label="HTTPS — chat")
dep.connect("prisma_proc",  "zotero_cloud",  label="HTTPS")
dep.connect("prisma_proc",  "arxiv",         label="HTTPS")
dep.connect("prisma_proc",  "s2",            label="HTTPS")

# ── View 2: Network boundaries ────────────────────────────────────────────────
net = SystemMap(title="prisma-desktop — network boundaries")

net.group("Loopback",  color="#6366f1", label="127.0.0.1 (local loopback)")
net.group("LAN",       color="#f59e0b", label="LAN (forge.internal)")
net.group("Internet",  color="#64748b", label="Internet (HTTPS)")

net.add_component("tauri_webview", label="Tauri WebView",      layer="loopback", group="Loopback", tech="http://127.0.0.1:8765/app")
net.add_component("pwa_client",    label="Browser PWA",        layer="loopback", group="Loopback", tech="http://<host>:8765/app")
net.add_component("prisma_api",    label="prisma serve",       layer="loopback", group="Loopback", tech=":8765 (FastAPI)")
net.add_component("dev_reload",    label="/ui/dev/version",    layer="loopback", group="Loopback", tech="polled every 2s")

net.add_component("llama_swap_ep", label="llama-swap",         layer="lan",      group="LAN",       tech="forge.internal — KG extraction")

net.add_component("openrouter_ep", label="OpenRouter API",     layer="inet",     group="Internet",  tech="HTTPS — cloud chat LLM")
net.add_component("zotero_web_ep", label="api.zotero.org",     layer="inet",     group="Internet",  tech="HTTPS / REST + API key")
net.add_component("paper_apis",    label="Paper search APIs",  layer="inet",     group="Internet",  tech="arXiv / S2 / OpenLibrary")

net.connect("tauri_webview", "prisma_api",    label="GET /app")
net.connect("pwa_client",    "prisma_api",    label="GET /app")
net.connect("tauri_webview", "dev_reload",    label="poll", style="dashed")
net.connect("pwa_client",    "dev_reload",    label="poll", style="dashed")
net.connect("prisma_api",    "llama_swap_ep", label="HTTP")
net.connect("prisma_api",    "openrouter_ep", label="HTTPS")
net.connect("prisma_api",    "zotero_web_ep", label="HTTPS")
net.connect("prisma_api",    "paper_apis",    label="HTTPS")

SystemMap.save_collection(
    {"Deployment": dep, "Network": net},
    str(OUT),
    title="prisma-desktop — deployment",
)
print(f"[sysatlas] wrote {OUT}")
