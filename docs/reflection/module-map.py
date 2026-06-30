"""prisma-desktop system architecture diagram.

Run: .venv/bin/python docs/reflection/module-map.py

Re-run after structural changes to the shell or its relationship with
prisma server / UI.
Requires sysatlas in the dev venv.
"""
from pathlib import Path

from sysatlas import SystemMap

OUT = Path(__file__).with_suffix(".html")

m = SystemMap(title="prisma-desktop — system architecture")

# Clients
m.add_component("tauri_shell",  label="Tauri Shell",    layer="client",  group="Linux / WSL2", tech="Rust / Tauri v2")
m.add_component("pwa_android",  label="PWA (Android)",  layer="client",  group="Mobile / Mac",  tech="Chrome")
m.add_component("pwa_ios",      label="PWA (iOS/Mac)",  layer="client",  group="Mobile / Mac",  tech="Safari")

# Server
m.add_component("prisma_serve", label="prisma serve",   layer="server",  group="Python",        tech="FastAPI :8765")
m.add_component("ui_static",    label="UI /app",        layer="server",  group="Python",        tech="StaticFiles → ui/build/")
m.add_component("api",          label="REST API",       layer="server",  group="Python",        tech="JSON endpoints")
m.add_component("ui_watcher",   label="UI Watcher",     layer="server",  group="Python",        tech="mtime poll → npm build")

# UI source
m.add_component("svelte_ui",    label="SvelteKit UI",   layer="ui",      group="prisma/ui/",    tech="Svelte 5 / Vite")

# Tauri internals
m.add_component("window_mgmt",  label="Window mgmt",    layer="tauri",   group="src-tauri/",    tech="create / resize / drag")
m.add_component("settings",     label="Settings",       layer="tauri",   group="src-tauri/",    tech="~/.config/prisma-desktop/")
m.add_component("url_opener",   label="URL opener",     layer="tauri",   group="src-tauri/",    tech="WSL2-aware (explorer.exe / xdg-open)")

# Connections
m.connect("tauri_shell",  "prisma_serve",  label="loads /app")
m.connect("pwa_android",  "prisma_serve",  label="loads /app")
m.connect("pwa_ios",      "prisma_serve",  label="loads /app")
m.connect("prisma_serve", "ui_static",     label="mounts")
m.connect("prisma_serve", "api",           label="exposes")
m.connect("prisma_serve", "ui_watcher",    label="spawns thread")
m.connect("ui_watcher",   "svelte_ui",     label="watches → builds")
m.connect("svelte_ui",    "ui_static",     label="npm run build")
m.connect("tauri_shell",  "window_mgmt",   label="")
m.connect("tauri_shell",  "settings",      label="")
m.connect("tauri_shell",  "url_opener",    label="")

m.save(str(OUT))
print(f"[sysatlas] wrote {OUT}")
