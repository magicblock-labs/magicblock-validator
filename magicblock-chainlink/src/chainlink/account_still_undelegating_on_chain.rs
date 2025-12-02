use dlp::state::DelegationRecord;
use log::*;
use solana_pubkey::Pubkey;

/// Decides if an account that is undelegating should be updated
/// (overwritten) by the remote account state and the `undelegating` flag cleared.
///
/// The only case when an account should not be updated is when the following is true:
///
/// - account is still delegated to us on chain
/// - the delegation slot is older than the slot at which we last fetched
///   the account state from chain.
///
/// # Arguments
/// - `pubkey`: the account pubkey
/// - `is_delegated_on_chain`: whether the account is currently delegated to us on chain
/// - `remote_slot_in_bank`: the chain slot at which we last fetched and cloned state
///                  of the account in our bank
/// - `delegation_record`: the delegation record associated with the account in our bank, if found
/// - `validator_auth`: the validator authority pubkey
/// - returns `true` if the account is still undelegating, `false` otherwise.
pub(crate) fn account_still_undelegating_on_chain(
    pubkey: &Pubkey,
    is_delegated_to_us_on_chain: bool,
    remote_slot_in_bank: u64,
    deleg_record: Option<DelegationRecord>,
    validator_auth: &Pubkey,
) -> bool {
    // In the case of a subscription update for an account that was undelegating
    // we know that the undelegation or associated commit or possibly a previous
    // commit made after we subscribed to the account was completed, otherwise
    // there would be no update.
    //
    // Now the account could be in one the following states:
    //
    // A) the account was undelegated and remained so
    // B) the account was undelegated and was re-delegated to us or the system
    //    program (broadcast account)
    // C) the account was undelegated and was re-delegated to another validator
    // D) the account's undelegation request did not complete.
    //    In case of a subscription update the commit (or a commit scheduled previously) did trigger an update.
    //    Alternatively someone may be accessing the account while undelegation is still pending.
    //    Thus the account is still delegated to us on chain.
    //
    // In the case of D) we want to keep the bank version of the account.
    //
    // In all other cases we want to clone the remote version of the account into
    // our bank which will automatically set the correct delegated state and
    // untoggle the undelegating flag.
    if is_delegated_to_us_on_chain {
        // B) or D)
        // Since the account was found to be delegated we must have
        // found a delegation record and thus have the delegation slot.
        let delegation_slot = deleg_record
            .as_ref()
            .map(|d| d.delegation_slot)
            .unwrap_or_default();
        if delegation_slot <= remote_slot_in_bank {
            // The last update of the account was after the last delegation
            // Therefore the account was not redelegated which indicates
            // that the undelegation is still not completed. Case (D))
            debug!(
                "Undelegation for {pubkey} is still pending. Keeping bank account.",
            );
            true
        } else {
            // This is a re-delegation to us after undelegation completed.
            // Case (B))
            debug!(
                "Undelegation completed for account {pubkey} and it was re-delegated to us at slot: ({delegation_slot}).",
            );
            magicblock_metrics::metrics::inc_undelegation_completed();
            false
        }
    } else if let Some(deleg_record) = deleg_record {
        // Account delegated to other (Case C)) -> clone as is
        debug!(
            "Account {pubkey} was undelegated and re-delegated to another validator. authority: {}, delegated_to: {}",
            validator_auth, deleg_record.authority
        );
        magicblock_metrics::metrics::inc_undelegation_completed();
        false
    } else {
        // Account no longer delegated (Case A)) -> clone as is
        debug!("Account {pubkey} was undelegated and remained so");
        magicblock_metrics::metrics::inc_undelegation_completed();
        false
    }
}

#[cfg(test)]
mod tests {
    use dlp::state::DelegationRecord;
    use solana_pubkey::Pubkey;

    use super::*;

    fn create_delegation_record(delegation_slot: u64) -> DelegationRecord {
        DelegationRecord {
            authority: Pubkey::default(),
            owner: Pubkey::default(),
            delegation_slot,
            lamports: 1000,
            commit_frequency_ms: 100,
        }
    }

    #[test]
    fn test_account_was_undelegated_and_remained_so() {
        // Case A: The account was undelegated and remained so.
        // Conditions:
        // - is_delegated: false (account is not delegated to us on chain)
        // - deleg_record: None (no delegation record associated)
        // Expected: true (should override/clone as is)

        let pubkey = Pubkey::default();
        let is_delegated = false;
        let remote_slot = 100;
        let deleg_record = None;

        assert!(!account_still_undelegating_on_chain(
            &pubkey,
            is_delegated,
            remote_slot,
            deleg_record,
            &Pubkey::default(),
        ));
    }

    #[test]
    fn test_account_was_undelegated_and_redelegated_to_us() {
        // Case B: The account was undelegated and was re-delegated to us.
        // Conditions:
        // - is_delegated: true (account is delegated to us on chain)
        // - delegation_slot > remote_slot (delegation happend after we last updated the account)
        // Expected: true (should override/update)

        let pubkey = Pubkey::default();
        let is_delegated = true;
        let remote_slot = 100;

        // NOTE: this case led to incorrect unborking if delegation + undelegation + redelegation
        // happend all in the same slot
        // Subcase B1: delegation_slot == remote_slot
        /*
        let delegation_slot = 100;
        let deleg_record = Some(create_delegation_record(delegation_slot));
        assert!(!account_still_undelegating_on_chain(
            &pubkey,
            is_delegated,
            remote_slot,
            deleg_record,
            &Pubkey::default(),
        ));
        */

        // Subcase B2: delegation_slot > remote_slot
        let delegation_slot = 101;
        let deleg_record = Some(create_delegation_record(delegation_slot));
        assert!(!account_still_undelegating_on_chain(
            &pubkey,
            is_delegated,
            remote_slot,
            deleg_record,
            &Pubkey::default(),
        ));
    }

    #[test]
    fn test_account_was_undelegated_and_redelegated_to_another() {
        // Case C: The account was undelegated and then re-delegated to another validator.
        // Conditions:
        // - is_delegated: false (not delegated to us on chain)
        // - deleg_record: Some(...) (but record exists, maybe describing delegation to another)
        // Expected: true (should override/clone as is)

        let pubkey = Pubkey::default();
        let is_delegated = false;
        let remote_slot = 100;
        // Value doesn't matter for this path
        let delegation_slot = 90;
        let deleg_record = Some(create_delegation_record(delegation_slot));

        assert!(!account_still_undelegating_on_chain(
            &pubkey,
            is_delegated,
            remote_slot,
            deleg_record,
            &Pubkey::default(),
        ));
    }

    #[test]
    fn test_account_undelegation_pending() {
        // Case D: The account's undelegation request did not complete.
        // Conditions:
        // - is_delegated: true
        // - delegation_slot < remote_slot (delegation is older than remote update, implying pending undelegation)
        // Expected: false (should NOT override, keep bank account)

        let pubkey = Pubkey::default();
        let is_delegated = true;
        let remote_slot = 100;
        let delegation_slot = 99;
        let deleg_record = Some(create_delegation_record(delegation_slot));

        assert!(account_still_undelegating_on_chain(
            &pubkey,
            is_delegated,
            remote_slot,
            deleg_record,
            &Pubkey::default(),
        ));
    }
}
