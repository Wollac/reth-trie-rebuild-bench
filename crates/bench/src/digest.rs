//! Order-independent digest of an emitted node set, for checking parity without holding the
//! nodes.
//!
//! Every node is hashed with its identity (account, path, masks, hashes) and the hashes are
//! folded with XOR, so the result does not depend on emission order, which differs between
//! partitions, drains and runs. Two node multisets with equal count and equal fold are equal
//! with overwhelming probability. Updates are lock-free, so the digest can be fed from many
//! threads.
//!
//! The hash is two seeded foldhash passes, 128 bits: both sides of a parity check are produced
//! by code we control, so a non-cryptographic hash is enough and keeps the digest far cheaper
//! than the keccak work that produced the nodes.

use alloy_primitives::B256;
use foldhash::fast::FixedState;
use reth_trie_common::{updates::TrieUpdates, BranchNodeCompact, Nibbles};
use reth_trie_parallel::partitioned_root::TrieSink;
use std::{
    hash::BuildHasher,
    sync::atomic::{AtomicU64, Ordering},
};

const SEEDS: [FixedState; 2] =
    [FixedState::with_seed(0x7472_6965_2d67_656e), FixedState::with_seed(0x6e6f_6465_2d64_6967)];

/// Running digest of a node multiset.
#[derive(Debug, Default)]
pub struct NodeDigest {
    count: AtomicU64,
    fold: [AtomicU64; 2],
}

/// Snapshot of a [`NodeDigest`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Digest {
    /// Number of nodes folded in.
    pub count: u64,
    /// XOR of the per-node 128-bit hashes.
    pub fold: u128,
}

impl NodeDigest {
    /// Folds one node in. `hashed_address` is `None` for the state trie.
    pub fn push(&self, hashed_address: Option<B256>, path: Nibbles, node: &BranchNodeCompact) {
        let mut buf = Vec::with_capacity(32 + 1 + 1 + 32 + 6 + 32 * 17 + 33);
        buf.extend_from_slice(hashed_address.unwrap_or(B256::ZERO).as_slice());
        buf.push(hashed_address.is_some() as u8);
        buf.push(path.len() as u8);
        buf.extend_from_slice(&path.pack());
        buf.extend_from_slice(&node.state_mask.get().to_be_bytes());
        buf.extend_from_slice(&node.tree_mask.get().to_be_bytes());
        buf.extend_from_slice(&node.hash_mask.get().to_be_bytes());
        for hash in node.hashes.iter() {
            buf.extend_from_slice(hash.as_slice());
        }
        buf.extend_from_slice(node.root_hash.unwrap_or(B256::ZERO).as_slice());
        buf.push(node.root_hash.is_some() as u8);

        for (seed, acc) in SEEDS.iter().zip(&self.fold) {
            acc.fetch_xor(seed.hash_one(&buf), Ordering::Relaxed);
        }
        self.count.fetch_add(1, Ordering::Relaxed);
    }

    /// The digest so far.
    pub fn snapshot(&self) -> Digest {
        let hi = self.fold[0].load(Ordering::Relaxed);
        let lo = self.fold[1].load(Ordering::Relaxed);
        Digest {
            count: self.count.load(Ordering::Relaxed),
            fold: (u128::from(hi) << 64) | u128::from(lo),
        }
    }

    /// Digest of a reth [`TrieUpdates`], for comparison with a generator run. Removed nodes are
    /// not part of a build and are ignored.
    pub fn of_trie_updates(updates: &TrieUpdates) -> Digest {
        let digest = Self::default();
        for (path, node) in &updates.account_nodes {
            digest.push(None, *path, node);
        }
        for (hashed_address, trie) in &updates.storage_tries {
            for (path, node) in &trie.storage_nodes {
                digest.push(Some(*hashed_address), *path, node);
            }
        }
        digest.snapshot()
    }
}

impl TrieSink for NodeDigest {
    fn on_branch_node(&self, hashed_address: Option<B256>, path: Nibbles, node: BranchNodeCompact) {
        self.push(hashed_address, path, &node)
    }
}
