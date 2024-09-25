use std::{
    collections::HashSet,
    sync::atomic::{AtomicU64, Ordering},
};

use sleipnir_core::magic_program::MAGIC_CONTEXT_PUBKEY;
use solana_program_runtime::{ic_msg, invoke_context::InvokeContext};
use solana_sdk::{
    account::ReadableAccount, instruction::InstructionError, pubkey::Pubkey,
};

use crate::{
    magic_context::{MagicContext, ScheduledCommit},
    schedule_transactions::transaction_scheduler::TransactionScheduler,
    sleipnir_instruction::scheduled_commit_sent,
    utils::{
        account_actions::set_account_owner_to_delegation_program,
        accounts::{
            get_instruction_account_with_idx, get_instruction_pubkey_with_idx,
        },
    },
    validator_authority_id,
};

#[derive(Default)]
pub(crate) struct ProcessScheduleCommitOptions {
    pub request_undelegation: bool,
}

pub(crate) fn process_schedule_commit(
    signers: HashSet<Pubkey>,
    invoke_context: &mut InvokeContext,
    opts: ProcessScheduleCommitOptions,
) -> Result<(), InstructionError> {
    static COMMIT_ID: AtomicU64 = AtomicU64::new(0);

    const PAYER_IDX: u16 = 0;
    const MAGIC_CONTEXT_IDX: u16 = PAYER_IDX + 1;

    check_magic_context_id(invoke_context, MAGIC_CONTEXT_IDX)?;

    let transaction_context = &invoke_context.transaction_context.clone();
    let ix_ctx = transaction_context.get_current_instruction_context()?;
    let ix_accs_len = ix_ctx.get_number_of_instruction_accounts() as usize;
    const COMMITTEES_START: usize = MAGIC_CONTEXT_IDX as usize + 1;

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
    let first_committee_owner = {
        *get_instruction_account_with_idx(
            transaction_context,
            COMMITTEES_START as u16,
        )?
        .borrow()
        .owner()
    };

    #[cfg(test)]
    let parent_program_id = &first_committee_owner;

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

        {
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

        if opts.request_undelegation {
            // If the account is scheduled to be undelegated then we need to lock it
            // immediately in order to prevent the following actions:
            // - writes to the account
            // - scheduling further commits for this account
            //
            // Setting the owner will prevent both, since in both cases the _actual_
            // owner program needs to sign for the account which is not possible at
            // that point
            // NOTE: this owner change only takes effect if the transaction which
            // includes this instruction succeeds.
            set_account_owner_to_delegation_program(acc);
            ic_msg!(
                invoke_context,
                "ScheduleCommit: account {} owner set to delegation program",
                acc_pubkey
            );
        }
    }

    // Determine id and slot
    let commit_id = COMMIT_ID.fetch_add(1, Ordering::Relaxed);

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
    let commit_sent_transaction = scheduled_commit_sent(commit_id, blockhash);

    let commit_sent_sig = commit_sent_transaction.signatures[0];
    let scheduled_commit = ScheduledCommit {
        id: commit_id,
        slot: clock.slot,
        blockhash,
        accounts: pubkeys,
        payer: *payer_pubkey,
        owner: *parent_program_id,
        commit_sent_transaction,
        request_undelegation: opts.request_undelegation,
    };

    // NOTE: this is only protected by all the above checks however if the
    // instruction fails for other reasons detected afterward then the commit
    // stays scheduled
    let context_acc = get_instruction_account_with_idx(
        transaction_context,
        MAGIC_CONTEXT_IDX,
    )?;
    TransactionScheduler::schedule_commit(
        invoke_context,
        context_acc,
        scheduled_commit,
    )
    .map_err(|err| {
        ic_msg!(
            invoke_context,
            "ScheduleCommit ERR: failed to schedule commit: {}",
            err
        );
        InstructionError::GenericError
    })?;
    ic_msg!(invoke_context, "Scheduled commit with ID: {}", commit_id,);
    ic_msg!(
        invoke_context,
        "ScheduledCommitSent signature: {}",
        commit_sent_sig,
    );

    Ok(())
}

pub fn process_accept_scheduled_commits(
    signers: HashSet<Pubkey>,
    invoke_context: &mut InvokeContext,
) -> Result<(), InstructionError> {
    const VALIDATOR_AUTHORITY_IDX: u16 = 0;
    const MAGIC_CONTEXT_IDX: u16 = VALIDATOR_AUTHORITY_IDX + 1;

    let transaction_context = &invoke_context.transaction_context.clone();

    // 1. Read all scheduled commits from the `MagicContext` account
    //    We do this first so we can skip all checks in case there is nothing
    //    to be processed
    check_magic_context_id(invoke_context, MAGIC_CONTEXT_IDX)?;
    let magic_context_acc = get_instruction_account_with_idx(
        transaction_context,
        MAGIC_CONTEXT_IDX,
    )?;
    let mut magic_context =
        bincode::deserialize::<MagicContext>(magic_context_acc.borrow().data())
            .map_err(|err| {
                ic_msg!(
                    invoke_context,
                    "Failed to deserialize MagicContext: {}",
                    err
                );
                InstructionError::InvalidAccountData
            })?;
    if magic_context.scheduled_commits.is_empty() {
        ic_msg!(
            invoke_context,
            "AcceptScheduledCommits: no scheduled commits to accept"
        );
        // NOTE: we should have not been called if no commits are scheduled
        return Ok(());
    }

    // 2. Check that the validator authority (first account) is correct and signer
    let provided_validator_auth = get_instruction_pubkey_with_idx(
        transaction_context,
        VALIDATOR_AUTHORITY_IDX,
    )?;
    let validator_auth = validator_authority_id();
    if !provided_validator_auth.eq(&validator_auth) {
        ic_msg!(
             invoke_context,
             "AcceptScheduledCommits ERR: invalid validator authority {}, should be {}",
             provided_validator_auth,
             validator_auth
         );
        return Err(InstructionError::InvalidArgument);
    }
    if !signers.contains(&validator_auth) {
        ic_msg!(
            invoke_context,
            "AcceptScheduledCommits ERR: validator authority pubkey {} not in signers",
            validator_auth
        );
        return Err(InstructionError::MissingRequiredSignature);
    }

    // 3. Move scheduled commits (without copying)
    let scheduled_commits = magic_context.take_scheduled_commits();
    ic_msg!(
        invoke_context,
        "AcceptScheduledCommits: accepted {} scheduled commit(s)",
        scheduled_commits.len()
    );
    TransactionScheduler::default().accept_scheduled_commits(scheduled_commits);

    // 4. Serialize and store the updated `MagicContext` account
    // Zero fill account before updating data
    // NOTE: this may become expensive, but is a security measure and also prevents
    // accidentally interpreting old data when deserializing
    magic_context_acc
        .borrow_mut()
        .set_data_from_slice(&MagicContext::ZERO);

    magic_context_acc
        .borrow_mut()
        .serialize_data(&magic_context)
        .map_err(|err| {
            ic_msg!(
                invoke_context,
                "Failed to serialize MagicContext: {}",
                err
            );
            InstructionError::GenericError
        })?;

    Ok(())
}

fn check_magic_context_id(
    invoke_context: &InvokeContext,
    idx: u16,
) -> Result<(), InstructionError> {
    let provided_magic_context = get_instruction_pubkey_with_idx(
        invoke_context.transaction_context,
        idx,
    )?;
    if !provided_magic_context.eq(&MAGIC_CONTEXT_PUBKEY) {
        ic_msg!(
            invoke_context,
            "ERR: invalid magic context account {}",
            provided_magic_context
        );
        return Err(InstructionError::MissingAccount);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use assert_matches::assert_matches;
    use sleipnir_core::magic_program::MAGIC_CONTEXT_PUBKEY;
    use solana_sdk::{
        account::{
            create_account_shared_data_for_test, AccountSharedData,
            ReadableAccount,
        },
        clock,
        fee_calculator::DEFAULT_TARGET_LAMPORTS_PER_SIGNATURE,
        instruction::{AccountMeta, Instruction, InstructionError},
        pubkey::Pubkey,
        signature::Keypair,
        signer::{SeedDerivable, Signer},
        system_program,
        sysvar::SysvarId,
    };
    use test_tools_core::init_logger;

    use crate::{
        magic_context::MagicContext,
        schedule_transactions::transaction_scheduler::TransactionScheduler,
        sleipnir_instruction::{
            accept_scheduled_commits_instruction,
            schedule_commit_and_undelegate_instruction,
            schedule_commit_instruction, SleipnirInstruction,
        },
        test_utils::{ensure_funded_validator_authority, process_instruction},
        utils::DELEGATION_PROGRAM_ID,
        ScheduledCommit,
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

    fn prepare_transaction_with_single_committee(
        payer: &Keypair,
        program: Pubkey,
        committee: Pubkey,
    ) -> (
        HashMap<Pubkey, AccountSharedData>,
        Vec<(Pubkey, AccountSharedData)>,
    ) {
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
            // NOTE: the magic context is initialized with these properties at
            // validator startup
            // TODO(thlorenz): @@@@ not yet
            map.insert(
                MAGIC_CONTEXT_PUBKEY,
                AccountSharedData::new(
                    u64::MAX,
                    MagicContext::SIZE,
                    &crate::id(),
                ),
            );
            map.insert(committee, AccountSharedData::new(0, 0, &program));
            map
        };
        ensure_funded_validator_authority(&mut account_data);

        let transaction_accounts: Vec<(Pubkey, AccountSharedData)> = vec![(
            clock::Clock::id(),
            create_account_shared_data_for_test(&get_clock()),
        )];

        (account_data, transaction_accounts)
    }

    struct PreparedTransactionThreeCommittees {
        program: Pubkey,
        accounts_data: HashMap<Pubkey, AccountSharedData>,
        committee_uno: Pubkey,
        committee_dos: Pubkey,
        committee_tres: Pubkey,
        transaction_accounts: Vec<(Pubkey, AccountSharedData)>,
    }

    fn prepare_transaction_with_three_committees(
        payer: &Keypair,
        committees: Option<(Pubkey, Pubkey, Pubkey)>,
    ) -> PreparedTransactionThreeCommittees {
        let program = Pubkey::new_unique();
        let (committee_uno, committee_dos, committee_tres) = committees
            .unwrap_or((
                Pubkey::new_unique(),
                Pubkey::new_unique(),
                Pubkey::new_unique(),
            ));

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
                MAGIC_CONTEXT_PUBKEY,
                AccountSharedData::new(
                    u64::MAX,
                    MagicContext::SIZE,
                    &crate::id(),
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
            program,
            accounts_data,
            committee_uno,
            committee_dos,
            committee_tres,
            transaction_accounts,
        }
    }

    fn find_magic_context_account(
        accounts: &[AccountSharedData],
    ) -> Option<&AccountSharedData> {
        accounts.iter().find(|acc| {
            acc.owner() == &crate::id() && acc.lamports() == u64::MAX
        })
    }

    fn assert_non_accepted_commits<'a>(
        processed_scheduled: &'a [AccountSharedData],
        payer: &Pubkey,
        expected_non_accepted_commits: usize,
    ) -> &'a AccountSharedData {
        let magic_context_acc = find_magic_context_account(processed_scheduled)
            .expect("magic context account not found");
        let magic_context =
            bincode::deserialize::<MagicContext>(magic_context_acc.data())
                .unwrap();

        let accepted_scheduled_commits = TransactionScheduler::default()
            .get_scheduled_commits_by_payer(payer);
        assert_eq!(
            magic_context.scheduled_commits.len(),
            expected_non_accepted_commits
        );
        assert_eq!(accepted_scheduled_commits.len(), 0);

        magic_context_acc
    }

    fn assert_accepted_commits(
        processed_accepted: &[AccountSharedData],
        payer: &Pubkey,
        expected_scheduled_commits: usize,
    ) -> Vec<ScheduledCommit> {
        let magic_context_acc = find_magic_context_account(processed_accepted)
            .expect("magic context account not found");
        let magic_context =
            bincode::deserialize::<MagicContext>(magic_context_acc.data())
                .unwrap();

        let scheduled_commits = TransactionScheduler::default()
            .get_scheduled_commits_by_payer(payer);

        assert_eq!(magic_context.scheduled_commits.len(), 0);
        assert_eq!(scheduled_commits.len(), expected_scheduled_commits);

        scheduled_commits
    }

    fn extend_transaction_accounts_from_ix(
        ix: &Instruction,
        account_data: &mut HashMap<Pubkey, AccountSharedData>,
        transaction_accounts: &mut Vec<(Pubkey, AccountSharedData)>,
    ) {
        transaction_accounts.extend(ix.accounts.iter().flat_map(|acc| {
            account_data
                .remove(&acc.pubkey)
                .map(|shared_data| (acc.pubkey, shared_data))
        }));
    }

    fn extend_transaction_accounts_from_ix_adding_magic_context(
        ix: &Instruction,
        magic_context_acc: &AccountSharedData,
        account_data: &mut HashMap<Pubkey, AccountSharedData>,
        transaction_accounts: &mut Vec<(Pubkey, AccountSharedData)>,
    ) {
        transaction_accounts.extend(ix.accounts.iter().flat_map(|acc| {
            account_data.remove(&acc.pubkey).map(|shared_data| {
                let shared_data = if acc.pubkey == MAGIC_CONTEXT_PUBKEY {
                    magic_context_acc.clone()
                } else {
                    shared_data
                };
                (acc.pubkey, shared_data)
            })
        }));
    }

    fn assert_first_commit(
        scheduled_commits: &[ScheduledCommit],
        payer: &Pubkey,
        owner: &Pubkey,
        committees: &[Pubkey],
        expected_request_undelegation: bool,
    ) {
        let commit = &scheduled_commits[0];
        let test_clock = get_clock();
        assert_matches!(
            commit,
            ScheduledCommit {
                id,
                slot,
                accounts,
                payer: p,
                owner: o,
                blockhash: _,
                commit_sent_transaction,
                request_undelegation,
            } => {
                assert!(id >= &0);
                assert_eq!(slot, &test_clock.slot);
                assert_eq!(p, payer);
                assert_eq!(o, owner);
                assert_eq!(accounts, committees);
                let instruction = SleipnirInstruction::ScheduledCommitSent(*id);
                assert_eq!(commit_sent_transaction.data(0), instruction.try_to_vec().unwrap());
                assert_eq!(*request_undelegation, expected_request_undelegation);
            }
        );
    }

    #[test]
    fn test_schedule_commit_single_account_success() {
        init_logger!();
        let payer =
            Keypair::from_seed(b"schedule_commit_single_account_success")
                .unwrap();
        let program = Pubkey::new_unique();
        let committee = Pubkey::new_unique();

        // 1. We run the transaction that registers the intent to schedule a commit
        let (processed_scheduled, magic_context_acc) = {
            let (mut account_data, mut transaction_accounts) =
                prepare_transaction_with_single_committee(
                    &payer, program, committee,
                );

            let ix =
                schedule_commit_instruction(&payer.pubkey(), vec![committee]);

            extend_transaction_accounts_from_ix(
                &ix,
                &mut account_data,
                &mut transaction_accounts,
            );

            let processed_scheduled = process_instruction(
                ix.data.as_slice(),
                transaction_accounts.clone(),
                ix.accounts,
                Ok(()),
            );

            // At this point the intent to commit was added to the magic context account,
            // but not yet accepted
            let magic_context_acc = assert_non_accepted_commits(
                &processed_scheduled,
                &payer.pubkey(),
                1,
            );

            (processed_scheduled.clone(), magic_context_acc.clone())
        };

        // 2. We run the transaction that accepts the scheduled commit
        {
            let (mut account_data, mut transaction_accounts) =
                prepare_transaction_with_single_committee(
                    &payer, program, committee,
                );

            let ix = accept_scheduled_commits_instruction();
            extend_transaction_accounts_from_ix_adding_magic_context(
                &ix,
                &magic_context_acc,
                &mut account_data,
                &mut transaction_accounts,
            );

            let processed_accepted = process_instruction(
                ix.data.as_slice(),
                transaction_accounts,
                ix.accounts,
                Ok(()),
            );

            // At this point the intended commits were accepted and moved to the global
            let scheduled_commits = assert_accepted_commits(
                &processed_accepted,
                &payer.pubkey(),
                1,
            );

            assert_first_commit(
                &scheduled_commits,
                &payer.pubkey(),
                &program,
                &[committee],
                false,
            );
        }
        let committed_account = processed_scheduled.last().unwrap();
        assert_eq!(*committed_account.owner(), program);
    }

    #[test]
    fn test_schedule_commit_single_account_and_request_undelegate_success() {
        init_logger!();
        let payer =
            Keypair::from_seed(b"single_account_with_undelegate_success")
                .unwrap();
        let program = Pubkey::new_unique();
        let committee = Pubkey::new_unique();

        // 1. We run the transaction that registers the intent to schedule a commit
        let (processed_scheduled, magic_context_acc) = {
            let (mut account_data, mut transaction_accounts) =
                prepare_transaction_with_single_committee(
                    &payer, program, committee,
                );

            let ix = schedule_commit_and_undelegate_instruction(
                &payer.pubkey(),
                vec![committee],
            );

            extend_transaction_accounts_from_ix(
                &ix,
                &mut account_data,
                &mut transaction_accounts,
            );

            let processed_scheduled = process_instruction(
                ix.data.as_slice(),
                transaction_accounts.clone(),
                ix.accounts,
                Ok(()),
            );

            // At this point the intent to commit was added to the magic context account,
            // but not yet accepted
            let magic_context_acc = assert_non_accepted_commits(
                &processed_scheduled,
                &payer.pubkey(),
                1,
            );

            (processed_scheduled.clone(), magic_context_acc.clone())
        };

        // 2. We run the transaction that accepts the scheduled commit
        {
            let (mut account_data, mut transaction_accounts) =
                prepare_transaction_with_single_committee(
                    &payer, program, committee,
                );

            let ix = accept_scheduled_commits_instruction();
            extend_transaction_accounts_from_ix_adding_magic_context(
                &ix,
                &magic_context_acc,
                &mut account_data,
                &mut transaction_accounts,
            );

            let processed_accepted = process_instruction(
                ix.data.as_slice(),
                transaction_accounts,
                ix.accounts,
                Ok(()),
            );

            // At this point the intended commits were accepted and moved to the global
            let scheduled_commits = assert_accepted_commits(
                &processed_accepted,
                &payer.pubkey(),
                1,
            );

            assert_first_commit(
                &scheduled_commits,
                &payer.pubkey(),
                &program,
                &[committee],
                true,
            );
        }
        let committed_account = processed_scheduled.last().unwrap();
        assert_eq!(*committed_account.owner(), DELEGATION_PROGRAM_ID);
    }

    #[test]
    fn test_schedule_commit_three_accounts_success() {
        init_logger!();

        let payer =
            Keypair::from_seed(b"schedule_commit_three_accounts_success")
                .unwrap();

        // 1. We run the transaction that registers the intent to schedule a commit
        let (
            mut processed_scheduled,
            magic_context_acc,
            program,
            committee_uno,
            committee_dos,
            committee_tres,
        ) = {
            let PreparedTransactionThreeCommittees {
                mut accounts_data,
                committee_uno,
                committee_dos,
                committee_tres,
                mut transaction_accounts,
                program,
                ..
            } = prepare_transaction_with_three_committees(&payer, None);

            let ix = schedule_commit_instruction(
                &payer.pubkey(),
                vec![committee_uno, committee_dos, committee_tres],
            );
            extend_transaction_accounts_from_ix(
                &ix,
                &mut accounts_data,
                &mut transaction_accounts,
            );

            let processed_scheduled = process_instruction(
                ix.data.as_slice(),
                transaction_accounts,
                ix.accounts,
                Ok(()),
            );

            // At this point the intent to commit was added to the magic context account,
            // but not yet accepted
            let magic_context_acc = assert_non_accepted_commits(
                &processed_scheduled,
                &payer.pubkey(),
                1,
            );

            (
                processed_scheduled.clone(),
                magic_context_acc.clone(),
                program,
                committee_uno,
                committee_dos,
                committee_tres,
            )
        };

        // 2. We run the transaction that accepts the scheduled commit
        {
            let PreparedTransactionThreeCommittees {
                mut accounts_data,
                mut transaction_accounts,
                ..
            } = prepare_transaction_with_three_committees(
                &payer,
                Some((committee_uno, committee_dos, committee_tres)),
            );

            let ix = accept_scheduled_commits_instruction();
            extend_transaction_accounts_from_ix_adding_magic_context(
                &ix,
                &magic_context_acc,
                &mut accounts_data,
                &mut transaction_accounts,
            );

            let processed_accepted = process_instruction(
                ix.data.as_slice(),
                transaction_accounts,
                ix.accounts,
                Ok(()),
            );

            // At this point the intended commits were accepted and moved to the global
            let scheduled_commits = assert_accepted_commits(
                &processed_accepted,
                &payer.pubkey(),
                1,
            );

            assert_first_commit(
                &scheduled_commits,
                &payer.pubkey(),
                &program,
                &[committee_uno, committee_dos, committee_tres],
                false,
            );
            for _ in &[committee_uno, committee_dos, committee_tres] {
                let committed_account = processed_scheduled.pop().unwrap();
                assert_eq!(*committed_account.owner(), program);
            }
        }
    }

    #[test]
    fn test_schedule_commit_three_accounts_and_request_undelegate_success() {
        let payer = Keypair::from_seed(
            b"three_accounts_and_request_undelegate_success",
        )
        .unwrap();

        // 1. We run the transaction that registers the intent to schedule a commit
        let (
            mut processed_scheduled,
            magic_context_acc,
            program,
            committee_uno,
            committee_dos,
            committee_tres,
        ) = {
            let PreparedTransactionThreeCommittees {
                mut accounts_data,
                committee_uno,
                committee_dos,
                committee_tres,
                mut transaction_accounts,
                program,
                ..
            } = prepare_transaction_with_three_committees(&payer, None);

            let ix = schedule_commit_and_undelegate_instruction(
                &payer.pubkey(),
                vec![committee_uno, committee_dos, committee_tres],
            );

            extend_transaction_accounts_from_ix(
                &ix,
                &mut accounts_data,
                &mut transaction_accounts,
            );

            let processed_scheduled = process_instruction(
                ix.data.as_slice(),
                transaction_accounts,
                ix.accounts,
                Ok(()),
            );

            // At this point the intent to commit was added to the magic context account,
            // but not yet accepted
            let magic_context_acc = assert_non_accepted_commits(
                &processed_scheduled,
                &payer.pubkey(),
                1,
            );

            (
                processed_scheduled.clone(),
                magic_context_acc.clone(),
                program,
                committee_uno,
                committee_dos,
                committee_tres,
            )
        };

        // 2. We run the transaction that accepts the scheduled commit
        {
            let PreparedTransactionThreeCommittees {
                mut accounts_data,
                mut transaction_accounts,
                ..
            } = prepare_transaction_with_three_committees(
                &payer,
                Some((committee_uno, committee_dos, committee_tres)),
            );

            let ix = accept_scheduled_commits_instruction();
            extend_transaction_accounts_from_ix_adding_magic_context(
                &ix,
                &magic_context_acc,
                &mut accounts_data,
                &mut transaction_accounts,
            );

            let processed_accepted = process_instruction(
                ix.data.as_slice(),
                transaction_accounts,
                ix.accounts,
                Ok(()),
            );

            // At this point the intended commits were accepted and moved to the global
            let scheduled_commits = assert_accepted_commits(
                &processed_accepted,
                &payer.pubkey(),
                1,
            );

            assert_first_commit(
                &scheduled_commits,
                &payer.pubkey(),
                &program,
                &[committee_uno, committee_dos, committee_tres],
                true,
            );
            for _ in &[committee_uno, committee_dos, committee_tres] {
                let committed_account = processed_scheduled.pop().unwrap();
                assert_eq!(*committed_account.owner(), DELEGATION_PROGRAM_ID);
            }
        }
    }

    // -----------------
    // Failure Cases
    // ----------------
    fn get_account_metas_for_schedule_commit(
        payer: &Pubkey,
        pdas: Vec<Pubkey>,
    ) -> Vec<AccountMeta> {
        let mut account_metas = vec![
            AccountMeta::new(*payer, true),
            AccountMeta::new(MAGIC_CONTEXT_PUBKEY, false),
        ];
        for pubkey in &pdas {
            account_metas.push(AccountMeta::new_readonly(*pubkey, true));
        }
        account_metas
    }

    fn account_metas_last_committee_not_signer(
        payer: &Pubkey,
        pdas: Vec<Pubkey>,
    ) -> Vec<AccountMeta> {
        let mut account_metas =
            get_account_metas_for_schedule_commit(payer, pdas);
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

    #[test]
    fn test_schedule_commit_no_pdas_provided_to_ix() {
        init_logger!();

        let payer =
            Keypair::from_seed(b"schedule_commit_no_pdas_provided_to_ix")
                .unwrap();

        let PreparedTransactionThreeCommittees {
            mut accounts_data,
            mut transaction_accounts,
            ..
        } = prepare_transaction_with_three_committees(&payer, None);

        let ix = instruction_from_account_metas(
            get_account_metas_for_schedule_commit(&payer.pubkey(), vec![]),
        );
        extend_transaction_accounts_from_ix(
            &ix,
            &mut accounts_data,
            &mut transaction_accounts,
        );

        process_instruction(
            ix.data.as_slice(),
            transaction_accounts,
            ix.accounts,
            Err(InstructionError::NotEnoughAccountKeys),
        );
    }

    #[test]
    fn test_schedule_commit_three_accounts_second_not_owned_by_program() {
        init_logger!();

        let payer =
            Keypair::from_seed(b"three_accounts_last_not_owned_by_program")
                .unwrap();

        let PreparedTransactionThreeCommittees {
            mut accounts_data,
            committee_uno,
            committee_dos,
            committee_tres,
            mut transaction_accounts,
            ..
        } = prepare_transaction_with_three_committees(&payer, None);

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
        extend_transaction_accounts_from_ix(
            &ix,
            &mut accounts_data,
            &mut transaction_accounts,
        );

        process_instruction(
            ix.data.as_slice(),
            transaction_accounts,
            ix.accounts,
            Err(InstructionError::InvalidAccountOwner),
        );
    }
}
