use super::{Error, Result, pubkey};
use dlp_api::discriminator::DlpDiscriminator::{
    self, Undelegate, UndelegateWithRollbackAfterTimeout,
};
use smallvec::SmallVec;
use solana_pubkey::Pubkey;
use yellowstone_grpc_proto::prelude::SubscribeUpdateTransactionInfo;

/// Finds distinct undelegated accounts in a successful transaction and its CPIs.
/// Ordinary releases must name this authority; timeout rollbacks remain authority-unfiltered.
/// Reference lookup failures abort decoding rather than return a partial target list.
pub(super) fn undelegated_accounts(
    tx: &SubscribeUpdateTransactionInfo,
    authority: Pubkey,
) -> Result<SmallVec<[Pubkey; 1]>> {
    let meta = tx.meta.as_ref().ok_or(Error::Protocol("missing transaction metadata"))?;
    let transaction =
        tx.transaction.as_ref().ok_or(Error::Protocol("missing transaction message"))?;
    let message = transaction
        .message
        .as_ref()
        .ok_or(Error::Protocol("missing transaction message"))?;
    // Lookup-table transactions are outside the supported contract. Ignore
    // them without attempting partial lifecycle decoding or stopping the feed.
    if !message.address_table_lookups.is_empty() {
        return Ok(SmallVec::new());
    }
    let key = |index: usize| {
        let bytes = message
            .account_keys
            .get(index)
            .ok_or(Error::Protocol("invalid account index"))?;
        pubkey(bytes)
    };
    let dlp = dlp_api::id();
    // Resolve both instruction kinds against static keys only; lookup-table keys are unsupported.
    let outer = message
        .instructions
        .iter()
        .map(|ix| (ix.program_id_index, &ix.accounts, &ix.data));
    let inner = meta
        .inner_instructions
        .iter()
        .flat_map(|group| &group.instructions)
        .map(|ix| (ix.program_id_index, &ix.accounts, &ix.data));
    let mut undelegated = SmallVec::new();
    for (program, accounts, data) in outer.chain(inner) {
        if key(program as usize)? != dlp {
            continue;
        }
        // DLP dispatches on the first discriminator byte and ignores the remaining bytes.
        let Some(&tag) = data.first() else { continue };
        let Ok(discriminator) = DlpDiscriminator::try_from(tag) else { continue };
        // Resolve instruction-account positions through the transaction's static key indices.
        let account = |position, missing| {
            let index = *accounts.get(position).ok_or(Error::Protocol(missing))?;
            key(usize::from(index))
        };
        let position = match discriminator {
            Undelegate => {
                // DLP validates this validator signer against the delegation record's authority.
                let validator = account(0, "missing undelegation authority")?;
                if validator != authority {
                    continue;
                }
                1
            }
            UndelegateWithRollbackAfterTimeout => 0,
            _ => continue,
        };
        undelegated.push(account(position, "missing undelegated account")?);
    }
    undelegated.sort_unstable();
    undelegated.dedup();
    Ok(undelegated)
}
