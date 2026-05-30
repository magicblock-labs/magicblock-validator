use anyhow::Result;
use solana_sdk::signer::Signer;
use test_query_filtering::{authed_rpc, setup_query_filtering_fixture};

#[test]
fn query_filtering_filters_get_program_accounts_by_membership() -> Result<()> {
    let fixture = setup_query_filtering_fixture()?;

    for (user, can_access_restricted) in [
        (&fixture.allowed, true),
        (&fixture.account_only, true),
        (&fixture.denied, false),
    ] {
        let client = authed_rpc(&user.token);
        let program_accounts =
            client.get_program_accounts(&program_flexi_counter::id())?;
        assert_eq!(
            program_accounts
                .iter()
                .any(|(pubkey, _)| pubkey == &fixture.restricted_counter),
            can_access_restricted,
            "restricted counter program account visibility for {}",
            user.keypair.pubkey()
        );
        assert!(
            program_accounts
                .iter()
                .any(|(pubkey, _)| pubkey == &fixture.public_counter),
            "public counter should stay visible for {}",
            user.keypair.pubkey()
        );
    }

    Ok(())
}
