use dlp_api::{
    decrypt::Decrypt,
    dlp::{
        args::PostDelegationActions,
        pda::delegation_record_pda_from_delegated_account,
        state::DelegationRecord,
    },
};
use magicblock_accounts_db::traits::AccountsBank;
use magicblock_core::token_programs::{derive_eata, EATA_PROGRAM_ID};
use magicblock_metrics::metrics;
use solana_account::ReadableAccount;
use solana_keypair::Keypair;
use solana_program::program_error::ProgramError;
use solana_pubkey::Pubkey;
use solana_signer::Signer;
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
        if record.authority != validator_keypair.pubkey() {
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
    validator_keypair: &Keypair,
) -> ChainlinkResult<DelegationActions> {
    let actions: PostDelegationActions = borsh::from_slice(actions_data)
        .map_err(|err| {
            ChainlinkError::InvalidDelegationActions(
                delegation_record_pubkey,
                format!("Failed to deserialize PostDelegationActions: {err}"),
            )
        })?;

    let instructions = actions
        .decrypt_with_keypair(validator_keypair)
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
    account_pubkey: Pubkey,
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
    let is_raw_eata = parse_raw_eata_pda(
        &account_pubkey,
        account.data(),
        delegation_record.owner,
    )
    .is_some();

    // Always update owner and confined flags
    account
        .set_owner(delegation_record.owner)
        .set_confined(is_confined);

    if is_delegated_to_us && !is_raw_eata {
        account.set_delegated(true);
    } else {
        account.set_delegated(false);
    }
    if is_delegated_to_us && !is_raw_eata {
        Some(delegation_record.commit_frequency_ms)
    } else {
        None
    }
}

pub(crate) fn parse_raw_eata_pda(
    account_pubkey: &Pubkey,
    data: &[u8],
    owner_program: Pubkey,
) -> Option<(Pubkey, Pubkey)> {
    if owner_program != EATA_PROGRAM_ID || data.len() < 72 {
        return None;
    }

    let wallet_owner = Pubkey::new_from_array(data[0..32].try_into().ok()?);
    let mint = Pubkey::new_from_array(data[32..64].try_into().ok()?);
    (derive_eata(&wallet_owner, &mint) == *account_pubkey)
        .then_some((wallet_owner, mint))
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
    use dlp_api::dlp::args::{
        EncryptedBuffer, MaybeEncryptedAccountMeta, MaybeEncryptedInstruction,
        MaybeEncryptedIxData, MaybeEncryptedPubkey, PostDelegationActions,
    };
    use solana_instruction::{AccountMeta, Instruction};
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
                signers: vec![
                    *signer.as_array(),
                    *program_id.as_array(),
                    *account.as_array(),
                ],
                non_signers: vec![],
                instructions: vec![MaybeEncryptedInstruction {
                    program_id: 1,
                    accounts: vec![
                        MaybeEncryptedAccountMeta::ClearText(
                            dlp_api::dlp::compact::AccountMeta::new_readonly(
                                0, true,
                            ),
                        ),
                        MaybeEncryptedAccountMeta::ClearText(
                            dlp_api::dlp::compact::AccountMeta::new(2, false),
                        ),
                    ],
                    data: MaybeEncryptedIxData {
                        prefix: vec![7, 8, 9],
                        suffix: EncryptedBuffer::default(),
                    },
                }],
            },
        );

        let (_, actions) =
            parse_delegation_record(&payload, Pubkey::new_unique(), &validator)
                .unwrap();

        let actions: Vec<Instruction> = actions.unwrap().into();
        assert_eq!(
            actions,
            vec![Instruction {
                program_id,
                accounts: vec![
                    AccountMeta::new_readonly(signer, true),
                    AccountMeta::new(account, false),
                ],
                data: vec![7, 8, 9],
            }]
        );
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
                inserted_signers: 0,
                inserted_non_signers: 0,
                signers: vec![*signer.as_array(), *account.as_array()],
                non_signers: vec![MaybeEncryptedPubkey::Encrypted(
                    EncryptedBuffer::new(encrypted_program_id),
                )],
                instructions: vec![MaybeEncryptedInstruction {
                    program_id: 2,
                    accounts: vec![
                        MaybeEncryptedAccountMeta::ClearText(
                            dlp_api::dlp::compact::AccountMeta::new_readonly(
                                0, true,
                            ),
                        ),
                        MaybeEncryptedAccountMeta::ClearText(
                            dlp_api::dlp::compact::AccountMeta::new(1, false),
                        ),
                    ],
                    data: MaybeEncryptedIxData {
                        prefix: vec![1, 2],
                        suffix: EncryptedBuffer::new(encrypted_suffix),
                    },
                }],
            },
        );

        let (_, actions) =
            parse_delegation_record(&payload, Pubkey::new_unique(), &validator)
                .unwrap();

        let actions: Vec<Instruction> = actions.unwrap().into();
        assert_eq!(
            actions,
            vec![Instruction {
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
        let encrypted_program_id =
            dlp_api::encryption::encrypt_ed25519_recipient(
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
                non_signers: vec![MaybeEncryptedPubkey::Encrypted(
                    EncryptedBuffer::new(encrypted_program_id),
                )],
                instructions: vec![],
            },
        );

        let (_record, actions) = parse_delegation_record(
            &payload,
            Pubkey::new_unique(),
            &wrong_validator,
        )
        .unwrap();

        assert!(actions.is_none());
    }
}
