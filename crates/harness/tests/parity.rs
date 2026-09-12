//! Partitioned generation must be indistinguishable from reth's serial `StateRoot` over the same
//! cursors, and from the crate's own serial path.

use alloy_primitives::{B256, U256};
use proptest::prelude::*;
use reth_trie::{trie_cursor::noop::NoopTrieCursorFactory, StateRoot};
use reth_trie_common::{updates::TrieUpdates, Nibbles};
use trie_gen_core::{generate, Account, Config, TrieUpdatesCollector};
use trie_gen_harness::{
    synth::{synthesize, SynthConfig},
    MemorySink, MemoryState,
};

/// reth's own answer: root and `TrieUpdates` from a serial walk over the same cursors.
fn reth_reference(state: &MemoryState) -> (B256, TrieUpdates) {
    StateRoot::new(NoopTrieCursorFactory::default(), state.cursor_factory())
        .root_with_updates()
        .unwrap()
}

/// Drops empty storage-trie entries so maps compare on content, not on presence of empties.
fn normalize(mut updates: TrieUpdates) -> TrieUpdates {
    updates.storage_tries.retain(|_, t| !t.storage_nodes.is_empty() || !t.removed_nodes.is_empty());
    updates
}

fn check(state: &MemoryState) {
    let factory = state.cursor_factory();

    let serial_sink = MemorySink::default();
    let serial =
        generate(&factory, &Config { partitioned: false }, &|a, p, n| serial_sink.push(a, p, n))
            .unwrap();
    let par_sink = MemorySink::default();
    let par = generate(&factory, &Config { partitioned: true }, &|a, p, n| par_sink.push(a, p, n))
        .unwrap();
    assert_eq!(par, serial, "partitioned vs serial result");

    let (serial_accounts, serial_storages) = serial_sink.into_maps();
    let (par_accounts, par_storages) = par_sink.into_maps();
    assert_eq!(par_accounts, serial_accounts, "account branch nodes");
    assert_eq!(par_storages, serial_storages, "storage branch nodes");

    // Against reth itself, through the TrieUpdates-shaped sink.
    let (reth_root, reth_updates) = reth_reference(state);
    let collector = TrieUpdatesCollector::default();
    let ours =
        generate(&factory, &Config { partitioned: true }, &|a, p, n| collector.push(a, p, n))
            .unwrap();
    assert_eq!(ours.root, reth_root, "root vs reth StateRoot");
    assert_eq!(normalize(collector.into_updates()), normalize(reth_updates), "TrieUpdates vs reth");
}

fn eoa(nonce: u64) -> Account {
    Account { nonce, balance: U256::from(nonce + 1), bytecode_hash: None }
}

fn key(bytes: &[u8]) -> B256 {
    let mut k = [0u8; 32];
    k[..bytes.len()].copy_from_slice(bytes);
    B256::from(k)
}

#[test]
fn empty_state() {
    check(&MemoryState::default());
}

#[test]
fn single_account() {
    let mut state = MemoryState::default();
    state.insert_account(B256::repeat_byte(0x11), eoa(1));
    check(&state);
}

#[test]
fn all_accounts_share_first_nibble() {
    // Single populated partition: the real root is an extension, not a branch.
    let mut state = MemoryState::default();
    for i in 0..30u8 {
        state.insert_account(key(&[0x70 | (i % 16), i.wrapping_mul(17), i]), eoa(i as u64));
    }
    check(&state);
}

#[test]
fn two_leaf_partitions_only() {
    // Root branch with two leaf children: nothing is stored, not even the root.
    let mut state = MemoryState::default();
    state.insert_account(key(&[0x10]), eoa(0));
    state.insert_account(key(&[0xa0]), eoa(0));
    check(&state);
}

#[test]
fn partition_with_deep_common_prefix() {
    // One partition whose keys share several nibbles (extension child of the root), another
    // that branches immediately, another with a single leaf; some with storage.
    let mut state = MemoryState::default();
    let mut add = |bytes: [u8; 3], slots: usize| {
        let hashed_address = key(&bytes);
        let bytecode_hash = (slots > 0).then(|| B256::repeat_byte(0xcc));
        state.insert_account(
            hashed_address,
            Account { nonce: 3, balance: U256::from(9), bytecode_hash },
        );
        for s in 0..slots {
            state.insert_storage(hashed_address, B256::repeat_byte(s as u8 + 1), U256::from(s + 1));
        }
    };
    add([0x12, 0x34, 0x50], 0);
    add([0x12, 0x34, 0x60], 3);
    add([0x12, 0x34, 0x70], 0);
    add([0x40, 0x00, 0x00], 0);
    add([0x4f, 0x00, 0x00], 40);
    add([0xe0, 0x00, 0x00], 1);
    check(&state);
}

#[test]
fn synthetic_shapes() {
    for (seed, eoas, small, large) in [
        (1u64, 500usize, 40usize, vec![2_000usize]),
        (2, 5_000, 300, vec![]),
        (3, 0, 0, vec![10_000, 3]),
        (4, 3, 2, vec![]),
    ] {
        let state = synthesize(&SynthConfig {
            seed,
            eoas,
            small_contracts: small,
            small_slots_max: 12,
            large_contracts: large,
        });
        check(&state);
    }
}

fn arb_state() -> impl Strategy<Value = MemoryState> {
    // Keys drawn from a small alphabet of leading bytes so partitions collide and prefixes
    // are shared; the tail is random so keys stay distinct.
    let key = (0u8..6, any::<[u8; 31]>()).prop_map(|(lead, tail)| {
        let mut k = [0u8; 32];
        k[0] = [0x00, 0x0f, 0x10, 0x11, 0xa0, 0xff][lead as usize];
        k[1..].copy_from_slice(&tail);
        B256::from(k)
    });
    let account = (key, any::<u64>(), any::<u64>(), any::<bool>());
    let slot = (any::<[u8; 32]>(), 1u64..u64::MAX);
    prop::collection::vec((account, prop::collection::vec(slot, 0..6)), 0..40).prop_map(|rows| {
        let mut state = MemoryState::default();
        for ((hashed_address, nonce, balance, has_code), slots) in rows {
            let bytecode_hash = has_code.then(|| B256::repeat_byte(0xc0));
            state.insert_account(
                hashed_address,
                Account { nonce, balance: U256::from(balance), bytecode_hash },
            );
            for (hashed_slot, value) in slots {
                state.insert_storage(hashed_address, B256::from(hashed_slot), U256::from(value));
            }
        }
        state
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]
    #[test]
    fn partitioned_equals_serial_and_reth(state in arb_state()) {
        check(&state);
    }
}

#[test]
fn node_maps_are_populated_for_realistic_state() {
    let state = synthesize(&SynthConfig::default());
    let collector = TrieUpdatesCollector::default();
    generate(&state.cursor_factory(), &Config::default(), &|a, p, n| collector.push(a, p, n))
        .unwrap();
    let updates = collector.into_updates();
    assert!(!updates.account_nodes.contains_key(&Nibbles::new()), "root node is never stored");
    assert!(updates.account_nodes.len() > 16);
    assert!(
        updates.storage_tries.values().any(|t| t.storage_nodes.len() > 100),
        "large contract has many storage nodes"
    );
}
