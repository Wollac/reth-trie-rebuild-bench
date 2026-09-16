//! A sink that commits emitted nodes into the trie tables of a database, in chunks.
//!
//! Partition workers hand their nodes to one writer thread over a bounded channel, which fills a
//! `TrieUpdates` chunk and commits it in a write transaction once the chunk holds enough nodes,
//! the way the merkle stage commits its progress. The channel is bounded, so a slow disk holds
//! the workers back rather than letting the chunks pile up in memory.
//!
//! The database allows one writer alongside any number of readers, so the write transactions
//! run while the partitions' read transactions stay open. Those readers pin the pages they read,
//! which keeps the writer from reusing freed pages for the length of the build; on a rebuild
//! that only adds nodes to empty tables, nothing is freed anyway.

use crate::{mdbx::uses_packed_trie_keys, rebuild::write_trie_updates_with};
use alloy_primitives::B256;
use reth_db_api::{transaction::DbTx, Database};
use reth_storage_errors::db::DatabaseError;
use reth_trie::updates::TrieUpdates;
use reth_trie_common::{BranchNodeCompact, Nibbles};
use reth_trie_db::{LegacyKeyAdapter, PackedKeyAdapter};
use reth_trie_parallel::partitioned_root::TrieSink;
use std::{
    sync::{
        mpsc::{sync_channel, Receiver, SyncSender},
        Arc,
    },
    thread::{self, JoinHandle},
};

/// The number of nodes a chunk holds before it is committed. reth's serial walk commits every
/// 100,000 updated nodes on a rebuild.
pub const DEFAULT_CHUNK_NODES: usize = 100_000;

/// Nodes in flight between the workers and the writer before a worker blocks.
const CHANNEL_CAPACITY: usize = 1 << 16;

type Node = (Option<B256>, Nibbles, BranchNodeCompact);

/// What the writer committed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Written {
    /// Trie table entries written.
    pub nodes: usize,
    /// Write transactions committed.
    pub chunks: usize,
}

/// A [`TrieSink`] that writes nodes into the trie tables of `db` in chunks, on its own thread.
///
/// Call [`Self::finish`] after the build to flush the last chunk and learn what was written.
#[derive(Debug)]
pub struct MdbxWriteSink {
    sender: Option<SyncSender<Node>>,
    writer: Option<JoinHandle<Result<Written, DatabaseError>>>,
}

impl MdbxWriteSink {
    /// Starts a writer over `db`, committing every `chunk_nodes` nodes in the key encoding the
    /// database uses. The trie tables are expected to be empty.
    pub fn new<DB>(db: Arc<DB>, chunk_nodes: usize) -> Result<Self, DatabaseError>
    where
        DB: Database + Send + Sync + 'static,
    {
        let packed = uses_packed_trie_keys(&db.tx()?)?;
        let (sender, receiver) = sync_channel(CHANNEL_CAPACITY);
        let writer = thread::Builder::new()
            .name("trie-writer".into())
            .spawn(move || write_chunks(&*db, packed, chunk_nodes.max(1), &receiver))
            .expect("spawn writer thread");
        Ok(Self { sender: Some(sender), writer: Some(writer) })
    }

    /// Flushes the remaining nodes, waits for the writer and returns what it committed.
    pub fn finish(mut self) -> Result<Written, DatabaseError> {
        // Closing the channel ends the writer's loop.
        drop(self.sender.take());
        self.writer.take().expect("finish is called once").join().expect("writer thread panicked")
    }
}

impl TrieSink for MdbxWriteSink {
    fn on_branch_node(&self, hashed_address: Option<B256>, path: Nibbles, node: BranchNodeCompact) {
        self.sender
            .as_ref()
            .expect("sink is used before finish")
            .send((hashed_address, path, node))
            .expect("writer thread is alive");
    }
}

/// The writer loop: fills a chunk from `receiver` and commits it every `chunk_nodes` nodes.
fn write_chunks<DB: Database>(
    db: &DB,
    packed: bool,
    chunk_nodes: usize,
    receiver: &Receiver<Node>,
) -> Result<Written, DatabaseError> {
    let mut written = Written { nodes: 0, chunks: 0 };
    let mut chunk = TrieUpdates::default();
    let mut pending = 0;
    let commit = |chunk: TrieUpdates| -> Result<usize, DatabaseError> {
        let tx = db.tx_mut()?;
        let nodes = if packed {
            write_trie_updates_with::<_, PackedKeyAdapter>(&tx, chunk)?
        } else {
            write_trie_updates_with::<_, LegacyKeyAdapter>(&tx, chunk)?
        };
        tx.commit()?;
        Ok(nodes)
    };

    for (hashed_address, path, node) in receiver {
        let prev = match hashed_address {
            None => chunk.account_nodes.insert(path, node),
            Some(hashed_address) => chunk
                .storage_tries
                .entry(hashed_address)
                .or_default()
                .storage_nodes
                .insert(path, node),
        };
        debug_assert!(prev.is_none(), "node emitted twice for {hashed_address:?} {path:?}");
        pending += 1;
        if pending >= chunk_nodes {
            written.nodes += commit(std::mem::take(&mut chunk))?;
            written.chunks += 1;
            pending = 0;
        }
    }
    if pending > 0 {
        written.nodes += commit(chunk)?;
        written.chunks += 1;
    }
    Ok(written)
}
