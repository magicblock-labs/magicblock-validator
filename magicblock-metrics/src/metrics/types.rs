use std::{
    fmt,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use solana_signature::Signature;

// -----------------
// Outcome
// -----------------
const OUTCOME_SUCCESS: &str = "success";
const OUTCOME_ERROR: &str = "error";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Success,
    Error,
}

impl fmt::Display for Outcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use Outcome::*;
        match self {
            Success => write!(f, "{OUTCOME_SUCCESS}"),
            Error => write!(f, "{OUTCOME_ERROR}"),
        }
    }
}

impl Outcome {
    fn as_str(&self) -> &str {
        use Outcome::*;
        match self {
            Success => OUTCOME_SUCCESS,
            Error => OUTCOME_ERROR,
        }
    }
}

impl LabelValue for Outcome {
    fn value(&self) -> &str {
        self.as_str()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountFetchEntrypoint {
    RpcGetAccount,
    RpcGetMultipleAccounts,
    SendTransaction(Signature),
    SimulateTransaction(Signature),
    SubscriptionUpdate,
    ProjectAta,
    Internal,
}

impl AccountFetchEntrypoint {
    fn as_str(self) -> &'static str {
        match self {
            Self::RpcGetAccount => "rpc_get_account",
            Self::RpcGetMultipleAccounts => "rpc_get_multiple_accounts",
            Self::SendTransaction(_) => "send_transaction",
            Self::SimulateTransaction(_) => "simulate_transaction",
            Self::SubscriptionUpdate => "subscription_update",
            Self::ProjectAta => "project_ata",
            Self::Internal => "internal",
        }
    }

    fn signature(&self) -> Option<&Signature> {
        match self {
            Self::SendTransaction(sig) | Self::SimulateTransaction(sig) => {
                Some(sig)
            }
            _ => None,
        }
    }
}

impl fmt::Display for AccountFetchEntrypoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl LabelValue for AccountFetchEntrypoint {
    fn value(&self) -> &str {
        self.as_str()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountFetchReason {
    RequestedAccount,
    DelegationRecord,
    ProgramData,
    ActionDependencyMissing,
    ActionDependencyForcedRefresh,
    UndelegatingRefresh,
    SubscriptionUpdateClone,
    SubscriptionUpdateGreedyDiscovery,
    AtaProjection,
    ProgramLoad,
    Clock,
}

impl AccountFetchReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::RequestedAccount => "requested_account",
            Self::DelegationRecord => "delegation_record",
            Self::ProgramData => "program_data",
            Self::ActionDependencyMissing => "action_dependency_missing",
            Self::ActionDependencyForcedRefresh => {
                "action_dependency_forced_refresh"
            }
            Self::UndelegatingRefresh => "undelegating_refresh",
            Self::SubscriptionUpdateClone => "subscription_update_clone",
            Self::SubscriptionUpdateGreedyDiscovery => {
                "subscription_update_greedy_discovery"
            }
            Self::AtaProjection => "ata_projection",
            Self::ProgramLoad => "program_load",
            Self::Clock => "clock",
        }
    }
}

impl fmt::Display for AccountFetchReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl LabelValue for AccountFetchReason {
    fn value(&self) -> &str {
        self.as_str()
    }
}

#[derive(Debug, Clone)]
pub struct AccountFetchContext {
    entrypoint: AccountFetchEntrypoint,
    reason: AccountFetchReason,
    remote_account_claims: Arc<AtomicU64>,
}

impl AccountFetchContext {
    fn new(
        entrypoint: AccountFetchEntrypoint,
        reason: AccountFetchReason,
    ) -> Self {
        Self {
            entrypoint,
            reason,
            remote_account_claims: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn rpc_get_account() -> Self {
        AccountFetchEntrypoint::RpcGetAccount.into()
    }

    pub fn rpc_get_multiple_accounts() -> Self {
        AccountFetchEntrypoint::RpcGetMultipleAccounts.into()
    }

    pub fn send_transaction(signature: Signature) -> Self {
        AccountFetchEntrypoint::SendTransaction(signature).into()
    }

    pub fn simulate_transaction(signature: Signature) -> Self {
        AccountFetchEntrypoint::SimulateTransaction(signature).into()
    }

    pub fn subscription_update(reason: AccountFetchReason) -> Self {
        Self::new(AccountFetchEntrypoint::SubscriptionUpdate, reason)
    }

    pub fn project_ata() -> Self {
        Self::new(
            AccountFetchEntrypoint::ProjectAta,
            AccountFetchReason::AtaProjection,
        )
    }

    pub fn internal(reason: AccountFetchReason) -> Self {
        Self::new(AccountFetchEntrypoint::Internal, reason)
    }

    pub fn entrypoint(&self) -> AccountFetchEntrypoint {
        self.entrypoint
    }

    pub fn reason(&self) -> AccountFetchReason {
        self.reason
    }

    pub fn with_reason(self, reason: AccountFetchReason) -> Self {
        Self { reason, ..self }
    }

    pub fn should_count_remote_account_claims(&self) -> bool {
        // We only count fetches due to direct user requests, not due to internal
        // fetches, i.e. to get a companion account
        if self.reason != AccountFetchReason::RequestedAccount {
            return false;
        }
        matches!(
            self.entrypoint,
            AccountFetchEntrypoint::RpcGetAccount
                | AccountFetchEntrypoint::RpcGetMultipleAccounts
                | AccountFetchEntrypoint::SendTransaction(_)
                | AccountFetchEntrypoint::SimulateTransaction(_)
        )
    }

    pub fn add_remote_account_claims(&self, count: usize) {
        self.remote_account_claims
            .fetch_add(count as u64, Ordering::Relaxed);
    }

    pub fn remote_account_claims_value(&self) -> u64 {
        self.remote_account_claims.load(Ordering::Relaxed)
    }

    pub fn signature(&self) -> Option<&Signature> {
        self.entrypoint.signature()
    }
}

/// Starts shared requested-account fetch tracking from a copyable origin.
impl From<AccountFetchEntrypoint> for AccountFetchContext {
    fn from(entrypoint: AccountFetchEntrypoint) -> Self {
        Self::new(entrypoint, AccountFetchReason::RequestedAccount)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChainlinkPendingFetchLayer {
    RemoteAccountProvider,
}

impl ChainlinkPendingFetchLayer {
    fn as_str(&self) -> &str {
        match self {
            Self::RemoteAccountProvider => "remote_account_provider",
        }
    }
}

impl fmt::Display for ChainlinkPendingFetchLayer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl LabelValue for ChainlinkPendingFetchLayer {
    fn value(&self) -> &str {
        self.as_str()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChainlinkPendingFetchOutcome {
    Owned,
    JoinedExisting,
    OwnerSucceeded,
    OwnerFailed,
    ResolvedBySubscriptionUpdate,
    RpcFetchCompletedAfterUpdate,
}

impl ChainlinkPendingFetchOutcome {
    fn as_str(&self) -> &str {
        match self {
            Self::Owned => "owned",
            Self::JoinedExisting => "joined_existing",
            Self::OwnerSucceeded => "owner_succeeded",
            Self::OwnerFailed => "owner_failed",
            Self::ResolvedBySubscriptionUpdate => {
                "resolved_by_subscription_update"
            }
            Self::RpcFetchCompletedAfterUpdate => {
                "rpc_fetch_completed_after_update"
            }
        }
    }
}

impl fmt::Display for ChainlinkPendingFetchOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl LabelValue for ChainlinkPendingFetchOutcome {
    fn value(&self) -> &str {
        self.as_str()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChainlinkCompanionFetchKind {
    ProgramData,
    DelegationRecord,
    AtaProjection,
}

impl ChainlinkCompanionFetchKind {
    fn as_str(&self) -> &str {
        match self {
            Self::ProgramData => "program_data",
            Self::DelegationRecord => "delegation_record",
            Self::AtaProjection => "ata_projection",
        }
    }
}

impl fmt::Display for ChainlinkCompanionFetchKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl LabelValue for ChainlinkCompanionFetchKind {
    fn value(&self) -> &str {
        self.as_str()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChainlinkCompanionFetchOutcome {
    Succeeded,
    FailedRpc,
    FailedSlotMismatch,
    FailedMinContextSlot,
}

impl ChainlinkCompanionFetchOutcome {
    fn as_str(&self) -> &str {
        match self {
            Self::Succeeded => "succeeded",
            Self::FailedRpc => "failed_rpc",
            Self::FailedSlotMismatch => "failed_slot_mismatch",
            Self::FailedMinContextSlot => "failed_min_context_slot",
        }
    }
}

impl fmt::Display for ChainlinkCompanionFetchOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl LabelValue for ChainlinkCompanionFetchOutcome {
    fn value(&self) -> &str {
        self.as_str()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChainlinkCloneRemoteResult {
    Found,
    NotFound,
    Failed,
}

impl ChainlinkCloneRemoteResult {
    fn as_str(&self) -> &str {
        match self {
            Self::Found => "found",
            Self::NotFound => "not_found",
            Self::Failed => "failed",
        }
    }
}

impl fmt::Display for ChainlinkCloneRemoteResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl LabelValue for ChainlinkCloneRemoteResult {
    fn value(&self) -> &str {
        self.as_str()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChainlinkCloneIntent {
    NormalAccount,
    EmptyPlaceholder,
    ProgramData,
    DelegationRecord,
    ActionDependency,
    Unknown,
}

impl ChainlinkCloneIntent {
    fn as_str(&self) -> &str {
        match self {
            Self::NormalAccount => "normal_account",
            Self::EmptyPlaceholder => "empty_placeholder",
            Self::ProgramData => "program_data",
            Self::DelegationRecord => "delegation_record",
            Self::ActionDependency => "action_dependency",
            Self::Unknown => "unknown",
        }
    }
}

impl fmt::Display for ChainlinkCloneIntent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl LabelValue for ChainlinkCloneIntent {
    fn value(&self) -> &str {
        self.as_str()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChainlinkCloneOutcome {
    Submitted,
    SubmitFailed,
    CloneSucceeded,
    CloneFailed,
    Skipped,
}

impl ChainlinkCloneOutcome {
    fn as_str(&self) -> &str {
        match self {
            Self::Submitted => "submitted",
            Self::SubmitFailed => "submit_failed",
            Self::CloneSucceeded => "clone_succeeded",
            Self::CloneFailed => "clone_failed",
            Self::Skipped => "skipped",
        }
    }
}

impl fmt::Display for ChainlinkCloneOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl LabelValue for ChainlinkCloneOutcome {
    fn value(&self) -> &str {
        self.as_str()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChainlinkEmptyPlaceholderStage {
    ConvertedToEmpty,
    CloneSubmitted,
    CloneSubmitFailed,
}

impl ChainlinkEmptyPlaceholderStage {
    fn as_str(&self) -> &str {
        match self {
            Self::ConvertedToEmpty => "converted_to_empty",
            Self::CloneSubmitted => "clone_submitted",
            Self::CloneSubmitFailed => "clone_submit_failed",
        }
    }
}

impl fmt::Display for ChainlinkEmptyPlaceholderStage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl LabelValue for ChainlinkEmptyPlaceholderStage {
    fn value(&self) -> &str {
        self.as_str()
    }
}

#[derive(Debug, Clone)]
pub enum SubscriptionRegistrationOrigin {
    Fetch(AccountFetchContext),
    Internal,
}

impl SubscriptionRegistrationOrigin {
    pub(super) fn entrypoint_str(&self) -> &str {
        match self {
            Self::Fetch(context) => context.entrypoint().as_str(),
            Self::Internal => AccountFetchEntrypoint::Internal.as_str(),
        }
    }

    pub(super) fn fetch_reason_str(&self) -> &str {
        match self {
            Self::Fetch(context) => context.reason().as_str(),
            Self::Internal => AccountFetchReason::RequestedAccount.as_str(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubscriptionReasonLabel {
    DirectAccount,
    DelegationRecord,
    ProgramData,
    UndelegationTracking,
    AtaProjection,
}

impl SubscriptionReasonLabel {
    fn as_str(&self) -> &str {
        match self {
            Self::DirectAccount => "direct_account",
            Self::DelegationRecord => "delegation_record",
            Self::ProgramData => "program_data",
            Self::UndelegationTracking => "undelegation_tracking",
            Self::AtaProjection => "ata_projection",
        }
    }
}

impl fmt::Display for SubscriptionReasonLabel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl LabelValue for SubscriptionReasonLabel {
    fn value(&self) -> &str {
        self.as_str()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubscriptionRegistrationOutcome {
    AlreadyPresent,
    Added,
    SubscribeError,
}

impl SubscriptionRegistrationOutcome {
    fn as_str(&self) -> &str {
        match self {
            Self::AlreadyPresent => "already_present",
            Self::Added => "added",
            Self::SubscribeError => "subscribe_error",
        }
    }
}

impl fmt::Display for SubscriptionRegistrationOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl LabelValue for SubscriptionRegistrationOutcome {
    fn value(&self) -> &str {
        self.as_str()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubscriptionReleaseOutcome {
    Unsubscribed,
    AlreadyAbsent,
    UnsubscribeFailed,
    RetainedIntentionally,
    RetainedOtherReasons,
}

impl SubscriptionReleaseOutcome {
    fn as_str(&self) -> &str {
        match self {
            Self::Unsubscribed => "unsubscribed",
            Self::AlreadyAbsent => "already_absent",
            Self::UnsubscribeFailed => "unsubscribe_failed",
            Self::RetainedIntentionally => "retained_intentionally",
            Self::RetainedOtherReasons => "retained_other_reasons",
        }
    }
}

impl fmt::Display for SubscriptionReleaseOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl LabelValue for SubscriptionReleaseOutcome {
    fn value(&self) -> &str {
        self.as_str()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubscriptionCleanupSource {
    NormalRelease,
    ManualUnsubscribe,
    DelegatedAccountSilent,
    Reconciler,
}

impl SubscriptionCleanupSource {
    fn as_str(&self) -> &str {
        match self {
            Self::NormalRelease => "normal_release",
            Self::ManualUnsubscribe => "manual_unsubscribe",
            Self::DelegatedAccountSilent => "delegated_account_silent",
            Self::Reconciler => "reconciler",
        }
    }
}

impl fmt::Display for SubscriptionCleanupSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl LabelValue for SubscriptionCleanupSource {
    fn value(&self) -> &str {
        self.as_str()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubscriptionCleanupOutcome {
    Unsubscribed,
    AlreadyAbsent,
    UnsubscribeFailed,
    RemovalUpdateFailed,
    RetainedIntentionally,
}

impl SubscriptionCleanupOutcome {
    fn as_str(&self) -> &str {
        match self {
            Self::Unsubscribed => "unsubscribed",
            Self::AlreadyAbsent => "already_absent",
            Self::UnsubscribeFailed => "unsubscribe_failed",
            Self::RemovalUpdateFailed => "removal_update_failed",
            Self::RetainedIntentionally => "retained_intentionally",
        }
    }
}

impl fmt::Display for SubscriptionCleanupOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl LabelValue for SubscriptionCleanupOutcome {
    fn value(&self) -> &str {
        self.as_str()
    }
}

pub trait LabelValue {
    fn value(&self) -> &str;
}

impl LabelValue for &str {
    fn value(&self) -> &str {
        self
    }
}

impl LabelValue for String {
    fn value(&self) -> &str {
        self
    }
}

impl<T, E> LabelValue for Result<T, E>
where
    T: LabelValue,
    E: LabelValue,
{
    fn value(&self) -> &str {
        match self {
            Ok(ok) => ok.value(),
            Err(err) => err.value(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_fetch_entrypoint_labels_are_static() {
        let signature = Signature::from([1u8; 64]);
        let cases = [
            (AccountFetchEntrypoint::RpcGetAccount, "rpc_get_account"),
            (
                AccountFetchEntrypoint::RpcGetMultipleAccounts,
                "rpc_get_multiple_accounts",
            ),
            (
                AccountFetchEntrypoint::SendTransaction(signature),
                "send_transaction",
            ),
            (
                AccountFetchEntrypoint::SimulateTransaction(signature),
                "simulate_transaction",
            ),
            (
                AccountFetchEntrypoint::SubscriptionUpdate,
                "subscription_update",
            ),
            (AccountFetchEntrypoint::ProjectAta, "project_ata"),
            (AccountFetchEntrypoint::Internal, "internal"),
        ];

        for (entrypoint, expected) in cases {
            assert_eq!(entrypoint.as_str(), expected);
            assert_eq!(entrypoint.to_string(), expected);
            assert_eq!(entrypoint.value(), expected);
        }
    }

    #[test]
    fn account_fetch_reason_labels_are_static() {
        let cases = [
            (AccountFetchReason::RequestedAccount, "requested_account"),
            (AccountFetchReason::DelegationRecord, "delegation_record"),
            (AccountFetchReason::ProgramData, "program_data"),
            (
                AccountFetchReason::ActionDependencyMissing,
                "action_dependency_missing",
            ),
            (
                AccountFetchReason::ActionDependencyForcedRefresh,
                "action_dependency_forced_refresh",
            ),
            (
                AccountFetchReason::UndelegatingRefresh,
                "undelegating_refresh",
            ),
            (
                AccountFetchReason::SubscriptionUpdateClone,
                "subscription_update_clone",
            ),
            (
                AccountFetchReason::SubscriptionUpdateGreedyDiscovery,
                "subscription_update_greedy_discovery",
            ),
            (AccountFetchReason::AtaProjection, "ata_projection"),
            (AccountFetchReason::ProgramLoad, "program_load"),
            (AccountFetchReason::Clock, "clock"),
        ];

        for (reason, expected) in cases {
            assert_eq!(reason.as_str(), expected);
            assert_eq!(reason.to_string(), expected);
            assert_eq!(reason.value(), expected);
        }
    }

    #[test]
    fn account_fetch_context_signature_is_for_transaction_entrypoints() {
        let signature = Signature::from([1u8; 64]);
        let send_context = AccountFetchContext::send_transaction(signature);
        let simulate_context =
            AccountFetchContext::simulate_transaction(signature);
        for context in [&send_context, &simulate_context] {
            assert_eq!(context.signature(), Some(&signature));
            assert_eq!(context.entrypoint().signature(), Some(&signature));
        }

        let contexts = [
            AccountFetchContext::rpc_get_account(),
            AccountFetchContext::rpc_get_multiple_accounts(),
            AccountFetchContext::subscription_update(
                AccountFetchReason::SubscriptionUpdateClone,
            ),
            AccountFetchContext::project_ata(),
            AccountFetchContext::internal(AccountFetchReason::Clock),
        ];

        for context in contexts {
            assert_eq!(context.signature(), None);
            assert_eq!(context.entrypoint().signature(), None);
        }
    }

    #[test]
    fn simulation_requested_accounts_count_remote_account_claims() {
        let context = AccountFetchContext::simulate_transaction(
            Signature::from([1u8; 64]),
        );
        assert!(context.should_count_remote_account_claims());
        assert!(
            !context
                .with_reason(AccountFetchReason::ProgramData)
                .should_count_remote_account_claims()
        );
    }
}
