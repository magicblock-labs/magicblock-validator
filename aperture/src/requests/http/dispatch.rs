use super::{
    HandlerResult, RpcHandlers, get_blocks::BlockRange, get_token_accounts::TokenAccountAuthority,
};
use crate::{error::RpcError, requests::JsonHttpRequest};
use magicblock_metrics::metrics::{RPC_REQUEST_HANDLING_TIME, RPC_REQUESTS_COUNT};

impl RpcHandlers {
    pub(crate) async fn process(
        &self,
        request: &JsonHttpRequest,
        claims: &mut u64,
    ) -> HandlerResult {
        // Route the request to the correct handler based on the method name.
        use crate::requests::JsonRpcHttpMethod::*;
        let method = request.method.as_str();
        RPC_REQUESTS_COUNT.with_label_values(&[method]).inc();
        let _timer = RPC_REQUEST_HANDLING_TIME.with_label_values(&[method]).start_timer();

        match request.method {
            GetAccountInfo => self.get_account_info(request, claims).await,
            GetBalance => self.get_balance(request, claims).await,
            GetBlock => self.get_block(request).await,
            GetBlockCommitment => self.get_block_commitment(request),
            GetBlockHeight => self.get_slot(request),
            GetBlockTime => self.get_block_time(request).await,
            GetBlocks => self.get_blocks(request, BlockRange::EndSlot),
            GetBlocksWithLimit => self.get_blocks(request, BlockRange::Limit),
            GetClusterNodes => self.get_cluster_nodes(request),
            GetEpochInfo => self.get_epoch_info(request),
            GetEpochSchedule => self.get_epoch_schedule(request),
            GetFeeForMessage => self.get_fee_for_message(request),
            GetFirstAvailableBlock => self.mock_zero(request),
            GetGenesisHash => self.get_genesis_hash(request),
            GetHealth => self.get_health(request),
            GetHighestSnapshotSlot => self.get_highest_snapshot_slot(request),
            GetIdentity => self.get_identity(request),
            GetLargestAccounts => self.mock_empty_context(request),
            GetLatestBlockhash => self.get_latest_blockhash(request),
            GetMultipleAccounts => self.get_multiple_accounts(request, claims).await,
            GetProgramAccounts => self.get_program_accounts(request),
            GetRecentPerformanceSamples => self.get_recent_performance_samples(request),
            GetSignatureStatuses => self.get_signature_statuses(request).await,
            GetSignaturesForAddress => self.get_signatures_for_address(request).await,
            GetSlot => self.get_slot(request),
            GetSlotLeader => self.get_slot_leader(request),
            GetSlotLeaders => self.get_slot_leaders(request),
            GetSupply => self.get_supply(request),
            GetTokenAccountBalance => self.get_token_account_balance(request, claims).await,
            GetTokenAccountsByDelegate => {
                self.get_token_accounts(request, TokenAccountAuthority::Delegate)
            }
            GetTokenAccountsByOwner => {
                self.get_token_accounts(request, TokenAccountAuthority::Owner)
            }
            GetTokenLargestAccounts => self.mock_empty_context(request),
            GetTokenSupply => self.get_token_supply(request),
            GetTransaction => self.get_transaction(request).await,
            GetTransactionCount => self.mock_zero(request),
            GetVersion => self.get_version(request),
            GetVoteAccounts => self.get_vote_accounts(request),
            IsBlockhashValid => self.is_blockhash_valid(request),
            MinimumLedgerSlot => self.mock_zero(request),
            RequestAirdrop => self.request_airdrop(request).await,
            SendTransaction => self.send_transaction(request, claims).await,
            SimulateTransaction => self.simulate_transaction(request, claims).await,
            GetRoutes => self.mock_empty(request),
            // Alias for getLatestBlockhash; exists for Magic Router SDK compatibility.
            GetBlockhashForAccounts => self.get_latest_blockhash(request),
            GetDelegationStatus => self.get_delegation_status(request, claims).await,
            MethodNotFound => Err(RpcError::method_not_found()),
        }
    }
}
