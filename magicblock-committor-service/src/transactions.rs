use solana_packet::PACKET_DATA_SIZE;
use solana_rpc_client::rpc_client::SerializableTransaction;
use static_assertions::const_assert;

/// Maximum serialized transaction size that can be sent over the wire.
pub(crate) const MAX_TRANSACTION_WIRE_SIZE: usize = PACKET_DATA_SIZE;

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
pub const MAX_CLOSE_PER_TX: u8 = 8;

/// How many close buffer instructions fit into a single transaction
/// when using address lookup tables but not including the buffer account
/// nor chunk account in the lookup table
#[allow(unused)] // serves as documentation as well
pub const MAX_CLOSE_PER_TX_USING_LOOKUP: u8 = 8;

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
pub const MAX_UNDELEGATE_PER_TX_USING_LOOKUP: u8 = 15;

// Allows us to run undelegate instructions without rechunking them since we know
// that we didn't process more than we also can undelegate
const_assert!(MAX_PROCESS_PER_TX <= MAX_UNDELEGATE_PER_TX,);

// Allows us to run undelegate instructions using lookup tables without rechunking
// them since we know that we didn't process more than we also can undelegate
const_assert!(
    MAX_PROCESS_PER_TX_USING_LOOKUP <= MAX_UNDELEGATE_PER_TX_USING_LOOKUP
);

pub fn serialized_transaction_size(
    transaction: &impl SerializableTransaction,
) -> usize {
    // SAFETY: runs on transactions we already serialize before sending.
    usize::try_from(bincode::serialized_size(transaction).unwrap()).unwrap()
}

#[cfg(test)]
mod test {
    use std::collections::HashSet;

    use dlp_api::{
        args::{CommitStateArgs, CommitStateFromBufferArgs},
        pda,
    };
    use magicblock_committor_program::{
        id as committor_program_id,
        instruction_builder::close_buffer::{
            create_close_ix, CreateCloseIxArgs,
        },
    };
    use solana_address_lookup_table_interface::state::LOOKUP_TABLE_MAX_ADDRESSES;
    use solana_compute_budget_interface::ComputeBudgetInstruction;
    use solana_hash::Hash;
    use solana_instruction::Instruction;
    use solana_keypair::Keypair;
    use solana_message::{
        v0::Message, AddressLookupTableAccount, VersionedMessage,
    };
    use solana_pubkey::Pubkey;
    use solana_sdk_ids::system_program;
    use solana_signer::Signer;
    use solana_transaction::versioned::VersionedTransaction;
    use tracing::info;

    use super::*;
    use crate::test_utils;

    type TransactionSizeError = Box<dyn std::error::Error + Send + Sync>;
    type TransactionSizeResult<T> = Result<T, TransactionSizeError>;

    fn get_lookup_tables(
        ixs: &[Instruction],
    ) -> Vec<AddressLookupTableAccount> {
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

    // -----------------
    // Helpers
    // -----------------
    #[allow(clippy::enum_variant_names)]
    enum TransactionOpts {
        NoLookupTable,
        UseLookupTable,
    }
    fn transaction_wire_size(
        auth: &Keypair,
        ixs: &[Instruction],
        opts: &TransactionOpts,
    ) -> TransactionSizeResult<usize> {
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
        .map_err(|err| -> TransactionSizeError { Box::new(err) })?;
        let versioned_tx = VersionedTransaction::try_new(
            VersionedMessage::V0(versioned_msg),
            &[&auth],
        )
        .map_err(|err| -> TransactionSizeError { Box::new(err) })?;

        Ok(serialized_transaction_size(&versioned_tx))
    }

    fn compute_budget_ixs() -> Vec<Instruction> {
        vec![
            ComputeBudgetInstruction::set_compute_unit_limit(125_000),
            ComputeBudgetInstruction::set_compute_unit_price(1_000_000),
        ]
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
        dlp_api::instruction_builder::commit_state_from_buffer(
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
        commit_id: u64,
        commit_args: CommitStateFromBufferArgs,
    ) -> Vec<Instruction> {
        let process_ix = process_commits_ix(
            validator_auth,
            pubkey,
            delegated_account_owner,
            buffer_pda,
            commit_args,
        );
        let close_ix = close_buffers_ix(validator_auth, pubkey, commit_id);

        vec![process_ix, close_ix]
    }

    // -----------------
    // Finalize
    // -----------------
    pub(crate) fn finalize_ix(
        validator_auth: Pubkey,
        pubkey: &Pubkey,
    ) -> Instruction {
        dlp_api::instruction_builder::finalize(validator_auth, *pubkey)
    }

    // These tests statically determine the optimal ix count to fit into a single
    // transaction and assert that the const we export in prod match those numbers.
    // Thus when an instruction changes and one of those numbers with it a failing
    // test alerts us.
    // This is less overhead than running those static functions each time at
    // startup.

    #[test]
    fn test_max_process_per_tx() {
        test_utils::init_test_logger();
        assert_eq!(super::MAX_PROCESS_PER_TX, max_process_per_tx());
        assert_eq!(
            super::MAX_PROCESS_PER_TX_USING_LOOKUP,
            max_process_per_tx_using_lookup()
        );
    }

    #[test]
    fn test_max_close_per_tx() {
        test_utils::init_test_logger();
        assert_eq!(super::MAX_CLOSE_PER_TX, max_close_per_tx());
        assert_eq!(
            super::MAX_CLOSE_PER_TX_USING_LOOKUP,
            max_close_per_tx_using_lookup()
        );
    }

    #[test]
    fn test_max_process_and_closes_per_tx() {
        test_utils::init_test_logger();
        assert_eq!(
            super::MAX_PROCESS_AND_CLOSE_PER_TX,
            max_process_and_close_per_tx()
        );
        assert_eq!(
            super::MAX_PROCESS_AND_CLOSE_PER_TX_USING_LOOKUP,
            max_process_and_close_per_tx_using_lookup()
        );
    }

    #[test]
    fn test_max_finalize_per_tx() {
        test_utils::init_test_logger();
        assert_eq!(super::MAX_FINALIZE_PER_TX, max_finalize_per_tx());
        assert_eq!(
            super::MAX_FINALIZE_PER_TX_USING_LOOKUP,
            max_finalize_per_tx_using_lookup()
        );
    }

    #[test]
    fn test_max_undelegate_per_tx() {
        test_utils::init_test_logger();
        assert_eq!(super::MAX_UNDELEGATE_PER_TX, max_undelegate_per_tx());
        assert_eq!(
            super::MAX_UNDELEGATE_PER_TX_USING_LOOKUP,
            max_undelegate_per_tx_using_lookup()
        );
    }

    // -----------------
    // Process Commitables using Args
    // -----------------
    #[test]
    fn test_log_commit_args_ix_sizes() {
        test_utils::init_test_logger();
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
                        transaction_wire_size(auth, &ixs, &tx_opts).unwrap();
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
            info!(instruction_count = ixs, sizes = %sizes, "Transaction size report");
        }
        fn make_ix(auth: &Keypair, data_size: usize) -> Instruction {
            let data = vec![1; data_size];
            let args = CommitStateArgs {
                data,
                ..CommitStateArgs::default()
            };
            dlp_api::instruction_builder::commit_state(
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
        // This logs raw wire sizes. The sendability limit is
        // MAX_TRANSACTION_WIRE_SIZE.
    }

    // -----------------
    // Process Commitables and Close Buffers
    // -----------------
    fn max_process_per_tx() -> u8 {
        max_chunks_per_transaction("Max process per tx", |auth_pubkey| {
            let pubkey = Pubkey::new_unique();
            let delegated_account_owner = Pubkey::new_unique();
            let buffer_pda = Pubkey::new_unique();
            let commit_args = CommitStateFromBufferArgs::default();
            vec![process_commits_ix(
                auth_pubkey,
                &pubkey,
                &delegated_account_owner,
                &buffer_pda,
                commit_args,
            )]
        })
    }

    fn max_process_per_tx_using_lookup() -> u8 {
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
    }

    fn max_close_per_tx() -> u8 {
        let commit_id = 0;
        max_chunks_per_transaction("Max close per tx", |auth_pubkey| {
            let pubkey = Pubkey::new_unique();
            vec![close_buffers_ix(auth_pubkey, &pubkey, commit_id)]
        })
    }

    fn max_close_per_tx_using_lookup() -> u8 {
        let commit_id = 0;
        max_chunks_per_transaction_using_lookup_table(
            "Max close per tx using lookup",
            |auth_pubkey, committee, _| {
                vec![close_buffers_ix(auth_pubkey, &committee, commit_id)]
            },
            None,
        )
    }

    fn max_process_and_close_per_tx() -> u8 {
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
    }

    fn max_process_and_close_per_tx_using_lookup() -> u8 {
        let commit_id = 0;
        max_chunks_per_transaction_using_lookup_table(
            "Max process and close per tx using lookup",
            |auth_pubkey, committee, delegated_account_owner| {
                let commit_args = CommitStateFromBufferArgs::default();
                let buffer_pda = Pubkey::new_unique();
                process_and_close_ixs(
                    auth_pubkey,
                    &committee,
                    &delegated_account_owner,
                    &buffer_pda,
                    commit_id,
                    commit_args,
                )
            },
            None,
        )
    }

    // -----------------
    // Finalize
    // -----------------
    fn max_finalize_per_tx() -> u8 {
        max_chunks_per_transaction("Max finalize per tx", |auth_pubkey| {
            let pubkey = Pubkey::new_unique();
            vec![finalize_ix(auth_pubkey, &pubkey)]
        })
    }

    fn max_finalize_per_tx_using_lookup() -> u8 {
        max_chunks_per_transaction_using_lookup_table(
            "Max finalize per tx using lookup",
            |auth_pubkey, committee, _| {
                vec![finalize_ix(auth_pubkey, &committee)]
            },
            Some(40),
        )
    }

    // -----------------
    // Undelegate
    // -----------------
    fn max_undelegate_per_tx() -> u8 {
        max_chunks_per_transaction("Max undelegate per tx", |auth_pubkey| {
            let pubkey = Pubkey::new_unique();
            let owner_program = Pubkey::new_unique();
            vec![dlp_api::instruction_builder::undelegate(
                auth_pubkey,
                pubkey,
                owner_program,
                auth_pubkey,
            )]
        })
    }

    fn max_undelegate_per_tx_using_lookup() -> u8 {
        max_chunks_per_transaction_using_lookup_table(
            "Max undelegate per tx using lookup",
            |auth_pubkey, committee, owner_program| {
                vec![dlp_api::instruction_builder::undelegate(
                    auth_pubkey,
                    committee,
                    owner_program,
                    auth_pubkey,
                )]
            },
            None,
        )
    }

    // -----------------
    // Max Chunks Per Transaction
    // -----------------

    fn max_chunks_per_transaction<F: Fn(Pubkey) -> Vec<Instruction>>(
        label: &str,
        create_ixs: F,
    ) -> u8 {
        info!(test_label = label, "Starting transaction chunking test");

        let auth = Keypair::new();
        let auth_pubkey = auth.pubkey();
        // NOTE: the size of the budget instructions is always the same, no matter
        // which budget we provide
        let mut ixs = compute_budget_ixs();
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
            let tx_size = serialized_transaction_size(&versioned_tx);
            info!(
                chunks = chunks,
                size_bytes = tx_size,
                "Transaction size measured"
            );
            if tx_size > MAX_TRANSACTION_WIRE_SIZE {
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
        let mut keys = HashSet::from([
            committee,
            pda::delegation_record_pda_from_delegated_account(&committee),
            pda::delegation_metadata_pda_from_delegated_account(&committee),
            pda::commit_state_pda_from_delegated_account(&committee),
            pda::commit_record_pda_from_delegated_account(&committee),
            pda::undelegate_buffer_pda_from_delegated_account(&committee),
            auth_pubkey,
            system_program::id(),
            dlp_api::id(),
            pda::fees_vault_pda(),
            pda::validator_fees_vault_pda_from_validator(&auth_pubkey),
            committor_program_id(),
        ]);
        if let Some(owner) = owner {
            keys.insert(pda::program_config_from_program_id(owner));
        }
        keys.extend(lookup_table.addresses.iter().cloned());
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
        info!(test_label = label, start_at = ?start_at, "Starting lookup table transaction test");
        let auth = Keypair::new();
        let auth_pubkey = auth.pubkey();
        let mut ixs = compute_budget_ixs();
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
            let tx_size = serialized_transaction_size(&versioned_tx);
            info!(
                chunks = chunks,
                size_bytes = tx_size,
                "Transaction size measured with lookup table"
            );
            if tx_size > MAX_TRANSACTION_WIRE_SIZE {
                return chunks - 1;
            }
        }
    }
}
