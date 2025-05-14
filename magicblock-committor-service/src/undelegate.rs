use std::collections::HashMap;

use dlp::state::DelegationMetadata;
use magicblock_rpc_client::MagicblockRpcClient;
use solana_account::ReadableAccount;
use solana_pubkey::Pubkey;
use solana_sdk::instruction::Instruction;

use crate::{
    error::{CommittorServiceError, CommittorServiceResult},
    transactions::{MAX_UNDELEGATE_PER_TX, MAX_UNDELEGATE_PER_TX_USING_LOOKUP},
    types::{InstructionsForCommitable, InstructionsKind},
    CommitInfo,
};

pub(crate) async fn undelegate_commitables_ixs(
    rpc_client: &MagicblockRpcClient,
    validator_auth: Pubkey,
    accs: Vec<(Pubkey, Pubkey)>,
) -> CommittorServiceResult<HashMap<Pubkey, Instruction>> {
    let delegation_metadata_pubkeys = accs
        .iter()
        .map(|(delegated_account, _)| {
            dlp::pda::delegation_metadata_pda_from_delegated_account(
                delegated_account,
            )
        })
        .collect::<Vec<_>>();
    let metadata_accs = rpc_client
        .get_multiple_accounts(&delegation_metadata_pubkeys, None)
        .await?;

    let mut ixs = HashMap::new();

    for (metadata_acc, (committee, owner)) in
        metadata_accs.iter().zip(accs.iter())
    {
        let Some(metadata_acc) = metadata_acc else {
            return Err(
                CommittorServiceError::FailedToFetchDelegationMetadata(
                    *committee,
                ),
            );
        };
        let metadata = DelegationMetadata::try_from_bytes_with_discriminator(
            metadata_acc.data(),
        )
        .map_err(|err| {
            CommittorServiceError::FailedToDeserializeDelegationMetadata(
                *committee, err,
            )
        })?;

        ixs.insert(
            *committee,
            dlp::instruction_builder::undelegate(
                validator_auth,
                *committee,
                *owner,
                metadata.rent_payer,
            ),
        );
    }
    Ok(ixs)
}

pub(crate) fn chunked_ixs_to_undelegate_commitables(
    mut ixs: HashMap<Pubkey, Instruction>,
    commit_infos: Vec<CommitInfo>,
    use_lookup: bool,
) -> Vec<Vec<InstructionsForCommitable>> {
    let max_per_chunk = if use_lookup {
        MAX_UNDELEGATE_PER_TX_USING_LOOKUP
    } else {
        MAX_UNDELEGATE_PER_TX
    };

    let chunks = commit_infos
        .chunks(max_per_chunk as usize)
        .map(|chunk| {
            chunk
                .iter()
                .flat_map(|commit_info| {
                    ixs.remove(&commit_info.pubkey()).map(|ix| {
                        InstructionsForCommitable {
                            instructions: vec![ix],
                            commit_info: commit_info.clone(),
                            kind: InstructionsKind::Undelegate,
                        }
                    })
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();

    debug_assert!(
        ixs.is_empty(),
        "BUG: Some undelegate instructions {:?} were not matched with a commit_info: {:?}",
        ixs, commit_infos
    );

    chunks
}
