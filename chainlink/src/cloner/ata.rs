use engine::{AccountAccessor, Engine, EngineError, PostFinalize};
use keeper::error::KeeperError;
use mbv_core::token_programs::try_get_magic_ata_info;
use solana_account::{AccountBuilder, AccountMode, AccountSharedData, ReadableAccount};
use solana_program::{program_option::COption, program_pack::Pack};
use solana_pubkey::Pubkey;
use spl_token::state::Account as SplAccount;

use super::errors::{ClonerError::InvalidAtaProjection, ClonerResult};

/// Recomputes from a fresh snapshot after conflicts, never replaying a stale merge.
pub(super) async fn materialize<'a>(
    engine: &'a Engine,
    mut accessor: AccountAccessor<'a>,
    projection: AccountBuilder,
    actions: Option<PostFinalize>,
) -> ClonerResult<()> {
    let pubkey = accessor.pubkey();
    let mut retries = 2;
    loop {
        let expected = engine
            .accounts()
            .loader()
            .read(&pubkey, |account| account.owned())
            .map_err(KeeperError::from)
            .map_err(EngineError::from)?
            .unwrap_or_default();
        let account = if expected.is(AccountMode::Magic) {
            let local = AccountSharedData::from(expected.clone());
            merge(&pubkey, &local, projection.clone())?
        } else {
            projection.clone()
        };
        match accessor.require(&expected).materialize(account, actions.clone()).await {
            Err(EngineError::AccountChanged) if retries > 0 => {
                retries -= 1;
                accessor = engine.account(pubkey).await?;
            }
            result => return result.map_err(Into::into),
        }
    }
}

fn merge(
    pubkey: &Pubkey,
    local: &AccountSharedData,
    projection: AccountBuilder,
) -> ClonerResult<AccountBuilder> {
    let magic = try_get_magic_ata_info(pubkey, local)
        .filter(|magic| magic.token_program == projection.read().owner())
        .ok_or(InvalidAtaProjection(
            "local account is not a matching Magic ATA",
        ))?;
    let local_token =
        unpack(local.data()).ok_or(InvalidAtaProjection("invalid local token data"))?;
    let mut token = unpack(projection.read().data())
        .ok_or(InvalidAtaProjection("invalid projection token data"))?;
    if token.mint != magic.mint || token.owner != magic.wallet_owner {
        return Err(InvalidAtaProjection(
            "projection mint or wallet owner does not match",
        ));
    }
    if magic.amount > 0 && (local_token.is_frozen() || token.is_frozen()) {
        return Err(InvalidAtaProjection(
            "funded takeover requires both accounts to be unfrozen",
        ));
    }
    (token.delegate, token.delegated_amount) = match (local_token.delegate, token.delegate) {
        (COption::None, _) => (token.delegate, token.delegated_amount),
        (_, COption::None) => (local_token.delegate, local_token.delegated_amount),
        (a, b) if a == b => {
            let local_allowance = local_token.amount.min(local_token.delegated_amount);
            let projected_allowance = token.amount.min(token.delegated_amount);
            let allowance = local_allowance
                .checked_add(projected_allowance)
                .ok_or(InvalidAtaProjection("merged delegate allowance overflows"))?;
            (a, allowance)
        }
        _ => {
            return Err(InvalidAtaProjection(
                "Magic ATA and projection approve different SPL delegates",
            ));
        }
    };
    token.amount = token
        .amount
        .checked_add(magic.amount)
        .ok_or(InvalidAtaProjection("merged balance overflows"))?;
    let mut data = projection.read().data().to_vec();
    SplAccount::pack(token, &mut data[..SplAccount::LEN])
        .map_err(|_| InvalidAtaProjection("invalid projection token layout"))?;
    Ok(projection.data(data))
}

fn unpack(data: &[u8]) -> Option<SplAccount> {
    SplAccount::unpack(data.get(..SplAccount::LEN)?).ok()
}
