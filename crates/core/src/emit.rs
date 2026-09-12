//! Handing finished branch nodes out of a [`HashBuilder`] as it runs.

use crate::OnNode;
use alloy_primitives::B256;
use reth_trie_common::{HashBuilder, Nibbles};

/// Hands every branch node the builder has finished so far to `on_node` and forgets it, keeping
/// the builder's memory bounded by the trie depth rather than the trie size. A node is final once
/// it appears in `updated_branch_nodes`: the builder never touches it again, and `drain` keeps the
/// map's allocation for the next burst.
///
/// `account` is `None` for the state trie and `Some(hashed_address)` for a storage trie; `prefix`
/// is the subtrie prefix the builder's keys were stripped of, joined back onto every path.
///
/// The builder's own root needs care to match reth's `StateRoot`: the whole-trie root node is
/// never stored, since `TrieUpdates::finalize` drops the empty path; and reth's walker reads a
/// stored `root_hash` as "this node is the trie root", so a subtrie root must not carry one. No
/// other node ever has a `root_hash`.
pub(crate) fn emit_completed(
    hb: &mut HashBuilder,
    account: Option<B256>,
    prefix: Nibbles,
    on_node: &impl OnNode,
) {
    let completed = hb.updated_branch_nodes.as_mut().expect("updates enabled");
    if completed.is_empty() {
        return;
    }
    for (path, mut node) in completed.drain() {
        if path.is_empty() {
            // The builder's own root. For a whole trie it is not stored; for a subtrie it is an
            // interior node of the whole trie and must not look like a root.
            if prefix.is_empty() {
                continue;
            }
            node.root_hash = None;
        }
        on_node(account, prefix.join(&path), node);
    }
}
