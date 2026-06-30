"""prisma-desktop — system topology.

Run: .venv/bin/python docs/diagrams/01_system_topology.py

Shows how clients, the prisma server, UI source, and Tauri internals
relate to each other. Re-run after structural changes.
"""
from pathlib import Path
from sysatlas import SystemMap

OUT = Path(__file__).with_suffix(".html")

m = SystemMap(title="prisma-desktop — system topology")

# Clients
m.add_component("tauri_shell",  label="Tauri Shell",     layer="client",   group="Linux / WSL2", tech="Rust / Tauri v2")
m.add_component("pwa_android",  label="PWA (Android)",   layer="client",   group="Mobile",       tech="Chrome")
m.add_component("pwa_ios",      label="PWA (iOS / Mac)", layer="client",   group="Mobile",       tech="Safari")

# Tauri internals (what the Rust crate actually does)
m.add_component("window_mgmt",  label="Window mgmt",     layer="tauri",    group="src-tauri/",   tech="create / resize / drag")
m.add_component("settings",     label="Settings store",  layer="tauri",    group="src-tauri/",   tech="~/.config/prisma-desktop/")
m.add_component("url_opener",   label="URL opener",      layer="tauri",    group="src-tauri/",   tech="WSL2-aware open_url command")

# Server
m.add_component("fastapi",      label="FastAPI",         layer="server",   group="prisma serve", tech=":8765")
m.add_component("ui_static",    label="Static /app",     layer="server",   group="prisma serve", tech="ui/build/ via StaticFiles")
m.add_component("rest_api",     label="REST API",        layer="server",   group="prisma serve", tech="vault / search / streams / zotero")
m.add_component("ui_watcher",   label="UI watcher",      layer="server",   group="prisma serve", tech="mtime hash → npm build → version++")

# UI source
m.add_component("svelte_src",   label="SvelteKit src",   layer="ui_src",   group="prisma/ui/",   tech="Svelte 5 / Vite")
m.add_component("svelte_build", label="Built assets",    layer="ui_src",   group="prisma/ui/",   tech="ui/build/ (adapter-static)")

# Clients → server
m.connect("tauri_shell", "fastapi",     label="WebView → HTTP :8765/app")
m.connect("pwa_android", "fastapi",     label="HTTP :8765/app")
m.connect("pwa_ios",     "fastapi",     label="HTTP :8765/app")

# Tauri shell internals
m.connect("tauri_shell", "window_mgmt", label="")
m.connect("tauri_shell", "settings",    label="")
m.connect("tauri_shell", "url_opener",  label="")

# Server internals
m.connect("fastapi",     "ui_static",   label="mounts at /app")
m.connect("fastapi",     "rest_api",    label="routes")
m.connect("fastapi",     "ui_watcher",  label="daemon thread")

# UI build pipeline
m.connect("ui_watcher",  "svelte_src",  label="watches src/")
m.connect("ui_watcher",  "svelte_build",label="npm run build")
m.connect("svelte_build","ui_static",   label="served by")

# Dev hot-reload
m.connect("tauri_shell", "rest_api",    label="polls /ui/dev/version every 2s", style="dashed")

m.save(str(OUT))
print(f"[sysatlas] wrote {OUT}")
