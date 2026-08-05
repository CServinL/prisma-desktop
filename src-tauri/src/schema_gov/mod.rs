//! Generic, domain-agnostic persistence-governance primitives: versioned
//! schema migration and JSON Schema validation. The Rust twin of prisma's
//! own `prisma/schema_gov` Python package -- same rule applies here:
//! nothing under this module references `Settings`, `SyncContext`, or any
//! other prisma-desktop-domain type.
//!
//! Currently unused by any prisma-desktop domain code (2026-08-04): chat
//! session structure genuinely belongs server-side only (Python) -- even
//! offline, that means a *local* prisma-server, never Rust reconstructing
//! session data itself, since prisma-desktop's own role is a webview onto
//! the same web UI plus byte-level file sync, not a second implementation
//! of server logic. Kept as tested, dependency-free-of-any-domain-type
//! groundwork in case `settings.json` (genuinely Rust-owned, no server
//! involved) wants schema-versioned governance later -- not wired to
//! anything today. See prisma's docs/wiki/adr/ADR-019-persisted-format-
//! governance-and-migrations.md for the full design.

pub mod migration;
pub mod validation;

pub use migration::MigrationChain;
pub use validation::validate_against_schema;
