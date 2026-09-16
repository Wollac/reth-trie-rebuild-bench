//! Partitioned generation must be indistinguishable from reth's serial `StateRoot` over the same
//! cursors, and from the crate's own serial path.

use alloy_primitives::{B256, U256};
use proptest::prelude::*;
use reth_primitives_traits::Account;
use reth_trie::{trie_cursor::noop::NoopTrieCursorFactory, StateRoot};
use reth_trie_common::{updates::TrieUpdates, Nibbles};
use reth_trie_parallel::partitioned_root::{PartitionedStateRoot, TrieSink};
use reth_trie_rebuild_bench::{
    runtime,
    synth::{synthesize, SynthConfig},
    without_empty_storage_tries, MemoryState, NodeDigest, ProgressSink, TrieUpdatesSink,
};
use std::time::Duration;

/// reth's own answer: root and `TrieUpdates` from a serial walk over the same cursors.
fn reth_reference(state: &MemoryState) -> (B256, TrieUpdates) {
    StateRoot::new(NoopTrieCursorFactory::default(), state.cursor_factory())
        .root_with_updates()
        .unwrap()
}

fn check(state: &MemoryState) {
    let (reth_root, reth_updates) = reth_reference(state);
    let reth_updates = without_empty_storage_tries(reth_updates);

    let collector = TrieUpdatesSink::default();
    let digest = NodeDigest::default();
    let entries = (state.account_count() + state.storage_count()) as u64;
    let sink = ProgressSink::new(
        |a, p, n| {
            digest.push(a, p, &n);
            collector.on_branch_node(a, p, n)
        },
        entries,
        Duration::ZERO,
        |_| {},
    );
    let ours_root = PartitionedStateRoot::new(state.provider_factory(), &runtime(None))
        .root_with_sink(&sink)
        .unwrap();
    assert_eq!(ours_root, reth_root, "root vs reth StateRoot");
    assert_eq!(sink.walked(), entries, "entries reported vs entries in state");
    // The digest is what a mainnet-scale run compares, where holding the nodes is not an
    // option; the exact map comparison is the ground truth that validates the digest here.
    assert_eq!(
        digest.snapshot(),
        NodeDigest::of_trie_updates(&reth_updates),
        "node digest vs reth"
    );
    assert_eq!(
        without_empty_storage_tries(collector.into_trie_updates()),
        reth_updates,
        "TrieUpdates vs reth"
    );
}

fn eoa(nonce: u64) -> Account {
    Account { nonce, balance: U256::from(nonce + 1), bytecode_hash: None }
}

/// A key starting with `bytes`, zero-padded.
fn key(bytes: &[u8]) -> B256 {
    B256::right_padding_from(bytes)
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

/// A contract at `hashed_address` with `slots`, as `(hashed_slot, value)`.
fn contract(state: &mut MemoryState, hashed_address: B256, slots: &[(B256, u64)]) {
    let account =
        Account { nonce: 1, balance: U256::from(5), bytecode_hash: Some(B256::repeat_byte(0xcc)) };
    state.insert_account(hashed_address, account);
    for (hashed_slot, value) in slots {
        state.insert_storage(hashed_address, *hashed_slot, U256::from(*value));
    }
}

#[test]
fn storage_tries_sized_by_first_slot() {
    // The first slot's leading zero bits drive the size estimate: 14 or more mean the trie is
    // built in partitions, whatever its real size. Slots spread over several first nibbles fill
    // several partitions; a lone near-zero slot makes a single-leaf partition.
    let mut state = MemoryState::default();
    let spread = |lead: u8| -> Vec<(B256, u64)> {
        (0u8..40)
            .map(|i| (key(&[lead.wrapping_add(i.wrapping_mul(7)), i, 0xee]), 1_000 + u64::from(i)))
            .collect()
    };
    let with_first = |first: B256, mut slots: Vec<(B256, u64)>| {
        slots.push((first, 77));
        slots
    };
    contract(&mut state, key(&[0x11]), &with_first(B256::with_last_byte(1), spread(0x10)));
    contract(&mut state, key(&[0x22]), &with_first(key(&[0x00, 0x00, 0x0f, 0x42]), spread(0x00)));
    contract(&mut state, key(&[0x33]), &with_first(key(&[0x00, 0x00, 0x10]), spread(0x20)));
    contract(&mut state, key(&[0x44]), &[(B256::with_last_byte(9), 3)]);
    // Either side of the threshold: 14 leading zero bits are partitioned, 13 are walked serially.
    contract(&mut state, key(&[0x55]), &with_first(key(&[0x00, 0x02, 0x42]), spread(0x30)));
    contract(&mut state, key(&[0x66]), &with_first(key(&[0x00, 0x04]), spread(0x20)));
    check(&state);
}

#[test]
fn inline_storage_subtries() {
    // Two slots sharing 63 nibbles with one-byte values form a branch of under 32 bytes of RLP,
    // inline in the extension above it. Near zero, partition 0 holds exactly that, and the
    // assembly has to spell it out as leaves; starting with 0x80, the serial walk's own root is
    // the answer. (An inline branch under a branch is not a case: reth's own builder assumes a
    // branch's branch children are hashed and panics on it.)
    let mut state = MemoryState::default();
    let low = B256::with_last_byte;
    contract(&mut state, key(&[0x11]), &[(low(0x01), 1), (low(0x02), 2)]);
    let high = |last: u8| {
        let mut k = key(&[0x80]);
        k[31] = last;
        k
    };
    contract(&mut state, key(&[0x22]), &[(high(0x01), 1), (high(0x02), 2)]);
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
    // Slot keys likewise, with leading bytes that put the size estimate on both sides of the
    // partitioning threshold, and small values often enough for inline branches to occur.
    let slot_key = (0u8..5, any::<[u8; 29]>()).prop_map(|(lead, tail)| {
        let mut k = [0u8; 32];
        k[..3].copy_from_slice(
            &[[0, 0, 0], [0, 0, 0x0f], [0x10, 0, 0], [0xa0, 0xff, 0], [0xff; 3]][lead as usize],
        );
        k[3..].copy_from_slice(&tail);
        B256::from(k)
    });
    let slot = (slot_key, prop_oneof![1u64..256, 1u64..u64::MAX]);
    prop::collection::vec((account, prop::collection::vec(slot, 0..6)), 0..40).prop_map(|rows| {
        let mut state = MemoryState::default();
        for ((hashed_address, nonce, balance, has_code), slots) in rows {
            let bytecode_hash = has_code.then(|| B256::repeat_byte(0xc0));
            state.insert_account(
                hashed_address,
                Account { nonce, balance: U256::from(balance), bytecode_hash },
            );
            for (hashed_slot, value) in slots {
                state.insert_storage(hashed_address, hashed_slot, U256::from(value));
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
    let collector = TrieUpdatesSink::default();
    PartitionedStateRoot::new(state.provider_factory(), &runtime(None))
        .root_with_sink(&collector)
        .unwrap();
    let updates = collector.into_trie_updates();
    assert!(!updates.account_nodes.contains_key(&Nibbles::new()), "root node is never stored");
    assert!(updates.account_nodes.len() > 16);
    assert!(
        updates.storage_tries.values().any(|t| t.storage_nodes.len() > 100),
        "large contract has many storage nodes"
    );
}
