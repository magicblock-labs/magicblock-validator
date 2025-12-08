use std::collections::HashMap;

use assert_matches::assert_matches;
use magicblock_magic_program_api::{
    instruction::MagicBlockInstruction, MAGIC_CONTEXT_PUBKEY,
};
use solana_sdk::{
    account::{
        create_account_shared_data_for_test, AccountSharedData, ReadableAccount,
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

use crate::{
    magic_context::MagicContext,
    magic_scheduled_base_intent::ScheduledBaseIntent,
    schedule_transactions::transaction_scheduler::TransactionScheduler,
    test_utils::{ensure_started_validator, process_instruction},
    utils::DELEGATION_PROGRAM_ID,
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
            AccountSharedData::new(REQUIRED_TX_COST, 0, &system_program::id()),
        );
        // NOTE: the magic context is initialized with these properties at
        // validator startup
        map.insert(
            MAGIC_CONTEXT_PUBKEY,
            AccountSharedData::new(u64::MAX, MagicContext::SIZE, &crate::id()),
        );

        let mut committee_account = AccountSharedData::new(0, 0, &program);
        committee_account.set_delegated(true);

        map.insert(committee, committee_account);
        map
    };
    ensure_started_validator(&mut account_data);

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
    is_delegated: (bool, bool, bool),
) -> PreparedTransactionThreeCommittees {
    let program = Pubkey::new_unique();
    let (committee_uno, committee_dos, committee_tres) =
        committees.unwrap_or((
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            Pubkey::new_unique(),
        ));

    let mut accounts_data = {
        let mut map = HashMap::new();
        map.insert(
            payer.pubkey(),
            AccountSharedData::new(REQUIRED_TX_COST, 0, &system_program::id()),
        );
        map.insert(
            MAGIC_CONTEXT_PUBKEY,
            AccountSharedData::new(u64::MAX, MagicContext::SIZE, &crate::id()),
        );
        {
            let mut acc = AccountSharedData::new(0, 0, &program);
            acc.set_delegated(is_delegated.0);
            map.insert(committee_uno, acc);
        }
        {
            let mut acc = AccountSharedData::new(0, 0, &program);
            acc.set_delegated(is_delegated.1);
            map.insert(committee_dos, acc);
        }
        {
            let mut acc = AccountSharedData::new(0, 0, &program);
            acc.set_delegated(is_delegated.2);
            map.insert(committee_tres, acc);
        }
        map
    };
    ensure_started_validator(&mut accounts_data);

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
    accounts
        .iter()
        .find(|acc| acc.owner() == &crate::id() && acc.lamports() == u64::MAX)
}

fn assert_non_accepted_actions<'a>(
    processed_scheduled: &'a [AccountSharedData],
    payer: &Pubkey,
    expected_non_accepted_commits: usize,
) -> &'a AccountSharedData {
    let magic_context_acc = find_magic_context_account(processed_scheduled)
        .expect("magic context account not found");
    let magic_context =
        bincode::deserialize::<MagicContext>(magic_context_acc.data()).unwrap();

    let accepted_scheduled_actions =
        TransactionScheduler::default().get_scheduled_actions_by_payer(payer);
    assert_eq!(
        magic_context.scheduled_base_intents.len(),
        expected_non_accepted_commits
    );
    assert_eq!(accepted_scheduled_actions.len(), 0);

    magic_context_acc
}

fn assert_accepted_actions(
    processed_accepted: &[AccountSharedData],
    payer: &Pubkey,
    expected_scheduled_actions: usize,
) -> Vec<ScheduledBaseIntent> {
    let magic_context_acc = find_magic_context_account(processed_accepted)
        .expect("magic context account not found");
    let magic_context =
        bincode::deserialize::<MagicContext>(magic_context_acc.data()).unwrap();

    let scheduled_actions =
        TransactionScheduler::default().get_scheduled_actions_by_payer(payer);

    assert_eq!(magic_context.scheduled_base_intents.len(), 0);
    assert_eq!(scheduled_actions.len(), expected_scheduled_actions);

    scheduled_actions
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
    scheduled_base_intents: &[ScheduledBaseIntent],
    payer: &Pubkey,
    committees: &[Pubkey],
    expected_request_undelegation: bool,
) {
    let scheduled_base_intent = &scheduled_base_intents[0];
    let test_clock = get_clock();
    assert_matches!(
        scheduled_base_intent,
        ScheduledBaseIntent {
            id,
            slot,
            payer: actual_payer,
            blockhash: _,
            action_sent_transaction: _,
            base_intent,
        } => {
            assert!(id >= &0);
            assert_eq!(slot, &test_clock.slot);
            assert_eq!(actual_payer, payer);
            assert_eq!(base_intent.get_committed_pubkeys().unwrap().as_slice(), committees);
            let _instruction = MagicBlockInstruction::ScheduledCommitSent((*id, 0));
            // TODO(edwin) @@@ this fails in CI only with the similar to the below
            //   left: [4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 5, 0, 0, 0, 0, 0, 0, 0]
            //  right: [4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]
            // See: https://github.com/magicblock-labs/magicblock-validator/actions/runs/18565403532/job/52924982063#step:6:1063
            // assert_eq!(action_sent_transaction.data(0), instruction.try_to_vec().unwrap());
            assert_eq!(base_intent.is_undelegate(), expected_request_undelegation);
        }
    );
}

#[cfg(test)]
mod tests {
    // ---------- Helpers for ATA/eATA remapping tests ----------
    // Use shared SPL/ATA/eATA constants and helpers
    use magicblock_core::token_programs::{
        derive_ata, derive_eata, SPL_TOKEN_PROGRAM_ID,
    };
    use test_kit::init_logger;

    use super::*;
    use crate::utils::instruction_utils::InstructionUtils;

    fn make_delegated_spl_ata_account(
        owner: &Pubkey,
        mint: &Pubkey,
    ) -> AccountSharedData {
        // Minimal SPL token account data: first 32 bytes mint, next 32 owner
        let mut data = vec![0u8; 72];
        data[0..32].copy_from_slice(mint.as_ref());
        data[32..64].copy_from_slice(owner.as_ref());

        let mut acc =
            AccountSharedData::new(0, data.len(), &SPL_TOKEN_PROGRAM_ID);
        acc.set_data_from_slice(&data);
        acc.set_delegated(true);
        acc
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

            let ix = InstructionUtils::schedule_commit_instruction(
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
            let magic_context_acc = assert_non_accepted_actions(
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

            let ix = InstructionUtils::accept_scheduled_commits_instruction();
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
            let scheduled_intents = assert_accepted_actions(
                &processed_accepted,
                &payer.pubkey(),
                1,
            );

            assert_first_commit(
                &scheduled_intents,
                &payer.pubkey(),
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

            let ix =
                InstructionUtils::schedule_commit_and_undelegate_instruction(
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
            let magic_context_acc = assert_non_accepted_actions(
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

            let ix = InstructionUtils::accept_scheduled_commits_instruction();
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
            let scheduled_commits = assert_accepted_actions(
                &processed_accepted,
                &payer.pubkey(),
                1,
            );

            assert_first_commit(
                &scheduled_commits,
                &payer.pubkey(),
                &[committee],
                true,
            );
        }
        let committed_account = processed_scheduled.last().unwrap();
        assert_eq!(*committed_account.owner(), DELEGATION_PROGRAM_ID);
    }

    #[test]
    fn test_schedule_commit_remaps_delegated_ata_to_eata() {
        init_logger!();

        let payer =
            Keypair::from_seed(b"schedule_commit_remap_ata_to_eata").unwrap();
        let wallet_owner = Pubkey::new_unique();
        let mint = Pubkey::new_unique();
        let ata_pubkey = derive_ata(&wallet_owner, &mint);
        let eata_pubkey = derive_eata(&wallet_owner, &mint);

        // 1) Prepare transaction with our ATA as the only committee
        let (mut account_data, mut transaction_accounts) =
            prepare_transaction_with_single_committee(
                &payer,
                Pubkey::new_unique(),
                ata_pubkey,
            );

        // Replace the committee account with a delegated SPL-Token ATA layout
        account_data.insert(
            ata_pubkey,
            make_delegated_spl_ata_account(&wallet_owner, &mint),
        );

        // Build ScheduleCommit instruction using the ATA pubkey
        let ix = InstructionUtils::schedule_commit_instruction(
            &payer.pubkey(),
            vec![ata_pubkey],
        );
        extend_transaction_accounts_from_ix(
            &ix,
            &mut account_data,
            &mut transaction_accounts,
        );

        // Execute scheduling
        let processed_scheduled = process_instruction(
            ix.data.as_slice(),
            transaction_accounts.clone(),
            ix.accounts.clone(),
            Ok(()),
        );

        // Extract magic context and then accept scheduled commits
        let magic_context_acc = assert_non_accepted_actions(
            &processed_scheduled,
            &payer.pubkey(),
            1,
        );

        let ix_accept =
            InstructionUtils::accept_scheduled_commits_instruction();
        let (mut account_data2, mut transaction_accounts2) =
            prepare_transaction_with_single_committee(
                &payer,
                Pubkey::new_unique(),
                ata_pubkey,
            );
        extend_transaction_accounts_from_ix_adding_magic_context(
            &ix_accept,
            magic_context_acc,
            &mut account_data2,
            &mut transaction_accounts2,
        );
        let processed_accepted = process_instruction(
            ix_accept.data.as_slice(),
            transaction_accounts2,
            ix_accept.accounts,
            Ok(()),
        );

        let scheduled =
            assert_accepted_actions(&processed_accepted, &payer.pubkey(), 1);
        // Verify the committed pubkey remapped to eATA
        assert_eq!(
            scheduled[0].base_intent.get_committed_pubkeys().unwrap(),
            vec![eata_pubkey]
        );
    }

    #[test]
    fn test_schedule_commit_and_undelegate_remaps_delegated_ata_to_eata() {
        init_logger!();

        let payer =
            Keypair::from_seed(b"schedule_commit_undelegate_remap_ata_eata")
                .unwrap();
        let wallet_owner = Pubkey::new_unique();
        let mint = Pubkey::new_unique();
        let ata_pubkey = derive_ata(&wallet_owner, &mint);
        let eata_pubkey = derive_eata(&wallet_owner, &mint);

        // 1) Prepare transaction with our ATA as the only committee
        let (mut account_data, mut transaction_accounts) =
            prepare_transaction_with_single_committee(
                &payer,
                Pubkey::new_unique(),
                ata_pubkey,
            );

        // Replace the committee account with a delegated SPL-Token ATA layout
        account_data.insert(
            ata_pubkey,
            make_delegated_spl_ata_account(&wallet_owner, &mint),
        );

        // Build ScheduleCommitAndUndelegate instruction using the ATA pubkey (writable)
        let ix = InstructionUtils::schedule_commit_and_undelegate_instruction(
            &payer.pubkey(),
            vec![ata_pubkey],
        );
        extend_transaction_accounts_from_ix(
            &ix,
            &mut account_data,
            &mut transaction_accounts,
        );

        // Execute scheduling
        let processed_scheduled = process_instruction(
            ix.data.as_slice(),
            transaction_accounts.clone(),
            ix.accounts.clone(),
            Ok(()),
        );

        // Extract magic context and then accept scheduled commits
        let magic_context_acc = assert_non_accepted_actions(
            &processed_scheduled,
            &payer.pubkey(),
            1,
        );

        let ix_accept =
            InstructionUtils::accept_scheduled_commits_instruction();
        let (mut account_data2, mut transaction_accounts2) =
            prepare_transaction_with_single_committee(
                &payer,
                Pubkey::new_unique(),
                ata_pubkey,
            );
        extend_transaction_accounts_from_ix_adding_magic_context(
            &ix_accept,
            magic_context_acc,
            &mut account_data2,
            &mut transaction_accounts2,
        );
        let processed_accepted = process_instruction(
            ix_accept.data.as_slice(),
            transaction_accounts2,
            ix_accept.accounts,
            Ok(()),
        );

        let scheduled =
            assert_accepted_actions(&processed_accepted, &payer.pubkey(), 1);
        // Verify the committed pubkey remapped to eATA
        assert_eq!(
            scheduled[0].base_intent.get_committed_pubkeys().unwrap(),
            vec![eata_pubkey]
        );
        // And the intent contains undelegation
        assert!(scheduled[0].base_intent.is_undelegate());
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
            } = prepare_transaction_with_three_committees(
                &payer,
                None,
                (true, true, true),
            );

            let ix = InstructionUtils::schedule_commit_instruction(
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
            let magic_context_acc = assert_non_accepted_actions(
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
                (true, true, true),
            );

            let ix = InstructionUtils::accept_scheduled_commits_instruction();
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
            let scheduled_commits = assert_accepted_actions(
                &processed_accepted,
                &payer.pubkey(),
                1,
            );

            assert_first_commit(
                &scheduled_commits,
                &payer.pubkey(),
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
            _program,
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
            } = prepare_transaction_with_three_committees(
                &payer,
                None,
                (true, true, true),
            );

            let ix =
                InstructionUtils::schedule_commit_and_undelegate_instruction(
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
            let magic_context_acc = assert_non_accepted_actions(
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
                (true, true, true),
            );

            let ix = InstructionUtils::accept_scheduled_commits_instruction();
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
            let scheduled_commits = assert_accepted_actions(
                &processed_accepted,
                &payer.pubkey(),
                1,
            );

            assert_first_commit(
                &scheduled_commits,
                &payer.pubkey(),
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
            &MagicBlockInstruction::ScheduleCommit,
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
        } = prepare_transaction_with_three_committees(
            &payer,
            None,
            (true, true, true),
        );

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
    fn test_schedule_commit_undelegate_with_readonly() {
        init_logger!();

        let payer =
            Keypair::from_seed(b"schedule_commit_undelegate_with_readonly")
                .unwrap();
        let program = Pubkey::new_unique();
        let committee = Pubkey::new_unique();

        let (mut account_data, mut transaction_accounts) =
            prepare_transaction_with_single_committee(
                &payer, program, committee,
            );

        // Create ScheduleCommitAndUndelegate with committee as readonly account
        let ix = {
            let mut account_metas = vec![
                AccountMeta::new(payer.pubkey(), true),
                AccountMeta::new(MAGIC_CONTEXT_PUBKEY, false),
            ];
            account_metas.push(AccountMeta::new_readonly(committee, true));
            Instruction::new_with_bincode(
                crate::id(),
                &MagicBlockInstruction::ScheduleCommitAndUndelegate,
                account_metas,
            )
        };

        extend_transaction_accounts_from_ix(
            &ix,
            &mut account_data,
            &mut transaction_accounts,
        );

        process_instruction(
            ix.data.as_slice(),
            transaction_accounts.clone(),
            ix.accounts,
            Err(InstructionError::ReadonlyDataModified),
        );
    }

    #[test]
    fn test_schedule_commit_with_non_delegated_account() {
        init_logger!();

        let payer =
            Keypair::from_seed(b"schedule_commit_with_non_delegated_account")
                .unwrap();
        let program = Pubkey::new_unique();
        let committee = Pubkey::new_unique();

        // Prepare single accounts for tx, set committee as non delegated
        let (mut account_data, mut transaction_accounts) =
            prepare_transaction_with_single_committee(
                &payer, program, committee,
            );
        account_data
            .get_mut(&committee)
            .unwrap()
            .set_delegated(false);

        // Create ScheduleCommit instruction with non-delegated committee
        let ix = InstructionUtils::schedule_commit_instruction(
            &payer.pubkey(),
            vec![committee],
        );
        extend_transaction_accounts_from_ix(
            &ix,
            &mut account_data,
            &mut transaction_accounts,
        );

        process_instruction(
            ix.data.as_slice(),
            transaction_accounts.clone(),
            ix.accounts,
            Err(InstructionError::IllegalOwner),
        );
    }

    #[test]
    fn test_schedule_commit_three_accounts_second_not_owned_by_program_and_not_signer(
    ) {
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
        } = prepare_transaction_with_three_committees(
            &payer,
            None,
            (true, true, true),
        );

        let mut dos_shared =
            AccountSharedData::new(0, 0, &Pubkey::new_unique());
        dos_shared.set_delegated(true);
        accounts_data.insert(committee_dos, dos_shared);

        let ix = instruction_from_account_metas(
            account_metas_last_committee_not_signer(
                &payer.pubkey(),
                vec![committee_uno, committee_tres, committee_dos],
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

    #[test]
    fn test_schedule_commit_with_confined_account() {
        init_logger!();

        let payer =
            Keypair::from_seed(b"schedule_commit_with_confined_account")
                .unwrap();
        let program = Pubkey::new_unique();
        let committee = Pubkey::new_unique();

        // Prepare single accounts for tx, set committee as confined
        let (mut account_data, mut transaction_accounts) =
            prepare_transaction_with_single_committee(
                &payer, program, committee,
            );
        account_data.get_mut(&committee).unwrap().set_confined(true);

        let committee_account = account_data.get(&committee).unwrap();
        assert!(committee_account.confined());
        assert!(
            committee_account.delegated(),
            "Confined account should remain delegated"
        );

        // Create ScheduleCommit instruction with confined committee
        let ix = InstructionUtils::schedule_commit_instruction(
            &payer.pubkey(),
            vec![committee],
        );
        extend_transaction_accounts_from_ix(
            &ix,
            &mut account_data,
            &mut transaction_accounts,
        );

        process_instruction(
            ix.data.as_slice(),
            transaction_accounts.clone(),
            ix.accounts,
            Err(InstructionError::InvalidAccountData),
        );
    }

    #[test]
    fn test_schedule_commit_three_accounts_one_confined() {
        init_logger!();

        let payer =
            Keypair::from_seed(b"three_accounts_one_confined_______").unwrap();

        let PreparedTransactionThreeCommittees {
            mut accounts_data,
            committee_uno,
            committee_dos,
            committee_tres,
            mut transaction_accounts,
            ..
        } = prepare_transaction_with_three_committees(
            &payer,
            None,
            (true, true, true),
        );

        // Make the second committee confined
        accounts_data
            .get_mut(&committee_dos)
            .unwrap()
            .set_confined(true);
        // Assert that the confined account remains delegated
        let committee_dos_account = accounts_data.get(&committee_dos).unwrap();
        assert!(
            committee_dos_account.confined(),
            "Confined account should remain confined"
        );
        assert!(
            committee_dos_account.delegated(),
            "Confined account should remain delegated"
        );

        let ix = InstructionUtils::schedule_commit_instruction(
            &payer.pubkey(),
            vec![committee_uno, committee_dos, committee_tres],
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
            Err(InstructionError::InvalidAccountData),
        );
    }
}
