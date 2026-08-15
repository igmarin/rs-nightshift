#![deny(missing_docs)]
//! rs-nightshift — local multi-agent overnight engineering harness around Ollama.
//!
//! The library holds check and model contracts used by the `nightshift` binary.
//! Process exit happens only in `main`.

pub mod artifacts;
pub mod cli;
pub mod context;
pub mod dev;
pub mod doctor;
pub mod error;
pub mod generate;
pub mod models;
pub mod ollama;
pub mod pipeline;
pub mod pm;
pub mod qa;
pub mod techlead;
pub mod testrun;
