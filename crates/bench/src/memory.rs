//! In-memory hashed state, exposed through reth's post-state cursors, plus a node-collecting sink.

use alloy_primitives::{B256, U256};
use reth_primitives_traits::Account;
use reth_storage_api::DatabaseProviderROFactory;
use reth_storage_errors::provider::ProviderResult;
use reth_trie::hashed_cursor::{
    noop::NoopHashedCursorFactory, HashedCursorFactory, HashedPostStateCursorFactory,
};
use reth_trie_common::{HashedPostState, HashedPostStateSorted, HashedStorage};
use std::{collections::BTreeMap, sync::OnceLock};

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

    /// A provider factory serving every partition with a cursor factory over this state.
    pub fn provider_factory(&self) -> SharedCursorFactory<MemoryCursorFactory<'_>> {
        SharedCursorFactory(self.cursor_factory())
    }
}

/// A [`DatabaseProviderROFactory`] that serves every partition with a clone of the same cursor
/// factory, standing in for reth's read-only database providers.
///
/// This suits factories whose cursors do not contend with each other, such as those over
/// in-memory state. A factory over a single database transaction does not qualify.
#[derive(Debug, Clone)]
pub struct SharedCursorFactory<H>(pub H);

impl<H: HashedCursorFactory + Clone> DatabaseProviderROFactory for SharedCursorFactory<H> {
    type Provider = H;

    fn database_provider_ro(&self) -> ProviderResult<H> {
        Ok(self.0.clone())
    }
}
