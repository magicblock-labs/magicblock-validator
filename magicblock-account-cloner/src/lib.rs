use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use async_trait::async_trait;
use log::*;
use magicblock_accounts_db::AccountsDb;
use magicblock_chainlink::{
    cloner::{
        errors::{ClonerError, ClonerResult},
        Cloner,
    },
    remote_account_provider::program_account::{
        DeployableV4Program, LoadedProgram, RemoteProgramLoader,
    },
};
use magicblock_committor_service::{
    error::{CommittorServiceError, CommittorServiceResult},
    BaseIntentCommittor, CommittorService,
};
use magicblock_config::{AccountsCloneConfig, PrepareLookupTables};
use magicblock_core::link::transactions::TransactionSchedulerHandle;
use magicblock_ledger::LatestBlock;
use magicblock_magic_program_api::instruction::AccountModification;
use magicblock_program::{
    instruction_utils::InstructionUtils, validator::validator_authority,
};
use magicblock_rpc_client::MagicblockRpcClient;
use solana_sdk::{
    account::{AccountSharedData, ReadableAccount},
    hash::Hash,
    loader_v4,
    pubkey::Pubkey,
    rent::Rent,
    signature::{Signature, Signer},
    transaction::Transaction,
};
use tokio::sync::oneshot;

use crate::bpf_loader_v1::BpfUpgradableProgramModifications;

mod account_cloner;
mod bpf_loader_v1;

pub use account_cloner::*;

pub struct ChainlinkCloner {
    changeset_committor: Option<Arc<CommittorService>>,
    clone_config: AccountsCloneConfig,
    tx_scheduler: TransactionSchedulerHandle,
    accounts_db: Arc<AccountsDb>,
    block: LatestBlock,
}

impl ChainlinkCloner {
    pub fn new(
        changeset_committor: Option<Arc<CommittorService>>,
        clone_config: AccountsCloneConfig,
        tx_scheduler: TransactionSchedulerHandle,
        accounts_db: Arc<AccountsDb>,
        block: LatestBlock,
    ) -> Self {
        Self {
            changeset_committor,
            clone_config,
            tx_scheduler,
            accounts_db,
            block,
        }
    }

    async fn send_transaction(
        &self,
        tx: solana_sdk::transaction::Transaction,
    ) -> ClonerResult<Signature> {
        let sig = tx.signatures[0];
        self.tx_scheduler.execute(tx).await?;
        Ok(sig)
    }

    fn transaction_to_clone_regular_account(
        &self,
        pubkey: &Pubkey,
        account: &AccountSharedData,
        recent_blockhash: Hash,
    ) -> Transaction {
        let account_modification = AccountModification {
            pubkey: *pubkey,
            lamports: Some(account.lamports()),
            owner: Some(*account.owner()),
            rent_epoch: Some(account.rent_epoch()),
            data: Some(account.data().to_owned()),
            executable: Some(account.executable()),
            delegated: Some(account.delegated()),
        };
        InstructionUtils::modify_accounts(
            vec![account_modification],
            recent_blockhash,
        )
    }

    /// Creates a transaction to clone the given program into the validator.
    /// Handles the initial (and only) clone of a BPF Loader V1 program which is just
    /// cloned as is without running an upgrade instruction.
    /// Also see [magicblock_chainlink::chainlink::fetch_cloner::FetchCloner::handle_executable_sub_update]
    /// For all other loaders we use the LoaderV4 and run a deploy instruction.
    /// Returns None if the program is currently retracted on chain.
    fn try_transaction_to_clone_program(
        &self,
        program: LoadedProgram,
        recent_blockhash: Hash,
    ) -> ClonerResult<Option<Transaction>> {
        use RemoteProgramLoader::*;
        match program.loader {
            V1 => {
                // NOTE: we don't support modifying this kind of program once it was
                // deployed into our validator once.
                // By nature of being immutable on chain this should never happen.
                // Thus we avoid having to run the upgrade instruction and get
                // away with just directly modifying the program and program data accounts.
                debug!("Loading V1 program {}", program.program_id);
                let validator_kp = validator_authority();

                // BPF Loader (non-upgradeable) cannot be loaded via newer loaders,
                // thus we just copy the account as is. It won't be upgradeable.
                let modifications =
                    BpfUpgradableProgramModifications::try_from(&program)?;
                let mod_ix =
                    InstructionUtils::modify_accounts_instruction(vec![
                        modifications.program_id_modification,
                        modifications.program_data_modification,
                    ]);

                Ok(Some(Transaction::new_signed_with_payer(
                    &[mod_ix],
                    Some(&validator_kp.pubkey()),
                    &[&validator_kp],
                    recent_blockhash,
                )))
            }
            _ => {
                let validator_kp = validator_authority();
                // All other versions are loaded via the LoaderV4, no matter what
                // the original loader was. We do this via a proper deploy instruction.
                let program_id = program.program_id;

                // We don't allow users to retract the program in the ER, since in that case any
                // accounts of that program still in the ER could never be committed nor
                // undelegated
                if matches!(
                    program.loader_status,
                    loader_v4::LoaderV4Status::Retracted
                ) {
                    debug!(
                        "Program {} is retracted on chain, won't retract it. When it is deployed on chain we deploy the new version.",
                        program.program_id
                    );
                    return Ok(None);
                }
                debug!(
                    "Deploying program with V4 loader {}",
                    program.program_id
                );

                // Create and initialize the program account in retracted state
                // and then deploy it and finally set the authority to match the
                // one on chain
                let DeployableV4Program {
                    pre_deploy_loader_state,
                    deploy_instruction,
                    post_deploy_loader_state,
                } = program
                    .try_into_deploy_data_and_ixs_v4(validator_kp.pubkey())?;

                let lamports = Rent::default()
                    .minimum_balance(pre_deploy_loader_state.len());

                let disable_executable_check_instruction =
                    InstructionUtils::disable_executable_check_instruction(
                        &validator_kp.pubkey(),
                    );

                let pre_deploy_mod_instruction = {
                    let pre_deploy_mods = vec![AccountModification {
                        pubkey: program_id,
                        lamports: Some(lamports),
                        owner: Some(loader_v4::id()),
                        executable: Some(true),
                        data: Some(pre_deploy_loader_state),
                        ..Default::default()
                    }];
                    InstructionUtils::modify_accounts_instruction(
                        pre_deploy_mods,
                    )
                };

                let post_deploy_mod_instruction = {
                    let post_deploy_mods = vec![AccountModification {
                        pubkey: program_id,
                        data: Some(post_deploy_loader_state),
                        ..Default::default()
                    }];
                    InstructionUtils::modify_accounts_instruction(
                        post_deploy_mods,
                    )
                };

                let enable_executable_check_instruction =
                    InstructionUtils::enable_executable_check_instruction(
                        &validator_kp.pubkey(),
                    );

                let ixs = vec![
                    disable_executable_check_instruction,
                    pre_deploy_mod_instruction,
                    deploy_instruction,
                    post_deploy_mod_instruction,
                    enable_executable_check_instruction,
                ];
                let tx = Transaction::new_signed_with_payer(
                    &ixs,
                    Some(&validator_kp.pubkey()),
                    &[&validator_kp],
                    recent_blockhash,
                );

                Ok(Some(tx))
            }
        }
    }

    fn maybe_prepare_lookup_tables(&self, pubkey: Pubkey, owner: Pubkey) {
        // Allow the committer service to reserve pubkeys in lookup tables
        // that could be needed when we commit this account
        if let Some(committor) = self.changeset_committor.as_ref() {
            if self.clone_config.prepare_lookup_tables
                == PrepareLookupTables::Always
            {
                let committor = committor.clone();
                tokio::spawn(async move {
                    match Self::map_committor_request_result(
                        committor.reserve_pubkeys_for_committee(pubkey, owner),
                        &committor,
                    )
                    .await
                    {
                        Ok(initiated) => {
                            trace!(
                                "Reserving lookup keys for {pubkey} took {:?}",
                                initiated.elapsed()
                            );
                        }
                        Err(err) => {
                            error!("Failed to reserve lookup keys for {pubkey}: {err:?}");
                        }
                    };
                });
            }
        }
    }

    async fn map_committor_request_result(
        res: oneshot::Receiver<CommittorServiceResult<Instant>>,
        committor: &Arc<CommittorService>,
    ) -> ClonerResult<Instant> {
        match res.await.map_err(|err| {
            // Send request error
            ClonerError::CommittorServiceError(format!(
                "error sending request {err:?}"
            ))
        })? {
            Ok(val) => Ok(val),
            Err(err) => {
                // Commit error
                match err {
                    CommittorServiceError::TableManiaError(table_mania_err) => {
                        let Some(sig) = table_mania_err.signature() else {
                            return Err(ClonerError::CommittorServiceError(
                                format!("{:?}", table_mania_err),
                            ));
                        };
                        let (logs, cus) = if let Ok(Ok(transaction)) =
                            committor.get_transaction(&sig).await
                        {
                            let cus =
                                MagicblockRpcClient::get_cus_from_transaction(
                                    &transaction,
                                );
                            let logs =
                                MagicblockRpcClient::get_logs_from_transaction(
                                    &transaction,
                                );
                            (logs, cus)
                        } else {
                            (None, None)
                        };

                        let cus_str = cus
                            .map(|cus| format!("{:?}", cus))
                            .unwrap_or("N/A".to_string());
                        let logs_str = logs
                            .map(|logs| format!("{:#?}", logs))
                            .unwrap_or("N/A".to_string());
                        Err(ClonerError::CommittorServiceError(format!(
                            "{:?}\nCUs: {cus_str}\nLogs: {logs_str}",
                            table_mania_err
                        )))
                    }
                    _ => Err(ClonerError::CommittorServiceError(format!(
                        "{:?}",
                        err
                    ))),
                }
            }
        }
    }
}

#[async_trait]
impl Cloner for ChainlinkCloner {
    async fn clone_account(
        &self,
        pubkey: Pubkey,
        account: AccountSharedData,
    ) -> ClonerResult<Signature> {
        let recent_blockhash = self.block.load().blockhash;
        let tx = self.transaction_to_clone_regular_account(
            &pubkey,
            &account,
            recent_blockhash,
        );
        if account.delegated() {
            self.maybe_prepare_lookup_tables(pubkey, *account.owner());
        }
        self.send_transaction(tx).await.map_err(|err| {
            ClonerError::FailedToCloneRegularAccount(pubkey, Box::new(err))
        })
    }

    async fn clone_program(
        &self,
        program: LoadedProgram,
    ) -> ClonerResult<Signature> {
        let recent_blockhash = self.block.load().blockhash;
        let program_id = program.program_id;
        if let Some(tx) = self
            .try_transaction_to_clone_program(program, recent_blockhash)
            .map_err(|err| {
                ClonerError::FailedToCreateCloneProgramTransaction(
                    program_id,
                    Box::new(err),
                )
            })?
        {
            let res = self.send_transaction(tx).await.map_err(|err| {
                ClonerError::FailedToCloneProgram(program_id, Box::new(err))
            })?;
            // After cloning a program we need to wait at least one slot for it to become
            // usable, so we do that here
            let current_slot = self.accounts_db.slot();
            while self.accounts_db.slot() == current_slot {
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            Ok(res)
        } else {
            // No-op, program was retracted
            Ok(Signature::default())
        }
    }
}
