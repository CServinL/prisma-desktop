# TODO

## Deferred: Flatpak packaging (2026-07-27)

Wanted eventually so prisma-desktop can be published on Flathub — deferred
until there's time to do it properly, not started yet. In the meantime,
`release.yml` bundles `.AppImage` only (see `tauri.conf.json`'s
`bundle.targets`) as the interim portable-file option; `.deb`/`.rpm` were
deliberately dropped rather than kept alongside it.

**What this actually involves** (scoped in conversation, not yet built):

- Flathub is source-based, not artifact-upload: a manifest (JSON/YAML)
  declaring the runtime/SDK, source refs (git tag/commit), build commands,
  and sandbox permissions (`finish-args`) gets submitted as a PR to
  `github.com/flathub/dev.cservinl.prisma-desktop` (app ID must match
  `tauri.conf.json`'s `identifier`, already reverse-DNS). Flathub's own
  buildbot compiles it from that manifest — never a pre-built binary we
  hand them.
- **Rust-specific wrinkle, the real blocker to budget time for:** Flatpak
  builds are network-isolated (no internet during the build step itself),
  so every Cargo dependency needs to be pre-vendored with checksums ahead
  of time — typically via the community `flatpak-cargo-generator.py` tool
  producing a `cargo-sources.json`, plus the
  `org.freedesktop.Sdk.Extension.rust-stable` SDK extension to get
  `cargo`/`rustc` inside the sandboxed builder at all.
- `finish-args` need real scoping decisions: `--share=network` (API/WS
  calls to `prisma serve`), some form of filesystem access for the
  user-chosen vault folder (ideally via the file-chooser portal rather
  than a broad `--filesystem=home` grant, to keep the sandbox meaningful),
  a display socket (`--socket=wayland` + X11 fallback).
- Submission goes through **manual human review** — Flathub reviewers
  check app ID legitimacy, `finish-args` aren't overly broad, AppStream
  metadata (description/screenshots/category) is present, and the build
  actually succeeds. Days to a couple weeks depending on reviewer
  bandwidth, often with requested changes before merge.
- Updates after initial acceptance: bump the manifest's source ref to a
  new tag, push to the Flathub app repo, buildbot rebuilds — same
  source-based model, no manual binary upload ever.

See also: `.github/workflows/ci.yml`/`release.yml`'s per-platform-job
structure (Linux x86_64 active, ARM64/Windows/macOS as `if: false`
placeholders) — Flatpak packaging is orthogonal to that per-target-triple
scaffolding, since Flatpak itself is architecture-agnostic at the manifest
level (the buildbot handles arch variants).
