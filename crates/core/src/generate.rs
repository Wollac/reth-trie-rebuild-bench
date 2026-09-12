//! Whole-trie generation: partitioned in parallel, or serial for reference.

use crate::{
    partition::{build_subtrie, node_hash, Subtrie},
    OnNode,
};
use alloy_primitives::B256;
use alloy_trie::nodes::{BranchNodeRef, RlpNode};
use rayon::prelude::*;
use reth_storage_errors::db::DatabaseError;
use reth_trie::hashed_cursor::HashedCursorFactory;
use reth_trie_common::{Nibbles, TrieMask, EMPTY_ROOT_HASH};

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
    if !config.partitioned {
        return Ok(match build_subtrie(factory, Nibbles::new(), on_node)? {
            Some(sub) => Generated { root: node_hash(&sub.root), accounts: sub.leaves },
            None => Generated { root: EMPTY_ROOT_HASH, accounts: 0 },
        });
    }

    let subtries: Vec<Option<Subtrie>> = (0u8..16)
        .into_par_iter()
        .map(|nibble| build_subtrie(factory, Nibbles::from_nibbles([nibble]), on_node))
        .collect::<Result<_, _>>()?;
    let populated: Vec<&Subtrie> = subtries.iter().flatten().collect();
    let accounts = populated.iter().map(|s| s.leaves).sum();

    Ok(match populated.len() {
        0 => Generated { root: EMPTY_ROOT_HASH, accounts: 0 },
        1 => {
            // With a single populated first nibble the real root is that subtrie's root wrapped
            // in the nibble: an extension or a leaf, never a branch. Every interior node the
            // partition emitted is identical to the serial build's, so only the root hash is
            // recomputed here, cheaply, since such tries are tiny.
            let sub = build_subtrie(factory, Nibbles::new(), &|_, _, _| {})?
                .expect("populated partition implies non-empty trie");
            Generated { root: node_hash(&sub.root), accounts }
        }
        _ => Generated { root: assemble_root(&populated), accounts },
    })
}

/// Hashes the root branch node formed by the populated first-nibble subtries. The node itself is
/// not emitted: reth never stores the root node.
fn assemble_root(subtries: &[&Subtrie]) -> B256 {
    let mut state_mask = TrieMask::default();
    let mut stack: Vec<RlpNode> = Vec::with_capacity(subtries.len());
    for sub in subtries {
        state_mask |= TrieMask::from_nibble(sub.prefix.first().expect("first-nibble partition"));
        stack.push(sub.root.clone());
    }
    let mut buf = Vec::with_capacity(17 * 33);
    node_hash(&BranchNodeRef::new(&stack, state_mask).rlp(&mut buf))
}
