use std::{sync::Arc, thread};

use log::error;
use magicblock_accounts_db::AccountsDb;
use magicblock_core::{
    link::{
        blocks::{BlockMeta, BlockUpdate, BlockUpdateTx},
        link,
        transactions::{
            SanitizeableTransaction, TransactionResult,
            TransactionSchedulerHandle, TransactionSimulationResult,
        },
        DispatchEndpoints,
    },
    magic_program::Pubkey,
    Slot,
};
use magicblock_ledger::Ledger;
use magicblock_processor::{
    build_svm_env,
    scheduler::{state::TransactionSchedulerState, TransactionScheduler},
};
use solana_account::AccountSharedData;
use solana_keypair::Keypair;
use solana_program::{hash::Hasher, native_token::LAMPORTS_PER_SOL};
use solana_signature::Signature;
pub use solana_signer::Signer;
use solana_transaction::Transaction;
use solana_transaction_status_client_types::TransactionStatusMeta;
use tempfile::TempDir;

pub use guinea;
pub use solana_instruction::*;

pub struct ExecutionTestEnv {
    pub payer: Keypair,
    pub accountsdb: Arc<AccountsDb>,
    pub ledger: Arc<Ledger>,
    pub transaction_scheduler: TransactionSchedulerHandle,
    pub dir: TempDir,
    pub dispatch: DispatchEndpoints,
    pub blocks_tx: BlockUpdateTx,
}

impl ExecutionTestEnv {
    pub fn new() -> Self {
        init_logger!();
        let dir =
            tempfile::tempdir().expect("creating temp dir for validator state");
        let accountsdb = Arc::new(
            AccountsDb::open(dir.path()).expect("opening test accountsdb"),
        );
        let ledger =
            Arc::new(Ledger::open(dir.path()).expect("opening test ledger"));
        let (dispatch, validator_channels) = link();
        let blockhash = ledger.latest_block().load().blockhash;
        let environment = build_svm_env(&accountsdb, blockhash, 0);
        let payer = Keypair::new();
        let this = Self {
            payer,
            accountsdb: accountsdb.clone(),
            ledger: ledger.clone(),
            transaction_scheduler: dispatch.transaction_scheduler.clone(),
            dir,
            dispatch,
            blocks_tx: validator_channels.block_update,
        };
        this.advance_slot();
        let scheduler_state = TransactionSchedulerState {
            accountsdb,
            ledger,
            account_update_tx: validator_channels.account_update,
            transaction_status_tx: validator_channels.transaction_status,
            txn_to_process_rx: validator_channels.transaction_to_process,
            environment,
        };
        scheduler_state
            .load_upgradeable_programs(&[(
                guinea::ID,
                "../programs/elfs/guinea.so".into(),
            )])
            .expect("failed to load test programs into test env");
        TransactionScheduler::new(1, scheduler_state).spawn();

        this.fund_account(this.payer.pubkey(), LAMPORTS_PER_SOL);
        this
    }

    pub fn create_account_with_config(
        &self,
        lamports: u64,
        space: usize,
        owner: Pubkey,
    ) -> Keypair {
        let keypair = Keypair::new();
        let mut account = AccountSharedData::new(lamports, space, &owner);
        account.set_delegated(true);
        self.accountsdb.insert_account(&keypair.pubkey(), &account);
        keypair
    }

    pub fn create_account(&self, lamports: u64) -> Keypair {
        self.create_account_with_config(lamports, 0, Default::default())
    }

    pub fn fund_account(&self, pubkey: Pubkey, lamports: u64) {
        let mut account =
            AccountSharedData::new(lamports, 0, &Default::default());
        account.set_delegated(true);
        self.accountsdb.insert_account(&pubkey, &account);
    }

    pub fn get_transaction(
        &self,
        sig: Signature,
    ) -> Option<TransactionStatusMeta> {
        self.ledger
            .get_transaction_status(sig, u64::MAX)
            .expect("failed to get transaction meta from ledger")
            .map(|(_, m)| m)
    }

    pub fn advance_slot(&self) -> Slot {
        let block = self.ledger.latest_block();
        let b = block.load();
        let slot = b.slot + 1;
        let hash = {
            let mut hasher = Hasher::default();
            hasher.hash(b.blockhash.as_ref());
            hasher.hash(&b.slot.to_le_bytes());
            hasher.result()
        };
        let time = slot as i64;
        self.ledger
            .write_block(slot, time, hash)
            .expect("failed to write new block to the ledger");
        self.accountsdb.set_slot(slot);
        let _ = self.blocks_tx.send(BlockUpdate {
            hash,
            meta: BlockMeta { slot, time },
        });
        // allow transaction executor to register slot advancement
        thread::yield_now();

        slot
    }

    pub fn build_transaction(&self, ixs: &[Instruction]) -> Transaction {
        Transaction::new_signed_with_payer(
            ixs,
            Some(&self.payer.pubkey()),
            &[&self.payer],
            self.ledger.latest_blockhash(),
        )
    }

    pub async fn execute_transaction(
        &self,
        txn: impl SanitizeableTransaction,
    ) -> TransactionResult {
        self.transaction_scheduler
            .execute(txn)
            .await
            .inspect_err(|err| error!("failed to execute transaction: {err}"))
    }

    pub async fn simulate_transaction(
        &self,
        txn: impl SanitizeableTransaction,
    ) -> TransactionSimulationResult {
        let result = self
            .transaction_scheduler
            .simulate(txn)
            .await
            .expect("transaction executor has shutdown during test");
        if let Err(ref err) = result.result {
            error!("failed to simulate transaction: {err}")
        }
        result
    }

    pub async fn replay_transaction(
        &self,
        txn: impl SanitizeableTransaction,
    ) -> TransactionResult {
        self.transaction_scheduler
            .replay(txn)
            .await
            .inspect_err(|err| error!("failed to replay transaction: {err}"))
    }
}

pub mod macros;
