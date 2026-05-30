use anyhow::Result;
use solana_commitment_config::CommitmentConfig;
use solana_sdk::{account::Account, pubkey::Pubkey};
use test_query_filtering::{
    assert_missing_url_token_is_rejected, authed_rpc,
    setup_query_filtering_fixture,
};

#[test]
fn query_filtering_filters_direct_account_rpc_by_membership() -> Result<()> {
    let fixture = setup_query_filtering_fixture()?;

    assert_missing_url_token_is_rejected(
        "getAccountInfo",
        &[fixture.restricted_counter.to_string()],
    )?;

    for (user, can_access_restricted) in [
        (&fixture.allowed, true),
        (&fixture.account_only, true),
        (&fixture.denied, false),
    ] {
        let client = authed_rpc(&user.token);

        let restricted = fetch_account(&client, &fixture.restricted_counter)?;
        assert_account_visibility(
            restricted.as_ref(),
            can_access_restricted,
            "restricted getAccountInfo",
        );

        let public = fetch_account(&client, &fixture.public_counter)?;
        assert_account_visibility(
            public.as_ref(),
            true,
            "public getAccountInfo",
        );

        let balance = client.get_balance(&fixture.restricted_counter)?;
        if can_access_restricted {
            assert!(balance > 0, "authorized user should see balance");
        } else {
            assert_eq!(balance, 0, "unauthorized user should see zero balance");
        }

        let multiple = client
            .get_multiple_accounts_with_commitment(
                &[fixture.restricted_counter, fixture.public_counter],
                CommitmentConfig::confirmed(),
            )?
            .value;
        assert_account_visibility(
            multiple[0].as_ref(),
            can_access_restricted,
            "restricted getMultipleAccounts",
        );
        assert_account_visibility(
            multiple[1].as_ref(),
            true,
            "public getMultipleAccounts",
        );
    }

    fixture.chain_account_exists(&fixture.restricted_counter)?;
    Ok(())
}

fn fetch_account(
    client: &solana_rpc_client::rpc_client::RpcClient,
    pubkey: &Pubkey,
) -> Result<Option<Account>> {
    Ok(client
        .get_account_with_commitment(pubkey, CommitmentConfig::confirmed())?
        .value)
}

fn assert_account_visibility(
    account: Option<&Account>,
    visible: bool,
    context: &str,
) {
    if visible {
        assert!(account.is_some(), "{context} should be visible");
    } else {
        assert!(account.is_none(), "{context} should be filtered");
    }
}
