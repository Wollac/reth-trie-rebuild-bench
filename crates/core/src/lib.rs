//! Bulk Merkle-Patricia trie generation from sorted hashed state.
//!
//! Input is reth's hashed state, read through a [`HashedCursorFactory`]: account leaves keyed by
//! `keccak(address)` and storage leaves keyed by `(keccak(address), keccak(slot))`, both in
//! ascending key order. Output is the state root plus every intermediate branch node that reth
//! persists in its `AccountsTrie` / `StoragesTrie` tables, as [`BranchNodeCompact`] keyed by
//! nibble path, handed to a caller-supplied closure as nodes complete.
//!
//! The account trie is split into 16 partitions by the first nibble of the hashed address. Each
//! partition is built independently with a [`HashBuilder`] over its key range, with absolute
//! keys, so its nodes come out exactly as the serial build's; the node above the partitions is
//! then formed by one more `HashBuilder` fed the 16 subtrie root nodes. Storage tries are built
//! per account before the account leaf is encoded, so a stale storage root in the hashed account
//! row (as left behind by block access list application during snap sync) is never trusted.
//!
//! Node emission and mask semantics are byte-for-byte those of reth's serial
//! [`reth_trie::StateRoot`] over the same cursors, which is what the harness tests assert. In
//! particular, root branch nodes (empty path) of the state trie and of every storage trie are not
//! emitted, because reth's `TrieUpdates` excludes them.
//!
//! Everything here uses reth's own types so the module can move into reth's trie crates
//! unchanged.

pub mod collect;
mod emit;
pub mod generate;
pub mod partition;
pub mod storage;

pub use collect::TrieUpdatesCollector;
pub use generate::{generate, Config, Generated};
pub use reth_primitives_traits::Account;
pub use reth_storage_errors::db::DatabaseError;
pub use reth_trie::hashed_cursor::{HashedCursor, HashedCursorFactory, HashedStorageCursor};
pub use reth_trie_common::{
    updates::{StorageTrieUpdates, TrieUpdates},
    BranchNodeCompact, HashBuilder, Nibbles, TrieAccount, TrieMask, EMPTY_ROOT_HASH,
};

/// Receives every stored branch node as `(account, path, node)`: `account` is `None` for the
/// state trie and `Some(hashed_address)` for that account's storage trie, following reth's own
/// convention; `path` is absolute within that trie.
///
/// Called from partition workers concurrently, hence `Fn + Sync`.
pub trait OnNode: Fn(Option<alloy_primitives::B256>, Nibbles, BranchNodeCompact) + Sync {}
impl<F: Fn(Option<alloy_primitives::B256>, Nibbles, BranchNodeCompact) + Sync> OnNode for F {}
