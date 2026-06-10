"""prisma-desktop Rust crate — architecture diagram.

Run: .venv/bin/python docs/reflection/module-map.py

Re-run after any module add / remove / rename.
Requires sysatlas[reflect-rust] in the dev venv.
"""
from pathlib import Path

import sysatlas

REPO = Path(__file__).resolve().parents[2]
OUT  = Path(__file__).with_suffix(".html")

r = sysatlas.reflect_rust(REPO / "src-tauri")
s = r.to_system(title="prisma-desktop — Rust crate")
s.save(str(OUT))
print(f"[sysatlas] wrote {OUT}")
