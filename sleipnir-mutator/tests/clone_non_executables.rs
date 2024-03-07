use std::str::FromStr;

use log::*;
use sleipnir_bank::bank_dev_utils::elfs;
use sleipnir_bank::bank_dev_utils::transactions::create_solx_send_post_transaction;
use solana_sdk::account::Account;
use solana_sdk::native_token::LAMPORTS_PER_SOL;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::transaction::Transaction;
use solana_sdk::{bpf_loader_upgradeable, system_program};
use test_tools::{init_logger, transactions_processor};

use assert_matches::assert_matches;
use sleipnir_mutator::Mutator;
use sleipnir_program::sleipnir_authority_id;
use solana_sdk::{genesis_config::ClusterType, hash::Hash};
use test_tools::account::{fund_account_addr, get_account_addr};
use test_tools::diagnostics::log_exec_details;
use test_tools::traits::TransactionsProcessor;

const SOLX_PROG: &str = "SoLXmnP9JvL6vJ7TN1VqtTxqsc2izmPfF9CsMDEuRzJ";
const SOLX_EXEC: &str = "J1ct2BY6srXCDMngz5JxkX3sHLwCqGPhy9FiJBc8nuwk";
const SOLX_IDL: &str = "EgrsyMAsGYMKjcnTvnzmpJtq3hpmXznKQXk21154TsaS";
const SOLX_TIPS: &str = "SoLXtipsYqzgFguFCX6vw3JCtMChxmMacWdTpz2noRX";
const SOLX_POST: &str = "5eYk1TwtEwsUTqF9FHhm6tdmvu45csFkKbC4W217TAts";
const LUZIFER: &str = "LuzifKo4E6QCF5r4uQmqbyko7zLS5WgayynivnCbtzk";

fn fund_luzifer(bank: &Box<dyn TransactionsProcessor>) {
    // TODO: we need to fund Luzifer at startup instead of doing it here
    fund_account_addr(bank.bank(), LUZIFER, u64::MAX / 2);
}

async fn verified_tx_to_clone_from_devnet(addr: &str, num_accounts_expected: usize) -> Transaction {
    let mutator = Mutator::default();

    let recent_blockhash = Hash::default();
    let tx = mutator
        .transaction_to_clone_account_from_cluster(ClusterType::Devnet, addr, recent_blockhash)
        .await
        .expect("Failed to create clone transaction");

    assert!(tx.is_signed());
    assert_eq!(tx.signatures.len(), 1);
    assert_eq!(tx.signer_key(0, 0).unwrap(), &sleipnir_authority_id());
    assert_eq!(tx.message().account_keys.len(), num_accounts_expected);

    tx
}

#[tokio::test]
async fn clone_non_executable_without_data() {
    init_logger!();

    let tx_processor = transactions_processor();
    fund_luzifer(&tx_processor);

    let tx = verified_tx_to_clone_from_devnet(SOLX_TIPS, 3).await;
    let result = tx_processor.process(vec![tx]).unwrap();

    let (_, exec_details) = result.transactions.values().next().unwrap();
    log_exec_details(exec_details);
    let solx_tips: Account = get_account_addr(tx_processor.bank(), SOLX_TIPS)
        .unwrap()
        .into();

    trace!("SolxTips account: {:#?}", solx_tips);

    assert_matches!(
        solx_tips,
        Account {
            lamports: l,
            data: d,
            owner: o,
            executable: false,
            rent_epoch: r
        } => {
            assert!(l > LAMPORTS_PER_SOL);
            assert!(d.is_empty());
            assert_eq!(o, system_program::id());
            assert_eq!(r, u64::MAX);
        }
    );
}

#[tokio::test]
async fn clone_non_executable_with_data() {
    init_logger!();

    let tx_processor = transactions_processor();
    fund_luzifer(&tx_processor);

    let tx = verified_tx_to_clone_from_devnet(SOLX_POST, 3).await;
    let result = tx_processor.process(vec![tx]).unwrap();

    let (_, exec_details) = result.transactions.values().next().unwrap();
    log_exec_details(exec_details);
    let solx_post: Account = get_account_addr(tx_processor.bank(), SOLX_POST)
        .unwrap()
        .into();

    trace!("SolxPost account: {:#?}", solx_post);

    let solx_prog = Pubkey::from_str(SOLX_PROG).unwrap();
    assert_matches!(
        solx_post,
        Account {
            lamports: l,
            data: d,
            owner: o,
            executable: false,
            rent_epoch: r
        } => {
            assert!(l > 0);
            assert_eq!(d.len(), 1180);
            assert_eq!(o, solx_prog);
            assert_eq!(r, u64::MAX);
        }
    );
}

#[tokio::test]
async fn clone_solx_executable() {
    init_logger!();

    let tx_processor = transactions_processor();
    fund_luzifer(&tx_processor);

    // 1. Exec Clone Transaction

    {
        let tx = verified_tx_to_clone_from_devnet(SOLX_PROG, 5).await;
        let result = tx_processor.process(vec![tx]).unwrap();

        let (_, exec_details) = result.transactions.values().next().unwrap();
        log_exec_details(exec_details);
    }

    // 2. Verify that all accounts were added to the validator
    {
        let solx_prog: Account = get_account_addr(tx_processor.bank(), SOLX_PROG)
            .unwrap()
            .into();
        trace!("SolxProg account: {:#?}", solx_prog);

        let solx_exec: Account = get_account_addr(tx_processor.bank(), SOLX_EXEC)
            .unwrap()
            .into();
        trace!("SolxExec account: {:#?}", solx_exec);

        let solx_idl: Account = get_account_addr(tx_processor.bank(), SOLX_IDL)
            .unwrap()
            .into();
        trace!("SolxIdl account: {:#?}", solx_idl);

        assert_matches!(
            solx_prog,
            Account {
                lamports: l,
                data: d,
                owner: o,
                executable: true,
                rent_epoch: r
            } => {
                assert!(l >= 1141440);
                assert!(d.len() >= 36);
                assert_eq!(o, bpf_loader_upgradeable::id());
                assert_eq!(r, u64::MAX);
            }
        );
        assert_matches!(
            solx_exec,
            Account {
                lamports: l,
                data: d,
                owner: o,
                executable: false,
                rent_epoch: r
            } => {
                assert!(l >= 2890996080);
                assert!(d.len() >= 415245);
                assert_eq!(o, bpf_loader_upgradeable::id());
                assert_eq!(r, u64::MAX);
            }
        );
        assert_matches!(
            solx_idl,
            Account {
                lamports: l,
                data: d,
                owner: o,
                executable: false,
                rent_epoch: r
            } => {
                assert!(l >= 6264000);
                assert!(d.len() >= 772);
                assert_eq!(o, elfs::solanax::id());
                assert_eq!(r, u64::MAX);
            }
        );
    }

    // 3. Run a transaction against the cloned program
    {
        let tx = create_solx_send_post_transaction(tx_processor.bank());
        let result = tx_processor.process_sanitized(vec![tx]).unwrap();
        debug!("Result: {:#?}", result);
    }
}
