//! Writes a synthetic hashed state into a reth-shaped datadir (`<dir>/db`) for exercising
//! `datadir_bench` locally.
//!
//! `cargo run --release -p reth-trie-rebuild-bench --example make_datadir -- <dir> [eoas]
//! [small_contracts]`

use reth_db::{init_db, mdbx::DatabaseArguments, ClientVersion};
use reth_trie_rebuild_bench::{
    mdbx::write_hashed_state,
    synth::{synthesize, SynthConfig},
};
use std::path::PathBuf;

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = PathBuf::from(args.next().expect("datadir path"));
    let eoas = args.next().map(|a| a.parse().expect("usize")).unwrap_or(50_000);
    let small_contracts = args.next().map(|a| a.parse().expect("usize")).unwrap_or(2_000);
    let state = synthesize(&SynthConfig {
        seed: 11,
        eoas,
        small_contracts,
        small_slots_max: 16,
        large_contracts: vec![20_000],
    });
    let db =
        init_db(dir.join("db"), DatabaseArguments::new(ClientVersion::default())).expect("init db");
    write_hashed_state(&db, &state).expect("write hashed state");
    println!(
        "wrote {} accounts, {} storage slots to {}",
        state.account_count(),
        state.storage_count(),
        dir.join("db").display()
    );
}
