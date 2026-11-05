//! Deterministic scheduling estimate. Base-chain state and account diffs are
//! unavailable here, so large accounts use buffers and v0 assumes full ALT
//! coverage. Passing this check does not guarantee successful settlement.
use std::{collections::BTreeSet, fmt};

use delegation_program_interface::legacy::{
    args::{CallHandlerArgs, CommitFinalizeArgs},
    instruction_builder as dlp,
};
use solana_compute_budget::compute_budget_limits::ComputeBudgetLimits;
use solana_compute_budget_interface::ComputeBudgetInstruction;
use solana_hash::Hash;
use solana_instruction::{AccountMeta, Instruction};
use solana_message::{AddressLookupTableAccount, CompileError, VersionedMessage, v0, v1};
use solana_pubkey::Pubkey;
use solana_signature::Signature;
use solana_transaction::versioned::VersionedTransaction;

use super::{BaseAction, CommitType, MagicIntentBundle, UndelegateType, types::CommittedAccount};

/// Above this size execution may use a diff; scheduling must assume a buffer.
pub const COMMIT_STATE_SIZE_THRESHOLD: usize = 256;
const TASK_CU: u32 = 120_000;
const MAX_INTENT_COMPUTE_UNITS: u32 = 1_400_000;
pub const MAX_BASE_LAYER_TX_ACCOUNT_LOCKS: usize = 64;
type EstimatedTransaction = Vec<(Instruction, u32)>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IntentLimitStage {
    Commit,
    Finalize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FormatFitError {
    AccountLockLimitExceeded { actual: usize, limit: usize },
    InstructionDataLimitExceeded { actual: usize, limit: usize },
    WireSizeExceeded { actual: usize, limit: usize },
    MessageCannotBeCompiled,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IntentLimitError {
    ComputeBudgetExceeded {
        stage: IntentLimitStage,
        estimated: u32,
        limit: u32,
    },
    NoSupportedFormatFits {
        stage: IntentLimitStage,
        v1: FormatFitError,
        v0_with_alts: FormatFitError,
    },
    FitCheckFailed {
        stage: IntentLimitStage,
        reason: String,
    },
}

impl IntentLimitError {
    pub fn is_fit_limit(&self) -> bool {
        !matches!(self, Self::FitCheckFailed { .. })
    }
}

impl fmt::Display for IntentLimitStage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Commit => f.write_str("commit"),
            Self::Finalize => f.write_str("finalize"),
        }
    }
}

impl fmt::Display for FormatFitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AccountLockLimitExceeded { actual, limit } => {
                write!(f, "account locks {actual} exceed limit {limit}")
            }
            Self::InstructionDataLimitExceeded { actual, limit } => {
                write!(f, "instruction data {actual} bytes exceeds limit {limit}")
            }
            Self::WireSizeExceeded { actual, limit } => {
                write!(f, "wire size {actual} bytes exceeds limit {limit}")
            }
            Self::MessageCannotBeCompiled => f.write_str("message cannot be compiled"),
        }
    }
}

impl fmt::Display for IntentLimitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ComputeBudgetExceeded { stage, estimated, limit } => write!(
                f,
                "{stage} stage compute estimate {estimated} CU exceeds limit {limit} CU"
            ),
            Self::NoSupportedFormatFits { stage, v1, v0_with_alts } => write!(
                f,
                "{stage} stage fits neither transaction format (v1: {v1}; v0+ALT: {v0_with_alts})"
            ),
            Self::FitCheckFailed { stage, reason } => {
                write!(f, "{stage} stage fit check failed: {reason}")
            }
        }
    }
}

impl std::error::Error for IntentLimitError {}

/// Counts account keys represented by a compiled transaction, including
/// read-only addresses loaded through v0 lookup tables.
pub fn resolved_account_lock_count(message: &VersionedMessage) -> usize {
    message.static_account_keys().len()
        + message.address_table_lookups().map_or(0, |lookups| {
            lookups
                .iter()
                .map(|lookup| lookup.writable_indexes.len() + lookup.readonly_indexes.len())
                .sum()
        })
}

/// Checks both settlement transactions without RPC, randomness, or signing.
/// `authority` is the effective validator identity, also available during replay.
pub fn validate_intent_within_limits(
    intent: &MagicIntentBundle,
    authority: &Pubkey,
) -> Result<(), IntentLimitError> {
    let (commits, follow_up) = estimate(intent, authority);
    fits(commits, authority, IntentLimitStage::Commit)?;
    fits(follow_up, authority, IntentLimitStage::Finalize)
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

fn fits(
    tasks: EstimatedTransaction,
    authority: &Pubkey,
    stage: IntentLimitStage,
) -> Result<(), IntentLimitError> {
    let compute_units = tasks.iter().fold(1_000u32, |total, (_, cu)| total.saturating_add(*cu));
    if compute_units > MAX_INTENT_COMPUTE_UNITS {
        return Err(IntentLimitError::ComputeBudgetExceeded {
            stage,
            estimated: compute_units,
            limit: MAX_INTENT_COMPUTE_UNITS,
        });
    }
    let mut instructions: Vec<_> = tasks.into_iter().map(|(ix, _)| ix).collect();
    instructions.push(Instruction {
        program_id: Pubkey::from_str_const("noopb9bkMVfRPU8AsbpTUg8AQkHtKwMYZiFUjNRtMmV"),
        accounts: vec![],
        data: 0u64.to_le_bytes().to_vec(),
    });

    let v1_failure = match fits_v1(&instructions, authority, compute_units) {
        Ok(()) => return Ok(()),
        Err(CandidateFitError::DoesNotFit(error)) => error,
        Err(CandidateFitError::Internal(reason)) => {
            return Err(IntentLimitError::FitCheckFailed { stage, reason });
        }
    };
    let v0_with_alts_failure = match fits_v0_with_alts(&instructions, authority, compute_units) {
        Ok(()) => return Ok(()),
        Err(CandidateFitError::DoesNotFit(error)) => error,
        Err(CandidateFitError::Internal(reason)) => {
            return Err(IntentLimitError::FitCheckFailed { stage, reason });
        }
    };

    Err(IntentLimitError::NoSupportedFormatFits {
        stage,
        v1: v1_failure,
        v0_with_alts: v0_with_alts_failure,
    })
}

fn fits_v1(
    instructions: &[Instruction],
    authority: &Pubkey,
    compute_units: u32,
) -> Result<(), CandidateFitError> {
    if let Some(actual) = instructions.iter().map(|ix| ix.data.len()).max()
        && actual > u16::MAX as usize
    {
        return Err(CandidateFitError::DoesNotFit(
            FormatFitError::InstructionDataLimitExceeded { actual, limit: u16::MAX as usize },
        ));
    }
    let config = v1::TransactionConfig::empty()
        .with_priority_fee(0)
        .with_compute_unit_limit(compute_units)
        .with_loaded_accounts_data_size_limit(
            ComputeBudgetLimits::default().loaded_accounts_bytes.get(),
        );
    let message =
        v1::Message::try_compile_with_config(authority, instructions, Hash::default(), config)
            .map_err(candidate_compile_error)?;
    let lock_count = message.account_keys.len();
    if lock_count > MAX_BASE_LAYER_TX_ACCOUNT_LOCKS {
        return Err(CandidateFitError::DoesNotFit(
            FormatFitError::AccountLockLimitExceeded {
                actual: lock_count,
                limit: MAX_BASE_LAYER_TX_ACCOUNT_LOCKS,
            },
        ));
    }

    message.validate().map_err(|error| {
        CandidateFitError::Internal(format!("v1 message validation failed: {error:?}"))
    })?;
    let actual_size = wire_size(VersionedMessage::V1(message));
    if actual_size > v1::MAX_TRANSACTION_SIZE {
        return Err(CandidateFitError::DoesNotFit(
            FormatFitError::WireSizeExceeded {
                actual: actual_size,
                limit: v1::MAX_TRANSACTION_SIZE,
            },
        ));
    }

    Ok(())
}

fn fits_v0_with_alts(
    instructions: &[Instruction],
    authority: &Pubkey,
    compute_units: u32,
) -> Result<(), CandidateFitError> {
    let mut instructions = instructions.to_vec();
    instructions.splice(
        0..0,
        [
            ComputeBudgetInstruction::set_compute_unit_limit(compute_units),
            ComputeBudgetInstruction::set_compute_unit_price(0),
        ],
    );

    if let Some(actual) = instructions.iter().map(|ix| ix.data.len()).max()
        && actual > u16::MAX as usize
    {
        return Err(CandidateFitError::DoesNotFit(
            FormatFitError::InstructionDataLimitExceeded { actual, limit: u16::MAX as usize },
        ));
    }

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
    let message = v0::Message::try_compile(authority, &instructions, &tables, Hash::default())
        .map_err(candidate_compile_error)?;
    let message = VersionedMessage::V0(message);
    let lock_count = resolved_account_lock_count(&message);
    if lock_count > MAX_BASE_LAYER_TX_ACCOUNT_LOCKS {
        return Err(CandidateFitError::DoesNotFit(
            FormatFitError::AccountLockLimitExceeded {
                actual: lock_count,
                limit: MAX_BASE_LAYER_TX_ACCOUNT_LOCKS,
            },
        ));
    }

    let actual_size = wire_size(message);
    if actual_size > solana_packet::PACKET_DATA_SIZE {
        return Err(CandidateFitError::DoesNotFit(
            FormatFitError::WireSizeExceeded {
                actual: actual_size,
                limit: solana_packet::PACKET_DATA_SIZE,
            },
        ));
    }
    Ok(())
}

fn candidate_compile_error(error: CompileError) -> CandidateFitError {
    match error {
        CompileError::AccountIndexOverflow | CompileError::AddressTableLookupIndexOverflow => {
            CandidateFitError::DoesNotFit(FormatFitError::MessageCannotBeCompiled)
        }
        CompileError::UnknownInstructionKey(key) => {
            CandidateFitError::Internal(format!("unknown instruction key {key}"))
        }
    }
}

enum CandidateFitError {
    DoesNotFit(FormatFitError),
    Internal(String),
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
                assert!(validate_intent_within_limits(&intent, &authority).is_ok());
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
            assert_eq!(
                validate_intent_within_limits(&intent, &authority).is_ok(),
                expected
            );
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
            .find(|size| validate_intent_within_limits(&make_intent(*size), &authority).is_ok())
            .unwrap();
        assert!(validate_intent_within_limits(&make_intent(boundary + 1), &authority).is_err());
        assert!(validate_intent_within_limits(&make_intent(boundary), &authority).is_ok());
    }

    #[test]
    fn resolved_account_lock_count_includes_readonly_alt_loaded_keys() {
        let lookup = v0::MessageAddressTableLookup {
            account_key: Pubkey::new_unique(),
            writable_indexes: (0..32).collect(),
            readonly_indexes: (32..63).collect(),
        };
        let message = VersionedMessage::V0(v0::Message {
            account_keys: vec![Pubkey::new_unique()],
            address_table_lookups: vec![lookup.clone()],
            ..Default::default()
        });
        assert_eq!(
            resolved_account_lock_count(&message),
            MAX_BASE_LAYER_TX_ACCOUNT_LOCKS
        );

        let over_limit = VersionedMessage::V0(v0::Message {
            account_keys: vec![Pubkey::new_unique()],
            address_table_lookups: vec![v0::MessageAddressTableLookup {
                readonly_indexes: (32..64).collect(),
                ..lookup
            }],
            ..Default::default()
        });
        assert_eq!(
            resolved_account_lock_count(&over_limit),
            MAX_BASE_LAYER_TX_ACCOUNT_LOCKS + 1
        );
    }

    #[test]
    fn oversized_finalize_reports_account_lock_count_for_both_formats() {
        let intent = MagicIntentBundle {
            commit_finalize_and_undelegate: Some(CommitAndUndelegate {
                commit_action: CommitType::Standalone(
                    (0..10).map(|_| make_committed_account(10)).collect(),
                ),
                undelegate_action: UndelegateType::Standalone,
            }),
            ..Default::default()
        };

        let error = validate_intent_within_limits(&intent, &Pubkey::new_unique()).unwrap_err();
        assert!(matches!(
            error,
            IntentLimitError::NoSupportedFormatFits {
                stage: IntentLimitStage::Finalize,
                v1: FormatFitError::AccountLockLimitExceeded {
                    limit: MAX_BASE_LAYER_TX_ACCOUNT_LOCKS,
                    ..
                },
                v0_with_alts: FormatFitError::AccountLockLimitExceeded {
                    limit: MAX_BASE_LAYER_TX_ACCOUNT_LOCKS,
                    ..
                },
            }
        ));
    }
}
