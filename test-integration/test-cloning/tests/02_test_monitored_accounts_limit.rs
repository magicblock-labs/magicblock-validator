use integration_test_tools::IntegrationTestContext;
use solana_sdk::{
    instruction::{AccountMeta, Instruction},
    native_token::LAMPORTS_PER_SOL,
    pubkey::Pubkey,
    signature::Keypair,
    signer::Signer,
    transaction::Transaction,
};

const TEST_PROGRAM_ID: Pubkey =
    Pubkey::from_str_const("3JnJ727jWEmPVU8qfXwtH63sCNDX7nMgsLbg8qy8aaPX");

#[test]
fn test_monitored_accounts_limiter() {
    let ctx = IntegrationTestContext::try_new().unwrap();
    let payer = Keypair::from_bytes(&[
        32, 181, 98, 251, 136, 61, 40, 174, 71, 44, 44, 192, 34, 202, 7, 120,
        55, 199, 50, 137, 8, 246, 114, 146, 117, 181, 217, 79, 132, 28, 222,
        123, 27, 184, 143, 64, 239, 203, 219, 140, 250, 104, 187, 165, 188, 77,
        129, 223, 86, 150, 183, 222, 123, 215, 11, 62, 14, 187, 176, 212, 145,
        98, 186, 13,
    ])
    .unwrap();
    ctx.airdrop_chain(&payer.pubkey(), LAMPORTS_PER_SOL)
        .expect("failed to fund the payer");

    // instruction which only reads accounts
    let data = [6, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0];

    // set of random accounts on devnet which we cloned for test purposes
    let readable1 =
        Pubkey::from_str_const("9yXjZTevvMp1XgZSZEaziPRgFiXtAQChpnP2oX9eCpvt");
    let readable2 =
        Pubkey::from_str_const("BHBuATGifAD4JbRpM5nVdyhKzPgv3p2CxLEHAqwBzAj5");
    let readable3 =
        Pubkey::from_str_const("669U43LNHx7LsVj95uYksnhXUfWKDsdzVqev3V4Jpw3P");
    let readable4 =
        Pubkey::from_str_const("2EmfL3MqL3YHABudGNmajjCpR13NNEn9Y4LWxbDm6SwR");

    let accounts = vec![
        AccountMeta::new_readonly(readable1, false),
        AccountMeta::new_readonly(readable2, false),
    ];
    let ix = Instruction::new_with_bytes(TEST_PROGRAM_ID, &data, accounts);
    let mut txn = Transaction::new_with_payer(&[ix], Some(&payer.pubkey()));
    // this transaction should clone the feepayer from chain along with two readonly accounts
    // this should fit exactly within the limit of 3 for LRU cache of monitored accounts
    ctx.send_transaction_ephem(&mut txn, &[&payer])
        .expect("failed to send transaction");
    // both accounts should be on ER after the TXN
    assert!(ctx.fetch_ephem_account(readable1).is_ok());
    assert!(ctx.fetch_ephem_account(readable2).is_ok());

    let accounts = vec![
        AccountMeta::new_readonly(readable3, false),
        AccountMeta::new_readonly(readable4, false),
    ];
    let ix = Instruction::new_with_bytes(TEST_PROGRAM_ID, &data, accounts);
    let mut txn = Transaction::new_with_payer(&[ix], Some(&payer.pubkey()));
    // send the same instruction with 2 other accounts, which should evict previous 2
    ctx.send_transaction_ephem(&mut txn, &[&payer])
        .expect("failed to send transaction");
    // first two accounts from previous txn should now be removed from accountsdb
    assert!(ctx.fetch_ephem_account(readable1).is_err());
    assert!(ctx.fetch_ephem_account(readable2).is_err());

    let accounts = vec![
        AccountMeta::new_readonly(readable1, false),
        AccountMeta::new_readonly(readable2, false),
    ];
    let ix = Instruction::new_with_bytes(TEST_PROGRAM_ID, &data, accounts);
    let mut txn = Transaction::new_with_payer(&[ix], Some(&payer.pubkey()));

    // resending the original transaction should re-clone the first pair of accounts
    ctx.send_transaction_ephem(&mut txn, &[&payer])
        .expect("failed to send transaction");

    assert!(ctx.fetch_ephem_account(readable1).is_ok());
    assert!(ctx.fetch_ephem_account(readable2).is_ok());
}
