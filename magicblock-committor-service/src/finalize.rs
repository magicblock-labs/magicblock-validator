use std::collections::HashMap;

use log::*;
use solana_pubkey::Pubkey;

use crate::{
    bundles::bundle_chunks,
    transactions::{
        finalize_ix, MAX_FINALIZE_PER_TX, MAX_FINALIZE_PER_TX_USING_LOOKUP,
    },
    types::{InstructionsForCommitable, InstructionsKind},
    CommitInfo,
};

fn finalize_commitable(
    validator_auth: Pubkey,
    commit_info: CommitInfo,
) -> InstructionsForCommitable {
    debug!("Finalizing commitable: {:?}", commit_info);
    let CommitInfo::BufferedDataAccount { pubkey, .. } = &commit_info else {
        panic!("Only data accounts are supported for now");
    };

    let ix = finalize_ix(validator_auth, pubkey);
    InstructionsForCommitable {
        instructions: vec![ix],
        commit_info,
        kind: InstructionsKind::Finalize,
    }
}

pub(crate) struct ChunkedIxsToFinalizeCommitablesResult {
    pub chunked_ixs: Vec<Vec<InstructionsForCommitable>>,
    pub unchunked: HashMap<u64, Vec<CommitInfo>>,
}

/// Finalizes the previously processed commits
/// Ensures that commitables with matching bundle id are in a single chunk
pub(crate) fn chunked_ixs_to_finalize_commitables(
    validator_auth: Pubkey,
    commit_infos: Vec<CommitInfo>,
    use_lookup: bool,
) -> ChunkedIxsToFinalizeCommitablesResult {
    let max_per_chunk = if use_lookup {
        MAX_FINALIZE_PER_TX_USING_LOOKUP
    } else {
        MAX_FINALIZE_PER_TX
    };
    let bundles = bundle_chunks(commit_infos, max_per_chunk as usize);
    let chunked_ixs: Vec<_> = bundles
        .chunks
        .into_iter()
        .map(|chunk| {
            chunk
                .into_iter()
                .map(|commit_info| {
                    finalize_commitable(validator_auth, commit_info)
                })
                .collect::<Vec<_>>()
        })
        .collect();
    ChunkedIxsToFinalizeCommitablesResult {
        chunked_ixs,
        unchunked: bundles.unchunked,
    }
}
