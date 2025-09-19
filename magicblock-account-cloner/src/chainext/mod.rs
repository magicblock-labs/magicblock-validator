use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use log::*;
use magicblock_accounts_db::AccountsDb;
use magicblock_chainlink::{
    cloner::{errors::ClonerResult, Cloner},
    remote_account_provider::program_account::{
        LoadedProgram, RemoteProgramLoader,
    },
};
use magicblock_core::link::transactions::TransactionSchedulerHandle;
use magicblock_ledger::LatestBlock;
use magicblock_mutator::AccountModification;
use magicblock_program::{
    instruction_utils::InstructionUtils, validator::validator_authority,
};
use solana_sdk::{
    account::{AccountSharedData, ReadableAccount},
    pubkey::Pubkey,
    signature::Signature,
    transaction::Transaction,
};
use solana_sdk::{hash::Hash, rent::Rent};
use solana_sdk::{loader_v4, signature::Signer};

use crate::chainext::bpf_loader_v1::BpfUpgradableProgramModifications;

mod bpf_loader_v1;

pub struct ChainlinkCloner {
    tx_scheduler: TransactionSchedulerHandle,
    accounts_db: Arc<AccountsDb>,
    block: LatestBlock,
}

impl ChainlinkCloner {
    pub fn new(
        tx_scheduler: TransactionSchedulerHandle,
        accounts_db: Arc<AccountsDb>,
        block: LatestBlock,
    ) -> Self {
        Self {
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
                // the original loader was. We do this via a proper upgrade instruction.
                let program_id = program.program_id;

                // We don't allow  users to retract the program in the ER, since in that case any
                // accounts of that program still in the ER could never be committed nor
                // undelegated
                if matches!(
                    program.loader_status,
                    loader_v4::LoaderV4Status::Retracted
                ) {
                    debug!(
                        "Program {} is retracted on chain, won't deploy until it is deployed on chain",
                        program.program_id
                    );
                    return Ok(None);
                }
                debug!(
                    "Deploying program with V4 loader {}",
                    program.program_id
                );

                // Create and initialize the program account in retracted state
                // and then deploy it
                let (loader_state, deploy_ix) = program
                    .try_into_deploy_data_and_ixs_v4(validator_kp.pubkey())?;

                let lamports =
                    Rent::default().minimum_balance(loader_state.len());

                let mods = vec![AccountModification {
                    pubkey: program_id,
                    lamports: Some(lamports),
                    owner: Some(loader_v4::id()),
                    executable: Some(true),
                    data: Some(loader_state),
                    ..Default::default()
                }];
                let init_program_account_ix =
                    InstructionUtils::modify_accounts_instruction(mods);

                let ixs = vec![init_program_account_ix, deploy_ix];
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
        self.send_transaction(tx).await
    }

    async fn clone_program(
        &self,
        program: LoadedProgram,
    ) -> ClonerResult<Signature> {
        let recent_blockhash = self.block.load().blockhash;
        if let Some(tx) =
            self.try_transaction_to_clone_program(program, recent_blockhash)?
        {
            let res = self.send_transaction(tx).await?;
            // After cloning a program we need to wait at least one slot for it to become
            // usable, so we do that here
            let current_slot = self.accounts_db.slot();
            while self.accounts_db.slot() == current_slot {
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            Ok(res)
        } else {
            Ok(Signature::default()) // No-op, program was retracted
        }
    }
}
