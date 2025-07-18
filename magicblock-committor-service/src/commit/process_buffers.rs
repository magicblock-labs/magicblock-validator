use std::collections::HashMap;

use dlp::args::CommitStateFromBufferArgs;
use log::*;
use solana_pubkey::Pubkey;

use crate::{
    bundles::{bundle_chunks, bundle_chunks_ignoring_bundle_id},
    transactions::{
        close_buffers_ix, process_and_close_ixs, process_commits_ix,
        MAX_CLOSE_PER_TX, MAX_CLOSE_PER_TX_USING_LOOKUP,
        MAX_PROCESS_AND_CLOSE_PER_TX,
        MAX_PROCESS_AND_CLOSE_PER_TX_USING_LOOKUP, MAX_PROCESS_PER_TX,
        MAX_PROCESS_PER_TX_USING_LOOKUP,
    },
    types::{InstructionsForCommitable, InstructionsKind},
    CommitInfo,
};

/// Returns instructions to process the commit/delegation request for a commitable.
/// Requires that the [CommitInfo::buffer_pda] holds all data to be committed.
/// It appends another instruction which closes both the [CommitInfo::buffer_pda]
/// and the [CommitInfo::chunks_pda].
fn process_commitable_and_close_ixs(
    validator_auth: Pubkey,
    commit_info: CommitInfo,
) -> InstructionsForCommitable {
    debug!("Processing commitable: {:?}", commit_info);
    let CommitInfo::BufferedDataAccount {
        pubkey,
        delegated_account_owner,
        slot,
        ephemeral_blockhash,
        undelegate,
        buffer_pda,
        lamports,
        ..
    } = &commit_info
    else {
        panic!("Only data accounts are supported for now");
    };

    let commit_args = CommitStateFromBufferArgs {
        slot: *slot,
        lamports: *lamports,
        allow_undelegation: *undelegate,
    };

    let instructions = process_and_close_ixs(
        validator_auth,
        pubkey,
        delegated_account_owner,
        buffer_pda,
        ephemeral_blockhash,
        commit_args,
    );
    InstructionsForCommitable {
        instructions,
        commit_info,
        kind: InstructionsKind::ProcessAndCloseBuffers,
    }
}

fn close_buffers_separate_ix(
    validator_auth: Pubkey,
    commit_info: CommitInfo,
) -> InstructionsForCommitable {
    debug!("Processing commitable: {:?}", commit_info);
    let CommitInfo::BufferedDataAccount { pubkey, .. } = &commit_info else {
        panic!("Only data accounts are supported for now");
    };

    // TODO(edwin)L fix commit_id
    let close_ix = close_buffers_ix(validator_auth, pubkey, 0);
    InstructionsForCommitable {
        instructions: vec![close_ix],
        commit_info,
        kind: InstructionsKind::CloseBuffers,
    }
}

fn process_commitable_separate_ix(
    validator_auth: Pubkey,
    commit_info: CommitInfo,
) -> InstructionsForCommitable {
    let CommitInfo::BufferedDataAccount {
        pubkey,
        delegated_account_owner,
        slot,
        undelegate,
        buffer_pda,
        lamports,
        ..
    } = &commit_info
    else {
        panic!("Only data accounts are supported for now");
    };

    let commit_args = CommitStateFromBufferArgs {
        slot: *slot,
        lamports: *lamports,
        allow_undelegation: *undelegate,
    };

    let process_ix = process_commits_ix(
        validator_auth,
        pubkey,
        delegated_account_owner,
        buffer_pda,
        commit_args,
    );
    InstructionsForCommitable {
        instructions: vec![process_ix],
        commit_info,
        kind: InstructionsKind::Process,
    }
}

pub(crate) struct ChunkedIxsToProcessCommitablesAndClosePdasResult {
    /// Chunked instructions to process buffers and possibly also close them
    /// Since they are part of the same transaction and correctly ordered, each
    /// chunk can run in parallel
    pub chunked_ixs: Vec<Vec<InstructionsForCommitable>>,
    /// Separate buffer close transactions.
    /// Since the process transactions need to complete first we need to run them
    /// after the [Self::chunked_ixs] transactions
    pub chunked_close_ixs: Option<Vec<Vec<InstructionsForCommitable>>>,
    /// Commitables that could not be chunked and thus cannot be committed while
    /// respecting the bundle
    pub unchunked: HashMap<u64, Vec<CommitInfo>>,
}

/// Processes commits
/// Creates single instruction chunk for commmitables with matching bundle_id
pub(crate) fn chunked_ixs_to_process_commitables_and_close_pdas(
    validator_auth: Pubkey,
    commit_infos: Vec<CommitInfo>,
    use_lookup: bool,
) -> ChunkedIxsToProcessCommitablesAndClosePdasResult {
    // First try to combine process and close into a single transaction
    let max_per_chunk = if use_lookup {
        MAX_PROCESS_AND_CLOSE_PER_TX_USING_LOOKUP
    } else {
        MAX_PROCESS_AND_CLOSE_PER_TX
    };
    let bundles_with_close =
        bundle_chunks(commit_infos, max_per_chunk as usize);

    // Add instruction chunks that include process and close
    let mut chunked_ixs: Vec<_> = bundles_with_close
        .chunks
        .into_iter()
        .map(|chunk| {
            chunk
                .into_iter()
                .map(|commit_info| {
                    process_commitable_and_close_ixs(
                        validator_auth,
                        commit_info,
                    )
                })
                .collect::<Vec<_>>()
        })
        .collect();

    // If all bundles can be handled combining process and close then we're done
    let all_bundles_handled = bundles_with_close.unchunked.is_empty();
    if all_bundles_handled {
        return ChunkedIxsToProcessCommitablesAndClosePdasResult {
            chunked_ixs,
            chunked_close_ixs: None,
            unchunked: bundles_with_close.unchunked,
        };
    }

    // If not all chunks fit when trying to close and process in one transaction
    // then let's process them separately
    let unbundled_commit_infos = bundles_with_close
        .unchunked
        .into_iter()
        .flat_map(|(_, commit_infos)| commit_infos)
        .collect::<Vec<_>>();

    // For the bundles that are too large to include the close instructions add them
    // as separate instruction chunks, one for process (which is the only part
    // that needs to run atomic for a bundle) and another chunk for the close buffer
    // instructions
    let close_bundles = {
        let max_per_chunk = if use_lookup {
            MAX_CLOSE_PER_TX_USING_LOOKUP
        } else {
            MAX_CLOSE_PER_TX
        };
        bundle_chunks_ignoring_bundle_id(
            &unbundled_commit_infos,
            max_per_chunk as usize,
        )
    };

    let process_bundles_with_separate_close = {
        let max_per_chunk = if use_lookup {
            MAX_PROCESS_PER_TX_USING_LOOKUP
        } else {
            MAX_PROCESS_PER_TX
        };
        bundle_chunks(unbundled_commit_infos, max_per_chunk as usize)
    };
    for bundle in process_bundles_with_separate_close.chunks {
        let mut process_ixs = Vec::new();
        for commit_info in bundle {
            let process_ix =
                process_commitable_separate_ix(validator_auth, commit_info);
            process_ixs.push(process_ix);
        }
        chunked_ixs.push(process_ixs);
    }

    let mut close_ixs_chunks = Vec::new();
    for bundle in close_bundles.chunks {
        let mut close_ixs = Vec::new();
        for commit_info in bundle {
            let close_ix =
                close_buffers_separate_ix(validator_auth, commit_info);
            close_ixs.push(close_ix);
        }
        close_ixs_chunks.push(close_ixs);
    }

    ChunkedIxsToProcessCommitablesAndClosePdasResult {
        chunked_ixs,
        chunked_close_ixs: Some(close_ixs_chunks),
        unchunked: process_bundles_with_separate_close.unchunked,
    }
}
