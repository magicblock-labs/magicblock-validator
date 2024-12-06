use smallvec::SmallVec;
use solana_sdk::{
    account::ReadableAccount, clock::Epoch, hash::Hash, pubkey::Pubkey,
};

use crate::accounts_hash::AccountHash;

pub fn hash_account<T: ReadableAccount>(
    account: &T,
    pubkey: &Pubkey,
) -> AccountHash {
    hash_account_data(
        account.lamports(),
        account.owner(),
        account.executable(),
        account.rent_epoch(),
        account.data(),
        pubkey,
    )
}

fn hash_account_data(
    lamports: u64,
    owner: &Pubkey,
    executable: bool,
    rent_epoch: Epoch,
    data: &[u8],
    pubkey: &Pubkey,
) -> AccountHash {
    if lamports == 0 {
        return AccountHash(Hash::default());
    }
    let mut hasher = blake3::Hasher::new();

    // allocate 128 bytes buffer on the stack
    const BUF_SIZE: usize = 128;
    const TOTAL_FIELD_SIZE: usize = 8 /* lamports */ + 8 /* slot */ + 8 /* rent_epoch */ + 1 /* exec_flag */ + 32 /* owner_key */ + 32 /* pubkey */;
    const DATA_SIZE_CAN_FIT: usize = BUF_SIZE - TOTAL_FIELD_SIZE;

    let mut buffer = SmallVec::<[u8; BUF_SIZE]>::new();

    // collect lamports, slot, rent_epoch into buffer to hash
    buffer.extend_from_slice(&lamports.to_le_bytes());

    buffer.extend_from_slice(&rent_epoch.to_le_bytes());

    if data.len() > DATA_SIZE_CAN_FIT {
        // For larger accounts whose data can't fit into the buffer, update the hash now.
        hasher.update(&buffer);
        buffer.clear();

        // hash account's data
        hasher.update(data);
    } else {
        // For small accounts whose data can fit into the buffer, append it to the buffer.
        buffer.extend_from_slice(data);
    }

    // collect exec_flag, owner, pubkey into buffer to hash
    if executable {
        buffer.push(1_u8);
    } else {
        buffer.push(0_u8);
    }
    buffer.extend_from_slice(owner.as_ref());
    buffer.extend_from_slice(pubkey.as_ref());
    hasher.update(&buffer);

    AccountHash(Hash::new_from_array(hasher.finalize().into()))
}
