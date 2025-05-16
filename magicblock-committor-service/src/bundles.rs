use std::collections::HashMap;

use crate::{bundle_strategy::efficient_bundle_chunks, CommitInfo};

#[derive(Debug, Default)]
pub struct BundleChunksResult {
    /// The valid chunks
    pub chunks: Vec<Vec<CommitInfo>>,
    /// Commit infos that were not included in any chunk since not all infos in
    /// a bundle could fit into a single chunk.
    /// key:   bundle_id
    /// value: commit infos
    pub unchunked: HashMap<u64, Vec<CommitInfo>>,
}

/// Creates chunks that respect the following requirements:
/// 1. A chunk cannot be larger than [max_per_chunk].
/// 2. All commit infos with the same bundle_id must be in the same chunk.
pub(crate) fn bundle_chunks(
    mut commit_infos: Vec<CommitInfo>,
    max_per_chunk: usize,
) -> BundleChunksResult {
    if commit_infos.is_empty() {
        return BundleChunksResult::default();
    }

    // Group commit infos by bundle_id
    let mut bundles: HashMap<u64, Vec<CommitInfo>> = HashMap::new();
    let mut not_bundled: Vec<CommitInfo> = Vec::new();
    for commit_info in commit_infos.drain(..) {
        bundles
            .entry(commit_info.bundle_id())
            .or_default()
            .push(commit_info);
    }

    // Remove bundles that are too large to fit into a single chunk
    let (bundles, unbundled) = bundles.into_iter().fold(
        (HashMap::new(), HashMap::new()),
        |(mut bundles, mut unbundled), (key, bundle)| {
            if bundle.len() > max_per_chunk {
                unbundled.insert(key, bundle);
            } else {
                bundles.insert(key, bundle);
            }
            (bundles, unbundled)
        },
    );

    // Merge small bundles
    let mut chunks = efficient_bundle_chunks(bundles, max_per_chunk);

    // Add any commits that were not bundled to any of the bundles that still
    // have some room
    for chunk in chunks.iter_mut() {
        let remaining_space = max_per_chunk - chunk.len();
        if remaining_space > 0 {
            let range_end = remaining_space.min(not_bundled.len());
            chunk.extend(&mut not_bundled.drain(..range_end));
        }
    }

    // If we still have unbundled commits then add chunks for those
    while !not_bundled.is_empty() {
        let range_end = max_per_chunk.min(not_bundled.len());
        chunks.push(not_bundled.drain(..range_end).collect());
    }

    BundleChunksResult {
        chunks,
        unchunked: unbundled,
    }
}

/// Use this for operations on commit infos that don't have to run atomically for a bundle.
/// As an example closing buffers needed for the commit can be done without respecting
/// bundles.
pub(crate) fn bundle_chunks_ignoring_bundle_id(
    commit_infos: &[CommitInfo],
    max_per_chunk: usize,
) -> BundleChunksResult {
    if commit_infos.is_empty() {
        return BundleChunksResult::default();
    }
    let chunks = commit_infos
        .chunks(max_per_chunk)
        .map(|chunk| chunk.to_vec())
        .collect::<Vec<_>>();

    BundleChunksResult {
        chunks,
        unchunked: HashMap::new(),
    }
}

#[cfg(test)]
mod test {
    use std::collections::HashSet;

    use solana_sdk::{hash::Hash, pubkey::Pubkey};

    use super::*;

    fn commit_info(bundle_id: u64) -> crate::CommitInfo {
        CommitInfo::BufferedDataAccount {
            pubkey: Pubkey::new_unique(),
            delegated_account_owner: Pubkey::new_unique(),
            slot: 0,
            ephemeral_blockhash: Hash::new_unique(),
            undelegate: false,
            buffer_pda: Pubkey::new_unique(),
            chunks_pda: Pubkey::new_unique(),
            commit_state: Pubkey::new_unique(),
            lamports: 0,
            bundle_id,
            finalize: false,
        }
    }

    macro_rules! chunk_and_verify {
        ($commit_infos:ident, $max_per_chunk:expr) => {{
            let res = bundle_chunks($commit_infos.clone(), $max_per_chunk);

            // 1. All commit infos are accounted for
            let bundled_commit_infos =
                res.chunks.iter().flatten().cloned().collect::<Vec<_>>();
            let unbundled_commit_infos = res
                .unchunked
                .values()
                .flatten()
                .cloned()
                .collect::<Vec<_>>();

            for commit_info in $commit_infos {
                assert!(
                    bundled_commit_infos.contains(&commit_info),
                    "{:#?} was not bundled in {:#?}",
                    commit_info,
                    bundled_commit_infos
                );
            }
            assert!(
                unbundled_commit_infos.is_empty(),
                "Unbundled: {:#?}",
                unbundled_commit_infos
            );

            // 2. Chunk size is within limits
            for chunk in res.chunks.iter() {
                assert!(chunk.len() <= $max_per_chunk);
            }

            // 3. All commit infos with the same bundle_id are in the same chunk
            //    If a chunk has a bundle id then no other chunk should have it
            let bundle_ids = bundled_commit_infos
                .iter()
                .map(|commit_info| commit_info.bundle_id())
                .collect::<HashSet<_>>();
            for id in bundle_ids {
                let mut count = 0;
                for chunk in res.chunks.iter() {
                    let mut in_chunk = false;
                    for commit_info in chunk {
                        if commit_info.bundle_id() == id {
                            in_chunk = true
                        }
                    }
                    if in_chunk {
                        count += 1;
                    }
                }
                assert_eq!(
                    count, 1,
                    "Bundle id {} is in {} chunks. {:#?}",
                    id, count, res.chunks
                );
            }
            res
        }};
    }

    const MAX_PER_CHUNK: usize = 3;

    #[test]
    fn test_empty_bundle() {
        let res = bundle_chunks(Vec::new(), MAX_PER_CHUNK);
        assert!(res.chunks.is_empty());
        assert!(res.unchunked.is_empty());
    }

    #[test]
    fn test_single_bundle_single_commit() {
        let commit_infos = vec![commit_info(0)];
        chunk_and_verify!(commit_infos, MAX_PER_CHUNK);
    }

    #[test]
    fn test_single_bundle() {
        let commit_infos = vec![commit_info(0), commit_info(0), commit_info(0)];
        chunk_and_verify!(commit_infos, MAX_PER_CHUNK);
    }

    #[test]
    fn test_single_bundle_too_large() {
        let commit_infos = vec![
            commit_info(0),
            commit_info(0),
            commit_info(0),
            commit_info(0),
        ];
        let res = bundle_chunks(commit_infos.clone(), MAX_PER_CHUNK);
        assert!(res.chunks.is_empty());
        assert_eq!(res.unchunked.len(), 1);
        assert_eq!(res.unchunked.get(&0).unwrap(), &commit_infos);
    }

    #[test]
    fn test_multiple_bundles() {
        let commit_infos = vec![
            // Bundle 0
            commit_info(0),
            commit_info(0),
            // Bundle 1
            commit_info(1),
            commit_info(1),
            commit_info(1),
            // Bundle 2
            commit_info(2),
            commit_info(2),
        ];
        chunk_and_verify!(commit_infos, MAX_PER_CHUNK);
    }

    #[test]
    fn test_multiple_bundles_with_unbundled() {
        let commit_infos = vec![
            // Bundle 0
            commit_info(0),
            commit_info(0),
            // Bundle 1
            commit_info(1),
            commit_info(5),
            commit_info(1),
            commit_info(6),
            commit_info(1),
            // Bundle 2
            commit_info(2),
            commit_info(2),
            commit_info(7),
        ];
        chunk_and_verify!(commit_infos, MAX_PER_CHUNK);
    }

    #[test]
    fn test_multiple_bundles_efficiency() {
        let commit_infos = vec![
            // Bundle 0
            commit_info(0),
            commit_info(0),
            commit_info(0),
            // Bundle 1
            commit_info(1),
            commit_info(1),
            commit_info(1),
            // Bundle 2
            commit_info(2),
            commit_info(2),
            // Bundle 3
            commit_info(3),
            commit_info(3),
        ];
        let res = chunk_and_verify!(commit_infos, 5);
        assert_eq!(res.chunks.len(), 2);
    }
}
