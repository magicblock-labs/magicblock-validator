//! Action preparation, assessed activation, and failed-activation rescue shared by all transports.

use std::collections::BTreeMap;

use delegation_program_api::{Decrypt, args::PostDelegationActions};
use engine::PostFinalize;
use solana_account::{AccountBuilder, AccountMode};
use solana_instruction::Instruction;
use solana_pubkey::Pubkey;
use tracing::{error, warn};

use super::{Delegation, appended, undelegation_action};
use crate::{
    AccountProperty, ChainSync, ChainSyncAccount, Error, Result, aml,
    metrics::{self, Op},
};

impl ChainSync {
    /// Decodes and decrypts actions, returning their dependencies without taking an account lease.
    pub(crate) fn prepare_delegation(
        &self,
        delegation: Delegation,
    ) -> Result<(PreparedDelegation, Vec<ChainSyncAccount>)> {
        let Delegation {
            pubkey,
            account,
            record,
            source_program,
        } = delegation;
        let appended = appended(&record).ok_or(Error::Record("record too short"))?;
        // Retain decoding errors with the validated delegation so materialization
        // can schedule rescue instead of trying to fetch unusable action dependencies.
        let actions = self.decode_actions(appended);
        let dependencies = match &actions {
            Ok(Some(actions)) => action_dependencies(pubkey, actions),
            _ => Vec::new(),
        };
        let prepared = PreparedDelegation {
            pubkey,
            account,
            actions,
            source_program,
        };
        Ok((prepared, dependencies))
    }

    /// Decrypts an optional action payload; callers retain failures for activation rescue.
    fn decode_actions(&self, payload: &[u8]) -> Result<Option<Vec<Instruction>>> {
        if payload.is_empty() {
            return Ok(None);
        }
        let actions = borsh::from_slice::<PostDelegationActions>(payload)?;
        Ok(Some(actions.decrypt_with_keypair(self.engine.signer())?))
    }

    /// Activates a delegation whose action dependencies have already been acquired.
    /// Invalid actions, rejected signers, and materialization failures enter rescue;
    /// an unavailable assessment service returns an error without rescue.
    pub(crate) async fn materialize_delegation(&self, prepared: PreparedDelegation) -> Result<()> {
        let _timer = metrics::time(Op::Delegate);
        let PreparedDelegation {
            pubkey,
            account,
            mut actions,
            source_program,
        } = prepared;
        // An unavailable AML service is not a rejection: return without scheduling rescue.
        if let (Some(aml), Ok(Some(instructions))) = (&self.aml, &actions) {
            let rejected = aml::check(aml, instructions).await?;
            if !rejected.is_empty() {
                actions = Err(aml::Error::Rejected(rejected).into());
            }
        }
        let result = match actions {
            Ok(actions) => {
                let actions = actions.map(|actions| PostFinalize { source_program, actions });
                let accessor = self.engine.account(pubkey).await?;
                accessor.materialize(account.clone(), actions).await.map_err(Error::from)
            }
            Err(error) => Err(error),
        };
        let Err(error) = result else { return Ok(()) };
        metrics::activation_failed();
        warn!(%pubkey, %error, "activation failed; rescuing");
        if let Err(rescue_error) = self.rescue_delegation(pubkey, account, source_program).await {
            metrics::rescue("failed");
            error!(%pubkey, %error, %rescue_error, "rescue failed");
            return Err(error);
        }
        Ok(())
    }

    /// Acquires the account's lease and schedules rescue unless local state already supersedes it.
    pub(super) async fn rescue_delegation(
        &self,
        pubkey: Pubkey,
        account: AccountBuilder,
        source_program: Pubkey,
    ) -> engine::Result<()> {
        let slot = account.read().slot();
        let accessor = self.engine.account(pubkey).await?;
        // Skip rescue for an already handled delegation, newer state, or confined account.
        let superseded = match accessor.observed() {
            Some((AccountMode::Magic, _)) => true,
            Some((AccountMode::Transient, observed)) if observed == slot => true,
            _ => accessor.skipped(AccountMode::Delegated, slot).is_some(),
        };
        if superseded {
            metrics::rescue("superseded");
            return Ok(());
        }
        let action = undelegation_action(self.engine.authority(), pubkey);
        let rescue = PostFinalize {
            source_program,
            actions: vec![action],
        };
        accessor.materialize(account, Some(rescue)).await?;
        metrics::rescue("scheduled");
        Ok(())
    }
}

/// Collects sorted action dependencies, excluding the account being activated.
/// Program roles replace earlier roles; writable metas override the role seen so far.
fn action_dependencies(pubkey: Pubkey, actions: &[Instruction]) -> Vec<ChainSyncAccount> {
    let mut dependencies = BTreeMap::new();
    for action in actions {
        dependencies.insert(action.program_id, AccountProperty::Program);
        for meta in &action.accounts {
            let property = dependencies.entry(meta.pubkey).or_insert(AccountProperty::Readonly);
            if meta.is_writable {
                *property = AccountProperty::Writable;
            }
        }
    }
    // The delegated account itself is materialized later; do not fetch it as a dependency.
    dependencies.remove(&pubkey);
    dependencies
        .into_iter()
        .map(|(pubkey, property)| ChainSyncAccount { pubkey, property })
        .collect()
}

/// Delegated account and action-decoding result held while action dependencies are fetched.
pub(crate) struct PreparedDelegation {
    pub(crate) pubkey: Pubkey,
    pub(super) account: AccountBuilder,
    /// Decrypted actions, or a decoding/decryption error that requires rescue undelegation.
    pub(super) actions: Result<Option<Vec<Instruction>>>,
    /// Original owning program from the validated delegation record.
    /// For an eATA projection, this is the eATA program, not the ATA's token program.
    pub(super) source_program: Pubkey,
}
