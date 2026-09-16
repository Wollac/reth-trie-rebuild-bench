//! The generator over reth's real MDBX cursors, one read transaction per partition worker,
//! against reth's `StateRoot::from_tx` on the same database.

use alloy_primitives::{B256, U256};
use reth_db::test_utils::create_test_rw_db;
use reth_db_api::{models::StorageSettings, Database};
use reth_trie_parallel::partitioned_root::{HashedStateFactory, PartitionedStateRoot, TrieSink};
use reth_trie_rebuild_bench::{
    digest_trie_tables, reth_rebuild, reth_root_with_updates, reth_serial_root, runtime,
    synth::{synthesize, SynthConfig},
    without_empty_storage_tries, write_hashed_state, write_storage_settings, write_trie_updates,
    DatabaseSource, MdbxWriteSink, NodeDigest, TrieUpdatesSink,
};

#[test]
fn matches_reth_on_mdbx_cursors() {
    let mut state = synthesize(&SynthConfig {
        seed: 7,
        eoas: 3_000,
        small_contracts: 200,
        small_slots_max: 16,
        large_contracts: vec![5_000],
    });
    // A near-zero first slot makes the largest contract's trie look huge to the size estimate,
    // so it is built in partitions, each on a read transaction of its own.
    let largest = state
        .iter_accounts()
        .map(|(hashed_address, _)| hashed_address)
        .max_by_key(|hashed_address| state.iter_storage(hashed_address).count())
        .unwrap();
    state.insert_storage(largest, B256::with_last_byte(1), U256::from(7));
    let db = create_test_rw_db();
    write_hashed_state(&db, &state).unwrap();
    let (reth_root, reth_updates) = reth_root_with_updates(&db).unwrap();

    let collector = TrieUpdatesSink::default();
    let digest = NodeDigest::default();
    let ours_root =
        PartitionedStateRoot::new(HashedStateFactory::new(DatabaseSource(&db)), &runtime(None))
            .root_with_sink(&|a, p, n| {
                digest.push(a, p, &n);
                collector.on_branch_node(a, p, n)
            })
            .unwrap();

    assert_eq!(ours_root, reth_root, "root vs reth StateRoot over MDBX");
    assert_eq!(digest.snapshot(), NodeDigest::of_trie_updates(&reth_updates));
    assert_eq!(
        without_empty_storage_tries(collector.into_trie_updates()),
        without_empty_storage_tries(reth_updates),
        "TrieUpdates vs reth over MDBX"
    );
}

/// Writes reth's updates into the trie tables the way the merkle stage commits them, in the
/// given key encoding, and checks that the table digest reproduces the updates digest.
fn check_trie_tables_digest(packed: bool) {
    let state = synthesize(&SynthConfig {
        seed: 11,
        eoas: 2_000,
        small_contracts: 300,
        small_slots_max: 24,
        large_contracts: vec![3_000],
    });
    let db = create_test_rw_db();
    write_hashed_state(&db, &state).unwrap();
    let (_, updates) = reth_root_with_updates(&db).unwrap();
    if packed {
        write_storage_settings(&db, StorageSettings::v2()).unwrap();
    }

    let expected_account_nodes = updates.account_nodes.len() as u64;
    let expected_storage_nodes =
        updates.storage_tries.values().map(|t| t.storage_nodes.len() as u64).sum::<u64>();
    let written = write_trie_updates(&db, updates.clone()).unwrap();
    assert_eq!(written as u64, expected_account_nodes + expected_storage_nodes);

    let tx = db.tx().unwrap();
    let tables = digest_trie_tables(&tx).unwrap();
    assert_eq!(tables.account_nodes, expected_account_nodes);
    assert_eq!(tables.storage_nodes, expected_storage_nodes);
    assert!(tables.storage_nodes > 0, "test state must produce storage branch nodes");
    assert_eq!(tables.digest, NodeDigest::of_trie_updates(&updates));
}

#[test]
fn trie_tables_digest_matches_updates_legacy_keys() {
    check_trie_tables_digest(false);
}

#[test]
fn trie_tables_digest_matches_updates_packed_keys() {
    check_trie_tables_digest(true);
}

/// reth's chunked rebuild on the database, in the given key encoding, must leave the trie tables
/// holding exactly the nodes reth's one-shot walk reports, and the generator must agree.
fn check_reth_rebuild(packed: bool) {
    let state = synthesize(&SynthConfig {
        seed: 13,
        eoas: 4_000,
        small_contracts: 400,
        small_slots_max: 24,
        large_contracts: vec![6_000],
    });
    let db = create_test_rw_db();
    write_hashed_state(&db, &state).unwrap();
    if packed {
        write_storage_settings(&db, StorageSettings::v2()).unwrap();
    }
    let (reth_root, reth_updates) = reth_root_with_updates(&db).unwrap();

    // A small chunk (in updated nodes) so the intermediate state round-trips through several
    // commits.
    let rebuilt = reth_rebuild(&db, Some(50), |_| {}).unwrap();
    assert_eq!(rebuilt.root, reth_root);
    assert!(rebuilt.chunks > 3, "expected several chunks, got {}", rebuilt.chunks);

    let tx = db.tx().unwrap();
    let tables = digest_trie_tables(&tx).unwrap();
    assert_eq!(tables.digest, NodeDigest::of_trie_updates(&reth_updates));

    let digest = NodeDigest::default();
    let ours_root =
        PartitionedStateRoot::new(HashedStateFactory::new(DatabaseSource(&db)), &runtime(None))
            .root_with_sink(&digest)
            .unwrap();
    assert_eq!(ours_root, reth_root);
    assert_eq!(digest.snapshot(), tables.digest);
}

#[test]
fn reth_rebuild_matches_updates_legacy_keys() {
    check_reth_rebuild(false);
}

#[test]
fn reth_rebuild_matches_updates_packed_keys() {
    check_reth_rebuild(true);
}

/// The writing sink, committing in small chunks from its own thread while the partitions read,
/// must leave the trie tables holding exactly the nodes reth's one-shot walk reports.
fn check_write_sink(packed: bool) {
    let state = synthesize(&SynthConfig {
        seed: 17,
        eoas: 4_000,
        small_contracts: 400,
        small_slots_max: 24,
        large_contracts: vec![6_000],
    });
    let db = create_test_rw_db();
    write_hashed_state(&db, &state).unwrap();
    if packed {
        write_storage_settings(&db, StorageSettings::v2()).unwrap();
    }
    let (reth_root, reth_updates) = reth_root_with_updates(&db).unwrap();

    let writer = MdbxWriteSink::new(db.clone(), 50).unwrap();
    let root =
        PartitionedStateRoot::new(HashedStateFactory::new(DatabaseSource(&db)), &runtime(None))
            .root_with_sink(&writer)
            .unwrap();
    let written = writer.finish().unwrap();
    assert_eq!(root, reth_root);
    assert!(written.chunks > 3, "expected several chunks, got {}", written.chunks);

    let tx = db.tx().unwrap();
    let tables = digest_trie_tables(&tx).unwrap();
    assert_eq!(tables.digest, NodeDigest::of_trie_updates(&reth_updates));
    assert_eq!(tables.digest.count as usize, written.nodes);
    // reth's own walker over the written tables reproduces the root from the stored nodes.
    assert_eq!(reth_serial_root(&tx).unwrap(), reth_root);
}

#[test]
fn write_sink_matches_reth_legacy_keys() {
    check_write_sink(false);
}

#[test]
fn write_sink_matches_reth_packed_keys() {
    check_write_sink(true);
}
