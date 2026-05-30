#![allow(dead_code)]

use std::{thread::sleep, time::Duration};

use anyhow::{Context, Result};
use integration_test_tools::IntegrationTestContext;
use magicblock_query_filtering::types::Permission;
use program_flexi_counter::{
    instruction::{
        create_create_permission_ix, create_delegate_ix, create_init_ix,
        PermissionMemberArgs,
    },
    state::FlexiCounter,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use solana_commitment_config::CommitmentConfig;
use solana_rpc_client::rpc_client::RpcClient;
use solana_sdk::{
    instruction::Instruction, native_token::LAMPORTS_PER_SOL, pubkey::Pubkey,
    signature::Keypair, signer::Signer, transaction::Transaction,
};

pub const APERTURE_URL: &str = "http://localhost:8899";
pub const MEMBER_FLAG_TX_LOGS: u8 = 1 << 1;
pub const MEMBER_FLAG_TX_BALANCES: u8 = 1 << 2;
pub const MEMBER_FLAG_TX_MESSAGE: u8 = 1 << 3;
pub const MEMBER_FLAG_ACCOUNT_SIGNATURE: u8 = 1 << 4;
pub const ALL_VISIBILITY_FLAGS: u8 = MEMBER_FLAG_TX_LOGS
    | MEMBER_FLAG_TX_BALANCES
    | MEMBER_FLAG_TX_MESSAGE
    | MEMBER_FLAG_ACCOUNT_SIGNATURE;

#[derive(Debug, Deserialize)]
struct ChallengeResponse {
    challenge: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct LoginRequest {
    pubkey: String,
    signature: String,
    challenge: String,
}

#[derive(Debug, Deserialize)]
struct LoginResponse {
    token: String,
}

pub struct QueryFilteringFixture {
    pub ctx: IntegrationTestContext,
    pub allowed: User,
    pub account_only: User,
    pub denied: User,
    pub restricted_owner: Keypair,
    pub restricted_counter: Pubkey,
    pub public_counter: Pubkey,
}

pub struct User {
    pub keypair: Keypair,
    pub token: String,
}

pub fn setup_query_filtering_fixture() -> Result<QueryFilteringFixture> {
    let mut ctx = IntegrationTestContext::try_new_chain_only()?;

    let allowed = Keypair::new();
    let account_only = Keypair::new();
    let denied = Keypair::new();
    let restricted_owner = Keypair::new();
    let public_owner = Keypair::new();
    for keypair in [
        &allowed,
        &account_only,
        &denied,
        &restricted_owner,
        &public_owner,
    ] {
        ctx.airdrop_chain(&keypair.pubkey(), 5 * LAMPORTS_PER_SOL)?;
    }

    let restricted_counter = FlexiCounter::pda(&restricted_owner.pubkey()).0;
    let public_counter = FlexiCounter::pda(&public_owner.pubkey()).0;
    let members = vec![
        PermissionMemberArgs {
            pubkey: allowed.pubkey(),
            flags: ALL_VISIBILITY_FLAGS,
        },
        PermissionMemberArgs {
            pubkey: account_only.pubkey(),
            flags: 0,
        },
        PermissionMemberArgs {
            pubkey: program_flexi_counter::id(),
            flags: 0,
        },
    ];

    ctx.send_and_confirm_instructions_with_payer_chain(
        &[
            create_init_ix(restricted_owner.pubkey(), "PRIVATE".to_string()),
            create_delegate_ix(restricted_owner.pubkey()),
        ],
        &restricted_owner,
    )?;
    ctx.send_and_confirm_instructions_with_payer_chain(
        &[
            create_init_ix(public_owner.pubkey(), "PUBLIC".to_string()),
            create_delegate_ix(public_owner.pubkey()),
        ],
        &public_owner,
    )?;

    let restricted_owner_token = login(&restricted_owner)?;
    ctx.ephem_client = Some(authed_rpc(&restricted_owner_token));
    wait_for_account(&restricted_owner_token, &restricted_counter)?;
    let restricted_permission_ix =
        create_create_permission_ix(restricted_owner.pubkey(), Some(members));
    let (_, confirmed) = ctx.send_and_confirm_instructions_with_payer_ephem(
        std::slice::from_ref(&restricted_permission_ix),
        &restricted_owner,
    )?;
    ensure_confirmed_permission_ix(
        &ctx,
        &restricted_owner,
        &restricted_permission_ix,
        confirmed,
        "restricted",
    )?;

    let public_owner_token = login(&public_owner)?;
    ctx.ephem_client = Some(authed_rpc(&public_owner_token));
    wait_for_account(&public_owner_token, &public_counter)?;
    let public_permission_ix =
        create_create_permission_ix(public_owner.pubkey(), None);
    let (_, confirmed) = ctx.send_and_confirm_instructions_with_payer_ephem(
        std::slice::from_ref(&public_permission_ix),
        &public_owner,
    )?;
    ensure_confirmed_permission_ix(
        &ctx,
        &public_owner,
        &public_permission_ix,
        confirmed,
        "public",
    )?;

    let allowed = User {
        token: login(&allowed)?,
        keypair: allowed,
    };
    let account_only = User {
        token: login(&account_only)?,
        keypair: account_only,
    };
    let denied = User {
        token: login(&denied)?,
        keypair: denied,
    };

    wait_for_account(&allowed.token, &restricted_counter)?;
    wait_for_account(&allowed.token, &public_counter)?;

    Ok(QueryFilteringFixture {
        ctx,
        allowed,
        account_only,
        denied,
        restricted_owner,
        restricted_counter,
        public_counter,
    })
}

fn ensure_confirmed_permission_ix(
    ctx: &IntegrationTestContext,
    payer: &Keypair,
    ix: &Instruction,
    confirmed: bool,
    label: &str,
) -> Result<()> {
    if confirmed {
        return Ok(());
    }

    let ephem_client = ctx.try_ephem_client()?;
    let blockhash = ephem_client.get_latest_blockhash()?;
    let mut tx = Transaction::new_with_payer(
        std::slice::from_ref(ix),
        Some(&payer.pubkey()),
    );
    tx.sign(&[payer], blockhash);
    let simulation = ephem_client.simulate_transaction(&tx)?;
    anyhow::bail!(
        "{label} permission creation should confirm; simulation result: {:?}",
        simulation.value,
    );
}

pub fn authed_rpc(token: &str) -> RpcClient {
    RpcClient::new_with_commitment(
        format!("{APERTURE_URL}?token={token}"),
        CommitmentConfig::confirmed(),
    )
}

pub fn login(keypair: &Keypair) -> Result<String> {
    let challenge_url =
        format!("{APERTURE_URL}/auth/challenge?pubkey={}", keypair.pubkey());
    let challenge: ChallengeResponse =
        ureq::get(&challenge_url).call()?.into_json()?;
    let signature = keypair.sign_message(challenge.challenge.as_bytes());
    let request = LoginRequest {
        pubkey: keypair.pubkey().to_string(),
        signature: signature.to_string(),
        challenge: challenge.challenge,
    };
    let response: LoginResponse =
        ureq::post(&format!("{APERTURE_URL}/auth/login"))
            .send_json(serde_json::to_value(request)?)?
            .into_json()?;
    Ok(response.token)
}

/// Raw JSON-RPC call against the aperture server that authenticates via the
/// `?token=` query parameter — the same path the Solana `RpcClient` uses
/// under `authed_rpc`.
///
/// Bulk data tests should use `authed_rpc(token)` and the typed RpcClient
/// methods. This helper exists only for auth-specific tests that need to
/// inspect raw HTTP status codes or exercise the `?token=` path with an
/// invalid or missing token.
pub fn raw_rpc_with_url_token(
    token: Option<&str>,
    method: &str,
    params: Value,
) -> Result<Value> {
    let url = match token {
        Some(token) => format!("{APERTURE_URL}?token={token}"),
        None => APERTURE_URL.to_string(),
    };
    raw_post(&url, &[], method, params)
}

/// Raw JSON-RPC call against the aperture server that authenticates via the
/// `Authorization: Bearer <token>` header. Used exclusively by the auth test
/// that asserts the production dispatcher still honors the Bearer fallback
/// (Solana's `RpcClient` does not send custom auth headers, so the Bearer
/// path must be exercised manually).
pub fn raw_rpc_with_bearer_header(
    token: &str,
    method: &str,
    params: Value,
) -> Result<Value> {
    raw_post(
        APERTURE_URL,
        &[("Authorization", format!("Bearer {token}"))],
        method,
        params,
    )
}

fn raw_post(
    url: &str,
    headers: &[(&str, String)],
    method: &str,
    params: Value,
) -> Result<Value> {
    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": method,
        "params": params,
    });
    let mut request = ureq::post(url);
    for (name, value) in headers {
        request = request.set(name, value);
    }
    Ok(request.send_json(body)?.into_json()?)
}

pub fn assert_missing_url_token_is_rejected(
    method: &str,
    params: &[String],
) -> Result<()> {
    let err = raw_rpc_with_url_token(None, method, json!(params)).unwrap_err();
    assert_http_status(&err, 401, "missing token should return HTTP 401");
    Ok(())
}

pub fn assert_http_status(err: &anyhow::Error, expected: u16, context: &str) {
    let status = err.downcast_ref::<ureq::Error>().and_then(|err| match err {
        ureq::Error::Status(status, _) => Some(*status),
        ureq::Error::Transport(_) => None,
    });
    assert_eq!(status, Some(expected), "{context}");
}

fn wait_for_account(token: &str, pubkey: &Pubkey) -> Result<()> {
    let client = authed_rpc(token);
    let commitment = CommitmentConfig::confirmed();
    let mut last_response = None;
    for _ in 0..50 {
        match client.get_account_with_commitment(pubkey, commitment) {
            Ok(response) if response.value.is_some() => return Ok(()),
            Ok(response) => last_response = Some(format!("{response:?}")),
            Err(err) => last_response = Some(err.to_string()),
        }
        sleep(Duration::from_millis(200));
    }
    let permission_account = client
        .get_account_with_commitment(&Permission::pda(pubkey), commitment)
        .map(|response| format!("{response:?}"))
        .unwrap_or_else(|err| err.to_string());
    anyhow::bail!(
        "account {pubkey} was not visible on ephemeral validator; last response: {}; permission response: {}",
        last_response.unwrap_or_else(|| "<none>".to_string()),
        permission_account,
    )
}

impl QueryFilteringFixture {
    pub fn chain_account_exists(&self, pubkey: &Pubkey) -> Result<()> {
        self.ctx
            .chain_client
            .as_ref()
            .context("missing chain")?
            .get_account(pubkey)?;
        Ok(())
    }
}
