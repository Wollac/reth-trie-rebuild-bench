# trie-gen

Bulk Merkle-Patricia trie generation from sorted hashed state.

Given the *hashed state* of an Ethereum secure trie (account leaves keyed by `keccak(address)`,
storage leaves keyed by `(keccak(address), keccak(slot))`, both sorted), produce the state root
and every intermediate branch node that reth persists in its `AccountsTrie` / `StoragesTrie`
tables (`BranchNodeCompact` keyed by nibble path).

This is the closing step of snap sync under [EIP-8189](https://eips.ethereum.org/EIPS/eip-8189):
after flat state is downloaded and block access lists have been applied, rebuild the trie once
and verify the root against the pivot header. It is also what reth's merkle stage does on a full
rebuild, serially.

## Design

* **Input** is reth's own `HashedCursorFactory`: hashed account and storage cursors, exactly
  what `StateRoot` reads. Storage roots are recomputed from the storage cursor, so stale roots
  left by BAL application are never trusted. The harness feeds it through reth's in-memory
  `HashedPostStateCursorFactory`; the real thing is the MDBX-backed factory.
* **Partitioning** by the first nibble of the hashed address. Each of the 16 subtries is built by
  its own `alloy_trie::HashBuilder` over keys with the leading nibble stripped, in parallel with
  rayon. The builder's root node is then exactly the child a full build would place under that
  nibble, and emitted branch nodes are re-keyed to absolute paths.
* **Root assembly** encodes the 17-slot root branch from the 16 subtrie root references and
  hashes it. The root node is not emitted: reth's `TrieUpdates` excludes root nodes, of the state
  trie and of every storage trie, so the trie tables never hold them. Fewer than two populated
  partitions means the real root is an extension or leaf, handled by falling back to a serial
  pass (such tries are tiny).
* **Output** is handed to one caller-supplied closure as nodes complete, as
  `(account, path, node)` with `account == None` for the state trie, reth's own convention for
  telling the two apart. `TrieUpdatesCollector` is the ready-made target that accumulates reth's
  `TrieUpdates`, the shape the provider's trie-table writer consumes.
* **Parity** is asserted against reth itself: the partitioned build must produce the same root
  and the same `TrieUpdates` as `reth_trie::StateRoot::root_with_updates` over the same cursors,
  and the same node set as the crate's own serial path. See `crates/harness/tests/parity.rs`
  (fixed cases plus proptest).

## Layout

* `crates/core`: the generator. Depends only on reth's trie crates (pinned to a release tag,
  since they are not on crates.io) and what they already pull in. Meant to move into reth's trie
  crates verbatim.
* `crates/harness`: in-memory state, synthetic data, the parity oracle, benchmarks. Never goes
  into reth.

## Status

Scaffold. Node-for-node identical to reth's `StateRoot` on synthetic and property-tested state;
not yet resumable, not yet streaming within a single storage trie, not wired into reth.

Next:

1. Stream storage-trie nodes out per account instead of holding a partition's map (needed for
   contracts with 10^8 slots, where `TrieUpdates` blows up).
2. Split the largest storage tries by first nibble too; a single huge contract pins one thread.
3. Per-partition checkpoints (serialize `HashBuilder` state + last key) with a completion bitmap.
4. Flat binary dump backing a `HashedCursorFactory`, and the MDBX-backed factory from
   `reth-trie-db` for real datadirs.
5. Mainnet-scale run against a `reth download` state snapshot; compare with `reth stage run merkle`.

## Run

```sh
cargo test --workspace
cargo run --release -p trie-gen-harness --example synth_bench -- [eoas] [small_contracts] [large_slots...]
```
