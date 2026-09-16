//! Builds the state trie from a reth datadir's hashed state and times it.
//!
//! Opens the database read-only, so a stopped node's datadir or a freshly downloaded snapshot
//! works as is. Nothing is written; nodes are counted and digested, not kept. The flags that do
//! write, `--write`, `--clear-trie-tables` and `--reth-rebuild`, open it read-write and touch only
//! the trie tables.
//!
//! ```text
//! datadir_bench --datadir ~/.local/share/reth/mainnet [--threads N] [--repeat N] [--expected-root 0x..]
//! datadir_bench --datadir ... --write [--chunk-nodes N] [--threads N] [--expected-root 0x..]
//! datadir_bench --datadir ... --reth-rebuild [--expected-root 0x..]
//! datadir_bench --datadir ... --reth-serial [--expected-root 0x..]
//! ```
//!
//! `--write` is `PartitionedStateRoot` end to end: clear the trie tables, build, commit every
//! branch node into the tables in chunks from a writer thread, then digest the tables it left
//! behind and let reth's own walker derive the root from them. That is the merkle stage's
//! rebuild work done by the generator, and the like-for-like counterpart of `--reth-rebuild`.
//!
//! `--reth-rebuild` is the merkle stage's full rebuild itself, run on the database without the
//! pipeline: clear the trie tables, then reth's serial walk with its nodes committed in chunks,
//! then the same digest and walk. `reth stage run merkle` cannot do this on a snapshot without
//! changeset static files, since reth's CLI refuses the datadir before the stage starts.
//!
//! `--reth-serial` runs reth's serial `StateRoot` walk over whatever the trie tables hold. On
//! empty tables (after `--clear-trie-tables`) that is the full-rebuild walk minus persistence, the
//! serial baseline for a read-only generator run. On written tables it reads the stored nodes
//! instead of descending, which is the check `--write` and `--reth-rebuild` run at the end.
//!
//! `--digest-trie-tables` reads the trie tables and prints their node count and digest in the
//! same fold a generator run prints.
//!
//! `--threads` sizes the CPU pool of the reth `Runtime` the generator builds on; the default is
//! reth's, the available parallelism. Every run reports its peak heap, counted at the allocator,
//! so the memory-mapped database does not obscure what the build itself holds.
//!
//! Prints the tip block from the stage checkpoints so the root can be checked against that
//! block's header, and, when `--expected-root` is given, does the check itself.

use alloy_primitives::B256;
use reth_db::{mdbx::DatabaseArguments, open_db, open_db_read_only, tables, ClientVersion};
use reth_db_api::{transaction::DbTx, Database};
use reth_libmdbx::MaxReadTransactionDuration;
use reth_stages_types::StageId;
use reth_trie_parallel::partitioned_root::{HashedStateFactory, PartitionedStateRoot};
use reth_trie_rebuild_bench::{
    clear_trie_tables, digest_trie_tables, human_bytes, reth_rebuild, reth_serial_root, runtime,
    DatabaseSource, MdbxWriteSink, NodeDigest, PeakAlloc, ProgressSink, DEFAULT_CHUNK_NODES,
};
use std::{
    path::PathBuf,
    process::exit,
    sync::Arc,
    time::{Duration, Instant},
};

#[global_allocator]
static HEAP: PeakAlloc = PeakAlloc::new();

/// How often a generator run prints a progress line, matching reth's execution stage.
const PROGRESS_INTERVAL: Duration = Duration::from_secs(10);

struct Args {
    datadir: PathBuf,
    threads: Option<usize>,
    repeat: usize,
    expected_root: Option<B256>,
    write: bool,
    chunk_nodes: usize,
    reth_serial: bool,
    reth_rebuild: bool,
    clear_trie_tables: bool,
    digest_trie_tables: bool,
}

fn parse_args() -> Args {
    let mut args = Args {
        datadir: PathBuf::new(),
        threads: None,
        repeat: 1,
        expected_root: None,
        write: false,
        chunk_nodes: DEFAULT_CHUNK_NODES,
        reth_serial: false,
        reth_rebuild: false,
        clear_trie_tables: false,
        digest_trie_tables: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let mut value = || it.next().unwrap_or_else(|| usage(&format!("{flag} needs a value")));
        match flag.as_str() {
            "--datadir" => args.datadir = PathBuf::from(value()),
            "--threads" => {
                args.threads = Some(value().parse().unwrap_or_else(|_| usage("bad --threads")))
            }
            "--repeat" => args.repeat = value().parse().unwrap_or_else(|_| usage("bad --repeat")),
            "--expected-root" => {
                args.expected_root =
                    Some(value().parse().unwrap_or_else(|_| usage("bad --expected-root")))
            }
            "--write" => args.write = true,
            "--chunk-nodes" => {
                args.chunk_nodes = value().parse().unwrap_or_else(|_| usage("bad --chunk-nodes"))
            }
            "--reth-serial" => args.reth_serial = true,
            "--reth-rebuild" => args.reth_rebuild = true,
            "--clear-trie-tables" => args.clear_trie_tables = true,
            "--digest-trie-tables" => args.digest_trie_tables = true,
            _ => usage(&format!("unknown flag {flag}")),
        }
    }
    if args.datadir.as_os_str().is_empty() {
        usage("--datadir is required");
    }
    args
}

fn usage(msg: &str) -> ! {
    eprintln!(
        "{msg}\nusage: datadir_bench --datadir PATH [--threads N] [--repeat N] [--expected-root 0x..] [--write] [--chunk-nodes N] [--reth-serial] [--reth-rebuild] [--clear-trie-tables] [--digest-trie-tables]"
    );
    exit(2)
}

fn main() {
    let args = parse_args();
    let runtime = runtime(args.threads);
    let threads = runtime.cpu_pool().current_num_threads();

    // A mainnet build runs longer than reth's default five-minute read-transaction limit.
    let db_args = DatabaseArguments::new(ClientVersion::default())
        .with_max_read_transaction_duration(Some(MaxReadTransactionDuration::Unbounded));

    if args.clear_trie_tables || args.reth_rebuild || args.write {
        let db = Arc::new(
            open_db(args.datadir.join("db"), db_args).expect("open datadir db read-write"),
        );
        if args.clear_trie_tables {
            clear_trie_tables(&*db).expect("clear trie tables");
            println!("trie tables cleared");
        }
        if args.reth_rebuild {
            print_tip(&db.tx().expect("read transaction"));
            HEAP.reset_peak();
            let started = Instant::now();
            let done = reth_rebuild(&*db, None, |line| println!("{line}")).expect("reth rebuild");
            println!(
                "reth rebuild: root {} entries {} written {} chunks {} elapsed {:.2?} peak heap {}",
                done.root,
                done.entries,
                done.written,
                done.chunks,
                started.elapsed(),
                human_bytes(HEAP.peak())
            );
            check_root(done.root, args.expected_root, "reth rebuild");
            check_written_tables(&db.tx().expect("read transaction"), done.root);
        }
        if args.write {
            print_tip(&db.tx().expect("read transaction"));
            println!("threads {threads}");
            let entries = hashed_entries(&db.tx().expect("read transaction"));
            println!("hashed entries {entries}");
            for run in 1..=args.repeat {
                clear_trie_tables(&*db).expect("clear trie tables");
                HEAP.reset_peak();
                let started = Instant::now();
                let writer = MdbxWriteSink::new(Arc::clone(&db), args.chunk_nodes).expect("writer");
                let sink = ProgressSink::new(writer, entries, PROGRESS_INTERVAL, |line| {
                    println!("{line}")
                });
                let root = PartitionedStateRoot::new(
                    HashedStateFactory::new(DatabaseSource(&*db)),
                    &runtime,
                )
                .root_with_sink(&sink)
                .expect("state root");
                let walked = sink.walked();
                let written = sink.into_inner().finish().expect("write trie tables");
                println!(
                    "run {run}: root {root} entries {walked} written {} chunks {} elapsed {:.2?} peak heap {}",
                    written.nodes,
                    written.chunks,
                    started.elapsed(),
                    human_bytes(HEAP.peak())
                );
                check_root(root, args.expected_root, &format!("run {run}"));
                check_written_tables(&db.tx().expect("read transaction"), root);
            }
        }
        return;
    }

    let db =
        open_db_read_only(args.datadir.join("db"), db_args).expect("open datadir db read-only");
    let tx = db.tx().expect("read transaction");
    print_tip(&tx);
    println!("threads {threads}");

    if args.digest_trie_tables {
        print_trie_tables_digest(&tx);
        return;
    }

    if args.reth_serial {
        HEAP.reset_peak();
        let started = Instant::now();
        let root = reth_serial_root(&tx).expect("reth StateRoot");
        println!(
            "reth serial: root {root} elapsed {:.2?} peak heap {}",
            started.elapsed(),
            human_bytes(HEAP.peak())
        );
        check_root(root, args.expected_root, "reth serial");
        return;
    }

    let entries = hashed_entries(&tx);
    println!("hashed entries {entries}");

    for run in 1..=args.repeat {
        let sink = ProgressSink::new(NodeDigest::default(), entries, PROGRESS_INTERVAL, |line| {
            println!("{line}")
        });
        HEAP.reset_peak();
        let started = Instant::now();
        // One read transaction per partition; see `HashedStateFactory`.
        let root =
            PartitionedStateRoot::new(HashedStateFactory::new(DatabaseSource(&db)), &runtime)
                .root_with_sink(&sink)
                .expect("state root");
        let elapsed = started.elapsed();
        let snapshot = sink.inner().snapshot();
        println!(
            "run {run}: root {root} entries {} nodes {} digest {:032x} elapsed {elapsed:.2?} peak heap {}",
            sink.walked(),
            snapshot.count,
            snapshot.fold,
            human_bytes(HEAP.peak())
        );
        check_root(root, args.expected_root, &format!("run {run}"));
    }
}

/// Prints the tip block from the stage checkpoints.
fn print_tip(tx: &impl DbTx) {
    let tip = tx
        .get::<tables::StageCheckpoints>(StageId::Finish.to_string())
        .expect("read stage checkpoints")
        .map(|c| c.block_number);
    match tip {
        Some(block) => println!("tip block {block} (Finish stage checkpoint)"),
        None => println!("tip block unknown (no Finish checkpoint)"),
    }
}

/// The table stats, as the merkle stage sizes its progress; a run walks exactly this many.
fn hashed_entries(tx: &impl DbTx) -> u64 {
    (tx.entries::<tables::HashedAccounts>().expect("count hashed accounts")
        + tx.entries::<tables::HashedStorages>().expect("count hashed storages")) as u64
}

/// Digests the trie tables and prints the counts, in the same fold a generator run reports.
fn print_trie_tables_digest(tx: &(impl DbTx + Sync)) {
    let started = Instant::now();
    let tables = digest_trie_tables(tx).expect("digest trie tables");
    println!(
        "trie tables: account nodes {} storage nodes {} nodes {} digest {:032x} elapsed {:.2?}",
        tables.account_nodes,
        tables.storage_nodes,
        tables.digest.count,
        tables.digest.fold,
        started.elapsed()
    );
}

/// What a run that wrote the trie tables leaves behind: the tables' digest, and the root reth's
/// own walker derives from the stored nodes, which must be the root the run computed.
fn check_written_tables(tx: &(impl DbTx + Sync), root: B256) {
    print_trie_tables_digest(tx);
    let started = Instant::now();
    let walked = reth_serial_root(tx).expect("reth StateRoot over written tables");
    if walked == root {
        println!("reth walk over written tables: root {walked} elapsed {:.2?}", started.elapsed());
    } else {
        eprintln!("reth walk over written tables: ROOT MISMATCH got {walked} expected {root}");
        exit(1);
    }
}

fn check_root(root: B256, expected: Option<B256>, label: &str) {
    if let Some(expected) = expected {
        if root == expected {
            println!("{label}: root matches expected");
        } else {
            eprintln!("{label}: ROOT MISMATCH expected {expected}");
            exit(1);
        }
    }
}
