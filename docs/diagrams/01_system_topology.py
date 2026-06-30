"""prisma-desktop — system topology (multi-view collection).

Run: .venv/bin/python docs/diagrams/01_system_topology.py

Two views in one HTML file:
  - System topology : clients, Tauri internals, server, UI build pipeline
  - UI pipeline     : zoom into the SvelteKit build + hot-reload cycle
"""
from pathlib import Path
from sysatlas import SystemMap

OUT = Path(__file__).with_suffix(".html")

# ── View 1: System topology ────────────────────────────────────────────────────
top = SystemMap(title="prisma-desktop — system topology")

top.group("Desktop",     color="#6366f1", label="Linux / WSL2 desktop")
top.group("Mobile",      color="#8b5cf6", label="Mobile / Mac (PWA)")
top.group("TauriRust",   color="#ef4444", label="Tauri Rust shell (src-tauri/)")
top.group("Server",      color="#0ea5e9", label="prisma serve (:8765)")
top.group("UISource",    color="#10b981", label="SvelteKit source (prisma/ui/)")

top.add_component("tauri_shell",  label="Tauri Shell",     layer="clients",  group="Desktop",   tech="Rust / Tauri v2 — opens WebView")
top.add_component("pwa_android",  label="PWA (Android)",   layer="clients",  group="Mobile",    tech="Chrome — install from browser")
top.add_component("pwa_ios",      label="PWA (iOS/Mac)",   layer="clients",  group="Mobile",    tech="Safari — install from browser")

top.add_component("window_mgmt",  label="Window mgmt",     layer="tauri",    group="TauriRust", tech="create / resize / minimize / drag")
top.add_component("settings",     label="Settings store",  layer="tauri",    group="TauriRust", tech="~/.config/prisma-desktop/settings.json")
top.add_component("url_opener",   label="URL opener",      layer="tauri",    group="TauriRust", tech="WSL2-aware: explorer.exe / xdg-open")

top.add_component("fastapi",      label="FastAPI",         layer="server",   group="Server",    tech=":8765")
top.add_component("ui_static",    label="Static /app",     layer="server",   group="Server",    tech="ui/build/ via StaticFiles")
top.add_component("rest_api",     label="REST API",        layer="server",   group="Server",    tech="vault / search / streams / zotero")
top.add_component("ui_watcher",   label="UI watcher",      layer="server",   group="Server",    tech="mtime hash → npm build → version++")
top.add_component("dev_version",  label="GET /ui/dev/version", layer="server", group="Server",  tech="hot-reload signal for clients")

top.add_component("svelte_src",   label="SvelteKit src",   layer="ui",       group="UISource",  tech="Svelte 5 — src/routes/+page.svelte")
top.add_component("svelte_build", label="Built assets",    layer="ui",       group="UISource",  tech="adapter-static → ui/build/")

top.connect("tauri_shell", "fastapi",     label="WebView → :8765/app")
top.connect("pwa_android", "fastapi",     label=":8765/app")
top.connect("pwa_ios",     "fastapi",     label=":8765/app")
top.connect("tauri_shell", "window_mgmt")
top.connect("tauri_shell", "settings")
top.connect("tauri_shell", "url_opener")
top.connect("fastapi",     "ui_static",   label="mounts /app")
top.connect("fastapi",     "rest_api",    label="routes")
top.connect("fastapi",     "ui_watcher",  label="daemon thread")
top.connect("fastapi",     "dev_version", label="endpoint")
top.connect("ui_watcher",  "svelte_src",  label="watches src/ mtime")
top.connect("ui_watcher",  "svelte_build",label="npm run build")
top.connect("svelte_build","ui_static",   label="output")
top.connect("tauri_shell", "dev_version", label="polls every 2s → reload", style="dashed")
top.connect("pwa_android", "dev_version", label="polls every 2s → reload", style="dashed")

# ── View 2: UI build pipeline (zoom) ──────────────────────────────────────────
pipe = SystemMap(title="UI build pipeline — source to served assets")

pipe.group("Source",   color="#10b981", label="Source (prisma/ui/src/)")
pipe.group("Vite",     color="#6366f1", label="Vite build")
pipe.group("Output",   color="#f59e0b", label="Build output (ui/build/)")
pipe.group("Watcher",  color="#0ea5e9", label="Server-side watcher")
pipe.group("Clients",  color="#8b5cf6", label="Clients")

pipe.add_component("svelte_files",  label="+page.svelte",        layer="source",  group="Source",  tech="Svelte 5 components")
pipe.add_component("app_html",      label="app.html",            layer="source",  group="Source",  tech="HTML shell")
pipe.add_component("vite_config",   label="vite.config.js",      layer="source",  group="Source",  tech="adapter-static, no Tauri")
pipe.add_component("svelte_config", label="svelte.config.js",    layer="source",  group="Source",  tech="SPA fallback: index.html")

pipe.add_component("vite_build",    label="vite build",          layer="build",   group="Vite",    tech="npm run build")
pipe.add_component("adapter",       label="adapter-static",      layer="build",   group="Vite",    tech="emits index.html + chunks")

pipe.add_component("index_html",    label="index.html",          layer="output",  group="Output",  tech="entry point")
pipe.add_component("js_chunks",     label="_app/ chunks",        layer="output",  group="Output",  tech="JS / CSS split")

pipe.add_component("watcher_th",    label="UI watcher thread",   layer="watcher", group="Watcher", tech="mtime hash every 1s")
pipe.add_component("version_ep",    label="/ui/dev/version",     layer="watcher", group="Watcher", tech="version counter")
pipe.add_component("static_mount",  label="StaticFiles /app",    layer="watcher", group="Watcher", tech="FastAPI mount")

pipe.add_component("tauri_wv",      label="Tauri WebView",       layer="clients", group="Clients", tech="loads :8765/app")
pipe.add_component("browser",       label="Browser / PWA",       layer="clients", group="Clients", tech="loads :8765/app")

pipe.connect("svelte_files",  "vite_build")
pipe.connect("app_html",      "vite_build")
pipe.connect("vite_config",   "vite_build",    label="configures")
pipe.connect("svelte_config", "vite_build",    label="configures")
pipe.connect("vite_build",    "adapter",       label="runs")
pipe.connect("adapter",       "index_html",    label="emits")
pipe.connect("adapter",       "js_chunks",     label="emits")
pipe.connect("watcher_th",    "svelte_files",  label="polls mtime", style="dashed")
pipe.connect("watcher_th",    "vite_build",    label="triggers on change")
pipe.connect("watcher_th",    "version_ep",    label="version++")
pipe.connect("index_html",    "static_mount",  label="served by")
pipe.connect("js_chunks",     "static_mount",  label="served by")
pipe.connect("static_mount",  "tauri_wv",      label="GET /app/…")
pipe.connect("static_mount",  "browser",       label="GET /app/…")
pipe.connect("version_ep",    "tauri_wv",      label="poll → reload", style="dashed")
pipe.connect("version_ep",    "browser",       label="poll → reload", style="dashed")

SystemMap.save_collection(
    {"System topology": top, "UI pipeline": pipe},
    str(OUT),
    title="prisma-desktop — architecture",
)
print(f"[sysatlas] wrote {OUT}")
