//! Generic, domain-agnostic persistence-governance primitives: versioned
//! schema migration and JSON Schema validation. The Rust twin of prisma's
//! own `prisma/schema_gov` Python package -- same rule applies here:
//! nothing under this module references `Settings`, `SyncContext`, or any
//! other prisma-desktop-domain type.
//!
//! Wired into `json_store::load_json` (2026-08-06): `Settings`/`AuthStore`/
//! `SyncState` (the three genuinely Rust-owned persisted files -- chat
//! session structure itself stays server-side only, Python, even offline,
//! since prisma-desktop's role is a webview onto the same web UI plus
//! byte-level file sync, not a second implementation of server logic) each
//! carry a real `schema_version` field and their own `MigrationChain`
//! (empty for now -- nothing's ever needed a real migration yet, same
//! posture the Python side had before its first one). `validate_against_schema`
//! remains unused -- no JSON Schema source of truth exists yet for these
//! Rust structs the way `prisma schema export` produces one for the Python
//! models; wiring that in is separate, follow-on scope, not done here. See
//! prisma's docs/wiki/adr/ADR-019-persisted-format-governance-and-
//! migrations.md for the full design this mirrors.

pub mod migration;
pub mod validation;

pub use migration::MigrationChain;

// No JSON Schema source of truth exists yet for Settings/AuthStore/SyncState
// (unlike the Python side's `prisma schema export`) -- validate_against_schema
// has nothing to validate against today, so it stays genuinely unused until
// that's built. Scoped allow, not a blanket one -- migration.rs's items are
// real callers now and should warn again if that ever regresses.
#[allow(unused_imports)]
pub use validation::validate_against_schema;
