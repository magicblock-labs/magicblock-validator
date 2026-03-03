use dlp::{
    args::PostDelegationActions,
    pda::delegation_record_pda_from_delegated_account, state::DelegationRecord,
};
use dlp_api::decrypt::Decrypt;
use dlp_api::encryption::KEY_LEN;
use magicblock_accounts_db::traits::AccountsBank;
use magicblock_core::token_programs::EATA_PROGRAM_ID;
use magicblock_metrics::metrics;
use solana_account::ReadableAccount;
use solana_keypair::Keypair;
use solana_program::program_error::ProgramError;
use solana_pubkey::Pubkey;
use tracing::*;

use super::FetchCloner;
use crate::{
    chainlink::errors::{ChainlinkError, ChainlinkResult},
    cloner::{Cloner, DelegationActions},
    remote_account_provider::{
        ChainPubsubClient, ChainRpcClient, MatchSlotsConfig,
        ResolvedAccountSharedData,
    },
};

/// Parses a delegation record from account data bytes.
/// Returns the parsed DelegationRecord, or InvalidDelegationRecord error
/// if parsing fails.
pub(crate) fn parse_delegation_record(
    data: &[u8],
    delegation_record_pubkey: Pubkey,
    validator_pubkey: Pubkey,
    validator_keypair: Option<&Keypair>,
) -> ChainlinkResult<(DelegationRecord, Option<DelegationActions>)> {
    let delegation_record_size = DelegationRecord::size_with_discriminator();
    if data.len() < delegation_record_size {
        return Err(ChainlinkError::InvalidDelegationRecord(
            delegation_record_pubkey,
            ProgramError::InvalidAccountData,
        ));
    }
    let record = DelegationRecord::try_from_bytes_with_discriminator(
        &data[..delegation_record_size],
    )
    .copied()
    .map_err(|err| {
        ChainlinkError::InvalidDelegationRecord(delegation_record_pubkey, err)
    })?;

    if data.len() <= delegation_record_size {
        Ok((record, None))
    } else {
        // Actions for accounts delegated to other validators are not relevant
        // for this node and may be encrypted for a different recipient.
        if record.authority != validator_pubkey
            && record.authority != Pubkey::default()
        {
            return Ok((record, None));
        }

        let actions_data = &data[delegation_record_size..];
        let actions = parse_post_delegation_actions(
            actions_data,
            delegation_record_pubkey,
            validator_keypair,
        )?;
        Ok((record, Some(actions)))
    }
}

fn parse_post_delegation_actions(
    actions_data: &[u8],
    delegation_record_pubkey: Pubkey,
    validator_keypair: Option<&Keypair>,
) -> ChainlinkResult<DelegationActions> {
    let actions: PostDelegationActions = bincode::deserialize(actions_data)
        .map_err(|err| {
            ChainlinkError::InvalidDelegationActions(
                delegation_record_pubkey,
                format!("Failed to deserialize PostDelegationActions: {err}"),
            )
        })?;

    let instructions = match validator_keypair {
        Some(keypair) => actions.decrypt_with_keypair(keypair),
        None => actions.decrypt(&[0; KEY_LEN], &[0; KEY_LEN]),
    }
    .map_err(|err| {
        ChainlinkError::InvalidDelegationActions(
            delegation_record_pubkey,
            format!("Failed to parse/decrypt PostDelegationActions: {err}"),
        )
    })?;

    Ok(instructions.into())
}

pub(crate) fn apply_delegation_record_to_account<T, U, V, C>(
    this: &FetchCloner<T, U, V, C>,
    account: &mut ResolvedAccountSharedData,
    delegation_record: &DelegationRecord,
) -> Option<u64>
where
    T: ChainRpcClient,
    U: ChainPubsubClient,
    V: AccountsBank,
    C: Cloner,
{
    let is_confined = delegation_record.authority.eq(&Pubkey::default());
    let is_delegated_to_us =
        delegation_record.authority.eq(&this.validator_pubkey) || is_confined;

    // Always update owner and confined flags
    account
        .set_owner(delegation_record.owner)
        .set_confined(is_confined);

    if is_delegated_to_us && delegation_record.owner != EATA_PROGRAM_ID {
        account.set_delegated(true);
    } else if !is_delegated_to_us {
        account.set_delegated(false);
    }
    if is_delegated_to_us {
        Some(delegation_record.commit_frequency_ms)
    } else {
        None
    }
}

pub(crate) fn get_delegated_to_other<T, U, V, C>(
    this: &FetchCloner<T, U, V, C>,
    delegation_record: &DelegationRecord,
) -> Option<Pubkey>
where
    T: ChainRpcClient,
    U: ChainPubsubClient,
    V: AccountsBank,
    C: Cloner,
{
    let is_delegated_to_us =
        delegation_record.authority.eq(&this.validator_pubkey)
            || delegation_record.authority.eq(&Pubkey::default());

    (!is_delegated_to_us).then_some(delegation_record.authority)
}

#[instrument(skip(this))]
pub(crate) async fn fetch_and_parse_delegation_record<T, U, V, C>(
    this: &FetchCloner<T, U, V, C>,
    account_pubkey: Pubkey,
    min_context_slot: u64,
    fetch_origin: metrics::AccountFetchOrigin,
) -> Option<(DelegationRecord, Option<DelegationActions>)>
where
    T: ChainRpcClient,
    U: ChainPubsubClient,
    V: AccountsBank,
    C: Cloner,
{
    let delegation_record_pubkey =
        delegation_record_pda_from_delegated_account(&account_pubkey);
    let was_watching_deleg_record = this
        .remote_account_provider
        .is_watching(&delegation_record_pubkey);

    let res = match this
        .remote_account_provider
        .try_get_multi_until_slots_match(
            &[delegation_record_pubkey],
            Some(MatchSlotsConfig {
                min_context_slot: Some(min_context_slot),
                ..Default::default()
            }),
            fetch_origin,
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
        Err(_) => None,
    };

    if !was_watching_deleg_record
        // Handle edge case where it was cloned in the meantime.
        // The small possiblility of a fetch + clone of this delegation record being in process
        // still exits, but it's negligible
        && this
            .accounts_bank
            .get_account(&delegation_record_pubkey)
            .is_none()
    {
        // We only subscribed to fetch the delegation record, so unsubscribe now
        if let Err(err) = this
            .remote_account_provider
            .unsubscribe(&delegation_record_pubkey)
            .await
        {
            warn!(pubkey = %delegation_record_pubkey, error = %err, "Failed to unsubscribe from delegation record");
        }
    }

    res
}

#[cfg(test)]
mod tests {
    use dlp::args::{
        EncryptedBuffer, MaybeEncryptedAccountMeta, MaybeEncryptedInstruction,
        MaybeEncryptedIxData, PostDelegationActions,
    };
    use solana_instruction::Instruction;
    use solana_program::pubkey::Pubkey;
    use solana_signer::Signer;

    use super::*;

    fn serialize_record_with_actions(
        authority: Pubkey,
        actions: PostDelegationActions,
    ) -> Vec<u8> {
        let record = DelegationRecord {
            owner: Pubkey::new_unique(),
            authority,
            commit_frequency_ms: 1000,
            delegation_slot: 1,
            lamports: 1_000_000,
        };

        let mut data = vec![0; DelegationRecord::size_with_discriminator()];
        record.to_bytes_with_discriminator(&mut data).unwrap();
        data.extend_from_slice(&bincode::serialize(&actions).unwrap());
        data
    }

    #[test]
    fn parses_post_delegation_actions_cleartext() {
        let signer = Pubkey::new_unique();
        let program_id = Pubkey::new_unique();
        let account = Pubkey::new_unique();

        let validator_pubkey = Pubkey::new_unique();
        let payload = serialize_record_with_actions(
            validator_pubkey,
            PostDelegationActions {
                signers: vec![signer, program_id, account],
                non_signers: vec![],
                instructions: vec![MaybeEncryptedInstruction {
                    program_id: 1,
                    accounts: vec![
                        dlp::compact::AccountMeta::new_readonly(0, true),
                        dlp::compact::AccountMeta::new(2, false),
                    ],
                    data: MaybeEncryptedIxData {
                        prefix: vec![7, 8, 9],
                        suffix: EncryptedBuffer::default(),
                    },
                }],
            },
        );

        let (_, actions) = parse_delegation_record(
            &payload,
            Pubkey::new_unique(),
            validator_pubkey,
            None,
        )
        .unwrap();

        let actions: Vec<Instruction> = actions.unwrap().into();
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].program_id, program_id);
        assert_eq!(actions[0].accounts.len(), 2);
        assert_eq!(actions[0].accounts[0].pubkey, signer);
        assert!(actions[0].accounts[0].is_signer);
        assert!(!actions[0].accounts[0].is_writable);
        assert_eq!(actions[0].accounts[1].pubkey, account);
        assert!(!actions[0].accounts[1].is_signer);
        assert!(actions[0].accounts[1].is_writable);
        assert_eq!(actions[0].data, vec![7, 8, 9]);
    }

    #[test]
    fn decrypts_encrypted_post_delegation_actions() {
        let validator = Keypair::new();

        let signer = Pubkey::new_unique();
        let program_id = Pubkey::new_unique();
        let account = Pubkey::new_unique();

        let encrypted_program_id =
            dlp_api::encryption::encrypt_ed25519_recipient(
                program_id.as_array(),
                validator.pubkey().as_array(),
            )
            .unwrap();
        let encrypted_suffix = dlp_api::encryption::encrypt_ed25519_recipient(
            &[3, 4, 5],
            validator.pubkey().as_array(),
        )
        .unwrap();

        let payload = serialize_record_with_actions(
            validator.pubkey(),
            PostDelegationActions {
                signers: vec![signer, account],
                non_signers: vec![MaybeEncryptedAccountMeta::Encrypted(
                    EncryptedBuffer::new(encrypted_program_id),
                )],
                instructions: vec![MaybeEncryptedInstruction {
                    program_id: 2,
                    accounts: vec![
                        dlp::compact::AccountMeta::new_readonly(0, true),
                        dlp::compact::AccountMeta::new(1, false),
                    ],
                    data: MaybeEncryptedIxData {
                        prefix: vec![1, 2],
                        suffix: EncryptedBuffer::new(encrypted_suffix),
                    },
                }],
            },
        );

        let (_, actions) = parse_delegation_record(
            &payload,
            Pubkey::new_unique(),
            validator.pubkey(),
            Some(&validator),
        )
        .unwrap();

        let actions: Vec<Instruction> = actions.unwrap().into();
        assert_eq!(actions[0].program_id, program_id);
        assert_eq!(actions[0].accounts[0].pubkey, signer);
        assert_eq!(actions[0].accounts[1].pubkey, account);
        assert_eq!(actions[0].data, vec![1, 2, 3, 4, 5]);
    }

    #[test]
    fn fails_when_encrypted_actions_have_no_validator_keypair() {
        let validator = Keypair::new();
        let encrypted_program_id =
            dlp_api::encryption::encrypt_ed25519_recipient(
                Pubkey::new_unique().as_array(),
                validator.pubkey().as_array(),
            )
            .unwrap();

        let payload = serialize_record_with_actions(
            validator.pubkey(),
            PostDelegationActions {
                signers: vec![],
                non_signers: vec![MaybeEncryptedAccountMeta::Encrypted(
                    EncryptedBuffer::new(encrypted_program_id),
                )],
                instructions: vec![],
            },
        );

        let err = parse_delegation_record(
            &payload,
            Pubkey::new_unique(),
            validator.pubkey(),
            None,
        )
        .unwrap_err();
        assert!(matches!(
            err,
            ChainlinkError::InvalidDelegationActions(_, _)
        ));
    }
}
