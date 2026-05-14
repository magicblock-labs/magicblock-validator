use dlp_api::{
    pda::delegation_record_pda_from_delegated_account, state::DelegationRecord,
};
use solana_pubkey::Pubkey;
use solana_rpc_client_api::{client_error, config::RpcAccountInfoConfig};
use tracing::{debug, trace, warn};

use crate::remote_account_provider::{
    pubsub_common::is_internal_dlp_account_data, ChainRpcClient,
};

pub(crate) fn is_delegated_to_validator_or_confined(
    authority: &Pubkey,
    validator_pubkey: &Pubkey,
) -> bool {
    authority == validator_pubkey || authority == &Pubkey::default()
}

pub(crate) fn parse_delegation_record_header(
    data: &[u8],
) -> Option<DelegationRecord> {
    let delegation_record_size = DelegationRecord::size_with_discriminator();
    if data.len() < delegation_record_size {
        return None;
    }

    DelegationRecord::try_from_bytes_with_discriminator(
        &data[..delegation_record_size],
    )
    .copied()
    .ok()
}

#[derive(Debug)]
pub(crate) enum FetchError {
    Transport(client_error::Error),
    Deserialize,
}

pub(crate) async fn fetch_delegation_record_header<T: ChainRpcClient>(
    rpc_client: &T,
    delegated_account_pubkey: Pubkey,
    min_context_slot: u64,
) -> Result<Option<DelegationRecord>, FetchError> {
    let delegation_record_pubkey =
        delegation_record_pda_from_delegated_account(&delegated_account_pubkey);
    let Some(account) = rpc_client
        .get_account_with_config(
            &delegation_record_pubkey,
            RpcAccountInfoConfig {
                commitment: Some(rpc_client.commitment()),
                min_context_slot: Some(min_context_slot),
                ..Default::default()
            },
        )
        .await
        .map_err(FetchError::Transport)?
        .value
    else {
        return Ok(None);
    };

    if account.data.len() < DelegationRecord::size_with_discriminator() {
        return Ok(None);
    }

    parse_delegation_record_header(&account.data)
        .map(Some)
        .ok_or(FetchError::Deserialize)
}

pub(crate) async fn should_forward_dlp_program_update<T: ChainRpcClient>(
    rpc_client: &T,
    validator_pubkey: &Pubkey,
    delegated_account_pubkey: Pubkey,
    owner: &Pubkey,
    account_data: &[u8],
    min_context_slot: u64,
) -> bool {
    if owner != &dlp_api::id() {
        trace!(pubkey = %delegated_account_pubkey, owner = %owner, "Dropping non-DLP program update");
        return false;
    }
    if is_internal_dlp_account_data(account_data) {
        trace!(pubkey = %delegated_account_pubkey, "Dropping internal DLP program update");
        return false;
    }

    let record = match fetch_delegation_record_header(
        rpc_client,
        delegated_account_pubkey,
        min_context_slot,
    )
    .await
    {
        Ok(Some(record)) => record,
        Ok(None) => {
            // Preserve greedy cloning when the subscription update wins the race
            // against delegation-record visibility. FetchCloner refetches the
            // record before cloning and only receives actions for this validator.
            trace!(pubkey = %delegated_account_pubkey, slot = min_context_slot, "Forwarding DLP program update without visible delegation record");
            return true;
        }
        Err(FetchError::Transport(err)) => {
            debug!(pubkey = %delegated_account_pubkey, slot = min_context_slot, error = %err, "Forwarding DLP program update after delegation record fetch error");
            return true;
        }
        Err(FetchError::Deserialize) => {
            warn!(pubkey = %delegated_account_pubkey, slot = min_context_slot, "Dropping DLP program update after invalid delegation record data");
            return false;
        }
    };

    if is_delegated_to_validator_or_confined(
        &record.authority,
        validator_pubkey,
    ) {
        true
    } else {
        debug!(pubkey = %delegated_account_pubkey, authority = %record.authority, "Dropping DLP program update delegated elsewhere");
        false
    }
}

#[cfg(test)]
mod tests {
    use dlp_api::pda::delegation_record_pda_from_delegated_account;
    use solana_account::Account;
    use solana_pubkey::Pubkey;

    use super::*;
    use crate::testing::rpc_client_mock::ChainRpcClientMockBuilder;

    fn serialize_valid_delegation_record(authority: Pubkey) -> Vec<u8> {
        let record = DelegationRecord {
            owner: Pubkey::new_unique(),
            authority,
            commit_frequency_ms: 1_000,
            delegation_slot: 42,
            lamports: 1_000_000,
        };
        let mut data = vec![0; DelegationRecord::size_with_discriminator()];
        record.to_bytes_with_discriminator(&mut data).unwrap();
        data
    }

    fn append_trailing_bytes(mut data: Vec<u8>, trailing: &[u8]) -> Vec<u8> {
        data.extend_from_slice(trailing);
        data
    }

    fn make_dlp_owned_non_internal_account_data() -> Vec<u8> {
        vec![0xde, 0xad, 0xbe, 0xef]
    }

    fn make_dlp_owned_delegated_account_payload() -> Account {
        Account {
            lamports: 1,
            data: make_dlp_owned_non_internal_account_data(),
            owner: dlp_api::id(),
            executable: false,
            rent_epoch: 0,
        }
    }

    #[test]
    fn predicate_matrix() {
        let validator_pubkey = Pubkey::new_unique();
        assert!(is_delegated_to_validator_or_confined(
            &validator_pubkey,
            &validator_pubkey
        ));
        assert!(is_delegated_to_validator_or_confined(
            &Pubkey::default(),
            &validator_pubkey
        ));
        assert!(!is_delegated_to_validator_or_confined(
            &Pubkey::new_unique(),
            &validator_pubkey
        ));
    }

    #[test]
    fn parse_header_accepts_trailing_bytes() {
        let authority = Pubkey::new_unique();
        let record = DelegationRecord {
            owner: Pubkey::new_unique(),
            authority,
            commit_frequency_ms: 7,
            delegation_slot: 9,
            lamports: 11,
        };
        let mut data = vec![0; DelegationRecord::size_with_discriminator()];
        record.to_bytes_with_discriminator(&mut data).unwrap();
        let data = append_trailing_bytes(data, &[1, 2, 3, 4]);

        assert_eq!(parse_delegation_record_header(&data), Some(record));
    }

    #[tokio::test]
    async fn fetch_header_handles_missing_stale_short_and_invalid() {
        let delegated_account_pubkey = Pubkey::new_unique();
        let delegation_record_pubkey =
            delegation_record_pda_from_delegated_account(
                &delegated_account_pubkey,
            );
        let validator_pubkey = Pubkey::new_unique();
        let valid_record = serialize_valid_delegation_record(validator_pubkey);
        let valid_account = Account {
            lamports: 1,
            data: valid_record.clone(),
            owner: dlp_api::id(),
            executable: false,
            rent_epoch: 0,
        };

        let missing_client = ChainRpcClientMockBuilder::new().slot(10).build();
        assert!(matches!(
            fetch_delegation_record_header(
                &missing_client,
                delegated_account_pubkey,
                10,
            )
            .await,
            Ok(None)
        ));

        let stale_client = ChainRpcClientMockBuilder::new()
            .slot(5)
            .account(delegation_record_pubkey, valid_account.clone())
            .build();
        assert!(matches!(
            fetch_delegation_record_header(
                &stale_client,
                delegated_account_pubkey,
                10,
            )
            .await,
            Err(FetchError::Transport(_))
        ));

        let short_client = ChainRpcClientMockBuilder::new()
            .slot(10)
            .account(
                delegation_record_pubkey,
                Account {
                    lamports: 1,
                    data: vec![1, 2, 3],
                    owner: dlp_api::id(),
                    executable: false,
                    rent_epoch: 0,
                },
            )
            .build();
        assert!(matches!(
            fetch_delegation_record_header(
                &short_client,
                delegated_account_pubkey,
                10,
            )
            .await,
            Ok(None)
        ));

        let invalid_client = ChainRpcClientMockBuilder::new()
            .slot(10)
            .account(
                delegation_record_pubkey,
                Account {
                    lamports: 1,
                    data: {
                        let mut data = valid_record;
                        data[0] ^= 0xff;
                        data
                    },
                    owner: dlp_api::id(),
                    executable: false,
                    rent_epoch: 0,
                },
            )
            .build();
        assert!(matches!(
            fetch_delegation_record_header(
                &invalid_client,
                delegated_account_pubkey,
                10,
            )
            .await,
            Err(FetchError::Deserialize)
        ));
    }

    #[tokio::test]
    async fn should_forward_dlp_program_update_matrix() {
        let delegated_account_pubkey = Pubkey::new_unique();
        let delegation_record_pubkey =
            delegation_record_pda_from_delegated_account(
                &delegated_account_pubkey,
            );
        let validator_pubkey = Pubkey::new_unique();
        let delegated_elsewhere = Pubkey::new_unique();

        let rpc_client = ChainRpcClientMockBuilder::new()
            .slot(10)
            .account(
                delegation_record_pubkey,
                Account {
                    lamports: 1,
                    data: serialize_valid_delegation_record(validator_pubkey),
                    owner: dlp_api::id(),
                    executable: false,
                    rent_epoch: 0,
                },
            )
            .build();

        let delegated_account = make_dlp_owned_delegated_account_payload();
        let internal_data = serialize_valid_delegation_record(validator_pubkey);

        assert!(
            should_forward_dlp_program_update(
                &rpc_client,
                &validator_pubkey,
                delegated_account_pubkey,
                &delegated_account.owner,
                delegated_account.data.as_slice(),
                10,
            )
            .await
        );
        assert_eq!(rpc_client.single_account_fetches(), 1);

        let elsewhere_rpc = ChainRpcClientMockBuilder::new()
            .slot(10)
            .account(
                delegation_record_pubkey,
                Account {
                    lamports: 1,
                    data: serialize_valid_delegation_record(
                        delegated_elsewhere,
                    ),
                    owner: dlp_api::id(),
                    executable: false,
                    rent_epoch: 0,
                },
            )
            .build();
        assert!(
            !should_forward_dlp_program_update(
                &elsewhere_rpc,
                &validator_pubkey,
                delegated_account_pubkey,
                &delegated_account.owner,
                delegated_account.data.as_slice(),
                10,
            )
            .await
        );
        assert_eq!(elsewhere_rpc.single_account_fetches(), 1);

        let missing_record_rpc =
            ChainRpcClientMockBuilder::new().slot(10).build();
        assert!(
            should_forward_dlp_program_update(
                &missing_record_rpc,
                &validator_pubkey,
                delegated_account_pubkey,
                &delegated_account.owner,
                delegated_account.data.as_slice(),
                10,
            )
            .await
        );
        assert_eq!(missing_record_rpc.single_account_fetches(), 1);

        let stale_record_rpc = ChainRpcClientMockBuilder::new()
            .slot(5)
            .account(
                delegation_record_pubkey,
                Account {
                    lamports: 1,
                    data: serialize_valid_delegation_record(validator_pubkey),
                    owner: dlp_api::id(),
                    executable: false,
                    rent_epoch: 0,
                },
            )
            .build();
        assert!(
            should_forward_dlp_program_update(
                &stale_record_rpc,
                &validator_pubkey,
                delegated_account_pubkey,
                &delegated_account.owner,
                delegated_account.data.as_slice(),
                10,
            )
            .await
        );
        assert_eq!(stale_record_rpc.single_account_fetches(), 1);

        let unconfined_rpc = ChainRpcClientMockBuilder::new()
            .slot(10)
            .account(
                delegation_record_pubkey,
                Account {
                    lamports: 1,
                    data: serialize_valid_delegation_record(Pubkey::default()),
                    owner: dlp_api::id(),
                    executable: false,
                    rent_epoch: 0,
                },
            )
            .build();
        assert!(
            should_forward_dlp_program_update(
                &unconfined_rpc,
                &validator_pubkey,
                delegated_account_pubkey,
                &delegated_account.owner,
                delegated_account.data.as_slice(),
                10,
            )
            .await
        );
        assert_eq!(unconfined_rpc.single_account_fetches(), 1);

        let internal_rpc = ChainRpcClientMockBuilder::new().slot(10).build();
        assert!(
            !should_forward_dlp_program_update(
                &internal_rpc,
                &validator_pubkey,
                delegated_account_pubkey,
                &delegated_account.owner,
                internal_data.as_slice(),
                10,
            )
            .await
        );
        assert_eq!(internal_rpc.single_account_fetches(), 0);
    }
}
