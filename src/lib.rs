#![deny(missing_docs)]
// Broken doc links fail `cargo doc` (run in CI).
#![deny(rustdoc::broken_intra_doc_links, rustdoc::private_intra_doc_links)]
//! rs-nightshift — local multi-agent overnight engineering harness for local
//! models (Ollama or any OpenAI-compatible server).
//!
//! The library holds check and model contracts used by the `nightshift` binary.
//! Process exit happens only in `main`.

pub mod adapters;
pub mod application;
pub mod artifacts;
pub mod bench;
pub mod cli;
pub mod dev;
pub mod doctor;
pub mod domain;
pub mod error;
pub mod generate;
pub mod models;
pub mod pipeline;
pub mod pm;
pub mod ports;
pub mod qa;
pub mod techlead;
pub mod writer;
