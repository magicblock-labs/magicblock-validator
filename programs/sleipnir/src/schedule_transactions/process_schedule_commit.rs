use std::{
    collections::HashSet,
    sync::atomic::{AtomicU64, Ordering},
};

use solana_program_runtime::{ic_msg, invoke_context::InvokeContext};
use solana_sdk::{
    account::ReadableAccount, instruction::InstructionError, pubkey::Pubkey,
};

use super::transaction_scheduler::ScheduledCommit;
use crate::{
    schedule_transactions::transaction_scheduler::TransactionScheduler,
    sleipnir_instruction::scheduled_commit_sent,
    utils::accounts::{
        get_instruction_account_with_idx, get_instruction_pubkey_with_idx,
    },
};

pub(crate) fn process_schedule_commit(
    signers: HashSet<Pubkey>,
    invoke_context: &mut InvokeContext,
) -> Result<(), InstructionError> {
    static ID: AtomicU64 = AtomicU64::new(0);

    const PAYER_IDX: u16 = 0;

    let transaction_context = &invoke_context.transaction_context.clone();
    let ix_ctx = transaction_context.get_current_instruction_context()?;
    let ix_accs_len = ix_ctx.get_number_of_instruction_accounts() as usize;
    const COMMITTEES_START: usize = PAYER_IDX as usize + 1;

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

    //
    // Get the program_id of the parent instruction that invoked this one via CPI
    //

    // We cannot easily simulate the transaction being invoked via CPI
    // from the owning program during unit tests
    // Instead the integration tests ensure that this works as expected
    #[cfg(not(test))]
    let frames = crate::utils::instruction_context_frames::InstructionContextFrames::try_from(transaction_context)?;
    #[cfg(not(test))]
    let parent_program_id = {
        let parent_program_id = frames
            .find_program_id_of_parent_of_current_instruction()
            .ok_or_else(|| {
                ic_msg!(
                    invoke_context,
                    "ScheduleCommit ERR: failed to find parent program id"
                );
                InstructionError::InvalidInstructionData
            })?;

        ic_msg!(
            invoke_context,
            "ScheduleCommit: parent program id: {}",
            parent_program_id
        );
        parent_program_id
    };

    // During unit tests we assume the first committee has the correct program ID
    #[cfg(test)]
    let first_committee = get_instruction_account_with_idx(
        transaction_context,
        COMMITTEES_START as u16,
    )?
    .borrow();
    #[cfg(test)]
    let parent_program_id = first_committee.owner();

    // Assert all PDAs are owned by invoking program
    // NOTE: we don't require them to be signers as in our case verifying that the
    // program owning the PDAs invoked us via CPI is sufficient
    // Thus we can be `invoke`d unsigned and no seeds need to be provided
    let mut pubkeys = Vec::new();
    for idx in COMMITTEES_START..ix_accs_len {
        let acc_pubkey =
            get_instruction_pubkey_with_idx(transaction_context, idx as u16)?;
        let acc =
            get_instruction_account_with_idx(transaction_context, idx as u16)?;

        if parent_program_id != acc.borrow().owner() {
            ic_msg!(
                invoke_context,
                "ScheduleCommit ERR: account {} needs to be owned by the invoking program {} to be committed, but is owned by {}",
                acc_pubkey, parent_program_id, acc.borrow().owner()
            );
            return Err(InstructionError::InvalidAccountOwner);
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

    let blockhash = invoke_context.blockhash;
    let commit_sent_transaction = scheduled_commit_sent(id, blockhash);

    let commit_sent_sig = commit_sent_transaction.signatures[0];
    let scheduled_commit = ScheduledCommit {
        id,
        slot: clock.slot,
        blockhash,
        accounts: pubkeys,
        payer: *payer_pubkey,
        commit_sent_transaction,
    };

    // NOTE: this is only protected by all the above checks however if the
    // instruction fails for other reasons detected afterwards then the commit
    // stays scheduled
    TransactionScheduler::default().schedule_commit(scheduled_commit);
    ic_msg!(invoke_context, "Scheduled commit with ID: {}", id,);
    ic_msg!(
        invoke_context,
        "ScheduledCommitSent signature: {}",
        commit_sent_sig,
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use assert_matches::assert_matches;
    use solana_sdk::{
        account::{create_account_shared_data_for_test, AccountSharedData},
        clock,
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
            map.insert(committee, AccountSharedData::new(0, 0, &program));
            map
        };
        ensure_funded_validator_authority(&mut account_data);

        let ix = schedule_commit_instruction(&payer.pubkey(), vec![committee]);

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

        process_instruction(
            ix.data.as_slice(),
            transaction_accounts,
            ix.accounts,
            Ok(()),
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
                commit_sent_transaction: tx,
            } => {
                assert!(i >= &0);
                assert_eq!(s, &test_clock.slot);
                assert_eq!(p, &payer.pubkey());
                assert_eq!(accs, &vec![committee]);
                let ix = SleipnirInstruction::ScheduledCommitSent(*i);
                assert_eq!(tx.data(0), ix.try_to_vec().unwrap());
            }
        );
    }

    #[test]
    fn test_schedule_commit_three_accounts_success() {
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
            map.insert(committee_uno, AccountSharedData::new(0, 0, &program));
            map.insert(committee_dos, AccountSharedData::new(0, 0, &program));
            map.insert(committee_tres, AccountSharedData::new(0, 0, &program));
            map
        };
        ensure_funded_validator_authority(&mut account_data);

        let ix = schedule_commit_instruction(
            &payer.pubkey(),
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

        process_instruction(
            ix.data.as_slice(),
            transaction_accounts,
            ix.accounts,
            Ok(()),
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
                commit_sent_transaction: tx,
            } => {
                assert!(i >= &0);
                assert_eq!(s, &test_clock.slot);
                assert_eq!(p, &payer.pubkey());
                assert_eq!(accs, &vec![committee_uno, committee_dos, committee_tres]);
                let ix = SleipnirInstruction::ScheduledCommitSent(*i);
                assert_eq!(tx.data(0), ix.try_to_vec().unwrap());
            }
        );
    }

    // -----------------
    // Failure Cases
    // ----------------
    fn get_account_metas(
        payer: &Pubkey,
        pdas: Vec<Pubkey>,
    ) -> Vec<AccountMeta> {
        let mut account_metas = vec![AccountMeta::new(*payer, true)];
        for pubkey in &pdas {
            account_metas.push(AccountMeta::new_readonly(*pubkey, true));
        }
        account_metas
    }

    fn account_metas_last_committee_not_signer(
        payer: &Pubkey,
        pdas: Vec<Pubkey>,
    ) -> Vec<AccountMeta> {
        let mut account_metas = get_account_metas(payer, pdas);
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
            committee_uno,
            committee_dos,
            committee_tres,
            transaction_accounts,
        }
    }

    #[test]
    fn test_schedule_commit_no_pdas_provided_to_ix() {
        let payer =
            Keypair::from_seed(b"test_schedule_commit_no_pdas_provided_to_ix")
                .unwrap();

        let PreparedTransactionThreeCommittees {
            mut accounts_data,
            mut transaction_accounts,
            ..
        } = prepare_transaction_with_three_committees(&payer);

        let ix = instruction_from_account_metas(get_account_metas(
            &payer.pubkey(),
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
            committee_uno,
            committee_dos,
            committee_tres,
            mut transaction_accounts,
            ..
        } = prepare_transaction_with_three_committees(&payer);

        accounts_data.insert(
            committee_dos,
            AccountSharedData::new(0, 0, &Pubkey::new_unique()),
        );

        let ix = instruction_from_account_metas(
            account_metas_last_committee_not_signer(
                &payer.pubkey(),
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
}
