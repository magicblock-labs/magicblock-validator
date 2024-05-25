use std::{
    sync::{Arc, RwLock},
    thread::JoinHandle,
};

use crossbeam_channel::unbounded;
use sleipnir_bank::{
    bank::Bank,
    genesis_utils::{create_genesis_config, GenesisConfigInfo},
};
use sleipnir_messaging::{banking_tracer::BankingTracer, BankingPacketBatch};
use sleipnir_stage_banking::banking_stage::BankingStage;
use sleipnir_transaction_status::{
    TransactionStatusMessage, TransactionStatusSender,
};
use solana_perf::packet::{to_packet_batches, PacketBatch};
use solana_sdk::{
    pubkey::Pubkey,
    transaction::{SanitizedTransaction, Transaction},
};

use crate::{
    bank::bank_for_tests,
    traits::{TransactionsProcessor, TransactionsProcessorProcessResult},
    transaction::sanitized_into_transaction,
};

// NOTE: we could make this live inside sleipnir-stage-baking for the sole reason that
// we could use it in there as well

const DEFAULT_SEND_CHUNK_SIZE: usize = 100;
const DEFAULT_LOG_MSGS_BYTE_LIMT: Option<usize> = None;
const DEFAULT_TIMEOUT_MILLIS: u64 = 5000;
pub struct BankingStageTransactionsProcessorConfig {
    pub send_chunk_size: usize,
    pub log_msgs_byte_limit: Option<usize>,
    pub timeout_millis: u64,
}

impl Default for BankingStageTransactionsProcessorConfig {
    fn default() -> Self {
        Self {
            send_chunk_size: DEFAULT_SEND_CHUNK_SIZE,
            log_msgs_byte_limit: DEFAULT_LOG_MSGS_BYTE_LIMT,
            timeout_millis: DEFAULT_TIMEOUT_MILLIS,
        }
    }
}

pub struct BankingStageTransactionsProcessor {
    config: BankingStageTransactionsProcessorConfig,
    pub bank: Arc<Bank>,
}

impl BankingStageTransactionsProcessor {
    pub fn new(config: BankingStageTransactionsProcessorConfig) -> Self {
        let GenesisConfigInfo { genesis_config, .. } =
            create_genesis_config(u64::MAX, &Pubkey::new_unique());
        let bank = bank_for_tests(&genesis_config, None, None);
        let bank = Arc::new(bank);

        Self { config, bank }
    }
}

impl Default for BankingStageTransactionsProcessor {
    fn default() -> Self {
        Self::new(BankingStageTransactionsProcessorConfig::default())
    }
}

impl TransactionsProcessor for BankingStageTransactionsProcessor {
    fn process(
        &self,
        transactions: Vec<Transaction>,
    ) -> Result<TransactionsProcessorProcessResult, String> {
        // 1. Track Transaction Execution
        let banking_tracer = BankingTracer::new_disabled();
        let (non_vote_sender, non_vote_receiver) =
            banking_tracer.create_channel_non_vote();

        let result =
            Arc::<RwLock<TransactionsProcessorProcessResult>>::default();
        let (transaction_status_sender, tx_status_thread) =
            track_transactions(result.clone());

        let banking_stage = BankingStage::new(
            non_vote_receiver,
            Some(transaction_status_sender),
            self.config.log_msgs_byte_limit,
            self.bank.clone(),
            None,
        );

        // 2. Create Packet Batches from Transactions
        let packet_batches =
            to_packet_batches(&transactions, self.config.send_chunk_size);
        let packet_batches = packet_batches
            .into_iter()
            .map(|batch| (batch, vec![1u8]))
            .collect::<Vec<_>>();

        let packet_batches = convert_from_old_verified(packet_batches);

        // 3. Send Packet Batches to BankingStage
        non_vote_sender
            .send(BankingPacketBatch::new((packet_batches, None)))
            .unwrap();

        // 4. Wait for Transactions to be Processed
        let max_ticks = self.config.timeout_millis / 10;
        let mut tick = 0;
        loop {
            let num_received = result.read().unwrap().len();
            if num_received >= transactions.len() {
                break;
            }
            if tick >= max_ticks {
                return Err(format!(
                    "TransactionsProcessor:process failed to process all transactions before timing out.  num_received: {}  transactions.len: {}",
                    num_received, transactions.len()
                ));
            }
            tick += 1;
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        // 5. Shut all threads down
        drop(non_vote_sender);
        banking_stage.join().unwrap();
        tx_status_thread.join().unwrap();

        // 6. Return the processed transactions
        let transactions =
            result.write().unwrap().transactions.drain().collect();
        let balances = result.write().unwrap().balances.drain(0..).collect();
        Ok(TransactionsProcessorProcessResult {
            transactions,
            balances,
        })
    }

    fn process_sanitized(
        &self,
        transactions: Vec<SanitizedTransaction>,
    ) -> Result<TransactionsProcessorProcessResult, String> {
        let transactions = transactions
            .into_iter()
            .map(sanitized_into_transaction)
            .collect();
        self.process(transactions)
    }

    fn bank(&self) -> &Bank {
        &self.bank
    }
}

fn track_transactions(
    result: Arc<RwLock<TransactionsProcessorProcessResult>>,
) -> (TransactionStatusSender, JoinHandle<()>) {
    let (transaction_status_sender, transaction_status_receiver) = unbounded();
    let transaction_status_sender = TransactionStatusSender {
        sender: transaction_status_sender,
    };
    let tx_status_handle = std::thread::spawn(move || {
        let transaction_status_receiver = transaction_status_receiver;
        loop {
            let status = transaction_status_receiver.recv();
            match status {
                Ok(TransactionStatusMessage::Batch(batch)) => {
                    result.write().unwrap().balances.push(batch.balances);
                    for (idx, tx) in batch.transactions.into_iter().enumerate()
                    {
                        result.write().unwrap().transactions.insert(
                            *tx.signature(),
                            (
                                tx,
                                batch
                                    .execution_results
                                    .get(idx)
                                    .cloned()
                                    .unwrap()
                                    .unwrap(),
                            ),
                        );
                    }
                }
                Err(_) => {
                    // disconnected
                    break;
                }
                other => panic!("Should never encounter other: {:?}", other),
            }
        }
    });
    (transaction_status_sender, tx_status_handle)
}

fn convert_from_old_verified(
    mut with_vers: Vec<(PacketBatch, Vec<u8>)>,
) -> Vec<PacketBatch> {
    with_vers.iter_mut().for_each(|(b, v)| {
        b.iter_mut()
            .zip(v)
            .for_each(|(p, f)| p.meta_mut().set_discard(*f == 0))
    });
    with_vers.into_iter().map(|(b, _)| b).collect()
}

#[cfg(test)]
mod tests {
    use sleipnir_bank::bank_dev_utils::transactions::create_funded_accounts;
    use solana_sdk::{
        native_token::LAMPORTS_PER_SOL, pubkey::Pubkey, system_transaction,
    };

    use super::*;
    use crate::{diagnostics::log_exec_details, init_logger};

    #[tokio::test]
    async fn test_system_transfer_enough_funds() {
        init_logger!();
        let tx_processor = BankingStageTransactionsProcessor::default();
        let payers = create_funded_accounts(
            &tx_processor.bank,
            1,
            Some(LAMPORTS_PER_SOL),
        );
        let start_hash = tx_processor.bank.last_blockhash();
        let to = Pubkey::new_unique();
        let tx = system_transaction::transfer(
            &payers[0],
            &to,
            890_880_000,
            start_hash,
        );
        let result = tx_processor.process(vec![tx]).unwrap();

        assert_eq!(result.len(), 1);

        let (tx, _) = result.transactions.values().next().unwrap();
        assert_eq!(tx.signatures().len(), 1);
        assert_eq!(tx.message().account_keys().len(), 3);

        let status = tx_processor
            .bank
            .get_signature_status(&tx.signatures()[0])
            .unwrap();
        assert!(status.is_ok());
    }

    #[tokio::test]
    async fn test_system_transfer_not_enough_funds() {
        init_logger!();
        let tx_processor = BankingStageTransactionsProcessor::default();
        let payers =
            create_funded_accounts(&tx_processor.bank, 1, Some(890_850_000));
        let start_hash = tx_processor.bank.last_blockhash();
        let to = Pubkey::new_unique();
        let tx = system_transaction::transfer(
            &payers[0],
            &to,
            890_880_000,
            start_hash,
        );
        let result = tx_processor.process(vec![tx]).unwrap();

        assert_eq!(result.len(), 1);

        let (tx, exec_details) = result.transactions.values().next().unwrap();
        assert_eq!(tx.signatures().len(), 1);
        assert_eq!(tx.message().account_keys().len(), 3);

        let status = tx_processor
            .bank
            .get_signature_status(&tx.signatures()[0])
            .unwrap();
        assert!(status.is_err());

        log_exec_details(exec_details);
    }
}
