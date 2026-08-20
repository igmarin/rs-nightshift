//! Config-driven role graph: roles, verdicts, routing, run state, and config.
//!
//! This module is the data model for the role-graph harness. A `nightshift.toml`
//! declares roles (provider + model + prompt + output + routing) and the harness
//! walks them, routing on a small deterministic verdict vocabulary. See
//! `docs/role-graph.md` for the design and the full config schema.

pub mod config;
pub mod routing;
pub mod state;
pub mod verdict;
