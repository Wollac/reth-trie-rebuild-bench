//! In-memory hashed state, exposed through reth's post-state cursors, plus a node-collecting sink.

use alloy_primitives::{B256, U256};
use reth_trie::hashed_cursor::{noop::NoopHashedCursorFactory, HashedPostStateCursorFactory};
use reth_trie_common::{
    BranchNodeCompact, HashedPostState, HashedPostStateSorted, HashedStorage, Nibbles,
};
use std::{
    collections::BTreeMap,
    sync::{Mutex, OnceLock},
};
use trie_gen_core::Account;

/// Cursor factory over an in-memory state: reth's post-state overlay on top of nothing.
pub type MemoryCursorFactory<'a> =
    HashedPostStateCursorFactory<NoopHashedCursorFactory, &'a HashedPostStateSorted>;

/// Hashed state held in ordered maps, sortable into reth's `HashedPostStateSorted`.
#[derive(Debug, Default)]
pub struct MemoryState {
    accounts: BTreeMap<B256, Account>,
    storages: BTreeMap<B256, BTreeMap<B256, U256>>,
    sorted: OnceLock<HashedPostStateSorted>,
}

impl Clone for MemoryState {
    fn clone(&self) -> Self {
        Self {
            accounts: self.accounts.clone(),
            storages: self.storages.clone(),
            sorted: OnceLock::new(),
        }
    }
}

impl MemoryState {
    /// Inserts or replaces an account.
    pub fn insert_account(&mut self, hashed_address: B256, account: Account) {
        self.sorted = OnceLock::new();
        self.accounts.insert(hashed_address, account);
    }

    /// Sets a storage slot; zero removes it.
    pub fn insert_storage(&mut self, hashed_address: B256, hashed_slot: B256, value: U256) {
        self.sorted = OnceLock::new();
        let slots = self.storages.entry(hashed_address).or_default();
        if value.is_zero() {
            slots.remove(&hashed_slot);
        } else {
            slots.insert(hashed_slot, value);
        }
    }

    /// Number of accounts.
    pub fn account_count(&self) -> usize {
        self.accounts.len()
    }

    /// Total number of storage slots.
    pub fn storage_count(&self) -> usize {
        self.storages.values().map(BTreeMap::len).sum()
    }

    /// All accounts in key order.
    pub fn iter_accounts(&self) -> impl Iterator<Item = (B256, Account)> + '_ {
        self.accounts.iter().map(|(k, v)| (*k, *v))
    }

    /// Storage of one account in key order.
    pub fn iter_storage(&self, hashed_address: &B256) -> impl Iterator<Item = (B256, U256)> + '_ {
        self.storages
            .get(hashed_address)
            .into_iter()
            .flat_map(|slots| slots.iter().map(|(k, v)| (*k, *v)))
    }

    /// The state as reth's sorted post-state, built once and cached.
    pub fn sorted(&self) -> &HashedPostStateSorted {
        self.sorted.get_or_init(|| {
            let mut post = HashedPostState::default();
            for (addr, account) in &self.accounts {
                post.accounts.insert(*addr, Some(*account));
            }
            for (addr, slots) in &self.storages {
                if !slots.is_empty() {
                    post.storages.insert(
                        *addr,
                        HashedStorage::from_iter(slots.iter().map(|(k, v)| (*k, *v))),
                    );
                }
            }
            post.into_sorted()
        })
    }

    /// A reth hashed-cursor factory reading this state.
    pub fn cursor_factory(&self) -> MemoryCursorFactory<'_> {
        HashedPostStateCursorFactory::new(NoopHashedCursorFactory::default(), self.sorted())
    }
}

/// Collects nodes into ordered maps for exact comparison between runs.
#[derive(Debug, Default)]
pub struct MemorySink {
    accounts: Mutex<BTreeMap<Nibbles, BranchNodeCompact>>,
    storages: Mutex<BTreeMap<(B256, Nibbles), BranchNodeCompact>>,
}

/// Account nodes by path, and storage nodes by `(hashed address, path)`.
pub type NodeMaps =
    (BTreeMap<Nibbles, BranchNodeCompact>, BTreeMap<(B256, Nibbles), BranchNodeCompact>);

impl MemorySink {
    /// Consumes the sink and returns the collected nodes.
    pub fn into_maps(self) -> NodeMaps {
        (self.accounts.into_inner().unwrap(), self.storages.into_inner().unwrap())
    }
}

impl MemorySink {
    /// Records a node; `account` is `None` for the state trie. Panics on duplicates.
    pub fn push(&self, account: Option<B256>, path: Nibbles, node: BranchNodeCompact) {
        let prev = match account {
            None => self.accounts.lock().unwrap().insert(path, node),
            Some(hashed_address) => {
                self.storages.lock().unwrap().insert((hashed_address, path), node)
            }
        };
        assert!(prev.is_none(), "node emitted twice for {account:?} {path:?}");
    }
}
