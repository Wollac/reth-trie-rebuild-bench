//! Times serial vs partitioned generation on a synthetic state.
//!
//! `cargo run --release --example synth_bench -- [eoas] [small_contracts] [large_slots...]`

use std::time::Instant;
use trie_gen_core::{generate, Config, TrieUpdatesCollector};
use trie_gen_harness::synth::{synthesize, SynthConfig};

fn main() {
    let args: Vec<usize> = std::env::args().skip(1).map(|a| a.parse().expect("usize")).collect();
    let config = SynthConfig {
        seed: 42,
        eoas: args.first().copied().unwrap_or(200_000),
        small_contracts: args.get(1).copied().unwrap_or(20_000),
        small_slots_max: 24,
        large_contracts: if args.len() > 2 { args[2..].to_vec() } else { vec![1_000_000, 200_000] },
    };
    let t = Instant::now();
    let state = synthesize(&config);
    println!(
        "synthesized {} accounts, {} storage slots in {:.1?}",
        state.account_count(),
        state.storage_count(),
        t.elapsed()
    );

    let t = Instant::now();
    let sorted_time = {
        state.sorted();
        t.elapsed()
    };
    println!("sorted into reth post-state in {sorted_time:.1?}");
    let factory = state.cursor_factory();

    let t = Instant::now();
    let serial = generate(&factory, &Config { partitioned: false }, &|_, _, _| {}).unwrap();
    let serial_time = t.elapsed();
    println!("serial       root={} {:.2?}", serial.root, serial_time);

    let t = Instant::now();
    let par = generate(&factory, &Config { partitioned: true }, &|_, _, _| {}).unwrap();
    let par_time = t.elapsed();
    println!(
        "partitioned  root={} {:.2?}  speedup {:.2}x on {} threads",
        par.root,
        par_time,
        serial_time.as_secs_f64() / par_time.as_secs_f64(),
        rayon::current_num_threads()
    );
    assert_eq!(serial.root, par.root);

    let collector = TrieUpdatesCollector::default();
    let t = Instant::now();
    generate(&factory, &Config { partitioned: true }, &|a, p, n| collector.push(a, p, n)).unwrap();
    let updates = collector.into_updates();
    println!(
        "partitioned + TrieUpdates: {} account nodes, {} storage nodes in {:.2?}",
        updates.account_nodes.len(),
        updates.storage_tries.values().map(|t| t.storage_nodes.len()).sum::<usize>(),
        t.elapsed()
    );
}
