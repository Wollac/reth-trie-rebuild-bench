//! Handing finished branch nodes out of a [`HashBuilder`] as it runs.

use crate::OnNode;
use alloy_primitives::B256;
use reth_trie_common::HashBuilder;

/// Hands every branch node the builder has finished so far to `on_node` and forgets it, keeping
/// the builder's memory bounded by the trie depth rather than the trie size. A node is final once
/// it appears in `updated_branch_nodes`: the builder never touches it again, and `drain` keeps the
/// map's allocation for the next burst.
///
/// `account` is `None` for the state trie and `Some(hashed_address)` for a storage trie. Paths are
/// forwarded as the builder produced them, so every builder must run over absolute keys.
///
/// A node at the empty path is a whole-trie root, which reth never stores (`TrieUpdates::finalize`
/// drops it), so it is skipped. Nothing else needs fixing up: a subtrie builder over absolute keys
/// never produces a node at the empty path, so it never sets `root_hash` either.
pub(crate) fn emit_completed(hb: &mut HashBuilder, account: Option<B256>, on_node: &impl OnNode) {
    let completed = hb.updated_branch_nodes.as_mut().expect("updates enabled");
    if completed.is_empty() {
        return;
    }
    for (path, node) in completed.drain() {
        if path.is_empty() {
            continue;
        }
        on_node(account, path, node);
    }
}
