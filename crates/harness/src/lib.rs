//! Test and benchmark harness for `trie-gen-core`.
//!
//! Nothing here is meant for reth: in-memory state and sinks, a synthetic state generator, and
//! (later) flat-dump and datadir tooling for mainnet-scale runs.

pub mod memory;
pub mod synth;

pub use memory::{MemorySink, MemoryState};
pub use trie_gen_core as core;
