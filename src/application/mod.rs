//! Application layer: the use cases (role executor, graph orchestrator).
//!
//! Pure orchestration — depends only on [`crate::domain`] and [`crate::ports`],
//! never on I/O. Adapters are selected and wired at the CLI edge. See
//! `docs/role-graph.md` §Hexagonal and ADR-007.

pub mod executor;
pub mod orchestrator;
pub mod report;
