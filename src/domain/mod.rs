//! Pure domain for the role-graph harness — no I/O.
//!
//! This is the hexagonal core: config, verdicts, and routing are pure data +
//! validation with no network or filesystem access, so they stay trivially
//! unit-testable. Ports live in [`crate::ports`]; adapters and orchestration
//! live outside the domain. See `docs/role-graph.md` §Hexagonal and ADR-007.

pub mod rolegraph;
