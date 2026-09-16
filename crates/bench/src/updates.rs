//! Collecting emitted nodes into reth's [`TrieUpdates`], for comparison with its serial walk.

use alloy_primitives::B256;
use reth_trie_common::{updates::TrieUpdates, BranchNodeCompact, Nibbles};
use reth_trie_parallel::partitioned_root::TrieSink;
use std::sync::Mutex;

/// An [`TrieSink`] that collects all nodes into [`TrieUpdates`].
///
/// All nodes are kept in memory, so this is only suitable for tries of the size the merkle stage
/// already handles, not for the full mainnet state.
#[derive(Debug, Default)]
pub struct TrieUpdatesSink([Mutex<TrieUpdates>; 16]);

impl TrieUpdatesSink {
    /// Consumes the sink and returns the collected updates.
    pub fn into_trie_updates(self) -> TrieUpdates {
        let mut updates = TrieUpdates::default();
        // The shards have disjoint keys, so they can be merged by plain insertion.
        for shard in self.0 {
            let shard = shard.into_inner().expect("lock is not poisoned");
            updates.account_nodes.extend(shard.account_nodes);
            updates.storage_tries.extend(shard.storage_tries);
        }
        updates
    }
}

impl TrieSink for TrieUpdatesSink {
    fn on_branch_node(&self, hashed_address: Option<B256>, path: Nibbles, node: BranchNodeCompact) {
        // Shard by first nibble. Each partition worker only emits nodes under its own nibble, so
        // workers never contend for a lock, except for the partitions of a single storage trie,
        // which all use the shard of their account.
        let shard = match hashed_address {
            Some(hashed_address) => hashed_address[0] >> 4,
            None => path.first().expect("stored nodes have a non-empty path"),
        };
        let mut updates = self.0[shard as usize].lock().expect("lock is not poisoned");
        let prev = match hashed_address {
            None => updates.account_nodes.insert(path, node),
            Some(hashed_address) => updates
                .storage_tries
                .entry(hashed_address)
                .or_default()
                .storage_nodes
                .insert(path, node),
        };
        debug_assert!(prev.is_none(), "node emitted twice for {hashed_address:?} {path:?}");
    }
}
