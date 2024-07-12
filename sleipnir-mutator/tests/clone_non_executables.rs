use std::str::FromStr;

use assert_matches::assert_matches;
use log::*;
use solana_sdk::{
    account::Account, native_token::LAMPORTS_PER_SOL, pubkey::Pubkey,
    system_program,
};
use test_tools::{
    account::get_account_addr, diagnostics::log_exec_details, init_logger,
    skip_if_devnet_down, transactions_processor,
    validator::ensure_funded_validator_authority,
};

use crate::utils::{
    fund_luzifer, verified_tx_to_clone_from_devnet, SOLX_POST, SOLX_PROG,
    SOLX_TIPS,
};

mod utils;

#[tokio::test]
async fn clone_non_executable_without_data() {
    init_logger!();
    skip_if_devnet_down!();

    let tx_processor = transactions_processor();
    ensure_funded_validator_authority(tx_processor.bank());
    fund_luzifer(&*tx_processor);

    let slot = tx_processor.bank().slot();
    let tx = verified_tx_to_clone_from_devnet(
        SOLX_TIPS,
        slot,
        3,
        tx_processor.bank().last_blockhash(),
    )
    .await;
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
    skip_if_devnet_down!();

    let tx_processor = transactions_processor();
    ensure_funded_validator_authority(tx_processor.bank());
    fund_luzifer(&*tx_processor);

    let slot = tx_processor.bank().slot();
    let tx = verified_tx_to_clone_from_devnet(
        SOLX_POST,
        slot,
        3,
        tx_processor.bank().last_blockhash(),
    )
    .await;
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
