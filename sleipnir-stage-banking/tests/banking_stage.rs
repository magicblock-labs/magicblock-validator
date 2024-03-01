use std::sync::Arc;

use sleipnir_bank::{
    bank::Bank,
    bank_dev_utils::{
        init_logger,
        transactions::{create_funded_accounts, execute_transactions},
    },
    genesis_utils::{create_genesis_config, GenesisConfigInfo},
};
use sleipnir_stage_banking::{
    banking_stage::BankingStage, packet::BankingPacketBatch,
    transport::banking_tracer::BankingTracer,
};
use solana_perf::packet::{to_packet_batches, PacketBatch};
use solana_sdk::{
    native_token::LAMPORTS_PER_SOL, signature::Keypair, signer::Signer, system_transaction,
};

const LOG_MSGS_BYTE_LIMT: Option<usize> = None;

fn convert_from_old_verified(mut with_vers: Vec<(PacketBatch, Vec<u8>)>) -> Vec<PacketBatch> {
    with_vers.iter_mut().for_each(|(b, v)| {
        b.iter_mut()
            .zip(v)
            .for_each(|(p, f)| p.meta_mut().set_discard(*f == 0))
    });
    with_vers.into_iter().map(|(b, _)| b).collect()
}

#[test]
fn test_banking_stage_shutdown1() {
    init_logger();

    let genesis_config_info = create_genesis_config(u64::MAX);
    let bank = Bank::new_for_tests(&genesis_config_info.genesis_config);
    let bank = Arc::new(bank);

    let banking_tracer = BankingTracer::new_disabled();
    let (non_vote_sender, non_vote_receiver) = banking_tracer.create_channel_non_vote();

    let banking_stage = BankingStage::new(non_vote_receiver, None, LOG_MSGS_BYTE_LIMT, bank);
    drop(non_vote_sender);
    banking_stage.join().unwrap();
}

#[test]
fn test_banking_stage_without_transaction_status_sender() {
    init_logger();
    solana_logger::setup();
    let GenesisConfigInfo { genesis_config, .. } = create_genesis_config(u64::MAX);

    let bank = Bank::new_for_tests(&genesis_config);
    let start_hash = bank.last_blockhash();
    let bank = Arc::new(bank);

    let banking_tracer = BankingTracer::new_disabled();
    let (non_vote_sender, non_vote_receiver) = banking_tracer.create_channel_non_vote();

    // 1. Fund an account so we can send 2 good transactions in a single batch.
    let payer = create_funded_accounts(&bank, 2, Some(LAMPORTS_PER_SOL)).remove(0);

    // 2. Create the banking stage
    let banking_stage = BankingStage::new(non_vote_receiver, None, LOG_MSGS_BYTE_LIMT, bank);

    // 3. Create Transactions and send them via packets
    let to = solana_sdk::pubkey::new_rand();
    let tx = system_transaction::transfer(&payer, &to, 1, start_hash);

    let packet_batches = to_packet_batches(&[tx], 1);
    let packet_batches = packet_batches
        .into_iter()
        .map(|batch| (batch, vec![1u8]))
        .collect::<Vec<_>>();

    let packet_batches = convert_from_old_verified(packet_batches);
    non_vote_sender
        .send(BankingPacketBatch::new((packet_batches, None)))
        .unwrap();

    drop(non_vote_sender);
    banking_stage.join().unwrap();
}
