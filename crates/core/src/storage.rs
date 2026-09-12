//! Per-account storage trie construction.

use crate::{emit::emit_completed, OnNode};
use alloy_primitives::{B256, U256};
use reth_storage_errors::db::DatabaseError;
use reth_trie::hashed_cursor::HashedStorageCursor;
use reth_trie_common::{HashBuilder, Nibbles, EMPTY_ROOT_HASH};

/// Builds the storage trie of `hashed_address` by walking `cursor` from the start, hands its
/// branch nodes to `on_node`, and returns the storage root.
///
/// Zero-valued entries are skipped, matching reth's walker. An empty storage yields
/// [`EMPTY_ROOT_HASH`] and emits nothing. The storage trie's root branch node (empty path) is
/// never emitted: reth excludes root nodes from `TrieUpdates`, so the trie tables never hold them.
pub fn storage_root_with_nodes<C>(
    hashed_address: B256,
    cursor: &mut C,
    on_node: &impl OnNode,
) -> Result<B256, DatabaseError>
where
    C: HashedStorageCursor<Value = U256>,
{
    // Positions the cursor on the first slot; `None` means the account has no storage, so a
    // separate `is_storage_empty` probe would only repeat this lookup.
    let mut entry = cursor.seek(B256::ZERO)?;
    if entry.is_none() {
        return Ok(EMPTY_ROOT_HASH);
    }

    let mut hb = HashBuilder::default().with_updates(true);
    while let Some((hashed_slot, value)) = entry {
        if !value.is_zero() {
            hb.add_leaf(
                Nibbles::unpack(hashed_slot),
                alloy_rlp::encode_fixed_size(&value).as_ref(),
            );
            emit_completed(&mut hb, Some(hashed_address), on_node);
        }
        entry = cursor.next()?;
    }
    let root = hb.root();
    emit_completed(&mut hb, Some(hashed_address), on_node);
    Ok(root)
}
