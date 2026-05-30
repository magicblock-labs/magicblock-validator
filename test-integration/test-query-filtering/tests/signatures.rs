use anyhow::Result;
use program_flexi_counter::instruction::create_add_ix;
use solana_sdk::{signer::Signer, transaction::Transaction};
use test_query_filtering::{authed_rpc, setup_query_filtering_fixture};

#[test]
fn query_filtering_filters_signatures_by_account_signature_flag() -> Result<()>
{
    let fixture = setup_query_filtering_fixture()?;

    let ephem_rpc = authed_rpc(&fixture.allowed.token);
    let blockhash = ephem_rpc.get_latest_blockhash()?;
    let tx = Transaction::new_signed_with_payer(
        &[create_add_ix(fixture.restricted_owner.pubkey(), 7)],
        Some(&fixture.restricted_owner.pubkey()),
        &[&fixture.restricted_owner],
        blockhash,
    );
    let signature = ephem_rpc.send_and_confirm_transaction(&tx)?;

    for (user, can_see_signatures) in [
        (&fixture.allowed, true),
        (&fixture.account_only, false),
        (&fixture.denied, false),
    ] {
        let client = authed_rpc(&user.token);
        let signatures =
            client.get_signatures_for_address(&fixture.restricted_counter)?;
        let signature_str = signature.to_string();
        assert_eq!(
            signatures
                .iter()
                .any(|info| info.signature == signature_str),
            can_see_signatures,
            "signature visibility for {}",
            user.keypair.pubkey()
        );
    }

    Ok(())
}
