use std::{
    collections::HashSet,
    sync::atomic::{AtomicU64, Ordering},
};

use solana_program_runtime::{ic_msg, invoke_context::InvokeContext};
use solana_sdk::{
    account::ReadableAccount,
    fee_calculator::DEFAULT_TARGET_LAMPORTS_PER_SIGNATURE,
    instruction::InstructionError, pubkey::Pubkey,
    transaction_context::TransactionContext,
};

use crate::{
    schedule_transactions::transaction_scheduler::TransactionScheduler,
    utils::accounts::{
        credit_instruction_account_at_index,
        debit_instruction_account_at_index, get_instruction_account_with_idx,
        get_instruction_pubkey_with_idx,
    },
};

use super::transaction_scheduler::ScheduledCommit;

pub(crate) fn process_schedule_commit(
    signers: HashSet<Pubkey>,
    invoke_context: &InvokeContext,
    transaction_context: &TransactionContext,
) -> Result<(), InstructionError> {
    static ID: AtomicU64 = AtomicU64::new(0);

    const PAYER_IDX: u16 = 0;
    const PROGRAM_IDX: u16 = 1;
    const VALIDATOR_IDX: u16 = 2;
    // Need SystemProgram for PDA signing
    const SYSTEM_PROG_IDX: u16 = 3;

    let ix_ctx = transaction_context.get_current_instruction_context()?;
    let ix_accs_len = ix_ctx.get_number_of_instruction_accounts() as usize;

    const COMMITTEES_START: usize = SYSTEM_PROG_IDX as usize + 1;

    // Assert MagicBlock program
    ix_ctx
        .find_index_of_program_account(transaction_context, &crate::id())
        .ok_or_else(|| {
            ic_msg!(
                invoke_context,
                "ScheduleCommit ERR: Magic program account not found"
            );
            InstructionError::UnsupportedProgramId
        })?;

    // Assert enough accounts
    if ix_accs_len <= COMMITTEES_START {
        ic_msg!(
            invoke_context,
            "ScheduleCommit ERR: not enough accounts to schedule commit ({}), need payer, signing program an account for each pubkey to be committed",
            ix_accs_len
        );
        return Err(InstructionError::NotEnoughAccountKeys);
    }

    // Assert Payer is signer
    let payer_pubkey =
        get_instruction_pubkey_with_idx(transaction_context, PAYER_IDX)?;
    if !signers.contains(payer_pubkey) {
        ic_msg!(
            invoke_context,
            "ScheduleCommit ERR: payer pubkey {} not in signers",
            payer_pubkey
        );
        return Err(InstructionError::MissingRequiredSignature);
    }

    let owner_pubkey =
        get_instruction_pubkey_with_idx(transaction_context, PROGRAM_IDX)?;

    // Assert validator identity matches
    let validator_pubkey =
        get_instruction_pubkey_with_idx(transaction_context, VALIDATOR_IDX)?;
    let validator_authority_id = crate::validator_authority_id();
    if validator_pubkey != &validator_authority_id {
        ic_msg!(
            invoke_context,
            "ScheduleCommit ERR: provided validator account {} does not match validator identity {}",
            validator_pubkey, validator_authority_id
        );
        return Err(InstructionError::IncorrectAuthority);
    }

    // Assert all PDAs are owned by invoking program and are signers
    let mut pubkeys = Vec::new();
    for idx in COMMITTEES_START..ix_accs_len {
        let acc_pubkey =
            get_instruction_pubkey_with_idx(transaction_context, idx as u16)?;
        let acc =
            get_instruction_account_with_idx(transaction_context, idx as u16)?;

        if owner_pubkey != acc.borrow().owner() {
            ic_msg!(
                invoke_context,
                "ScheduleCommit ERR: account {} needs to be owned by invoking program {} to be committed, but is owned by {}",
                acc_pubkey, owner_pubkey, acc.borrow().owner()
            );
            return Err(InstructionError::InvalidAccountOwner);
        }
        if !signers.contains(acc_pubkey) {
            ic_msg!(
                invoke_context,
                "ScheduleCommit ERR: account pubkey {} not in signers,
                 which means it's not a PDA of the invoking program or the corresponding signer seed were not included",
                acc_pubkey
            );
            return Err(InstructionError::MissingRequiredSignature);
        }

        pubkeys.push(*acc_pubkey);
    }

    // Determine id and slot
    let id = ID.fetch_add(1, Ordering::Relaxed);

    // It appears that in builtin programs `Clock::get` doesn't work as expected, thus
    // we have to get it directly from the sysvar cache.
    let clock =
        invoke_context
            .get_sysvar_cache()
            .get_clock()
            .map_err(|err| {
                ic_msg!(invoke_context, "Failed to get clock sysvar: {}", err);
                InstructionError::UnsupportedSysvar
            })?;

    // Deduct lamports from payer to pay for transaction and credit the validator
    // identity with it.
    // For now we assume that chain cost match the defaults
    // We may have to charge more here if we want to pay extra to ensure the
    // transaction lands.
    let tx_cost = DEFAULT_TARGET_LAMPORTS_PER_SIGNATURE;
    debit_instruction_account_at_index(
        transaction_context,
        PAYER_IDX,
        tx_cost,
    )?;
    credit_instruction_account_at_index(
        transaction_context,
        VALIDATOR_IDX,
        tx_cost,
    )?;

    let scheduled_commit = ScheduledCommit {
        id,
        slot: clock.slot,
        blockhash: invoke_context.blockhash,
        accounts: pubkeys,
        payer: *payer_pubkey,
    };

    TransactionScheduler::default().schedule_commit(scheduled_commit);
    ic_msg!(invoke_context, "Scheduled commit: {}", id,);

    Ok(())
}

#[cfg(test)]
mod tests {
    use assert_matches::assert_matches;
    use std::collections::HashMap;

    use solana_sdk::{
        account::{
            create_account_shared_data_for_test, AccountSharedData,
            ReadableAccount,
        },
        bpf_loader_upgradeable, clock,
        fee_calculator::DEFAULT_TARGET_LAMPORTS_PER_SIGNATURE,
        instruction::{AccountMeta, Instruction, InstructionError},
        pubkey::Pubkey,
        signature::Keypair,
        signer::{SeedDerivable, Signer},
        system_program,
        sysvar::SysvarId,
    };

    use crate::{
        schedule_transactions::transaction_scheduler::{
            ScheduledCommit, TransactionScheduler,
        },
        sleipnir_instruction::{
            schedule_commit_instruction, SleipnirInstruction,
        },
        test_utils::{ensure_funded_validator_authority, process_instruction},
        validator_authority_id,
    };

    // For the scheduling itself and the debit to fund the scheduled transaction
    const REQUIRED_TX_COST: u64 = DEFAULT_TARGET_LAMPORTS_PER_SIGNATURE * 2;

    fn get_clock() -> clock::Clock {
        clock::Clock {
            slot: 100,
            unix_timestamp: 1_000,
            epoch_start_timestamp: 0,
            epoch: 10,
            leader_schedule_epoch: 10,
        }
    }

    fn account_idx(
        transaction_accounts: &[(Pubkey, AccountSharedData)],
        pubkey: &Pubkey,
    ) -> usize {
        transaction_accounts
            .iter()
            .enumerate()
            .find_map(
                |(idx, (x, _))| {
                    if x.eq(pubkey) {
                        Some(idx)
                    } else {
                        None
                    }
                },
            )
            .unwrap()
    }

    #[test]
    fn test_schedule_commit_single_account() {
        // Ensuring unique payers for each test to isolate scheduled commits
        let payer =
            Keypair::from_seed(b"test_schedule_commit_single_account").unwrap();
        let program = Pubkey::new_unique();
        let committee = Pubkey::new_unique();
        let mut account_data = {
            let mut map = HashMap::new();
            map.insert(
                payer.pubkey(),
                AccountSharedData::new(
                    REQUIRED_TX_COST,
                    0,
                    &system_program::id(),
                ),
            );
            map.insert(
                program,
                AccountSharedData::new(0, 0, &bpf_loader_upgradeable::id()),
            );
            map.insert(committee, AccountSharedData::new(0, 0, &program));
            map
        };
        ensure_funded_validator_authority(&mut account_data);

        let ix = schedule_commit_instruction(
            &payer.pubkey(),
            &program,
            &validator_authority_id(),
            vec![committee],
        );

        let mut transaction_accounts: Vec<(Pubkey, AccountSharedData)> = ix
            .accounts
            .iter()
            .flat_map(|acc| {
                account_data
                    .remove(&acc.pubkey)
                    .map(|shared_data| (acc.pubkey, shared_data))
            })
            .collect();

        transaction_accounts.push((
            clock::Clock::id(),
            create_account_shared_data_for_test(&get_clock()),
        ));

        let payer_idx = account_idx(&transaction_accounts, &payer.pubkey());
        let auth_idx =
            account_idx(&transaction_accounts, &validator_authority_id());

        let (_, payer_before) = &transaction_accounts[payer_idx].clone();
        let (_, auth_before) = &transaction_accounts[auth_idx].clone();

        let accounts = process_instruction(
            ix.data.as_slice(),
            transaction_accounts,
            ix.accounts,
            Ok(()),
        );

        let payer_after = &accounts[payer_idx];
        let auth_after = &accounts[auth_idx];

        assert_eq!(
            payer_after.lamports(),
            payer_before.lamports() - DEFAULT_TARGET_LAMPORTS_PER_SIGNATURE
        );

        assert_eq!(
            auth_after.lamports(),
            auth_before.lamports() + DEFAULT_TARGET_LAMPORTS_PER_SIGNATURE
        );

        let scheduler = TransactionScheduler::default();
        let scheduled_commits =
            scheduler.get_scheduled_commits_by_payer(&payer.pubkey());
        assert_eq!(scheduled_commits.len(), 1);

        let commit = &scheduled_commits[0];
        let test_clock = get_clock();
        assert_matches!(
            commit,
            ScheduledCommit {
                id: i,
                slot: s,
                accounts: accs,
                payer: p,
                blockhash: _,
            } => {
                assert!(i >= &0);
                assert_eq!(s, &test_clock.slot);
                assert_eq!(p, &payer.pubkey());
                assert_eq!(accs, &vec![committee]);
            }
        );
    }

    #[test]
    fn test_schedule_commit_three_accounts() {
        let payer =
            Keypair::from_seed(b"test_schedule_commit_three_accounts").unwrap();
        let program = Pubkey::new_unique();
        let committee_uno = Pubkey::new_unique();
        let committee_dos = Pubkey::new_unique();
        let committee_tres = Pubkey::new_unique();
        let mut account_data = {
            let mut map = HashMap::new();
            map.insert(
                payer.pubkey(),
                AccountSharedData::new(
                    REQUIRED_TX_COST,
                    0,
                    &system_program::id(),
                ),
            );
            map.insert(
                program,
                AccountSharedData::new(0, 0, &bpf_loader_upgradeable::id()),
            );
            map.insert(committee_uno, AccountSharedData::new(0, 0, &program));
            map.insert(committee_dos, AccountSharedData::new(0, 0, &program));
            map.insert(committee_tres, AccountSharedData::new(0, 0, &program));
            map
        };
        ensure_funded_validator_authority(&mut account_data);

        let ix = schedule_commit_instruction(
            &payer.pubkey(),
            &program,
            &validator_authority_id(),
            vec![committee_uno, committee_dos, committee_tres],
        );

        let mut transaction_accounts: Vec<(Pubkey, AccountSharedData)> = ix
            .accounts
            .iter()
            .flat_map(|acc| {
                account_data
                    .remove(&acc.pubkey)
                    .map(|shared_data| (acc.pubkey, shared_data))
            })
            .collect();

        transaction_accounts.push((
            clock::Clock::id(),
            create_account_shared_data_for_test(&get_clock()),
        ));

        let payer_idx = account_idx(&transaction_accounts, &payer.pubkey());
        let auth_idx =
            account_idx(&transaction_accounts, &validator_authority_id());

        let (_, payer_before) = &transaction_accounts[payer_idx].clone();
        let (_, auth_before) = &transaction_accounts[auth_idx].clone();

        let accounts = process_instruction(
            ix.data.as_slice(),
            transaction_accounts,
            ix.accounts,
            Ok(()),
        );

        let payer_after = &accounts[payer_idx];
        let auth_after = &accounts[auth_idx];

        assert_eq!(
            payer_after.lamports(),
            payer_before.lamports() - DEFAULT_TARGET_LAMPORTS_PER_SIGNATURE
        );

        assert_eq!(
            auth_after.lamports(),
            auth_before.lamports() + DEFAULT_TARGET_LAMPORTS_PER_SIGNATURE
        );

        let scheduler = TransactionScheduler::default();
        let scheduled_commits =
            scheduler.get_scheduled_commits_by_payer(&payer.pubkey());
        assert_eq!(scheduled_commits.len(), 1);

        let commit = &scheduled_commits[0];
        let test_clock = get_clock();
        assert_matches!(
            commit,
            ScheduledCommit {
                id: i,
                slot: s,
                accounts: accs,
                payer: p,
                blockhash: _,
            } => {
                assert!(i >= &0);
                assert_eq!(s, &test_clock.slot);
                assert_eq!(p, &payer.pubkey());
                assert_eq!(accs, &vec![committee_uno, committee_dos, committee_tres]);
            }
        );
    }

    // -----------------
    // Failure Cases
    // ----------------
    fn get_account_metas(
        payer: &Pubkey,
        program: &Pubkey,
        validator_authority: &Pubkey,
        pdas: Vec<Pubkey>,
    ) -> Vec<AccountMeta> {
        let mut account_metas = vec![
            AccountMeta::new(*payer, true),
            AccountMeta::new_readonly(*program, false),
            AccountMeta::new(*validator_authority, false),
            AccountMeta::new_readonly(system_program::id(), false),
        ];
        for pubkey in &pdas {
            account_metas.push(AccountMeta::new_readonly(*pubkey, true));
        }
        account_metas
    }

    fn account_metas_last_committee_not_signer(
        payer: &Pubkey,
        program: &Pubkey,
        validator_authority: &Pubkey,
        pdas: Vec<Pubkey>,
    ) -> Vec<AccountMeta> {
        let mut account_metas =
            get_account_metas(payer, program, validator_authority, pdas);
        let last = account_metas.pop().unwrap();
        account_metas.push(AccountMeta::new_readonly(last.pubkey, false));
        account_metas
    }

    fn instruction_from_account_metas(
        account_metas: Vec<AccountMeta>,
    ) -> solana_sdk::instruction::Instruction {
        Instruction::new_with_bincode(
            crate::id(),
            &SleipnirInstruction::ScheduleCommit,
            account_metas,
        )
    }

    struct PreparedTransactionThreeCommittees {
        accounts_data: HashMap<Pubkey, AccountSharedData>,
        program: Pubkey,
        committee_uno: Pubkey,
        committee_dos: Pubkey,
        committee_tres: Pubkey,
        transaction_accounts: Vec<(Pubkey, AccountSharedData)>,
    }

    fn prepare_transaction_with_three_committees(
        payer: &Keypair,
    ) -> PreparedTransactionThreeCommittees {
        let program = Pubkey::new_unique();
        let committee_uno = Pubkey::new_unique();
        let committee_dos = Pubkey::new_unique();
        let committee_tres = Pubkey::new_unique();
        let mut accounts_data = {
            let mut map = HashMap::new();
            map.insert(
                payer.pubkey(),
                AccountSharedData::new(
                    REQUIRED_TX_COST,
                    0,
                    &system_program::id(),
                ),
            );
            map.insert(
                program,
                AccountSharedData::new(0, 0, &bpf_loader_upgradeable::id()),
            );
            map.insert(committee_uno, AccountSharedData::new(0, 0, &program));
            map.insert(committee_dos, AccountSharedData::new(0, 0, &program));
            map.insert(committee_tres, AccountSharedData::new(0, 0, &program));
            map
        };
        ensure_funded_validator_authority(&mut accounts_data);

        let transaction_accounts: Vec<(Pubkey, AccountSharedData)> = vec![(
            clock::Clock::id(),
            create_account_shared_data_for_test(&get_clock()),
        )];

        PreparedTransactionThreeCommittees {
            accounts_data,
            program,
            committee_uno,
            committee_dos,
            committee_tres,
            transaction_accounts,
        }
    }

    #[test]
    fn test_schedule_commit_three_accounts_last_not_signer() {
        let payer = Keypair::from_seed(
            b"test_schedule_commit_three_accounts_last_not_signer",
        )
        .unwrap();

        let PreparedTransactionThreeCommittees {
            mut accounts_data,
            program,
            committee_uno,
            committee_dos,
            committee_tres,
            mut transaction_accounts,
        } = prepare_transaction_with_three_committees(&payer);

        let ix = instruction_from_account_metas(
            account_metas_last_committee_not_signer(
                &payer.pubkey(),
                &program,
                &validator_authority_id(),
                vec![committee_uno, committee_dos, committee_tres],
            ),
        );

        transaction_accounts.extend(ix.accounts.iter().flat_map(|acc| {
            accounts_data
                .remove(&acc.pubkey)
                .map(|shared_data| (acc.pubkey, shared_data))
        }));

        process_instruction(
            ix.data.as_slice(),
            transaction_accounts,
            ix.accounts,
            Err(InstructionError::MissingRequiredSignature),
        );
    }

    #[test]
    fn test_schedule_commit_no_pdas_provided_to_ix() {
        let payer =
            Keypair::from_seed(b"test_schedule_commit_no_pdas_provided_to_ix")
                .unwrap();

        let PreparedTransactionThreeCommittees {
            mut accounts_data,
            program,
            mut transaction_accounts,
            ..
        } = prepare_transaction_with_three_committees(&payer);

        let ix = instruction_from_account_metas(get_account_metas(
            &payer.pubkey(),
            &program,
            &validator_authority_id(),
            vec![],
        ));

        transaction_accounts.extend(ix.accounts.iter().flat_map(|acc| {
            accounts_data
                .remove(&acc.pubkey)
                .map(|shared_data| (acc.pubkey, shared_data))
        }));

        process_instruction(
            ix.data.as_slice(),
            transaction_accounts,
            ix.accounts,
            Err(InstructionError::NotEnoughAccountKeys),
        );
    }

    #[test]
    fn test_schedule_commit_three_accounts_second_not_owned_by_program() {
        let payer = Keypair::from_seed(
            b"test_schedule_commit_three_accounts_last_not_owned_by_program",
        )
        .unwrap();

        let PreparedTransactionThreeCommittees {
            mut accounts_data,
            program,
            committee_uno,
            committee_dos,
            committee_tres,
            mut transaction_accounts,
        } = prepare_transaction_with_three_committees(&payer);

        accounts_data.insert(
            committee_dos,
            AccountSharedData::new(0, 0, &Pubkey::new_unique()),
        );

        let ix = instruction_from_account_metas(
            account_metas_last_committee_not_signer(
                &payer.pubkey(),
                &program,
                &validator_authority_id(),
                vec![committee_uno, committee_dos, committee_tres],
            ),
        );

        transaction_accounts.extend(ix.accounts.iter().flat_map(|acc| {
            accounts_data
                .remove(&acc.pubkey)
                .map(|shared_data| (acc.pubkey, shared_data))
        }));

        process_instruction(
            ix.data.as_slice(),
            transaction_accounts,
            ix.accounts,
            Err(InstructionError::InvalidAccountOwner),
        );
    }

    #[test]
    fn test_schedule_commit_three_accounts_invalid_validator_auth() {
        let payer = Keypair::from_seed(
            b"test_schedule_commit_three_accounts_invalid_validator_auth",
        )
        .unwrap();

        let PreparedTransactionThreeCommittees {
            mut accounts_data,
            program,
            committee_uno,
            committee_dos,
            committee_tres,
            mut transaction_accounts,
        } = prepare_transaction_with_three_committees(&payer);

        let ix = instruction_from_account_metas(
            account_metas_last_committee_not_signer(
                &payer.pubkey(),
                &program,
                &Pubkey::new_unique(),
                vec![committee_uno, committee_dos, committee_tres],
            ),
        );

        transaction_accounts.extend(ix.accounts.iter().flat_map(|acc| {
            accounts_data
                .remove(&acc.pubkey)
                .map(|shared_data| (acc.pubkey, shared_data))
        }));

        process_instruction(
            ix.data.as_slice(),
            transaction_accounts,
            ix.accounts,
            Err(InstructionError::IncorrectAuthority),
        );
    }
}
