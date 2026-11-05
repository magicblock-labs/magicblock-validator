//! Deterministic scheduling estimate. Base-chain state and account diffs are
//! unavailable here, so large accounts use buffers and v0 assumes full ALT
//! coverage. Passing this check does not guarantee successful settlement.
use std::collections::BTreeSet;

use delegation_program_interface::legacy::{
    args::{CallHandlerArgs, CommitFinalizeArgs},
    instruction_builder as dlp,
};
use solana_compute_budget::compute_budget_limits::ComputeBudgetLimits;
use solana_compute_budget_interface::ComputeBudgetInstruction;
use solana_hash::Hash;
use solana_instruction::{AccountMeta, Instruction};
use solana_message::{AddressLookupTableAccount, VersionedMessage, v0, v1};
use solana_pubkey::Pubkey;
use solana_signature::Signature;
use solana_transaction::versioned::VersionedTransaction;

use super::{BaseAction, CommitType, MagicIntentBundle, UndelegateType, types::CommittedAccount};

/// Above this size execution may use a diff; scheduling must assume a buffer.
pub const COMMIT_STATE_SIZE_THRESHOLD: usize = 256;
const TASK_CU: u32 = 120_000;
type EstimatedTransaction = Vec<(Instruction, u32)>;

/// Checks both settlement transactions without RPC, randomness, or signing.
/// `authority` is the effective validator identity, also available during replay.
pub fn is_intent_within_limits(intent: &MagicIntentBundle, authority: &Pubkey) -> bool {
    let (commits, follow_up) = estimate(intent, authority);
    fits(commits, authority) && fits(follow_up, authority)
}

fn estimate(
    intent: &MagicIntentBundle,
    authority: &Pubkey,
) -> (EstimatedTransaction, EstimatedTransaction) {
    let mut commits: Vec<_> =
        intent.standalone_actions.iter().map(|a| action(a, authority)).collect();
    let mut follow_up = Vec::new();
    let mut undelegations = Vec::new();
    for (commit, undelegate, combined) in [
        (intent.commit.as_ref(), None, false),
        (intent.commit_finalize.as_ref(), None, true),
        (
            intent.commit_and_undelegate.as_ref().map(|c| &c.commit_action),
            intent.commit_and_undelegate.as_ref().map(|c| &c.undelegate_action),
            false,
        ),
        (
            intent.commit_finalize_and_undelegate.as_ref().map(|c| &c.commit_action),
            intent.commit_finalize_and_undelegate.as_ref().map(|c| &c.undelegate_action),
            true,
        ),
    ] {
        let Some(commit) = commit else { continue };
        commits.extend(
            commit
                .get_committed_accounts()
                .iter()
                .map(|a| (commit_instruction(a, authority), TASK_CU)),
        );
        if let CommitType::WithBaseActions { base_actions, .. } = commit {
            let stage = if combined { &mut commits } else { &mut follow_up };
            stage.extend(base_actions.iter().map(|a| action(a, authority)));
        }
        if let Some(undelegate) = undelegate {
            for account in commit.get_committed_accounts() {
                undelegations.push((follow_up.len(), account));
                follow_up.push((
                    dlp::undelegate_with_request(
                        *authority,
                        account.pubkey,
                        account.account.owner,
                        Pubkey::default(),
                    ),
                    TASK_CU,
                ));
            }
            if let UndelegateType::WithBaseActions(actions) = undelegate {
                follow_up.extend(actions.iter().map(|a| action(a, authority)));
            }
        }
    }
    // Assign unknown rent payers after collecting every actual instruction key.
    // Each payer is distinct and cannot alias an authority, account, or program.
    let mut used: BTreeSet<_> = commits
        .iter()
        .chain(&follow_up)
        .flat_map(|(ix, _)| {
            std::iter::once(ix.program_id).chain(ix.accounts.iter().map(|a| a.pubkey))
        })
        .collect();
    used.insert(*authority);
    let mut counter = 0u64;
    for (index, account) in undelegations {
        let payer = loop {
            let mut bytes = [0u8; 32];
            bytes[..8].copy_from_slice(&counter.to_le_bytes());
            counter += 1;
            let key = Pubkey::new_from_array(bytes);
            if used.insert(key) {
                break key;
            }
        };
        follow_up[index].0 =
            dlp::undelegate_with_request(*authority, account.pubkey, account.account.owner, payer);
    }
    (commits, follow_up)
}

fn commit_instruction(account: &CommittedAccount, authority: &Pubkey) -> Instruction {
    let mut args = CommitFinalizeArgs {
        commit_id: account.commit_id,
        lamports: account.account.lamports,
        data_is_diff: false.into(),
        allow_undelegation: false.into(),
        bumps: Default::default(),
        reserved_padding: Default::default(),
    };
    if account.account.data.len() > COMMIT_STATE_SIZE_THRESHOLD {
        let buffer = committor_interface::pdas::buffer_pda(
            authority,
            &account.pubkey,
            &account.commit_id.to_le_bytes(),
        )
        .0;
        dlp::commit_finalize_from_buffer(*authority, account.pubkey, buffer, &mut args).0
    } else {
        dlp::commit_finalize(*authority, account.pubkey, &mut args, &account.account.data).0
    }
}

fn action(action: &BaseAction, authority: &Pubkey) -> (Instruction, u32) {
    let accounts = action
        .account_metas_per_program
        .iter()
        .map(|a| AccountMeta {
            pubkey: a.pubkey,
            is_writable: a.is_writable,
            is_signer: false,
        })
        .collect();
    let args = CallHandlerArgs {
        data: action.data_per_program.data.clone(),
        escrow_index: action.data_per_program.escrow_index,
    };
    #[allow(deprecated)]
    let ix = match action.source_program {
        Some(source) => dlp::call_handler_v2(
            *authority,
            action.destination_program,
            source,
            action.escrow_authority,
            accounts,
            args,
        ),
        None => dlp::call_handler(
            *authority,
            action.destination_program,
            action.escrow_authority,
            accounts,
            args,
        ),
    };
    (ix, action.compute_units)
}

fn fits(tasks: EstimatedTransaction, authority: &Pubkey) -> bool {
    let compute_units = tasks.iter().fold(1_000u32, |total, (_, cu)| total.saturating_add(*cu));
    if compute_units > 1_400_000 {
        return false;
    }
    let mut instructions: Vec<_> = tasks.into_iter().map(|(ix, _)| ix).collect();
    instructions.push(Instruction {
        program_id: Pubkey::from_str_const("noopb9bkMVfRPU8AsbpTUg8AQkHtKwMYZiFUjNRtMmV"),
        accounts: vec![],
        data: 0u64.to_le_bytes().to_vec(),
    });
    if instructions.iter().any(|ix| ix.data.len() > u16::MAX as usize) {
        return false;
    }
    let config = v1::TransactionConfig::empty()
        .with_priority_fee(0)
        .with_compute_unit_limit(compute_units)
        .with_loaded_accounts_data_size_limit(
            ComputeBudgetLimits::default().loaded_accounts_bytes.get(),
        );
    if let Ok(message) =
        v1::Message::try_compile_with_config(authority, &instructions, Hash::default(), config)
        && message.validate().is_ok()
        && wire_size(VersionedMessage::V1(message)) <= v1::MAX_TRANSACTION_SIZE
    {
        return true;
    }
    instructions.splice(
        0..0,
        [
            ComputeBudgetInstruction::set_compute_unit_limit(compute_units),
            ComputeBudgetInstruction::set_compute_unit_price(0),
        ],
    );
    let keys: BTreeSet<_> = instructions
        .iter()
        .flat_map(|ix| ix.accounts.iter().map(|a| a.pubkey))
        .collect();
    let keys: Vec<_> = keys.into_iter().collect();
    let tables: Vec<_> = keys
        .chunks(256)
        .enumerate()
        .map(|(i, addresses)| {
            let mut bytes = [0; 32];
            bytes[..8].copy_from_slice(&(i as u64).to_le_bytes());
            AddressLookupTableAccount {
                key: Pubkey::new_from_array(bytes),
                addresses: addresses.to_vec(),
            }
        })
        .collect();
    v0::Message::try_compile(authority, &instructions, &tables, Hash::default()).is_ok_and(
        |message| wire_size(VersionedMessage::V0(message)) <= solana_packet::PACKET_DATA_SIZE,
    )
}

fn wire_size(message: VersionedMessage) -> usize {
    let transaction = VersionedTransaction {
        signatures: vec![Signature::default(); message.header().num_required_signatures as usize],
        message,
    };
    wincode::serialized_size(&transaction).map_or(usize::MAX, |size| size as usize)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::intent::{CommitAndUndelegate, ProgramArgs};
    use solana_account::Account;
    fn make_committed_account(data_len: usize) -> CommittedAccount {
        CommittedAccount {
            commit_id: 1,
            pubkey: Pubkey::new_unique(),
            account: Account {
                lamports: 1_000,
                data: vec![0; data_len],
                owner: Pubkey::new_unique(),
                executable: false,
                rent_epoch: 0,
            },
            remote_slot: 0,
        }
    }

    fn make_base_action(data_len: usize) -> BaseAction {
        BaseAction {
            id: 0,
            compute_units: 10_000,
            destination_program: Pubkey::new_unique(),
            source_program: None,
            escrow_authority: Pubkey::new_unique(),
            data_per_program: ProgramArgs {
                escrow_index: 0,
                data: vec![0; data_len],
            },
            account_metas_per_program: vec![],
            callback: None,
        }
    }

    #[test]
    fn settlement_estimates_are_deterministic_and_bounded() {
        let authority = Pubkey::new_unique();
        for data_len in [40, 10_240] {
            for combined in [false, true] {
                let commit = CommitAndUndelegate {
                    commit_action: CommitType::WithBaseActions {
                        committed_accounts: vec![make_committed_account(data_len)],
                        base_actions: vec![make_base_action(0)],
                    },
                    undelegate_action: UndelegateType::Standalone,
                };
                let mut intent = MagicIntentBundle::default();
                if combined {
                    intent.commit_finalize_and_undelegate = Some(commit);
                } else {
                    intent.commit_and_undelegate = Some(commit);
                }
                let (commits, follow_up) = estimate(&intent, &authority);
                assert_eq!(commits.len(), 1 + usize::from(combined));
                assert_eq!(follow_up.len(), 1 + usize::from(!combined));
                assert_eq!((commits, follow_up), estimate(&intent, &authority));
                assert!(is_intent_within_limits(&intent, &authority));
            }
        }
        for (data_len, cu, expected) in
            [(2_000, 10_000, true), (5_000, 10_000, false), (0, 1_400_000, false)]
        {
            let mut action = make_base_action(data_len);
            action.compute_units = cu;
            let intent = MagicIntentBundle {
                standalone_actions: vec![action],
                ..Default::default()
            };
            assert_eq!(is_intent_within_limits(&intent, &authority), expected);
        }
    }

    #[test]
    fn unknown_rent_payers_preserve_wire_boundary() {
        let authority = Pubkey::new_unique();
        let mut accounts = vec![make_committed_account(10), make_committed_account(10)];
        // Force collisions with the counter-derived placeholder candidates.
        accounts[0].pubkey = Pubkey::default();
        let mut bytes = [0; 32];
        bytes[0] = 1;
        accounts[1].account.owner = Pubkey::new_from_array(bytes);
        let make_intent = |data_len| MagicIntentBundle {
            commit_finalize_and_undelegate: Some(CommitAndUndelegate {
                commit_action: CommitType::Standalone(accounts.clone()),
                undelegate_action: UndelegateType::WithBaseActions(vec![make_base_action(
                    data_len,
                )]),
            }),
            ..Default::default()
        };
        let (_, stage) = estimate(&make_intent(0), &authority);
        let payers = [stage[0].0.accounts[8].pubkey, stage[1].0.accounts[8].pubkey];
        assert_ne!(payers[0], payers[1]);
        for payer in payers {
            assert!(accounts.iter().all(|a| payer != a.pubkey && payer != a.account.owner));
        }
        // Find the accepted payload boundary without hard-coding DLP wire sizes.
        let boundary = (0..=v1::MAX_TRANSACTION_SIZE)
            .rev()
            .find(|size| is_intent_within_limits(&make_intent(*size), &authority))
            .unwrap();
        assert!(!is_intent_within_limits(
            &make_intent(boundary + 1),
            &authority
        ));
        assert!(is_intent_within_limits(&make_intent(boundary), &authority));
    }
}
