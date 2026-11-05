use ::metrics::metrics::{self, ChainlinkCompanionFetchKind};
use delegation_program_interface::{
    self as dpi, DelegationCluster, DelegationState,
    decrypt::Decrypt,
    legacy::{
        args::PostDelegationActions, pda::delegation_record_pda_from_delegated_account,
        state::DelegationRecord,
    },
};
use mbv_core::token_programs::{EATA_PROGRAM_ID, EphemeralAta, try_derive_eata_address_and_bump};
use solana_account::{AccountBuilder, AccountMode, ReadableAccount};
use solana_keypair::Keypair;
use solana_program::program_error::ProgramError;
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use tracing::*;
use wheels::layout::Decodable;

use super::{
    CompanionFetchLogContext, FetchCloner, log_companion_fetch_failure,
    subscription::{SubscriptionRelease, release_subs},
};
use crate::{
    chainlink::errors::{ChainlinkError, ChainlinkResult},
    cloner::DelegationActions,
    remote_account_provider::{
        ChainPubsubClient, ChainRpcClient, MatchSlotsConfig, SubscriptionReason,
    },
};

/// Resolves a v2 state into the shared delegation record used by Chainlink's
/// account-mode projection. V2 authority comes from the validated cluster.
pub(super) async fn parse_v2_delegation_record<T: ChainRpcClient, U: ChainPubsubClient>(
    this: &FetchCloner<T, U>,
    delegated_pubkey: Pubkey,
    state_pubkey: Pubkey,
    state_account: &AccountBuilder,
    fetch_context: metrics::AccountFetchContext,
) -> ChainlinkResult<DelegationRecord> {
    let invalid =
        || ChainlinkError::InvalidDelegationRecord(state_pubkey, ProgramError::InvalidAccountData);
    let state = state_account.read();
    if state.owner() != dpi::id() || dpi::delegation_state_pda(&delegated_pubkey) != state_pubkey {
        return Err(invalid());
    }
    let state_slot = state.slot();
    let state = DelegationState::decode(state.data()).map_err(|_| invalid())?;
    let (owner, delegation_slot, lamports, cluster_pubkey) = (
        *state.original_owner(),
        state.delegation_slot(),
        state.lamports(),
        *state.delegation_cluster(),
    );
    if state.discriminator() != DelegationState::DISCRIMINATOR
        || *state.delegated_account() != delegated_pubkey
        || delegation_slot > state_slot
    {
        return Err(invalid());
    }
    let cluster_accounts = this
        .remote_account_provider
        .fetch_snapshot_until_slot(
            &[cluster_pubkey],
            state_slot,
            fetch_context,
            ChainlinkCompanionFetchKind::DelegationRecord,
        )
        .await?;
    let [cluster_account] = cluster_accounts.as_slice() else {
        return Err(invalid());
    };
    let cluster_account = cluster_account.fresh_account().ok_or_else(invalid)?;
    if cluster_account.owner() != &dpi::id() {
        return Err(invalid());
    }
    let cluster = DelegationCluster::decode(cluster_account.data()).map_err(|_| invalid())?;
    if cluster.discriminator() != DelegationCluster::DISCRIMINATOR
        || *cluster.cluster_id() != cluster_pubkey
        || dpi::delegation_cluster_pda(cluster.cluster_id()) != cluster_pubkey
        || cluster.challenge_config().is_some()
    {
        return Err(invalid());
    }
    Ok(DelegationRecord {
        authority: *cluster.current_validator(),
        owner,
        delegation_slot,
        lamports,
        commit_frequency_ms: 0,
    })
}

/// Parses a delegation record from account data bytes. Returns the parsed DelegationRecord
/// or {InvalidDelegationRecord, InvalidDelegationActions} error if parsing fails.
pub(crate) fn parse_delegation_record(
    data: &[u8],
    delegation_record_pubkey: Pubkey,
    validator_keypair: &Keypair,
) -> ChainlinkResult<(DelegationRecord, Option<DelegationActions>)> {
    let delegation_record_size = DelegationRecord::size_with_discriminator();
    if data.len() < delegation_record_size {
        return Err(ChainlinkError::InvalidDelegationRecord(
            delegation_record_pubkey,
            ProgramError::InvalidAccountData,
        ));
    }
    let record =
        DelegationRecord::try_from_bytes_with_discriminator(&data[..delegation_record_size])
            .copied()
            .map_err(|err| {
                ChainlinkError::InvalidDelegationRecord(delegation_record_pubkey, err)
            })?;

    if data.len() <= delegation_record_size {
        Ok((record, None))
    } else {
        // Actions for accounts delegated to other validators are not relevant
        // for this node and may be encrypted for a different recipient.
        if record.authority != validator_keypair.pubkey() {
            return Ok((record, None));
        }

        let actions_data = &data[delegation_record_size..];
        let actions = parse_post_delegation_actions(
            actions_data,
            delegation_record_pubkey,
            validator_keypair,
        )?;

        Ok((record, DelegationActions::new(record.owner, actions)))
    }
}

fn parse_post_delegation_actions(
    actions_data: &[u8],
    delegation_record_pubkey: Pubkey,
    validator_keypair: &Keypair,
) -> ChainlinkResult<Vec<solana_instruction::Instruction>> {
    let actions: PostDelegationActions = borsh::from_slice(actions_data).map_err(|err| {
        ChainlinkError::InvalidDelegationActions(
            delegation_record_pubkey,
            format!("Failed to deserialize PostDelegationActions: {err}"),
        )
    })?;

    let instructions = actions.decrypt_with_keypair(validator_keypair).map_err(|err| {
        ChainlinkError::InvalidDelegationActions(
            delegation_record_pubkey,
            format!("Failed to parse/decrypt PostDelegationActions: {err}"),
        )
    })?;

    Ok(instructions)
}

/// Restores the owner and resolves the account mode from its delegation record.
/// Confined accounts have zero lamports; raw eATAs never become committable.
pub(super) fn apply_record(
    validator: &Pubkey,
    account_pubkey: Pubkey,
    account: AccountBuilder,
    delegation_record: &DelegationRecord,
) -> AccountBuilder {
    let is_confined = delegation_record.authority.eq(&Pubkey::default());
    let is_delegated_to_us = delegated_to_other(validator, delegation_record).is_none();
    let is_raw_eata = parse_raw_eata_pda(
        &account_pubkey,
        account.read().data(),
        delegation_record.owner,
    )
    .is_some();

    // Delegation state is a single exclusive mode, so it is resolved once here
    // rather than by flipping independent `delegated`/`confined` flags. A
    // confined account is one delegated with no authority to commit back to
    // chain, which the engine represents as `Magic`; it takes precedence,
    // since losing it would make the account committable.
    let mode = if is_confined {
        AccountMode::Magic
    } else if is_delegated_to_us && !is_raw_eata {
        AccountMode::Delegated
    } else {
        AccountMode::Mirrored
    };
    let account = account.owner(delegation_record.owner).mode(mode);
    if is_confined {
        account.lamports(0)
    } else if mode == AccountMode::Delegated {
        // Repeated observations carry the same delegation stamp. RPC fetch
        // freshness is retained separately on the clone request.
        account.slot(delegation_record.delegation_slot)
    } else {
        account
    }
}

pub(crate) fn parse_raw_eata_pda(
    account_pubkey: &Pubkey,
    data: &[u8],
    owner_program: Pubkey,
) -> Option<(Pubkey, Pubkey)> {
    if owner_program != EATA_PROGRAM_ID {
        return None;
    }

    let eata = EphemeralAta::try_from_account_data(data)?;
    let (derived_eata, bump) = try_derive_eata_address_and_bump(&eata.owner, &eata.mint)?;
    (derived_eata == *account_pubkey && bump == eata.bump).then_some((eata.owner, eata.mint))
}

/// Returns the other validator, or `None` for local or confined delegations.
pub(super) fn delegated_to_other(
    validator: &Pubkey,
    delegation_record: &DelegationRecord,
) -> Option<Pubkey> {
    let is_delegated_to_us = delegation_record.authority.eq(validator)
        || delegation_record.authority.eq(&Pubkey::default());

    (!is_delegated_to_us).then_some(delegation_record.authority)
}

#[instrument(skip(this))]
pub(crate) async fn fetch_and_parse_delegation_record<T, U>(
    this: &FetchCloner<T, U>,
    account_pubkey: Pubkey,
    min_context_slot: u64,
    fetch_context: metrics::AccountFetchContext,
    companion_fetch_log_context: &CompanionFetchLogContext,
) -> Option<(DelegationRecord, Option<DelegationActions>)>
where
    T: ChainRpcClient,
    U: ChainPubsubClient,
{
    let delegation_record_pubkey = delegation_record_pda_from_delegated_account(&account_pubkey);

    let acquired_delegation_record_reason = this
        .acquire_subscription_reason(
            &delegation_record_pubkey,
            SubscriptionReason::DelegationRecord,
        )
        .await
        .map(|_| true)
        .unwrap_or_else(|err| {
            warn!(
                pubkey = %delegation_record_pubkey,
                error = ?err,
                "Failed to acquire delegation record subscription reason"
            );
            false
        });

    let res = match this
        .remote_account_provider
        .try_get_multi_until_slots_match(
            &[delegation_record_pubkey],
            Some(MatchSlotsConfig {
                min_context_slot: Some(min_context_slot),
                ..MatchSlotsConfig::new(ChainlinkCompanionFetchKind::DelegationRecord)
            }),
            fetch_context.with_reason(metrics::AccountFetchReason::DelegationRecord),
        )
        .await
    {
        Ok(mut delegation_records) => {
            if let Some(delegation_record_remote) = delegation_records.pop() {
                match delegation_record_remote.fresh_account() {
                    Some(delegation_record_account) => this
                        .parse_delegation_record(
                            delegation_record_account.data(),
                            delegation_record_pubkey,
                        )
                        .ok(),
                    None => None,
                }
            } else {
                None
            }
        }
        Err(err) => {
            log_companion_fetch_failure(
                companion_fetch_log_context,
                delegation_record_pubkey,
                ChainlinkCompanionFetchKind::DelegationRecord,
                &err,
            );
            None
        }
    };

    let mut releases = Vec::new();
    // Handle edge case where it was cloned in the meantime.
    // The small possibility of a fetch + clone of this delegation record being in process
    // still exists, but it's negligible.
    if !this.contains_account(&delegation_record_pubkey) {
        releases.push(SubscriptionRelease::Pubkey {
            pubkey: delegation_record_pubkey,
            reason: SubscriptionReason::DirectAccount,
        });
    }
    if acquired_delegation_record_reason {
        releases.push(SubscriptionRelease::Pubkey {
            pubkey: delegation_record_pubkey,
            reason: SubscriptionReason::DelegationRecord,
        });
    }
    if !releases.is_empty() {
        release_subs(&this.remote_account_provider, releases).await;
    }

    res
}

/// Seed only a newly materialized session. Existing protected images are skipped
/// by the engine, so later observations cannot overwrite ER reservations.
///
/// CHECKPOINT: A fresh delegation starts at commit ID 0. Once commits advance it,
/// the account already exists locally, and cloning preserves its counter.
/// For this normal lifecycle, initializing to 0 may be enough and this lookup
/// may be unnecessary. We keep it for now: a nonzero initial value would matter
/// when rebuilding an active delegation without its existing local ER state.
pub(super) async fn initial_commit_id<T: ChainRpcClient, U: ChainPubsubClient>(
    this: &FetchCloner<T, U>,
    request: &crate::cloner::AccountCloneRequest,
    fetch_context: metrics::AccountFetchContext,
) -> ChainlinkResult<u32> {
    use delegation_program_interface::DelegationState;
    use delegation_program_interface::legacy::{
        pda::delegation_metadata_pda_from_delegated_account, state::DelegationMetadata,
    };
    let image = request.account.read();
    let delegated_pubkey = if request.is_ata_projection() {
        super::ata_projection::derive_eata_pubkey_from_ata_account(
            &request.pubkey,
            &request.account,
        )
        .ok_or(ChainlinkError::InvalidDelegationRecord(
            request.pubkey,
            ProgramError::InvalidAccountData,
        ))?
    } else {
        request.pubkey
    };
    let record_pubkey = delegation_record_pda_from_delegated_account(&delegated_pubkey);
    let metadata_pubkey = delegation_metadata_pda_from_delegated_account(&delegated_pubkey);
    let state_pubkey = dpi::delegation_state_pda(&delegated_pubkey);
    let accounts = this
        .remote_account_provider
        .fetch_snapshot_until_slot(
            &[record_pubkey, metadata_pubkey, state_pubkey],
            request.source_slots().view,
            fetch_context,
            ChainlinkCompanionFetchKind::DelegationRecord,
        )
        .await?;
    let invalid =
        || ChainlinkError::InvalidDelegationRecord(record_pubkey, ProgramError::InvalidAccountData);
    let [record, metadata, state] = accounts.as_slice() else {
        return Err(invalid());
    };
    if let Some(state) = state.fresh_account() {
        if record.is_found() || metadata.is_found() || state.owner() != &dpi::id() {
            return Err(invalid());
        }
        let state_view = DelegationState::decode(state.data()).map_err(|_| invalid())?;
        if state_view.discriminator() != DelegationState::DISCRIMINATOR
            || *state_view.delegated_account() != delegated_pubkey
            || *state_view.original_owner()
                != if request.is_ata_projection() { EATA_PROGRAM_ID } else { image.owner() }
            || state_view.delegation_slot() != image.slot()
        {
            return Err(invalid());
        }
        return u32::try_from(state_view.last_commit_id()).map_err(|_| {
            ChainlinkError::InvalidDelegationCommitSequence(
                state_pubkey,
                state_view.last_commit_id(),
            )
        });
    }
    let record = record.fresh_account().ok_or_else(invalid)?;
    let metadata = metadata.fresh_account().ok_or_else(invalid)?;
    if record.owner() != &delegation_program_interface::id()
        || metadata.owner() != &delegation_program_interface::id()
    {
        return Err(invalid());
    }
    let data = record
        .data()
        .get(..DelegationRecord::size_with_discriminator())
        .ok_or_else(invalid)?;
    let record =
        DelegationRecord::try_from_bytes_with_discriminator(data).map_err(|_| invalid())?;
    let expected_owner = if request.is_ata_projection() { EATA_PROGRAM_ID } else { image.owner() };
    if record.authority != this.validator_pubkey
        || record.owner != expected_owner
        || record.delegation_slot != image.slot()
    {
        return Err(invalid());
    }
    let metadata = DelegationMetadata::try_from_bytes_with_discriminator(metadata.data())
        .map_err(|_| invalid())?;
    u32::try_from(metadata.last_commit_id).map_err(|_| {
        ChainlinkError::InvalidDelegationCommitSequence(metadata_pubkey, metadata.last_commit_id)
    })
}

#[cfg(test)]
mod tests {
    use delegation_program_interface::{
        self as dpi,
        legacy::args::{
            EncryptedBuffer, MaybeEncryptedAccountMeta, MaybeEncryptedInstruction,
            MaybeEncryptedIxData, MaybeEncryptedPubkey, PostDelegationActions,
        },
    };
    use solana_instruction::{AccountMeta, Instruction};
    use solana_program::pubkey::Pubkey;
    use solana_signer::Signer;

    use super::*;

    fn serialize_record_with_actions(authority: Pubkey, actions: PostDelegationActions) -> Vec<u8> {
        let record = DelegationRecord {
            owner: Pubkey::new_unique(),
            authority,
            commit_frequency_ms: 1000,
            delegation_slot: 1,
            lamports: 1_000_000,
        };

        let mut data = vec![0; DelegationRecord::size_with_discriminator()];
        record.to_bytes_with_discriminator(&mut data).unwrap();
        data.extend_from_slice(&borsh::to_vec(&actions).unwrap());
        data
    }

    #[test]
    fn parses_post_delegation_actions_cleartext() {
        let signer = Pubkey::new_unique();
        let program_id = Pubkey::new_unique();
        let account = Pubkey::new_unique();

        let validator = Keypair::new();
        let payload = serialize_record_with_actions(
            validator.pubkey(),
            PostDelegationActions {
                inserted_signers: 0,
                inserted_non_signers: 0,
                signers: vec![*signer.as_array(), *program_id.as_array(), *account.as_array()],
                non_signers: vec![],
                instructions: vec![MaybeEncryptedInstruction {
                    program_id: 1,
                    accounts: vec![
                        MaybeEncryptedAccountMeta::ClearText(
                            dpi::compact::AccountMeta::new_readonly(0, true),
                        ),
                        MaybeEncryptedAccountMeta::ClearText(dpi::compact::AccountMeta::new(
                            2, false,
                        )),
                    ],
                    data: MaybeEncryptedIxData {
                        prefix: vec![7, 8, 9],
                        suffix: EncryptedBuffer::default(),
                    },
                }],
            },
        );

        let (record, actions) =
            parse_delegation_record(&payload, Pubkey::new_unique(), &validator).unwrap();

        let actions = actions.unwrap();
        assert_eq!(actions.source_program(), record.owner);
        assert_eq!(
            actions.actions(),
            [Instruction {
                program_id,
                accounts: vec![
                    AccountMeta::new_readonly(signer, true),
                    AccountMeta::new(account, false),
                ],
                data: vec![7, 8, 9],
            }]
        );
    }

    /// Proves projected ATA actions retain the eATA program from their
    /// slot-matched delegation record as invocation provenance.
    #[test]
    fn preserves_eata_source_program_for_post_delegation_actions() {
        let validator = Keypair::new();
        let record = DelegationRecord {
            owner: EATA_PROGRAM_ID,
            authority: validator.pubkey(),
            commit_frequency_ms: 1_000,
            delegation_slot: 1,
            lamports: 1_000_000,
        };
        let mut payload = vec![0; DelegationRecord::size_with_discriminator()];
        record.to_bytes_with_discriminator(&mut payload).unwrap();
        payload.extend_from_slice(
            &borsh::to_vec(&PostDelegationActions {
                inserted_signers: 0,
                inserted_non_signers: 0,
                signers: vec![*Pubkey::new_unique().as_array()],
                non_signers: vec![],
                instructions: vec![MaybeEncryptedInstruction {
                    program_id: 0,
                    accounts: vec![],
                    data: MaybeEncryptedIxData {
                        prefix: vec![],
                        suffix: EncryptedBuffer::default(),
                    },
                }],
            })
            .unwrap(),
        );

        let (_, actions) =
            parse_delegation_record(&payload, Pubkey::new_unique(), &validator).unwrap();

        assert_eq!(actions.unwrap().source_program(), EATA_PROGRAM_ID);
    }

    #[test]
    fn decrypts_encrypted_post_delegation_actions() {
        let validator = Keypair::new();

        let signer = Pubkey::new_unique();
        let program_id = Pubkey::new_unique();
        let account = Pubkey::new_unique();

        let encrypted_program_id = dpi::encryption::encrypt_ed25519_recipient(
            program_id.as_array(),
            validator.pubkey().as_array(),
        )
        .unwrap();
        let encrypted_suffix =
            dpi::encryption::encrypt_ed25519_recipient(&[3, 4, 5], validator.pubkey().as_array())
                .unwrap();

        let payload = serialize_record_with_actions(
            validator.pubkey(),
            PostDelegationActions {
                inserted_signers: 0,
                inserted_non_signers: 0,
                signers: vec![*signer.as_array(), *account.as_array()],
                non_signers: vec![MaybeEncryptedPubkey::Encrypted(EncryptedBuffer::new(
                    encrypted_program_id,
                ))],
                instructions: vec![MaybeEncryptedInstruction {
                    program_id: 2,
                    accounts: vec![
                        MaybeEncryptedAccountMeta::ClearText(
                            dpi::compact::AccountMeta::new_readonly(0, true),
                        ),
                        MaybeEncryptedAccountMeta::ClearText(dpi::compact::AccountMeta::new(
                            1, false,
                        )),
                    ],
                    data: MaybeEncryptedIxData {
                        prefix: vec![1, 2],
                        suffix: EncryptedBuffer::new(encrypted_suffix),
                    },
                }],
            },
        );

        let (_, actions) =
            parse_delegation_record(&payload, Pubkey::new_unique(), &validator).unwrap();

        let actions = actions.unwrap();
        assert_eq!(
            actions.actions(),
            [Instruction {
                program_id,
                accounts: vec![
                    AccountMeta::new_readonly(signer, true),
                    AccountMeta::new(account, false),
                ],
                data: vec![1, 2, 3, 4, 5],
            }]
        );
    }

    #[test]
    fn fails_when_encrypted_actions_have_wrong_validator_keypair() {
        let validator = Keypair::new();
        let wrong_validator = Keypair::new();
        let encrypted_program_id = dpi::encryption::encrypt_ed25519_recipient(
            Pubkey::new_unique().as_array(),
            validator.pubkey().as_array(),
        )
        .unwrap();

        let payload = serialize_record_with_actions(
            validator.pubkey(),
            PostDelegationActions {
                inserted_signers: 0,
                inserted_non_signers: 0,
                signers: vec![],
                non_signers: vec![MaybeEncryptedPubkey::Encrypted(EncryptedBuffer::new(
                    encrypted_program_id,
                ))],
                instructions: vec![],
            },
        );

        let (_record, actions) =
            parse_delegation_record(&payload, Pubkey::new_unique(), &wrong_validator).unwrap();

        assert!(actions.is_none());
    }
}
