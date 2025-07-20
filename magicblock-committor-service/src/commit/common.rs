use std::{
    collections::{HashMap, HashSet},
    time::{Duration, Instant},
};

use log::*;
use magicblock_committor_program::Changeset;
use magicblock_rpc_client::{
    MagicBlockSendTransactionConfig, MagicblockRpcClient,
};
use magicblock_table_mania::TableMania;
use solana_pubkey::Pubkey;
use solana_sdk::{
    hash::Hash,
    instruction::Instruction,
    message::{v0::Message, VersionedMessage},
    signature::{Keypair, Signature},
    signer::Signer,
    transaction::VersionedTransaction,
};

use crate::{
    error::{CommittorServiceError, CommittorServiceResult},
    pubkeys_provider::{provide_committee_pubkeys, provide_common_pubkeys},
};

pub(crate) fn lookup_table_keys(
    authority: &Keypair,
    committees: &HashSet<Pubkey>,
    owners: &HashMap<Pubkey, Pubkey>,
) -> HashSet<Pubkey> {
    committees
        .iter()
        .flat_map(|x| provide_committee_pubkeys(x, owners.get(x)))
        .chain(provide_common_pubkeys(&authority.pubkey()))
        .collect::<HashSet<Pubkey>>()
}

/// Returns the pubkeys of the accounts that are marked for undelegation we finalized
/// the commits of those accounts.
/// If we didn't finalize the commits then we cannot yet undelegate those accounts.
/// Returns tuples of the account to undelegate and its original owner
pub(crate) fn get_accounts_to_undelegate(
    changeset: &Changeset,
    finalize: bool,
) -> Option<Vec<(Pubkey, Pubkey)>> {
    if finalize {
        let vec = changeset.accounts_to_undelegate.iter().flat_map(|x| {
            let Some(acc) = changeset.accounts.get(x) else {
                warn!("Account ({}) marked for undelegation not found in changeset", x);
                return None;
            };
            Some((*x, acc.owner()))
        }).collect::<Vec<_>>();
        (!vec.is_empty()).then_some(vec)
    } else {
        // if we don't finalize then we can only _mark_ accounts for undelegation
        // but cannot run the undelegation instruction itself
        None
    }
}

/// Gets the latest blockhash and sends and confirms a transaction with
/// the provided instructions.
/// Uses the commitment provided via the [ChainConfig::commitment] option when checking
/// the status of the transction signature.
/// - **rpc_client** - the rpc client to use
/// - **authority** - the authority to sign the transaction
/// - **ixs** - the instructions to include in the transaction
/// - **task_desc** - a description of the task included in logs
/// - **latest_blockhash** - the latest blockhash to use for the transaction,
///   if not provided it will be queried
/// - **send_config** - the send transaction config to use
/// - **use_table_mania** - whether to use table mania to optimize the size increase due
///   to accounts in the transaction via the use of lookup tables
///
/// Returns the signature of the transaction.
pub(crate) async fn send_and_confirm(
    rpc_client: MagicblockRpcClient,
    authority: Keypair,
    ixs: Vec<Instruction>,
    task_desc: String,
    latest_blockhash: Option<Hash>,
    send_config: MagicBlockSendTransactionConfig,
    table_mania_setup: Option<(&TableMania, HashSet<Pubkey>)>,
) -> CommittorServiceResult<Signature> {
    use CommittorServiceError::*;
    // When lots of txs are spawned in parallel we reuse the blockhash
    // instead of getting it for each tx
    let latest_blockhash = if let Some(blockhash) = latest_blockhash {
        blockhash
    } else {
        rpc_client.get_latest_blockhash().await.inspect_err(|err| {
            error!(
                "Failed to get latest blockhash to '{}': {:?}",
                task_desc, err
            )
        })?
    };

    let tables =
        if let Some((table_mania, keys_from_tables)) = table_mania_setup {
            let start = Instant::now();

            // Ensure all pubkeys have tables before proceeding
            table_mania
                .ensure_pubkeys_table(&authority, &keys_from_tables)
                .await?;

            // NOTE: we assume that all needed pubkeys were reserved by now
            let address_lookup_tables = table_mania
                .try_get_active_address_lookup_table_accounts(
                    &keys_from_tables,
                    // enough time for init/extend lookup table transaction to complete
                    Duration::from_secs(50),
                    // enough time for lookup table to finalize
                    Duration::from_secs(50),
                )
                .await?;

            if log_enabled!(Level::Trace) {
                let tables = address_lookup_tables
                    .iter()
                    .map(|table| {
                        format!(
                            "\n    {}: {} addresses",
                            table.key,
                            table.addresses.len()
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                trace!(
                    "Took {}ms to get finalized address lookup table(s) {}",
                    start.elapsed().as_millis(),
                    tables
                );
                let all_accounts = ixs
                    .iter()
                    .flat_map(|ix| ix.accounts.iter().map(|x| x.pubkey));
                let keys_not_from_table = all_accounts
                    .filter(|x| !keys_from_tables.contains(x))
                    .collect::<HashSet<_>>();
                trace!(
                    "{}/{} are provided from lookup tables",
                    keys_from_tables.len(),
                    keys_not_from_table.len() + keys_from_tables.len()
                );
                trace!(
                    "The following keys are not:\n{}",
                    keys_not_from_table
                        .iter()
                        .map(|x| format!("    {}", x))
                        .collect::<Vec<_>>()
                        .join("\n")
                );
            }

            address_lookup_tables
        } else {
            vec![]
        };

    let versioned_msg = match Message::try_compile(
        &authority.pubkey(),
        &ixs,
        &tables,
        latest_blockhash,
    ) {
        Ok(msg) => msg,
        Err(err) => {
            return Err(
                CommittorServiceError::FailedToCompileTransactionMessage(
                    task_desc.clone(),
                    err,
                ),
            );
        }
    };
    let tx = match VersionedTransaction::try_new(
        VersionedMessage::V0(versioned_msg),
        &[&authority],
    ) {
        Ok(tx) => tx,
        Err(err) => {
            return Err(CommittorServiceError::FailedToCreateTransaction(
                task_desc.clone(),
                err,
            ));
        }
    };

    let start = Instant::now();
    let res = rpc_client
        .send_transaction(&tx, &send_config)
        .await
        .map_err(|err| {
            FailedToSendAndConfirmTransaction(task_desc.clone(), err)
        })?;

    trace!(
        "Took {}ms to send and confirm transaction with {} instructions",
        start.elapsed().as_millis(),
        ixs.len()
    );

    if let Some(err) = res.error() {
        Err(EncounteredTransactionError(task_desc, err.clone()))
    } else {
        Ok(res.into_signature())
    }
}
