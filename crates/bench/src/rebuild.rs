//! reth's own full trie rebuild, run directly on a database.
//!
//! This is the work `MerkleStage` does on a rebuild, minus the pipeline around it: clear the trie
//! tables, walk the hashed state with reth's serial `StateRoot`, and persist the branch nodes in
//! chunks as `root_with_progress` hands them out. It exists because `reth stage run merkle`
//! cannot run on a partial datadir: reth's CLI first checks static files against the database,
//! and a snapshot without changeset static files fails that check.

use crate::mdbx::uses_packed_trie_keys;
use alloy_primitives::B256;
use reth_db::tables;
use reth_db_api::{
    cursor::{DbCursorRO, DbCursorRW},
    transaction::{DbTx, DbTxMut},
    Database,
};
use reth_storage_errors::db::DatabaseError;
use reth_trie::{updates::TrieUpdates, IntermediateStateRootState, StateRoot, StateRootProgress};
use reth_trie_db::{
    DatabaseStateRoot, DatabaseStorageTrieCursor, DatabaseTrieCursorFactory, LegacyKeyAdapter,
    PackedKeyAdapter, TrieTableAdapter,
};
use std::time::Instant;

/// Empties the trie tables, as the merkle stage does before a rebuild.
pub fn clear_trie_tables(db: &impl Database) -> Result<(), DatabaseError> {
    let tx = db.tx_mut()?;
    tx.clear::<tables::AccountsTrie>()?;
    tx.clear::<tables::StoragesTrie>()?;
    tx.commit()?;
    Ok(())
}

/// Outcome of a rebuild.
#[derive(Clone, Copy, Debug)]
pub struct Rebuilt {
    /// The state root.
    pub root: B256,
    /// Hashed entries walked.
    pub entries: usize,
    /// Trie table entries written.
    pub written: usize,
    /// Number of committed chunks.
    pub chunks: usize,
}

/// Rebuilds the trie tables from the hashed state with reth's serial state root, committing a
/// chunk of updates every time the walk yields progress, the way `MerkleStage` does. `threshold`
/// is the number of updated nodes after which a chunk is committed; `None` is reth's default.
pub fn reth_rebuild(
    db: &impl Database,
    threshold: Option<u64>,
    log: impl Fn(&str),
) -> Result<Rebuilt, DatabaseError> {
    let packed = uses_packed_trie_keys(&db.tx()?)?;
    clear_trie_tables(db)?;
    if packed {
        rebuild::<_, PackedKeyAdapter>(db, threshold, log)
    } else {
        rebuild::<_, LegacyKeyAdapter>(db, threshold, log)
    }
}

fn rebuild<DB: Database, A: TrieTableAdapter>(
    db: &DB,
    threshold: Option<u64>,
    log: impl Fn(&str),
) -> Result<Rebuilt, DatabaseError> {
    let started = Instant::now();
    let mut state: Option<Box<IntermediateStateRootState>> = None;
    let mut totals = Rebuilt { root: B256::ZERO, entries: 0, written: 0, chunks: 0 };
    loop {
        let tx = db.tx_mut()?;
        let mut walk = StateRoot::<DatabaseTrieCursorFactory<_, A>, _>::from_tx(&tx)
            .with_intermediate_state(state.take().map(|s| *s));
        if let Some(threshold) = threshold {
            walk = walk.with_threshold(threshold);
        }
        let progress =
            walk.root_with_progress().map_err(|e| DatabaseError::Other(e.to_string()))?;
        let (root, entries, updates) = match progress {
            StateRootProgress::Progress(next, entries, updates) => {
                state = Some(next);
                (None, entries, updates)
            }
            StateRootProgress::Complete(root, entries, updates) => (Some(root), entries, updates),
        };
        totals.written += write_trie_updates_with::<_, A>(&tx, updates)?;
        tx.commit()?;
        totals.entries += entries;
        totals.chunks += 1;
        if totals.chunks.is_multiple_of(100) {
            log(&format!(
                "chunk {}: {} entries walked, {} nodes written, {:.0?}",
                totals.chunks,
                totals.entries,
                totals.written,
                started.elapsed()
            ));
        }
        if let Some(root) = root {
            return Ok(Rebuilt { root, ..totals });
        }
    }
}

/// Writes `updates` into the trie tables in the database's key encoding, as the merkle stage
/// commits them, and returns the number of table entries written.
pub fn write_trie_updates(
    db: &impl Database,
    updates: TrieUpdates,
) -> Result<usize, DatabaseError> {
    let tx = db.tx_mut()?;
    let written = if uses_packed_trie_keys(&tx)? {
        write_trie_updates_with::<_, PackedKeyAdapter>(&tx, updates)?
    } else {
        write_trie_updates_with::<_, LegacyKeyAdapter>(&tx, updates)?
    };
    tx.commit()?;
    Ok(written)
}

/// The provider's `write_trie_updates`, without the provider: upserts and deletes account nodes
/// and hands each storage trie to reth's storage trie cursor.
pub(crate) fn write_trie_updates_with<TX: DbTxMut + DbTx, A: TrieTableAdapter>(
    tx: &TX,
    updates: TrieUpdates,
) -> Result<usize, DatabaseError> {
    let sorted = updates.into_sorted();
    let mut written = 0;

    let mut accounts = tx.cursor_write::<A::AccountTrieTable>()?;
    for (path, node) in sorted.account_nodes_ref() {
        let key = A::AccountKey::from(*path);
        match node {
            Some(node) => {
                if !path.is_empty() {
                    accounts.upsert(key, node)?;
                    written += 1;
                }
            }
            None => {
                if accounts.seek_exact(key)?.is_some() {
                    accounts.delete_current()?;
                    written += 1;
                }
            }
        }
    }

    let mut storage_tries: Vec<_> = sorted.storage_tries_ref().iter().collect();
    storage_tries.sort_unstable_by(|a, b| a.0.cmp(b.0));
    let mut cursor = tx.cursor_dup_write::<A::StorageTrieTable>()?;
    for (hashed_address, trie) in storage_tries {
        let mut storage: DatabaseStorageTrieCursor<_, A> =
            DatabaseStorageTrieCursor::new(cursor, *hashed_address);
        written += storage.write_storage_trie_updates_sorted(trie)?;
        cursor = storage.cursor;
    }
    Ok(written)
}
