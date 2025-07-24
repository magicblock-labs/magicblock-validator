use std::collections::HashSet;

use base64::{prelude::BASE64_STANDARD, Engine};
use dlp::args::{CommitStateArgs, CommitStateFromBufferArgs};
use magicblock_committor_program::{
    instruction_builder::close_buffer::{create_close_ix, CreateCloseIxArgs},
    ChangedBundle,
};
use solana_pubkey::Pubkey;
use solana_rpc_client::rpc_client::SerializableTransaction;
use solana_sdk::{
    hash::Hash,
    instruction::Instruction,
    message::{v0::Message, AddressLookupTableAccount, VersionedMessage},
    signature::Keypair,
    signer::Signer,
    transaction::VersionedTransaction,
};
use static_assertions::const_assert;

use crate::error::{CommittorServiceError, CommittorServiceResult};

/// From agave rpc/src/rpc.rs [MAX_BASE64_SIZE]
pub(crate) const MAX_ENCODED_TRANSACTION_SIZE: usize = 1644;

/// How many process and commit buffer instructions fit into a single transaction
#[allow(unused)] // serves as documentation as well
pub const MAX_PROCESS_PER_TX: u8 = 3;

/// How many process and commit buffer instructions fit into a single transaction
/// when using address lookup tables but not including the buffer account in the
/// lookup table
#[allow(unused)] // serves as documentation as well
pub const MAX_PROCESS_PER_TX_USING_LOOKUP: u8 = 12;

/// How many close buffer instructions fit into a single transaction
#[allow(unused)] // serves as documentation as well
pub const MAX_CLOSE_PER_TX: u8 = 7;

/// How many close buffer instructions fit into a single transaction
/// when using address lookup tables but not including the buffer account
/// nor chunk account in the lookup table
#[allow(unused)] // serves as documentation as well
pub const MAX_CLOSE_PER_TX_USING_LOOKUP: u8 = 7;

/// How many process and commit buffer instructions combined with close buffer instructions
/// fit into a single transaction
#[allow(unused)] // serves as documentation as well
pub const MAX_PROCESS_AND_CLOSE_PER_TX: u8 = 2;

/// How many process and commit buffer instructions combined with
/// close buffer instructions fit into a single transaction when
/// using lookup tables but not including the buffer account
#[allow(unused)] // serves as documentation as well
pub const MAX_PROCESS_AND_CLOSE_PER_TX_USING_LOOKUP: u8 = 4;

/// How many finalize instructions fit into a single transaction
#[allow(unused)] // serves as documentation as well
pub const MAX_FINALIZE_PER_TX: u8 = 5;

/// How many finalize instructions fit into a single transaction
/// when using address lookup tables
#[allow(unused)] // serves as documentation as well
pub const MAX_FINALIZE_PER_TX_USING_LOOKUP: u8 = 48;

/// How many undelegate instructions fit into a single transaction
/// NOTE: that we assume the rent reimbursement account to be the delegated account
#[allow(unused)] // serves as documentation as well
pub const MAX_UNDELEGATE_PER_TX: u8 = 3;

/// How many undelegate instructions fit into a single transaction
/// when using address lookup tables
/// NOTE: that we assume the rent reimbursement account to be the delegated account
#[allow(unused)] // serves as documentation as well
pub const MAX_UNDELEGATE_PER_TX_USING_LOOKUP: u8 = 16;

// Allows us to run undelegate instructions without rechunking them since we know
// that we didn't process more than we also can undelegate
const_assert!(MAX_PROCESS_PER_TX <= MAX_UNDELEGATE_PER_TX,);

// Allows us to run undelegate instructions using lookup tables without rechunking
// them since we know that we didn't process more than we also can undelegate
const_assert!(
    MAX_PROCESS_PER_TX_USING_LOOKUP <= MAX_UNDELEGATE_PER_TX_USING_LOOKUP
);

// -----------------
// Process Commitables using Args or Buffer
// -----------------
pub(crate) struct CommitTxReport {
    /// Size of the transaction without lookup tables.
    pub size_args: usize,

    /// The size of the transaction including the finalize instruction
    /// when not using lookup tables the `finalize` param of
    /// [size_of_commit_with_args_tx] is `true`.
    pub size_args_including_finalize: Option<usize>,

    /// If the bundle fits into a single transaction using buffers without
    /// using lookup tables.
    /// This does not depend on the size of the data, but only the number of
    /// accounts in the bundle.
    pub fits_buffer: bool,

    /// If the bundle fits into a single transaction using buffers using lookup tables.
    /// This does not depend on the size of the data, but only the number of
    /// accounts in the bundle.
    pub fits_buffer_using_lookup: bool,

    /// Size of the transaction when using lookup tables.
    /// This is only determined if the [SizeOfCommitWithArgs::size] is larger than
    /// [MAX_ENCODED_TRANSACTION_SIZE].
    pub size_args_with_lookup: Option<usize>,

    /// The size of the transaction including the finalize instruction
    /// when using lookup tables
    /// This is only determined if the [SizeOfCommitWithArgs::size_including_finalize]
    /// is larger than [MAX_ENCODED_TRANSACTION_SIZE].
    pub size_args_with_lookup_including_finalize: Option<usize>,
}

pub(crate) fn commit_tx_report(
    bundle: &ChangedBundle,
    finalize: bool,
) -> CommittorServiceResult<CommitTxReport> {
    let auth = Keypair::new();

    let ixs = bundle
        .iter()
        .map(|(_, account)| {
            let args = CommitStateArgs {
                // TODO(thlorenz): this is expensive, but seems unavoidable in order to reliably
                // calculate the size of the transaction
                data: account.data().to_vec(),
                ..CommitStateArgs::default()
            };
            dlp::instruction_builder::commit_state(
                auth.pubkey(),
                Pubkey::new_unique(),
                Pubkey::new_unique(),
                args,
            )
        })
        .collect::<Vec<_>>();

    let size = encoded_tx_size(&auth, &ixs, &TransactionOpts::NoLookupTable)?;
    let size_with_lookup = (size > MAX_ENCODED_TRANSACTION_SIZE)
        .then(|| encoded_tx_size(&auth, &ixs, &TransactionOpts::UseLookupTable))
        .transpose()?;

    if finalize {
        let mut ixs = ixs.clone();
        let finalize_ixs = bundle.iter().map(|(pubkey, _)| {
            dlp::instruction_builder::finalize(auth.pubkey(), *pubkey)
        });
        ixs.extend(finalize_ixs);

        let size_including_finalize =
            encoded_tx_size(&auth, &ixs, &TransactionOpts::NoLookupTable)?;
        let size_with_lookup_including_finalize = (size_including_finalize
            > MAX_ENCODED_TRANSACTION_SIZE)
            .then(|| {
                encoded_tx_size(&auth, &ixs, &TransactionOpts::UseLookupTable)
            })
            .transpose()?;

        Ok(CommitTxReport {
            size_args: size,
            fits_buffer: bundle.len() <= MAX_PROCESS_PER_TX as usize,
            fits_buffer_using_lookup: bundle.len()
                <= MAX_PROCESS_PER_TX_USING_LOOKUP as usize,
            size_args_with_lookup: size_with_lookup,
            size_args_including_finalize: Some(size_including_finalize),
            size_args_with_lookup_including_finalize:
                size_with_lookup_including_finalize,
        })
    } else {
        Ok(CommitTxReport {
            size_args: size,
            fits_buffer: bundle.len() <= MAX_PROCESS_PER_TX as usize,
            fits_buffer_using_lookup: bundle.len()
                <= MAX_PROCESS_PER_TX_USING_LOOKUP as usize,
            size_args_including_finalize: None,
            size_args_with_lookup: size_with_lookup,
            size_args_with_lookup_including_finalize: None,
        })
    }
}

// -----------------
// Process Commitables and Close Buffers
// -----------------
pub(crate) fn process_commits_ix(
    validator_auth: Pubkey,
    pubkey: &Pubkey,
    delegated_account_owner: &Pubkey,
    buffer_pda: &Pubkey,
    commit_args: CommitStateFromBufferArgs,
) -> Instruction {
    dlp::instruction_builder::commit_state_from_buffer(
        validator_auth,
        *pubkey,
        *delegated_account_owner,
        *buffer_pda,
        commit_args,
    )
}

pub(crate) fn close_buffers_ix(
    validator_auth: Pubkey,
    pubkey: &Pubkey,
    commit_id: u64,
) -> Instruction {
    create_close_ix(CreateCloseIxArgs {
        authority: validator_auth,
        pubkey: *pubkey,
        commit_id,
    })
}

pub(crate) fn process_and_close_ixs(
    validator_auth: Pubkey,
    pubkey: &Pubkey,
    delegated_account_owner: &Pubkey,
    buffer_pda: &Pubkey,
    ephemeral_blockhash: &Hash,
    commit_args: CommitStateFromBufferArgs,
) -> Vec<Instruction> {
    let process_ix = process_commits_ix(
        validator_auth,
        pubkey,
        delegated_account_owner,
        buffer_pda,
        commit_args,
    );
    let close_ix = close_buffers_ix(validator_auth, pubkey, 0); // TODO(edwin)

    vec![process_ix, close_ix]
}

// -----------------
// Finalize
// -----------------
pub(crate) fn finalize_ix(
    validator_auth: Pubkey,
    pubkey: &Pubkey,
) -> Instruction {
    dlp::instruction_builder::finalize(validator_auth, *pubkey)
}

// -----------------
// Helpers
// -----------------
#[allow(clippy::enum_variant_names)]
enum TransactionOpts {
    NoLookupTable,
    UseLookupTable,
}
fn encoded_tx_size(
    auth: &Keypair,
    ixs: &[Instruction],
    opts: &TransactionOpts,
) -> CommittorServiceResult<usize> {
    use CommittorServiceError::*;
    use TransactionOpts::*;
    let lookup_tables = match opts {
        NoLookupTable => vec![],
        UseLookupTable => get_lookup_tables(ixs),
    };

    let versioned_msg = Message::try_compile(
        &auth.pubkey(),
        ixs,
        &lookup_tables,
        Hash::default(),
    )
    .map_err(|err| {
        FailedToCompileTransactionMessage(
            "Calculating transaction size".to_string(),
            err,
        )
    })?;
    let versioned_tx = VersionedTransaction::try_new(
        VersionedMessage::V0(versioned_msg),
        &[&auth],
    )
    .map_err(|err| {
        FailedToCreateTransaction(
            "Calculating transaction size".to_string(),
            err,
        )
    })?;

    let encoded = serialize_and_encode_base64(&versioned_tx);
    Ok(encoded.len())
}

pub fn serialize_and_encode_base64(
    transaction: &impl SerializableTransaction,
) -> String {
    // SAFETY: runs statically
    let serialized = bincode::serialize(transaction).unwrap();
    BASE64_STANDARD.encode(serialized)
}

fn get_lookup_tables(ixs: &[Instruction]) -> Vec<AddressLookupTableAccount> {
    let pubkeys = ixs
        .iter()
        .flat_map(|ix| ix.accounts.iter().map(|acc| acc.pubkey))
        .collect::<HashSet<Pubkey>>();

    let lookup_table = AddressLookupTableAccount {
        key: Pubkey::default(),
        addresses: pubkeys.into_iter().collect(),
    };
    vec![lookup_table]
}

#[cfg(test)]
mod test {
    use dlp::args::{CommitStateArgs, CommitStateFromBufferArgs};
    use lazy_static::lazy_static;
    use solana_pubkey::Pubkey;
    use solana_sdk::{
        address_lookup_table::state::LOOKUP_TABLE_MAX_ADDRESSES,
        hash::Hash,
        instruction::Instruction,
        message::{v0::Message, AddressLookupTableAccount, VersionedMessage},
        signature::Keypair,
        signer::Signer,
        transaction::VersionedTransaction,
    };

    use super::*;
    use crate::{
        compute_budget::{Budget, ComputeBudget},
        pubkeys_provider::{provide_committee_pubkeys, provide_common_pubkeys},
    };

    // These tests statically determine the optimal ix count to fit into a single
    // transaction and assert that the const we export in prod match those numbers.
    // Thus when an instruction changes and one of those numbers with it a failing
    // test alerts us.
    // This is less overhead than running those static functions each time at
    // startup.

    #[test]
    fn test_max_process_per_tx() {
        assert_eq!(super::MAX_PROCESS_PER_TX, *MAX_PROCESS_PER_TX);
        assert_eq!(
            super::MAX_PROCESS_PER_TX_USING_LOOKUP,
            *MAX_PROCESS_PER_TX_USING_LOOKUP
        );
    }

    #[test]
    fn test_max_close_per_tx() {
        assert_eq!(super::MAX_CLOSE_PER_TX, *MAX_CLOSE_PER_TX);
        assert_eq!(
            super::MAX_CLOSE_PER_TX_USING_LOOKUP,
            *MAX_CLOSE_PER_TX_USING_LOOKUP
        );
    }

    #[test]
    fn test_max_process_and_closes_per_tx() {
        assert_eq!(
            super::MAX_PROCESS_AND_CLOSE_PER_TX,
            *MAX_PROCESS_AND_CLOSE_PER_TX
        );
        assert_eq!(
            super::MAX_PROCESS_AND_CLOSE_PER_TX_USING_LOOKUP,
            *MAX_PROCESS_AND_CLOSE_PER_TX_USING_LOOKUP
        );
    }

    #[test]
    fn test_max_finalize_per_tx() {
        assert_eq!(super::MAX_FINALIZE_PER_TX, *MAX_FINALIZE_PER_TX);
        assert_eq!(
            super::MAX_FINALIZE_PER_TX_USING_LOOKUP,
            *MAX_FINALIZE_PER_TX_USING_LOOKUP
        );
    }

    #[test]
    fn test_max_undelegate_per_tx() {
        assert_eq!(super::MAX_UNDELEGATE_PER_TX, *MAX_UNDELEGATE_PER_TX);
        assert_eq!(
            super::MAX_UNDELEGATE_PER_TX_USING_LOOKUP,
            *MAX_UNDELEGATE_PER_TX_USING_LOOKUP
        );
    }

    // -----------------
    // Process Commitables using Args
    // -----------------
    #[test]
    fn test_log_commit_args_ix_sizes() {
        // This test is used to investigate the size of the transaction related to
        // the amount of committed accounts and their data size.
        fn run(auth: &Keypair, ixs: usize) {
            let mut tx_lines = vec![];
            use TransactionOpts::*;
            for tx_opts in [NoLookupTable, UseLookupTable] {
                let mut tx_sizes = vec![];
                for size in [0, 10, 20, 50, 100, 200, 500, 1024] {
                    let ixs = (0..ixs)
                        .map(|_| make_ix(auth, size))
                        .collect::<Vec<_>>();

                    let tx_size =
                        encoded_tx_size(auth, &ixs, &tx_opts).unwrap();
                    tx_sizes.push((size, tx_size));
                }
                tx_lines.push(tx_sizes);
            }
            let sizes = tx_lines
                .into_iter()
                .map(|line| {
                    line.into_iter()
                        .map(|(size, len)| format!("{:4}:{:5}", size, len))
                        .collect::<Vec<_>>()
                        .join("|")
                })
                .collect::<Vec<_>>()
                .join("\n");
            eprintln!("{:3} ixs:\n{}", ixs, sizes);
        }
        fn make_ix(auth: &Keypair, data_size: usize) -> Instruction {
            let data = vec![1; data_size];
            let args = CommitStateArgs {
                data,
                ..CommitStateArgs::default()
            };
            dlp::instruction_builder::commit_state(
                auth.pubkey(),
                Pubkey::new_unique(),
                Pubkey::new_unique(),
                args,
            )
        }

        let auth = &Keypair::new();
        run(auth, 0);
        run(auth, 1);
        run(auth, 2);
        run(auth, 5);
        run(auth, 8);
        run(auth, 10);
        run(auth, 15);
        run(auth, 20);
        /*
         0 ixs:
          0:  184|  10:  184|  20:  184|  50:  184| 100:  184| 200:  184| 500:  184|1024:  184
          0:  184|  10:  184|  20:  184|  50:  184| 100:  184| 200:  184| 500:  184|1024:  184
         1 ixs:
          0:  620|  10:  636|  20:  648|  50:  688| 100:  756| 200:  888| 500: 1288|1024: 1988
          0:  336|  10:  348|  20:  364|  50:  404| 100:  472| 200:  604| 500: 1004|1024: 1704
         2 ixs:
          0:  932|  10:  960|  20:  984|  50: 1064| 100: 1200| 200: 1468| 500: 2268|1024: 3664
          0:  400|  10:  424|  20:  452|  50:  532| 100:  668| 200:  936| 500: 1736|1024: 3132
         5 ixs:
          0: 1864|  10: 1932|  20: 1996|  50: 2196| 100: 2536| 200: 3204| 500: 5204|1024: 8696
          0:  588|  10:  652|  20:  720|  50:  920| 100: 1260| 200: 1928| 500: 3928|1024: 7420
         8 ixs:
          0: 2796|  10: 2904|  20: 3008|  50: 3328| 100: 3872| 200: 4940| 500: 8140|1024:13728
          0:  776|  10:  880|  20:  988|  50: 1308| 100: 1852| 200: 2920| 500: 6120|1024:11708
        10 ixs:
          0: 3416|  10: 3552|  20: 3684|  50: 4084| 100: 4764| 200: 6096| 500:10096|1024:17084
          0:  900|  10: 1032|  20: 1168|  50: 1568| 100: 2248| 200: 3580| 500: 7580|1024:14568
        15 ixs:
          0: 4972|  10: 5172|  20: 5372|  50: 5972| 100: 6992| 200: 8992| 500:14992|1024:25472
          0: 1212|  10: 1412|  20: 1612|  50: 2212| 100: 3232| 200: 5232| 500:11232|1024:21712
        20 ixs:
          0: 6524|  10: 6792|  20: 7056|  50: 7856| 100: 9216| 200:11884| 500:19884|1024:33856
          0: 1528|  10: 1792|  20: 2060|  50: 2860| 100: 4220| 200: 6888| 500:14888|1024:28860

        Legend:

        x ixs:
            data size/ix: encoded size | ...
            data size/ix: encoded size | ...  (using lookup tables)

        Given that max transaction size is 1644 bytes, we can see that the max data size is:

        -  1 ixs: slightly larger than 500 bytes
        -  2 ixs: slightly larger than 200 bytes
        -  5 ixs: slightly larger than 100 bytes
        -  8 ixs: slightly larger than  50 bytes
        - 10 ixs: slightly larger than  20 bytes
        - 15 ixs: slightly larger than  10 bytes
        - 20 ixs: no data supported (only lamport changes)

        Also it is clear that using a lookup table makes a huge difference especially if we commit
        lots of different accounts.
        */
    }

    // -----------------
    // Process Commitables and Close Buffers
    // -----------------
    lazy_static! {
        pub(crate) static ref MAX_PROCESS_PER_TX: u8 = {
            max_chunks_per_transaction("Max process per tx", |auth_pubkey| {
                let pubkey = Pubkey::new_unique();
                let delegated_account_owner = Pubkey::new_unique();
                let buffer_pda = Pubkey::new_unique();
                let commit_args = CommitStateFromBufferArgs::default();
                vec![dlp::instruction_builder::process_commits_ix(
                    auth_pubkey,
                    &pubkey,
                    &delegated_account_owner,
                    &buffer_pda,
                    commit_args,
                )]
            })
        };
        pub(crate) static ref MAX_PROCESS_PER_TX_USING_LOOKUP: u8 = {
            max_chunks_per_transaction_using_lookup_table(
                "Max process per tx using lookup",
                |auth_pubkey, committee, delegated_account_owner| {
                    let buffer_pda = Pubkey::new_unique();
                    let commit_args = CommitStateFromBufferArgs::default();
                    vec![process_commits_ix(
                        auth_pubkey,
                        &committee,
                        &delegated_account_owner,
                        &buffer_pda,
                        commit_args,
                    )]
                },
                None,
            )
        };
        pub(crate) static ref MAX_CLOSE_PER_TX: u8 = {
            let commit_id = 0;
            max_chunks_per_transaction("Max close per tx", |auth_pubkey| {
                let pubkey = Pubkey::new_unique();
                vec![close_buffers_ix(
                    auth_pubkey,
                    &pubkey,
                    &ephemeral_blockhash,
                )]
            })
        };
        pub(crate) static ref MAX_CLOSE_PER_TX_USING_LOOKUP: u8 = {
            let commit_id = 0;
            max_chunks_per_transaction_using_lookup_table(
                "Max close per tx using lookup",
                |auth_pubkey, committee, _| {
                    vec![close_buffers_ix(
                        auth_pubkey,
                        &committee,
                        &ephemeral_blockhash,
                    )]
                },
                None,
            )
        };
        pub(crate) static ref MAX_PROCESS_AND_CLOSE_PER_TX: u8 = {
            let commit_id = 0;
            max_chunks_per_transaction(
                "Max process and close per tx",
                |auth_pubkey| {
                    let pubkey = Pubkey::new_unique();
                    let delegated_account_owner = Pubkey::new_unique();
                    let buffer_pda = Pubkey::new_unique();
                    let commit_args = CommitStateFromBufferArgs::default();
                    process_and_close_ixs(
                        auth_pubkey,
                        &pubkey,
                        &delegated_account_owner,
                        &buffer_pda,
                        commit_id,
                        commit_args,
                    )
                },
            )
        };
        pub(crate) static ref MAX_PROCESS_AND_CLOSE_PER_TX_USING_LOOKUP: u8 = {
            let ephemeral_blockhash = Hash::default();
            max_chunks_per_transaction_using_lookup_table(
                "Max process and close per tx using lookup",
                |auth_pubkey, committee, delegated_account_owner| {
                    let commit_args = CommitStateFromBufferArgs::default();
                    let buffer_pda = Pubkey::new_unique();
                    super::process_and_close_ixs(
                        auth_pubkey,
                        &committee,
                        &delegated_account_owner,
                        &buffer_pda,
                        &ephemeral_blockhash,
                        commit_args,
                    )
                },
                None,
            )
        };
    }

    // -----------------
    // Finalize
    // -----------------
    lazy_static! {
        pub(crate) static ref MAX_FINALIZE_PER_TX: u8 = {
            max_chunks_per_transaction("Max finalize per tx", |auth_pubkey| {
                let pubkey = Pubkey::new_unique();
                vec![super::finalize_ix(auth_pubkey, &pubkey)]
            })
        };
        pub(crate) static ref MAX_FINALIZE_PER_TX_USING_LOOKUP: u8 = {
            max_chunks_per_transaction_using_lookup_table(
                "Max finalize per tx using lookup",
                |auth_pubkey, committee, _| {
                    vec![super::finalize_ix(auth_pubkey, &committee)]
                },
                Some(40),
            )
        };
    }

    // -----------------
    // Undelegate
    // -----------------
    lazy_static! {
        pub(crate) static ref MAX_UNDELEGATE_PER_TX: u8 = {
            max_chunks_per_transaction("Max undelegate per tx", |auth_pubkey| {
                let pubkey = Pubkey::new_unique();
                let owner_program = Pubkey::new_unique();
                vec![dlp::instruction_builder::undelegate(
                    auth_pubkey,
                    pubkey,
                    owner_program,
                    auth_pubkey,
                )]
            })
        };
        pub(crate) static ref MAX_UNDELEGATE_PER_TX_USING_LOOKUP: u8 = {
            max_chunks_per_transaction_using_lookup_table(
                "Max undelegate per tx using lookup",
                |auth_pubkey, committee, owner_program| {
                    vec![dlp::instruction_builder::undelegate(
                        auth_pubkey,
                        committee,
                        owner_program,
                        auth_pubkey,
                    )]
                },
                None,
            )
        };
    }

    // -----------------
    // Max Chunks Per Transaction
    // -----------------

    fn max_chunks_per_transaction<F: Fn(Pubkey) -> Vec<Instruction>>(
        label: &str,
        create_ixs: F,
    ) -> u8 {
        eprintln!("{}", label);

        let auth = Keypair::new();
        let auth_pubkey = auth.pubkey();
        // NOTE: the size of the budget instructions is always the same, no matter
        // which budget we provide
        let mut ixs = ComputeBudget::Process(Budget::default()).instructions(1);
        let mut chunks = 0_u8;
        loop {
            ixs.extend(create_ixs(auth_pubkey));
            chunks += 1;

            // SAFETY: runs statically
            let versioned_msg =
                Message::try_compile(&auth_pubkey, &ixs, &[], Hash::default())
                    .unwrap();
            // SAFETY: runs statically
            let versioned_tx = VersionedTransaction::try_new(
                VersionedMessage::V0(versioned_msg),
                &[&auth],
            )
            .unwrap();
            let encoded = serialize_and_encode_base64(&versioned_tx);
            eprintln!("{} ixs -> {} bytes", chunks, encoded.len());
            if encoded.len() > MAX_ENCODED_TRANSACTION_SIZE {
                return chunks - 1;
            }
        }
    }

    fn extend_lookup_table(
        lookup_table: &mut AddressLookupTableAccount,
        auth_pubkey: Pubkey,
        committee: Pubkey,
        owner: Option<&Pubkey>,
    ) {
        let keys = provide_committee_pubkeys(&committee, owner)
            .into_iter()
            .chain(provide_common_pubkeys(&auth_pubkey))
            .chain(lookup_table.addresses.iter().cloned())
            .collect::<HashSet<_>>();
        lookup_table.addresses = keys.into_iter().collect();
        assert!(
            lookup_table.addresses.len() <= LOOKUP_TABLE_MAX_ADDRESSES,
            "Lookup table has too many ({}) addresses",
            lookup_table.addresses.len()
        );
    }

    fn max_chunks_per_transaction_using_lookup_table<
        FI: Fn(Pubkey, Pubkey, Pubkey) -> Vec<Instruction>,
    >(
        label: &str,
        create_ixs: FI,
        start_at: Option<u8>,
    ) -> u8 {
        eprintln!("{}", label);
        let auth = Keypair::new();
        let auth_pubkey = auth.pubkey();
        let mut ixs = ComputeBudget::Process(Budget::default()).instructions(1);
        let mut chunks = start_at.unwrap_or_default();
        let mut lookup_table = AddressLookupTableAccount {
            key: Pubkey::default(),
            addresses: vec![],
        };
        // If we start at specific chunk size let's prep the ixs and assume
        // we are using the same addresses to avoid blowing out the lookup table
        if chunks > 0 {
            let committee = Pubkey::new_unique();
            let owner_program = Pubkey::new_unique();
            extend_lookup_table(
                &mut lookup_table,
                auth_pubkey,
                committee,
                Some(&owner_program),
            );
            for _ in 0..chunks {
                ixs.extend(create_ixs(auth_pubkey, committee, owner_program));
            }
        }
        loop {
            let committee = Pubkey::new_unique();
            let owner_program = Pubkey::new_unique();
            ixs.extend(create_ixs(auth_pubkey, committee, owner_program));

            chunks += 1;
            extend_lookup_table(
                &mut lookup_table,
                auth_pubkey,
                committee,
                Some(&owner_program),
            );

            // SAFETY: runs statically
            let versioned_msg = Message::try_compile(
                &auth_pubkey,
                &ixs,
                &[lookup_table.clone()],
                Hash::default(),
            )
            .unwrap();
            // SAFETY: runs statically
            let versioned_tx = VersionedTransaction::try_new(
                VersionedMessage::V0(versioned_msg),
                &[&auth],
            )
            .unwrap();
            let encoded = serialize_and_encode_base64(&versioned_tx);
            eprintln!("{} ixs -> {} bytes", chunks, encoded.len());
            if encoded.len() > MAX_ENCODED_TRANSACTION_SIZE {
                return chunks - 1;
            }
        }
    }
}
