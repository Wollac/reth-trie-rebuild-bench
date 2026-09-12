//! Deterministic synthetic hashed state with a mainnet-like shape.

use crate::MemoryState;
use alloy_primitives::{B256, U256};
use trie_gen_core::Account;

/// Shape of a synthetic state.
#[derive(Clone, Debug)]
pub struct SynthConfig {
    /// Seed for the generator; equal seeds give equal states.
    pub seed: u64,
    /// Externally owned accounts (no code, no storage).
    pub eoas: usize,
    /// Contracts with a small random number of slots in `1..=small_slots_max`.
    pub small_contracts: usize,
    /// Upper bound on slots for small contracts.
    pub small_slots_max: usize,
    /// Slot counts of individually specified large contracts.
    pub large_contracts: Vec<usize>,
}

impl Default for SynthConfig {
    fn default() -> Self {
        Self {
            seed: 1,
            eoas: 2_000,
            small_contracts: 200,
            small_slots_max: 16,
            large_contracts: vec![5_000],
        }
    }
}

/// SplitMix64: tiny, deterministic, good enough for test data.
#[derive(Clone, Debug)]
pub struct Rng(u64);

impl Rng {
    /// Creates a generator from a seed.
    pub const fn new(seed: u64) -> Self {
        Self(seed)
    }

    /// Next 64 random bits.
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform 32 random bytes, standing in for a keccak output.
    pub fn next_b256(&mut self) -> B256 {
        let mut out = [0u8; 32];
        for chunk in out.chunks_mut(8) {
            chunk.copy_from_slice(&self.next_u64().to_le_bytes());
        }
        B256::from(out)
    }

    /// Uniform in `0..n`.
    pub fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }
}

/// Builds the synthetic state described by `config`.
pub fn synthesize(config: &SynthConfig) -> MemoryState {
    let mut rng = Rng::new(config.seed);
    let mut state = MemoryState::default();

    for _ in 0..config.eoas {
        state.insert_account(
            rng.next_b256(),
            Account {
                nonce: rng.below(50) as u64,
                balance: U256::from(rng.next_u64()),
                bytecode_hash: None,
            },
        );
    }

    let mut contract_sizes: Vec<usize> =
        (0..config.small_contracts).map(|_| 1 + rng.below(config.small_slots_max)).collect();
    contract_sizes.extend_from_slice(&config.large_contracts);
    for slots in contract_sizes {
        let hashed_address = rng.next_b256();
        state.insert_account(
            hashed_address,
            Account {
                nonce: 1,
                balance: U256::from(rng.below(1_000_000)),
                bytecode_hash: Some(rng.next_b256()),
            },
        );
        for _ in 0..slots {
            let value = U256::from(1 + rng.below(u32::MAX as usize));
            state.insert_storage(hashed_address, rng.next_b256(), value);
        }
    }
    state
}
