//! Real reth MDBX databases for cursor-level tests and datadir runs.

use crate::{Digest, MemoryState, NodeDigest};
use alloy_primitives::B256;
use rayon::prelude::*;
use reth_db::tables;
use reth_db_api::{
    cursor::DbCursorRO,
    models::StorageSettings,
    transaction::{DbTx, DbTxMut},
    Database,
};
use reth_primitives_traits::StorageEntry;
use reth_prune_types::PruneModes;
use reth_storage_api::{
    metadata::keys::STORAGE_SETTINGS, DBProvider, DatabaseProviderROFactory, DbTxProvider,
};
use reth_storage_errors::{db::DatabaseError, provider::ProviderResult};
use reth_trie::{updates::TrieUpdates, StateRoot};
use reth_trie_db::{
    DatabaseStateRoot, DatabaseTrieCursorFactory, LegacyKeyAdapter, PackedKeyAdapter,
    StorageTrieEntryLike, TrieTableAdapter,
};

/// A read-only provider factory over a bare database, standing in for reth's `ProviderFactory`,
/// which needs a full node setup. Every provider holds a new read transaction.
#[derive(Debug)]
pub struct DatabaseSource<'db, DB>(pub &'db DB);

impl<DB: Database> DatabaseProviderROFactory for DatabaseSource<'_, DB> {
    type Provider = TxProvider<DB::TX>;

    fn database_provider_ro(&self) -> ProviderResult<Self::Provider> {
        Ok(TxProvider { tx: self.0.tx()?, prune_modes: PruneModes::default() })
    }
}

/// A provider over a single transaction, with just what [`DBProvider`] requires.
#[derive(Debug)]
pub struct TxProvider<TX> {
    tx: TX,
    prune_modes: PruneModes,
}

impl<TX: DbTx> DbTxProvider for TxProvider<TX> {
    type Tx = TX;

    fn tx(&self) -> &TX {
        &self.tx
    }
}

impl<TX: DbTx> DBProvider for TxProvider<TX> {
    fn tx_mut(&mut self) -> &mut TX {
        &mut self.tx
    }

    fn into_tx(self) -> TX {
        self.tx
    }

    fn commit(self) -> ProviderResult<()> {
        Ok(self.tx.commit()?)
    }

    fn prune_modes_ref(&self) -> &PruneModes {
        &self.prune_modes
    }
}

/// Writes the state into the hashed state tables of `db`, the way the hashing stages would.
pub fn write_hashed_state(db: &impl Database, state: &MemoryState) -> Result<(), DatabaseError> {
    let tx = db.tx_mut()?;
    for (hashed_address, account) in state.iter_accounts() {
        tx.put::<tables::HashedAccounts>(hashed_address, account)?;
        for (key, value) in state.iter_storage(&hashed_address) {
            tx.put::<tables::HashedStorages>(hashed_address, StorageEntry { key, value })?;
        }
    }
    tx.commit()?;
    Ok(())
}

/// reth's own answer on `db`: root and `TrieUpdates` from its serial walk over the hashed
/// state. Meant for empty trie tables, where the key encoding adapter is irrelevant.
pub fn reth_root_with_updates(db: &impl Database) -> Result<(B256, TrieUpdates), DatabaseError> {
    let tx = db.tx()?;
    StateRoot::<DatabaseTrieCursorFactory<_, LegacyKeyAdapter>, _>::from_tx(&tx)
        .root_with_updates()
        .map_err(|e| DatabaseError::Other(e.to_string()))
}

/// The root from reth's serial `StateRoot` walk over whatever the trie tables of `tx` hold, in
/// the key encoding the database uses.
///
/// On empty tables this is the merkle stage's full-rebuild walk minus persistence. On tables a
/// rebuild has written, nothing is marked changed, so the walker takes every stored branch node's
/// hash instead of descending: the root then comes from the stored nodes alone, which shows that
/// reth reads the tables as the rebuild meant them.
pub fn reth_serial_root<T: DbTx>(tx: &T) -> Result<B256, DatabaseError> {
    let root = if uses_packed_trie_keys(tx)? {
        StateRoot::<DatabaseTrieCursorFactory<_, PackedKeyAdapter>, _>::from_tx(tx).root()
    } else {
        StateRoot::<DatabaseTrieCursorFactory<_, LegacyKeyAdapter>, _>::from_tx(tx).root()
    };
    root.map_err(|e| DatabaseError::Other(e.to_string()))
}

/// Records `settings` in the metadata table, as reth's provider does: v2 storage settings switch
/// the trie tables to the packed key encoding.
pub fn write_storage_settings(
    db: &impl Database,
    settings: StorageSettings,
) -> Result<(), DatabaseError> {
    let tx = db.tx_mut()?;
    let bytes = serde_json::to_vec(&settings).expect("storage settings serialize");
    tx.put::<tables::Metadata>(STORAGE_SETTINGS.to_string(), bytes)?;
    tx.commit()?;
    Ok(())
}

/// Whether the trie tables use the packed (storage v2) key encoding, as recorded by reth in the
/// metadata table. A database without the entry, or with one reth cannot read, is a legacy one.
pub fn uses_packed_trie_keys<T: DbTx>(tx: &T) -> Result<bool, DatabaseError> {
    Ok(tx
        .get::<tables::Metadata>(STORAGE_SETTINGS.to_string())?
        .and_then(|bytes| serde_json::from_slice::<StorageSettings>(&bytes).ok())
        .is_some_and(|settings| settings.is_v2()))
}

/// Node counts and digest of the trie tables reth has committed, for comparison with a
/// generator run: after a full rebuild the tables hold exactly the nodes the rebuild emitted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TrieTablesDigest {
    /// Nodes in the account trie table.
    pub account_nodes: u64,
    /// Nodes in the storage trie table.
    pub storage_nodes: u64,
    /// Digest over both, in the same fold a generator run reports.
    pub digest: Digest,
}

/// Digests the committed trie tables of `tx`, reading whichever key encoding the database uses.
/// The storage trie table is scanned in sixteen address ranges in parallel; the account trie
/// table serially.
pub fn digest_trie_tables<T: DbTx + Sync>(tx: &T) -> Result<TrieTablesDigest, DatabaseError> {
    if uses_packed_trie_keys(tx)? {
        digest_tables::<T, PackedKeyAdapter>(tx)
    } else {
        digest_tables::<T, LegacyKeyAdapter>(tx)
    }
}

fn digest_tables<T: DbTx + Sync, A: TrieTableAdapter>(
    tx: &T,
) -> Result<TrieTablesDigest, DatabaseError> {
    let digest = NodeDigest::default();

    let mut account_nodes = 0;
    for entry in tx.cursor_read::<A::AccountTrieTable>()?.walk(None)? {
        let (key, node) = entry?;
        digest.push(None, A::account_key_to_nibbles(&key), &node);
        account_nodes += 1;
    }

    let storage_nodes = (0u8..16)
        .into_par_iter()
        .map(|nibble| {
            let mut count = 0;
            let mut cursor = tx.cursor_dup_read::<A::StorageTrieTable>()?;
            let low = B256::right_padding_from(&[nibble << 4]);
            let mut high = B256::repeat_byte(0xff);
            high[0] = nibble << 4 | 0xf;
            for entry in cursor.walk_range(low..=high)? {
                let (hashed_address, value) = entry?;
                let (subkey, node) = value.into_parts();
                digest.push(Some(hashed_address), A::subkey_to_nibbles(&subkey), &node);
                count += 1;
            }
            Ok::<_, DatabaseError>(count)
        })
        .try_reduce(|| 0, |a, b| Ok(a + b))?;

    Ok(TrieTablesDigest { account_nodes, storage_nodes, digest: digest.snapshot() })
}
