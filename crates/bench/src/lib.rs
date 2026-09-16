//! Oracle and benchmark for `reth_trie_parallel::partitioned_root::PartitionedStateRoot`.
//!
//! Nothing here is meant for reth: in-memory state and sinks, a synthetic state generator, a
//! writer that commits emitted nodes into a database's trie tables, reth's own rebuild run
//! outside the pipeline, and a digest for comparing what the two leave behind.

pub mod alloc;
pub mod digest;
pub mod mdbx;
pub mod memory;
pub mod progress;
pub mod rebuild;
pub mod synth;
pub mod updates;
pub mod writer;

pub use alloc::{human_bytes, PeakAlloc};
pub use digest::{Digest, NodeDigest};
pub use mdbx::{
    digest_trie_tables, reth_root_with_updates, reth_serial_root, write_hashed_state,
    write_storage_settings, DatabaseSource, TrieTablesDigest, TxProvider,
};
pub use memory::{MemoryState, SharedCursorFactory};
pub use progress::ProgressSink;
pub use rebuild::{clear_trie_tables, reth_rebuild, write_trie_updates, Rebuilt};
pub use updates::TrieUpdatesSink;
pub use writer::{MdbxWriteSink, Written, DEFAULT_CHUNK_NODES};

use reth_tasks::{RayonConfig, Runtime, RuntimeBuilder, RuntimeConfig};
use reth_trie_common::updates::TrieUpdates;

/// A reth [`Runtime`] whose CPU pool has `threads` threads, or reth's default, the available
/// parallelism, when `None`. [`PartitionedStateRoot`] builds its partitions on that pool.
///
/// [`PartitionedStateRoot`]: reth_trie_parallel::partitioned_root::PartitionedStateRoot
pub fn runtime(threads: Option<usize>) -> Runtime {
    let rayon = RayonConfig { cpu_threads: threads, ..Default::default() };
    RuntimeBuilder::new(RuntimeConfig::default().with_rayon(rayon)).build().expect("runtime builds")
}

/// Drops the storage tries `updates` holds nothing for, so two `TrieUpdates` compare on content:
/// reth's walk records every account with storage, a generator run only those with nodes.
pub fn without_empty_storage_tries(mut updates: TrieUpdates) -> TrieUpdates {
    updates.storage_tries.retain(|_, t| !t.storage_nodes.is_empty() || !t.removed_nodes.is_empty());
    updates
}
