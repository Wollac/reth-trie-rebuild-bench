//! Building one subtrie of the account trie.
//!
//! A subtrie is the set of accounts whose hashed address starts with a nibble prefix. It is built
//! with a [`HashBuilder`] over keys with the prefix stripped, so the builder's own root is exactly
//! the node a full-trie build would place at that prefix. Emitted branch nodes are re-keyed to
//! absolute paths on the way out.
//!
//! The state trie's root branch node (empty path) is never emitted: reth excludes root nodes from
//! `TrieUpdates`, so the trie tables never hold them and nothing about the root's masks needs to
//! be reconstructed after partitioning.

use crate::{emit::emit_completed, storage::storage_root_with_nodes, OnNode};
use alloy_primitives::B256;
use alloy_rlp::Encodable;
use alloy_trie::nodes::RlpNode;
use reth_storage_errors::db::DatabaseError;
use reth_trie::hashed_cursor::{HashedCursor, HashedCursorFactory};
use reth_trie_common::{HashBuilder, Nibbles, TRIE_ACCOUNT_RLP_MAX_SIZE};

/// A built subtrie.
#[derive(Clone, Debug)]
pub struct Subtrie {
    /// Prefix of every key in this subtrie, as absolute nibbles.
    pub prefix: Nibbles,
    /// RLP reference to the subtrie's root node, as the parent branch would embed it.
    pub root: RlpNode,
    /// Number of account leaves in the subtrie.
    pub leaves: u64,
}

/// Builds the subtrie under `prefix`, handing its account and storage nodes, at absolute paths,
/// to `on_node`.
///
/// Returns `Ok(None)` if no account starts with `prefix`.
pub fn build_subtrie<H: HashedCursorFactory>(
    factory: &H,
    prefix: Nibbles,
    on_node: &impl OnNode,
) -> Result<Option<Subtrie>, DatabaseError> {
    let (start, end) = prefix_bounds(&prefix);
    let mut accounts = factory.hashed_account_cursor()?;
    let mut hb = HashBuilder::default().with_updates(true);
    let mut leaves = 0u64;
    let mut account_rlp = Vec::with_capacity(TRIE_ACCOUNT_RLP_MAX_SIZE);

    let mut entry = accounts.seek(start)?;
    while let Some((hashed_address, account)) = entry {
        if end.is_some_and(|end| hashed_address >= end) {
            break;
        }
        let mut storage = factory.hashed_storage_cursor(hashed_address)?;
        let storage_root = storage_root_with_nodes(hashed_address, &mut storage, on_node)?;

        account_rlp.clear();
        account.into_trie_account(storage_root).encode(&mut account_rlp);

        hb.add_leaf(Nibbles::unpack(hashed_address).slice(prefix.len()..), &account_rlp);
        leaves += 1;
        entry = accounts.next()?;
        emit_completed(&mut hb, None, prefix, on_node);
    }

    hb.root();
    let Some(root) = hb.stack.pop() else {
        return Ok(None);
    };
    emit_completed(&mut hb, None, prefix, on_node);

    Ok(Some(Subtrie { prefix, root, leaves }))
}

/// Hash of a subtrie root node reference: the embedded hash, or the hash of the inline RLP.
pub fn node_hash(node: &RlpNode) -> B256 {
    node.as_hash().unwrap_or_else(|| alloy_primitives::keccak256(node))
}

/// Inclusive lower and exclusive upper bound (if any) of the 32-byte keys starting with `prefix`.
///
/// Nibbles are stored most-significant-first, so packing a prefix into a zeroed 32-byte array is
/// the smallest key with that prefix. The exclusive end is the packed successor prefix;
/// [`Nibbles::increment`] yields `None` when there is none (empty prefix, or all nibbles `0xf`),
/// which means the range is unbounded above.
fn prefix_bounds(prefix: &Nibbles) -> (B256, Option<B256>) {
    (pack_padded(prefix), prefix.increment().map(|next| pack_padded(&next)))
}

fn pack_padded(nibbles: &Nibbles) -> B256 {
    let mut bytes = [0u8; 32];
    nibbles.pack_to(&mut bytes);
    B256::from(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_bounds_single_nibble() {
        let (start, end) = prefix_bounds(&Nibbles::from_nibbles([0x3]));
        assert_eq!(start.0[0], 0x30);
        assert_eq!(end.unwrap().0[0], 0x40);
        let (start, end) = prefix_bounds(&Nibbles::from_nibbles([0xf]));
        assert_eq!(start.0[0], 0xf0);
        assert!(end.is_none());
    }

    #[test]
    fn prefix_bounds_carry() {
        let (start, end) = prefix_bounds(&Nibbles::from_nibbles([0x2, 0xf, 0xf]));
        assert_eq!(&start.0[..2], &[0x2f, 0xf0]);
        assert_eq!(&end.unwrap().0[..2], &[0x30, 0x00]);
        let (_, end) = prefix_bounds(&Nibbles::new());
        assert!(end.is_none());
    }
}
