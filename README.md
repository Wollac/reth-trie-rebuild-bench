# reth-trie-rebuild-bench

Oracle and mainnet benchmark for `PartitionedStateRoot`, the parallel full rebuild of reth's
state trie proposed for reth in
[`feat/partitioned-state-root`](https://github.com/Wollac/reth/tree/feat/partitioned-state-root).
Every reth crate here comes from that branch, pinned by revision in `Cargo.toml`, so the code
measured is the code under review.

`PartitionedStateRoot` rebuilds the trie tables (`AccountsTrie`, `StoragesTrie`) from the hashed
state tables alone, in first-nibble partitions on reth's `Runtime` CPU pool, streaming every
branch node out as it completes. This repository checks that it produces exactly what reth's
serial rebuild produces, and measures both on mainnet.

## What is checked

* **Node-for-node parity, in memory** (`tests/parity.rs`): on fixed shapes and property-tested
  states, the root and the full `TrieUpdates` equal `reth_trie::StateRoot::root_with_updates`
  over the same cursors, with storage tries built both serially and partitioned.
* **Parity on MDBX** (`tests/mdbx.rs`): the same over reth's real database cursors, one read
  transaction per partition; reth's chunked rebuild and the generator's chunked writer both leave
  trie tables whose digest equals the one-shot walk's updates, in both key encodings; and reth's
  own walker derives the root from the written tables.
* **On mainnet** (`datadir_bench`): each run clears the trie tables, builds, commits every node
  in chunks, checks the root against the block header, digests the tables it left behind, and
  runs reth's serial walker over them. The digest is an order-independent fold over every
  stored row (account, path, the three masks, stored hashes). It exists because both runs write
  the same tables in the same datadir one after the other, so the rows are never side by side
  to compare; equal digest and count means the generator wrote reth's rows. The walker check is
  the complementary one: reth's reader accepts the stored nodes and reproduces the header root
  from them.

## Results

`results/pr/` will hold the run behind the PR's benchmark table once it has been produced from
this branch: AWS i4i.4xlarge (16 vCPU, local NVMe), mainnet, cold page cache before each run,
trie tables written end to end. Until then, no results are tracked here.

## Run

```sh
cargo test --workspace

# On a machine with a reth datadir (a `reth download` state snapshot is enough):
datadir_bench --datadir ~/.local/share/reth/mainnet --reth-rebuild --expected-root 0x..   # reth's rebuild
datadir_bench --datadir ~/.local/share/reth/mainnet --write --expected-root 0x..          # the generator
datadir_bench --datadir ~/.local/share/reth/mainnet --write --threads 1 --expected-root 0x..

# The three in order, cold cache each, with a summary that checks the digests agree:
scripts/aws/bench.sh

# Or on a fresh i4i.4xlarge with the datadir restored from S3, results fetched to results/aws/:
scripts/aws/run.sh --bucket <bucket-with-reth-mainnet.tar.zst> [--no-wait]
```

`--threads` sizes the `Runtime` CPU pool; the default is reth's, the available parallelism.

## Layout

* `crates/bench`: in-memory state and synthetic data, the digest, a chunked MDBX writer sink,
  reth's rebuild run outside the pipeline, a counting allocator, and the `datadir_bench` binary.
* `scripts/aws`: instance setup, datadir restore, the benchmark, and the driver.
* `results/pr`: the run cited in the PR. Other runs stay untracked under `results/`.
