use async_trait::async_trait;
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
use solana_sdk::hash::Hash;
use solana_sdk::signature::Signer;
use solana_sdk::{
    account::{AccountSharedData, ReadableAccount},
    pubkey::Pubkey,
    signature::Signature,
    transaction::Transaction,
};

pub struct ChainlinkCloner {
    tx_scheduler: TransactionSchedulerHandle,
    block: LatestBlock,
}

impl ChainlinkCloner {
    pub fn new(
        tx_scheduler: TransactionSchedulerHandle,
        block: LatestBlock,
    ) -> Self {
        Self {
            tx_scheduler,
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
        };
        InstructionUtils::modify_accounts(
            vec![account_modification],
            recent_blockhash,
        )
    }

    fn try_transaction_to_clone_program(
        &self,
        program: LoadedProgram,
        recent_blockhash: Hash,
    ) -> ClonerResult<Transaction> {
        use RemoteProgramLoader::*;
        match program.loader {
            V1 => {
                // BPF Loader (non-upgradeable) cannot be loaded via newer loaders,
                // thus we just copy the account as is. It won't be upgradeable.
                let program_modification = AccountModification {
                    pubkey: program.program_id,
                    lamports: Some(program.lamports()),
                    owner: Some(program.loader_id()),
                    rent_epoch: Some(0),
                    data: Some(program.program_data),
                    executable: Some(true),
                };
                Ok(InstructionUtils::modify_accounts(
                    vec![program_modification],
                    recent_blockhash,
                ))
            }
            _ => {
                let validator_kp = validator_authority();
                // All other versions are loaded via the LoaderV4, no matter what
                // the original loader was. We do this via a proper upgrade instruction.
                let deploy_ixs = program.try_into_deploy_ixs_v4()?;
                let tx = Transaction::new_signed_with_payer(
                    &deploy_ixs,
                    Some(&validator_kp.pubkey()),
                    &[&validator_kp],
                    recent_blockhash,
                );

                Ok(tx)
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
        let tx =
            self.try_transaction_to_clone_program(program, recent_blockhash)?;
        self.send_transaction(tx).await
    }
}
