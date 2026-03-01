use dlp::{
    args::{MaybeEncryptedIxData, MaybeEncryptedPubkey, PostDelegationActions},
    pda::delegation_record_pda_from_delegated_account,
    state::DelegationRecord,
};
use dlp_api::encryption;
use magicblock_accounts_db::traits::AccountsBank;
use magicblock_core::token_programs::EATA_PROGRAM_ID;
use magicblock_metrics::metrics;
use solana_account::ReadableAccount;
use solana_instruction::{AccountMeta, Instruction};
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

#[derive(Clone)]
pub(crate) struct ValidatorDecryptionContext {
    x25519_pubkey: [u8; encryption::KEY_LEN],
    x25519_secret: [u8; encryption::KEY_LEN],
}

impl ValidatorDecryptionContext {
    pub(crate) fn from_validator_keypair(
        keypair: &Keypair,
    ) -> ChainlinkResult<Self> {
        let x25519_pubkey =
            encryption::ed25519_pubkey_to_x25519(keypair.pubkey().as_array())
                .map_err(|err| {
                    ChainlinkError::InvalidDelegationActions(
                        keypair.pubkey(),
                        format!("Failed to derive validator x25519 pubkey: {err}"),
                    )
                })?;

        let keypair_bytes = keypair.to_bytes();
        let x25519_secret =
            encryption::ed25519_secret_to_x25519(&keypair_bytes).map_err(
                |err| {
                    ChainlinkError::InvalidDelegationActions(
                        keypair.pubkey(),
                        format!(
                            "Failed to derive validator x25519 secret key: {err}"
                        ),
                    )
                },
            )?;

        Ok(Self {
            x25519_pubkey,
            x25519_secret,
        })
    }
}

/// Parses a delegation record from account data bytes.
/// Returns the parsed DelegationRecord, or InvalidDelegationRecord error
/// if parsing fails.
pub(crate) fn parse_delegation_record(
    data: &[u8],
    delegation_record_pubkey: Pubkey,
    validator_pubkey: Pubkey,
    decryption_ctx: Option<&ValidatorDecryptionContext>,
) -> ChainlinkResult<(DelegationRecord, Option<DelegationActions>)> {
    let delegation_record_size = DelegationRecord::size_with_discriminator();
    if data.len() < delegation_record_size {
        return Err(ChainlinkError::InvalidDelegationRecord(
            delegation_record_pubkey,
            ProgramError::InvalidAccountData,
        ));
    }
    let record =
        DelegationRecord::try_from_bytes_with_discriminator(
            &data[..delegation_record_size],
        )
        .copied()
        .map_err(|err| {
            ChainlinkError::InvalidDelegationRecord(
                delegation_record_pubkey,
                err,
            )
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
            decryption_ctx,
        )?;
        Ok((record, Some(actions)))
    }
}

fn parse_post_delegation_actions(
    actions_data: &[u8],
    delegation_record_pubkey: Pubkey,
    decryption_ctx: Option<&ValidatorDecryptionContext>,
) -> ChainlinkResult<DelegationActions> {
    let actions: PostDelegationActions =
        bincode::deserialize(actions_data).map_err(|err| {
            ChainlinkError::InvalidDelegationActions(
                delegation_record_pubkey,
                format!("Failed to deserialize PostDelegationActions: {err}"),
            )
        })?;

    let mut pubkeys = actions.signers;
    for non_signer in actions.non_signers {
        pubkeys.push(decrypt_pubkey(
            non_signer,
            delegation_record_pubkey,
            decryption_ctx,
        )?);
    }

    let instructions = actions
        .instructions
        .into_iter()
        .map(|ix| {
            let program_id = pubkeys.get(ix.program_id as usize).copied().ok_or(
                ChainlinkError::InvalidDelegationActions(
                    delegation_record_pubkey,
                    format!(
                        "Invalid program_id index {} for pubkey table len {}",
                        ix.program_id,
                        pubkeys.len()
                    ),
                ),
            )?;

            let accounts = ix
                .accounts
                .into_iter()
                .map(|compact_meta| {
                    let account_pubkey = pubkeys
                        .get(compact_meta.key() as usize)
                        .copied()
                        .ok_or(ChainlinkError::InvalidDelegationActions(
                            delegation_record_pubkey,
                            format!(
                                "Invalid account index {} for pubkey table len {}",
                                compact_meta.key(),
                                pubkeys.len()
                            ),
                        ))?;

                    Ok(AccountMeta {
                        pubkey: account_pubkey,
                        is_signer: compact_meta.is_signer(),
                        is_writable: compact_meta.is_writable(),
                    })
                })
                .collect::<ChainlinkResult<Vec<_>>>()?;

            let data = decrypt_ix_data(
                ix.data,
                delegation_record_pubkey,
                decryption_ctx,
            )?;

            Ok(Instruction {
                program_id,
                accounts,
                data,
            })
        })
        .collect::<ChainlinkResult<Vec<_>>>()?;

    Ok(instructions.into())
}

fn decrypt_pubkey(
    maybe_encrypted_pubkey: MaybeEncryptedPubkey,
    delegation_record_pubkey: Pubkey,
    decryption_ctx: Option<&ValidatorDecryptionContext>,
) -> ChainlinkResult<Pubkey> {
    match maybe_encrypted_pubkey {
        MaybeEncryptedPubkey::ClearText(pubkey) => Ok(pubkey),
        MaybeEncryptedPubkey::Encrypted(buffer) => {
            let plaintext = decrypt_buffer(
                buffer.as_bytes(),
                delegation_record_pubkey,
                decryption_ctx,
                "pubkey",
            )?;
            Pubkey::try_from(plaintext.as_slice()).map_err(|_| {
                ChainlinkError::InvalidDelegationActions(
                    delegation_record_pubkey,
                    format!(
                        "Decrypted pubkey has invalid length: {}",
                        plaintext.len()
                    ),
                )
            })
        }
    }
}

fn decrypt_ix_data(
    maybe_encrypted_ix_data: MaybeEncryptedIxData,
    delegation_record_pubkey: Pubkey,
    decryption_ctx: Option<&ValidatorDecryptionContext>,
) -> ChainlinkResult<Vec<u8>> {
    let mut data = maybe_encrypted_ix_data.prefix;
    let encrypted_suffix = maybe_encrypted_ix_data.suffix.into_inner();

    if !encrypted_suffix.is_empty() {
        let suffix = decrypt_buffer(
            &encrypted_suffix,
            delegation_record_pubkey,
            decryption_ctx,
            "ix data suffix",
        )?;
        data.extend_from_slice(&suffix);
    }

    Ok(data)
}

fn decrypt_buffer(
    encrypted: &[u8],
    delegation_record_pubkey: Pubkey,
    decryption_ctx: Option<&ValidatorDecryptionContext>,
    field_label: &str,
) -> ChainlinkResult<Vec<u8>> {
    let Some(decryption_ctx) = decryption_ctx else {
        return Err(ChainlinkError::InvalidDelegationActions(
            delegation_record_pubkey,
            format!(
                "Encrypted {field_label} present but validator decryption context is unavailable"
            ),
        ));
    };

    encryption::decrypt(
        encrypted,
        &decryption_ctx.x25519_pubkey,
        &decryption_ctx.x25519_secret,
    )
    .map_err(|err| {
        ChainlinkError::InvalidDelegationActions(
            delegation_record_pubkey,
            format!("Failed to decrypt {field_label}: {err}"),
        )
    })
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
                    Some(delegation_record_account) => {
                        this.parse_delegation_record(
                            delegation_record_account.data(),
                            delegation_record_pubkey,
                        )
                        .ok()
                    }
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
            error!(pubkey = %delegation_record_pubkey, error = %err, "Failed to unsubscribe from delegation record");
        }
    }

    res
}

#[cfg(test)]
mod tests {
    use dlp::args::{
        EncryptedBuffer, MaybeEncryptedInstruction, MaybeEncryptedIxData,
        MaybeEncryptedPubkey, PostDelegationActions,
    };
    use solana_program::pubkey::Pubkey;

    use super::*;

    fn serialize_record_with_actions(actions: PostDelegationActions) -> Vec<u8> {
        let record = DelegationRecord {
            owner: Pubkey::new_unique(),
            authority: Pubkey::new_unique(),
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

        let payload = serialize_record_with_actions(PostDelegationActions {
            signers: vec![signer],
            non_signers: vec![
                MaybeEncryptedPubkey::ClearText(program_id),
                MaybeEncryptedPubkey::ClearText(account),
            ],
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
        });

        let (_, actions) = parse_delegation_record(
            &payload,
            Pubkey::new_unique(),
            Pubkey::new_unique(),
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
        let decryption_ctx =
            ValidatorDecryptionContext::from_validator_keypair(&validator)
                .unwrap();

        let signer = Pubkey::new_unique();
        let program_id = Pubkey::new_unique();
        let account = Pubkey::new_unique();

        let encrypted_program_id = dlp_api::encryption::encrypt_ed25519_recipient(
            program_id.as_array(),
            validator.pubkey().as_array(),
        )
        .unwrap();
        let encrypted_suffix = dlp_api::encryption::encrypt_ed25519_recipient(
            &[3, 4, 5],
            validator.pubkey().as_array(),
        )
        .unwrap();

        let payload = serialize_record_with_actions(PostDelegationActions {
            signers: vec![signer],
            non_signers: vec![
                MaybeEncryptedPubkey::Encrypted(EncryptedBuffer::new(
                    encrypted_program_id,
                )),
                MaybeEncryptedPubkey::ClearText(account),
            ],
            instructions: vec![MaybeEncryptedInstruction {
                program_id: 1,
                accounts: vec![
                    dlp::compact::AccountMeta::new_readonly(0, true),
                    dlp::compact::AccountMeta::new(2, false),
                ],
                data: MaybeEncryptedIxData {
                    prefix: vec![1, 2],
                    suffix: EncryptedBuffer::new(encrypted_suffix),
                },
            }],
        });

        let (_, actions) = parse_delegation_record(
            &payload,
            Pubkey::new_unique(),
            validator.pubkey(),
            Some(&decryption_ctx),
        )
        .unwrap();

        let actions: Vec<Instruction> = actions.unwrap().into();
        assert_eq!(actions[0].program_id, program_id);
        assert_eq!(actions[0].accounts[0].pubkey, signer);
        assert_eq!(actions[0].accounts[1].pubkey, account);
        assert_eq!(actions[0].data, vec![1, 2, 3, 4, 5]);
    }

    #[test]
    fn fails_when_encrypted_actions_have_no_decryption_context() {
        let validator = Keypair::new();
        let encrypted_program_id = dlp_api::encryption::encrypt_ed25519_recipient(
            Pubkey::new_unique().as_array(),
            validator.pubkey().as_array(),
        )
        .unwrap();

        let payload = serialize_record_with_actions(PostDelegationActions {
            signers: vec![],
            non_signers: vec![MaybeEncryptedPubkey::Encrypted(
                EncryptedBuffer::new(encrypted_program_id),
            )],
            instructions: vec![],
        });

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
