//! Whole-trie generation: partitioned in parallel, or serial for reference.

use crate::{
    emit::emit_completed,
    partition::{build_subtrie, Subtrie},
    OnNode,
};
use alloy_primitives::{keccak256, B256};
use alloy_trie::nodes::TrieNode;
use rayon::prelude::*;
use reth_storage_errors::db::DatabaseError;
use reth_trie::hashed_cursor::HashedCursorFactory;
use reth_trie_common::{HashBuilder, Nibbles};

/// Generation settings.
#[derive(Clone, Copy, Debug)]
pub struct Config {
    /// Split the account trie by first nibble and build the 16 subtries in parallel. When
    /// `false`, a single serial `HashBuilder` walks everything; this is the reference path.
    pub partitioned: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self { partitioned: true }
    }
}

/// Generation outcome.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Generated {
    /// State root.
    pub root: B256,
    /// Number of account leaves processed.
    pub accounts: u64,
}

/// Builds the whole state trie from the hashed state behind `factory`, handing every stored
/// branch node to `on_node` as it completes: `(None, path, node)` for the state trie,
/// `(Some(hashed_address), path, node)` for a storage trie.
///
/// `on_node` is invoked from partition workers concurrently. Pass `|_, _, _| {}` to compute
/// only the root.
pub fn generate<H>(
    factory: &H,
    config: &Config,
    on_node: &impl OnNode,
) -> Result<Generated, DatabaseError>
where
    H: HashedCursorFactory + Sync,
{
    let subtries: Vec<Option<Subtrie>> = if config.partitioned {
        (0u8..16)
            .into_par_iter()
            .map(|nibble| build_subtrie(factory, Nibbles::from_nibbles([nibble]), on_node))
            .collect::<Result<_, _>>()?
    } else {
        vec![build_subtrie(factory, Nibbles::new(), on_node)?]
    };
    let populated: Vec<&Subtrie> = subtries.iter().flatten().collect();
    let accounts = populated.iter().map(|s| s.leaves).sum();
    Ok(Generated { root: assemble_root(&populated, on_node), accounts })
}

/// Computes the state root from the subtrie roots, in key order, and hands `on_node` any branch
/// node that lies above the subtries.
///
/// This is a serial [`HashBuilder`] fed the way reth's walker feeds it: a subtrie whose root is a
/// leaf is added as that leaf, one whose root is (or hangs below) a branch is added as that
/// branch's hash at its path via `add_branch`, and the builder itself forms whatever leaf,
/// extension or branch the trie has above them. So no subtrie count is special: none gives the
/// empty root, one gives its root re-keyed by the builder, and more give the root branch. With
/// one-nibble partitions the only node above the subtries is the state root, which is never
/// stored, so nothing is emitted; deeper partitions would emit their common branches here.
///
/// Every node of the account trie is hashed rather than inlined, since an account leaf alone
/// exceeds 32 bytes of RLP, so a branch under a subtrie root is always referenced by hash.
pub fn assemble_root(subtries: &[&Subtrie], on_node: &impl OnNode) -> B256 {
    let mut hb = HashBuilder::default().with_updates(true);
    for sub in subtries {
        match &sub.root {
            TrieNode::Leaf(leaf) => hb.add_leaf(leaf.key, &leaf.value),
            TrieNode::Extension(ext) => {
                let child = ext.child.as_hash().expect("account trie nodes are never inline");
                hb.add_branch(ext.key, child, sub.stored);
            }
            TrieNode::Branch(branch) => {
                hb.add_branch(sub.prefix, keccak256(alloy_rlp::encode(branch)), sub.stored);
            }
            TrieNode::EmptyRoot => unreachable!("empty subtries are not built"),
        }
    }
    let root = hb.root();
    emit_completed(&mut hb, None, on_node);
    root
}
