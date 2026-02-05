use std::process::Child;

use cleanass::{assert, assert_eq};
use dlp;
use program_flexi_counter::{
    instruction::{create_delegate_ix, create_init_ix},
    state::FlexiCounter,
};
use solana_rpc_client::rpc_client::RpcClient;
use solana_sdk::{
    instruction::Instruction,
    native_token::LAMPORTS_PER_SOL,
    pubkey::Pubkey,
    signature::{Keypair, Signature, Signer},
    transaction::Transaction,
};
use tracing::debug;

use crate::{expect, validator::cleanup, IntegrationTestContext};

// -----------------
// Transactions and Account Updates
// -----------------
pub fn init_and_delegate_counter_and_payer(
    ctx: &IntegrationTestContext,
    validator: &mut Child,
    label: &str,
) -> (Keypair, Pubkey) {
    // 1. Airdrop to payer on chain
    let mut keypairs =
        airdrop_accounts_on_chain(ctx, validator, &[2 * LAMPORTS_PER_SOL]);
    let payer = keypairs.drain(0..1).next().unwrap();

    // 2. Init counter instruction on chain
    let ix = create_init_ix(payer.pubkey(), label.to_string());
    confirm_tx_with_payer_chain(ix, &payer, validator);

    // 3 Delegate counter PDA
    let ix = create_delegate_ix(payer.pubkey());
    confirm_tx_with_payer_chain(ix, &payer, validator);

    // 4. Now we can delegate the payer to use for counter instructions
    //    in the ephemeral
    delegate_accounts(ctx, validator, &[&payer]);

    // 5. Verify all accounts are initialized correctly
    let (counter_pda, _) = FlexiCounter::pda(&payer.pubkey());
    let counter = fetch_counter_chain(&payer.pubkey(), validator);
    assert_eq!(
        counter,
        FlexiCounter {
            count: 0,
            updates: 0,
            label: label.to_string()
        },
        cleanup(validator)
    );
    let owner = fetch_counter_owner_chain(&payer.pubkey(), validator);
    assert_eq!(owner, dlp::id(), cleanup(validator));

    let payer_chain =
        expect!(ctx.fetch_chain_account(payer.pubkey()), validator);
    assert_eq!(payer_chain.owner, dlp::id(), cleanup(validator));
    assert!(payer_chain.lamports > LAMPORTS_PER_SOL, cleanup(validator));

    debug!(
        "✅ Initialized and delegated counter {counter_pda} and payer {}",
        payer.pubkey()
    );

    (payer, counter_pda)
}

pub fn airdrop_accounts_on_chain(
    ctx: &IntegrationTestContext,
    validator: &mut Child,
    lamports: &[u64],
) -> Vec<Keypair> {
    let mut payers = vec![];
    for l in lamports.iter() {
        let payer_chain = Keypair::new();
        expect!(ctx.airdrop_chain(&payer_chain.pubkey(), *l), validator);
        payers.push(payer_chain);
    }
    payers
}

pub fn delegate_accounts(
    ctx: &IntegrationTestContext,
    validator: &mut Child,
    keypairs: &[&Keypair],
) {
    let payer_chain = Keypair::new();
    expect!(
        ctx.airdrop_chain(&payer_chain.pubkey(), LAMPORTS_PER_SOL),
        validator
    );
    for keypair in keypairs.iter() {
        expect!(
            ctx.delegate_account(&payer_chain, keypair),
            format!("Failed to delegate keypair {}", keypair.pubkey()),
            validator
        );
    }
}

pub fn airdrop_and_delegate_accounts(
    ctx: &IntegrationTestContext,
    validator: &mut Child,
    lamports: &[u64],
) -> Vec<Keypair> {
    let payer_chain = Keypair::new();

    let total_lamports: u64 = lamports.iter().sum();
    let payer_lamports = LAMPORTS_PER_SOL + total_lamports;
    // 1. Airdrop to payer on chain
    expect!(
        ctx.airdrop_chain(&payer_chain.pubkey(), payer_lamports),
        validator
    );
    // 2. Airdrop to ephem payers and delegate them
    let keypairs_lamports = lamports
        .iter()
        .map(|&l| (Keypair::new(), l))
        .collect::<Vec<_>>();

    for (keypair, l) in keypairs_lamports.iter() {
        expect!(
            ctx.airdrop_chain_and_delegate(&payer_chain, keypair, *l),
            format!("Failed to airdrop {l} and delegate keypair"),
            validator
        );
    }
    keypairs_lamports
        .into_iter()
        .map(|(k, _)| k)
        .collect::<Vec<_>>()
}

pub fn transfer_lamports(
    ctx: &IntegrationTestContext,
    validator: &mut Child,
    from: &Keypair,
    to: &Pubkey,
    lamports: u64,
) -> Signature {
    let transfer_ix =
        solana_sdk::system_instruction::transfer(&from.pubkey(), to, lamports);
    let (sig, confirmed) = expect!(
        ctx.send_and_confirm_instructions_with_payer_ephem(
            &[transfer_ix],
            from
        ),
        "Failed to send transfer",
        validator
    );

    assert!(confirmed, cleanup(validator));
    sig
}

pub fn send_tx_with_payer_chain(
    ix: Instruction,
    payer: &Keypair,
    validator: &mut Child,
) -> Signature {
    let ctx = expect!(IntegrationTestContext::try_new(), validator);
    let mut tx = Transaction::new_with_payer(&[ix], Some(&payer.pubkey()));
    let signers = &[payer];

    let sig = expect!(ctx.send_transaction_chain(&mut tx, signers), validator);
    sig
}

pub fn confirm_tx_with_payer_ephem(
    ix: Instruction,
    payer: &Keypair,
    ctx: &IntegrationTestContext,
    validator: &mut Child,
) -> Signature {
    let mut tx = Transaction::new_with_payer(&[ix], Some(&payer.pubkey()));
    let signers = &[payer];

    let (sig, confirmed) = expect!(
        ctx.send_and_confirm_transaction_ephem(&mut tx, signers),
        validator
    );
    if !confirmed {
        ctx.dump_ephemeral_logs(sig)
    }
    assert!(confirmed, cleanup(validator), "Should confirm transaction",);
    sig
}

pub fn confirm_tx_with_payer_chain(
    ix: Instruction,
    payer: &Keypair,
    validator: &mut Child,
) -> Signature {
    let ctx = expect!(IntegrationTestContext::try_new_chain_only(), validator);

    let mut tx = Transaction::new_with_payer(&[ix], Some(&payer.pubkey()));
    let signers = &[payer];

    let (sig, confirmed) = expect!(
        ctx.send_and_confirm_transaction_chain(&mut tx, signers),
        validator
    );
    if !confirmed {
        ctx.dump_chain_logs(sig)
    }
    assert!(confirmed, cleanup(validator), "Should confirm transaction");
    sig
}

pub fn fetch_counter_ephem(
    ctx: &IntegrationTestContext,
    payer: &Pubkey,
    validator: &mut Child,
) -> FlexiCounter {
    let ephem_client = expect!(ctx.try_ephem_client(), validator);
    fetch_counter(payer, ephem_client, validator, "ephem")
}

pub fn fetch_counter_chain(
    payer: &Pubkey,
    validator: &mut Child,
) -> FlexiCounter {
    let ctx = expect!(IntegrationTestContext::try_new_chain_only(), validator);
    let chain_client = expect!(ctx.try_chain_client(), validator);
    fetch_counter(payer, chain_client, validator, "chain")
}

fn fetch_counter(
    payer: &Pubkey,
    rpc_client: &RpcClient,
    validator: &mut Child,
    source: &str,
) -> FlexiCounter {
    let (counter, _) = FlexiCounter::pda(payer);
    debug!("Fetching counter {counter} for payer {payer} from {source}");
    let counter_acc = expect!(rpc_client.get_account(&counter), validator);
    expect!(FlexiCounter::try_decode(&counter_acc.data), validator)
}

pub fn fetch_counter_owner_chain(
    payer: &Pubkey,
    validator: &mut Child,
) -> Pubkey {
    let ctx = expect!(IntegrationTestContext::try_new_chain_only(), validator);
    let (counter, _) = FlexiCounter::pda(payer);
    expect!(ctx.fetch_chain_account_owner(counter), validator)
}
