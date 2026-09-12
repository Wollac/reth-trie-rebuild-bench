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
  its own `alloy_trie::HashBuilder` over its key range, with absolute keys, in parallel with
  rayon. That is the serial build restricted to a range: every branch node it emits has the path
  and masks the serial build gives it. Only the builder's own root differs from the whole trie's
  node at that nibble (it is a leaf or extension whose key still starts with the nibble), so the
  builder hands its root node back decoded, via a proof retainer with no targets, which keeps
  exactly the node at the empty path.
* **Root assembly** is one more `HashBuilder`, fed the 16 subtrie root nodes in key order the way
  reth's walker feeds stored subtries: a leaf root as `add_leaf`, a branch (or the branch under a
  root extension) as `add_branch` at its path. The builder forms whatever the trie has above the
  partitions, so no partition count is special: zero gives the empty root, one gives a leaf or
  extension re-keyed over the nibble, more give the root branch. The root node is not emitted:
  reth's `TrieUpdates` excludes root nodes, of the state trie and of every storage trie, so the
  trie tables never hold them.
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
