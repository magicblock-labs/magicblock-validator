use std::collections::HashMap;

use log::*;

use crate::CommitInfo;

/// Tries to merge bundles into chunks to leverage the max amount of commits
/// we can have in a single transaction.
pub(crate) fn efficient_bundle_chunks(
    mut bundles: HashMap<u64, Vec<CommitInfo>>,
    max_per_chunk: usize,
) -> Vec<Vec<CommitInfo>> {
    let lens = bundles
        .iter()
        .map(|(id, commits)| Len {
            id: *id,
            len: commits.len(),
        })
        .collect::<Vec<_>>();

    let chunked_ids = efficient_merge_strategy(lens, max_per_chunk);

    let mut chunked_bundles = Vec::new();
    for chunk in chunked_ids {
        let mut bundle_chunk = Vec::<CommitInfo>::new();
        for id in chunk {
            if let Some(bundles) = bundles.remove(&id) {
                bundle_chunk.extend(bundles);
            } else {
                debug_assert!(false, "BUG: bundle not found for id {}", id);
                continue;
            }
        }
        chunked_bundles.push(bundle_chunk);
    }

    debug_assert!(bundles.is_empty());

    chunked_bundles
}

#[derive(PartialEq, Eq, Debug, Clone, Copy)]
struct Len {
    id: u64,
    len: usize,
}

/// Returns the most efficient merge strategy for the given lens and max size.
/// WARN: Requires that no len is larger than max_size, otherwise this method will
///       get stuck
fn efficient_merge_strategy(
    mut lens: Vec<Len>,
    max_size: usize,
) -> Vec<Vec<u64>> {
    // NOTE: crash in dev, use escape hatch in release
    debug_assert!(lens.iter().all(|len| len.len <= max_size));

    for len in lens.iter() {
        if len.len > max_size {
            // NOTE: This is an escape hatch, if we have a len that is larger
            //       than the max size since we can't merge it.
            //       This is caused by a programmer error in the calling code.
            //       It will most likely cause an issue higher in the call stack
            //       but handling it this way is better than crashing or getting
            //       stuck.
            error!(
                "BUG: len {} is too large for the max_size {}",
                len.len, max_size
            );
            return lens.iter().map(|len| vec![len.id]).collect();
        }
    }

    lens.sort_by_key(|len| len.len);

    let mut chunks: Vec<Vec<u64>> = Vec::new();
    let Some(next_len) = lens.pop() else {
        return vec![];
    };
    let mut current_chunk = vec![next_len.id];
    let mut current_size = next_len.len;
    'outer: loop {
        let mut remaining_lens = vec![];
        for len in lens.iter().rev() {
            if current_size + len.len <= max_size {
                current_chunk.push(len.id);
                current_size += len.len;
            } else {
                remaining_lens.push(*len);
                continue;
            }
        }

        lens = lens
            .drain(..)
            .filter(|len| remaining_lens.contains(len))
            .collect();

        if lens.is_empty() {
            chunks.push(current_chunk);
            break;
        }

        if lens
            .first()
            .map(|len| current_size < len.len)
            .unwrap_or(false)
        {
            continue 'outer;
        }

        // If we have no more lens to add to the current chunk create a new one
        chunks.push(current_chunk);

        // No more lens to process, we are done with the entire process
        let Some(next_len) = lens.pop() else {
            break 'outer;
        };
        current_chunk = vec![next_len.id];
        current_size = next_len.len;
    }

    chunks
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_efficient_merge_strategy() {
        let lens = vec![
            Len { id: 1, len: 1 },
            Len { id: 2, len: 2 },
            Len { id: 3, len: 3 },
            Len { id: 4, len: 4 },
            Len { id: 5, len: 5 },
            Len { id: 6, len: 6 },
            Len { id: 7, len: 7 },
            Len { id: 8, len: 8 },
            Len { id: 9, len: 9 },
            Len { id: 10, len: 10 },
        ];

        let res = efficient_merge_strategy(lens.clone(), 10);
        assert_eq!(
            res,
            vec![
                vec![10],
                vec![9, 1],
                vec![8, 2],
                vec![7, 3],
                vec![6, 4],
                vec![5]
            ]
        );

        let res = efficient_merge_strategy(lens.clone(), 20);
        assert_eq!(res, vec![vec![10, 9, 1], vec![8, 7, 5], vec![6, 4, 3, 2]]);

        let lens = vec![
            Len { id: 1, len: 1 },
            Len { id: 2, len: 2 },
            Len { id: 3, len: 3 },
            Len { id: 4, len: 4 },
            Len { id: 5, len: 5 },
            Len { id: 6, len: 6 },
            Len { id: 7, len: 7 },
            Len { id: 8, len: 8 },
        ];
        let res = efficient_merge_strategy(lens.clone(), 8);
        assert_eq!(
            res,
            vec![vec![8], vec![7, 1], vec![6, 2], vec![5, 3], vec![4]]
        );
        let lens = vec![
            Len { id: 1, len: 1 },
            Len { id: 2, len: 2 },
            Len { id: 3, len: 2 },
            Len { id: 4, len: 2 },
            Len { id: 5, len: 2 },
            Len { id: 6, len: 6 },
            Len { id: 7, len: 6 },
            Len { id: 8, len: 8 },
        ];
        let res = efficient_merge_strategy(lens.clone(), 8);
        assert_eq!(res, vec![vec![8], vec![7, 5], vec![6, 4], vec![3, 2, 1]]);

        let lens = vec![
            Len { id: 1, len: 1 },
            Len { id: 3, len: 2 },
            Len { id: 4, len: 2 },
            Len { id: 5, len: 2 },
            Len { id: 6, len: 6 },
            Len { id: 7, len: 6 },
            Len { id: 8, len: 8 },
            Len { id: 9, len: 8 },
        ];
        let res = efficient_merge_strategy(lens.clone(), 8);
        assert_eq!(
            res,
            vec![vec![9], vec![8], vec![7, 5], vec![6, 4], vec![3, 1]]
        );
    }
}
