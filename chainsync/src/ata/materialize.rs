use engine::{AccountAccessor, Engine, EngineError, PostFinalize};
use mbv_core::token_programs::try_get_magic_ata_info;
use solana_account::{AccountBuilder, AccountMode, AccountSharedData, ReadableAccount};
use solana_program_option::COption;
use solana_program_pack::Pack;
use solana_pubkey::Pubkey;
use spl_token::state::Account as SplAccount;

use crate::{Error::InvalidAtaProjection, Result};

/// Recomputes from a fresh snapshot after conflicts, never replaying a stale merge.
pub(crate) async fn materialize<'a>(
    engine: &'a Engine,
    mut accessor: AccountAccessor<'a>,
    projection: AccountBuilder,
    actions: Option<PostFinalize>,
) -> Result<()> {
    let pubkey = accessor.pubkey();
    let mut retries = 3;
    loop {
        let (expected, account) = engine
            .accounts()
            .loader()
            .read(&pubkey, |local| -> Result<_> {
                let account = if local.is(AccountMode::Magic) {
                    merge(&pubkey, local, projection.clone())?
                } else {
                    projection.clone()
                };
                Ok((local.owned(), account))
            })
            .map_err(|error| EngineError::State(error.into()))?
            .transpose()?
            .unwrap_or_else(|| (Default::default(), projection.clone()));
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
) -> Result<AccountBuilder> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use mbv_core::token_programs::{MAGIC_ATA_CLOSE_AUTHORITY, derive_ata_with_token_program};
    use spl_token::state::AccountState;

    /// Takeover preserves local balances and compatible approvals, rejecting frozen or conflicting funds.
    #[test]
    fn takeover_merges_only_compatible_token_state() {
        let owner = Pubkey::new_unique();
        let mint = Pubkey::new_unique();
        let delegate = Pubkey::new_unique();
        let ata = derive_ata_with_token_program(&owner, &mint, &spl_token::id());
        let token = SplAccount {
            mint,
            owner,
            amount: 7,
            delegate: COption::Some(delegate),
            state: AccountState::Initialized,
            is_native: COption::None,
            delegated_amount: 20,
            close_authority: COption::Some(MAGIC_ATA_CLOSE_AUTHORITY),
        };
        let image = |token: SplAccount, mode| {
            let mut data = vec![0; SplAccount::LEN];
            SplAccount::pack(token, &mut data).unwrap();
            AccountBuilder::default()
                .lamports(1_000_000)
                .owner(spl_token::id())
                .data(data)
                .mode(mode)
        };
        let local = image(token, AccountMode::Magic).build();
        let mut remote = token;
        remote.amount = 11;
        remote.delegated_amount = 3;
        remote.close_authority = COption::Some(Pubkey::default());
        let projection = || image(remote, AccountMode::Delegated);
        let merged = merge(&ata, &local, projection()).unwrap();
        let merged = unpack(merged.read().data()).unwrap();
        assert_eq!(merged.amount, 18);
        // Local allowance is capped by local funds before summing with the remote allowance.
        assert_eq!(merged.delegated_amount, 10);
        assert_eq!(merged.close_authority, COption::Some(Pubkey::default()));
        remote.delegate = COption::Some(Pubkey::new_unique());
        assert!(merge(&ata, &local, image(remote, AccountMode::Delegated)).is_err());
        remote.delegate = COption::Some(delegate);
        remote.state = AccountState::Frozen;
        assert!(merge(&ata, &local, image(remote, AccountMode::Delegated)).is_err());
        remote.state = AccountState::Initialized;
        remote.amount = u64::MAX;
        assert!(merge(&ata, &local, image(remote, AccountMode::Delegated)).is_err());
    }
}
