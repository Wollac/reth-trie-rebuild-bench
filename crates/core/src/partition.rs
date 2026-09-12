//! Building one subtrie of the account trie.
//!
//! A subtrie is the set of accounts whose hashed address starts with a nibble prefix. It is built
//! with a [`HashBuilder`] over the *absolute* keys, exactly as the serial build would walk that
//! key range: every branch node it emits has the path and masks the serial build gives it, and no
//! re-keying is needed. What differs from the serial build is only the builder's own root: with
//! every key sharing the prefix, that root is a leaf or an extension whose key still carries the
//! prefix, rather than the node the whole trie has at the prefix. The root is therefore returned
//! decoded, and [`assemble_root`](crate::generate::assemble_root) splices it into the trie above.
//!
//! The builder's root node is retrieved through its proof retainer: a retainer with no targets
//! keeps exactly the node at the empty path, the root, and nothing else.

use crate::{emit::emit_completed, storage::storage_root_with_nodes, OnNode};
use alloy_primitives::B256;
use alloy_rlp::{Decodable, Encodable};
use alloy_trie::{nodes::TrieNode, proof::ProofRetainer};
use reth_storage_errors::db::DatabaseError;
use reth_trie::hashed_cursor::{HashedCursor, HashedCursorFactory};
use reth_trie_common::{HashBuilder, Nibbles, TRIE_ACCOUNT_RLP_MAX_SIZE};

/// A built subtrie.
#[derive(Clone, Debug)]
pub struct Subtrie {
    /// Prefix of every key in this subtrie, as absolute nibbles.
    pub prefix: Nibbles,
    /// The root node of the trie formed by the subtrie's leaves alone, with absolute keys. Under a
    /// non-empty prefix this is a leaf or an extension whose key starts with the prefix; it is
    /// never a branch, since all keys agree on the prefix.
    pub root: TrieNode,
    /// Whether the topmost branch node (the root itself, or the child of a root extension) was
    /// handed to `on_node`, i.e. has a row in the trie table. This is what reth's walker passes as
    /// `children_are_in_trie` when it feeds a stored subtrie into a `HashBuilder`; it only shapes
    /// the tree mask of the parent branch, which the assembly never stores for one-nibble
    /// partitions, but it is what makes assembly of deeper partitions come out right too.
    pub stored: bool,
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
    let mut hb = HashBuilder::default()
        .with_updates(true)
        .with_proof_retainer(ProofRetainer::new(Vec::new()));
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

        hb.add_leaf(Nibbles::unpack(hashed_address), &account_rlp);
        leaves += 1;
        entry = accounts.next()?;
        emit_completed(&mut hb, None, on_node);
    }

    hb.root();
    let root_rlp = hb
        .take_proof_nodes()
        .into_inner()
        .remove(&Nibbles::new())
        .expect("retainer keeps the root node");
    let root = TrieNode::decode(&mut root_rlp.as_ref()).expect("builder emits valid node RLP");
    let top_branch = match &root {
        TrieNode::EmptyRoot => return Ok(None),
        TrieNode::Branch(_) => Some(prefix),
        TrieNode::Extension(ext) => Some(ext.key),
        TrieNode::Leaf(_) => None,
    };
    // The topmost branch, if any, completed inside `root()` and has not been drained yet.
    let stored = top_branch.is_some_and(|path| {
        hb.updated_branch_nodes.as_ref().expect("updates enabled").contains_key(&path)
    });
    emit_completed(&mut hb, None, on_node);

    Ok(Some(Subtrie { prefix, root, stored, leaves }))
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
