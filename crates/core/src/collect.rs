//! Collecting emitted nodes into reth's [`TrieUpdates`].
//!
//! The generator itself hands nodes out through a plain closure (see [`generate`]); this is the
//! collector the reth integration plugs into it.
//!
//! [`generate`]: crate::generate

use alloy_primitives::B256;
use reth_trie_common::{updates::TrieUpdates, BranchNodeCompact, Nibbles};
use std::sync::Mutex;

/// Accumulates nodes into a [`TrieUpdates`], the shape `StateRoot::root_with_updates` returns
/// and the provider's trie-table writer consumes.
///
/// Callable from many threads at once. Holds everything in memory; suitable for the merkle-stage
/// integration at the sizes that path already handles, not for a 10^9-leaf build in one go.
#[derive(Debug, Default)]
pub struct TrieUpdatesCollector(Mutex<TrieUpdates>);

impl TrieUpdatesCollector {
    /// Records a branch node; `account` is `None` for the state trie.
    pub fn push(&self, account: Option<B256>, path: Nibbles, node: BranchNodeCompact) {
        let mut updates = self.0.lock().unwrap();
        let prev = match account {
            None => updates.account_nodes.insert(path, node),
            Some(hashed_address) => updates
                .storage_tries
                .entry(hashed_address)
                .or_default()
                .storage_nodes
                .insert(path, node),
        };
        debug_assert!(prev.is_none(), "node emitted twice for {account:?} {path:?}");
    }

    /// Consumes the collector and returns the accumulated updates.
    pub fn into_updates(self) -> TrieUpdates {
        self.0.into_inner().unwrap()
    }
}
