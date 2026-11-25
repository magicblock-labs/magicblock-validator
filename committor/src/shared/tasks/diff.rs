use std::ops::Range;

use delegation_program_api::diff::{
    SIZE_OF_CHANGED_LEN, SIZE_OF_NUM_OFFSET_PAIRS, SIZE_OF_SINGLE_OFFSET_PAIR,
};

const MAX_MERGED_GAP: usize = 8;
const COMPARE_BLOCK: usize = 64;

pub(crate) fn compute_merged_diff(original: &[u8], changed: &[u8]) -> Option<Vec<u8>> {
    let mut segments: Vec<Range<usize>> = Vec::new();
    let mut push = |i: usize| match segments.last_mut() {
        Some(last) if i - last.end <= MAX_MERGED_GAP => last.end = i + 1,
        _ => segments.push(i..i + 1),
    };
    let blocks = original.chunks(COMPARE_BLOCK).zip(changed.chunks(COMPARE_BLOCK));
    for (block, (old, new)) in blocks.enumerate().filter(|(_, (old, new))| old != new) {
        let start = block * COMPARE_BLOCK;
        old.iter()
            .zip(new)
            .enumerate()
            .filter(|(_, (a, b))| a != b)
            .for_each(|(j, _)| push(start + j));
    }
    (original.len()..changed.len()).for_each(&mut push);

    let changed_bytes: usize = segments.iter().map(|segment| segment.len()).sum();
    let diff_len = SIZE_OF_CHANGED_LEN
        + SIZE_OF_NUM_OFFSET_PAIRS
        + SIZE_OF_SINGLE_OFFSET_PAIR * segments.len()
        + changed_bytes;
    if diff_len >= changed.len() {
        return None;
    }

    let mut diff = Vec::with_capacity(diff_len);
    diff.extend_from_slice(&(changed.len() as u32).to_le_bytes());
    diff.extend_from_slice(&(segments.len() as u32).to_le_bytes());
    let mut offset_in_diff = 0u32;
    for segment in &segments {
        diff.extend_from_slice(&offset_in_diff.to_le_bytes());
        diff.extend_from_slice(&(segment.start as u32).to_le_bytes());
        offset_in_diff += segment.len() as u32;
    }
    for segment in segments {
        diff.extend_from_slice(&changed[segment]);
    }
    Some(diff)
}

#[cfg(test)]
mod tests {
    use delegation_program_api::diff::{DiffSet, apply_diff_copy};
    use mbv_core::intent::types::CommittedAccount;
    use solana_account::Account;
    use solana_pubkey::Pubkey;

    use super::compute_merged_diff;
    use crate::shared::tasks::{
        commit_delivery::CommitDelivery, utils::create_commit_finalize_task,
    };

    #[test]
    fn test_compute_merged_diff() {
        let original = vec![0u8; 1024];
        let with = |len: usize, offsets: &[usize]| {
            let mut data = original.clone();
            data.resize(len, 0);
            for &i in offsets {
                data[i] = 1;
            }
            data
        };

        for (changed, segments) in [
            (with(1024, &[60, 69]), 1),
            (with(1024, &[60, 70]), 2),
            (with(1030, &[1020]), 1),
            (with(1000, &[10]), 1),
        ] {
            let diff = compute_merged_diff(&original, &changed).unwrap();
            let diffset = DiffSet::try_new(&diff).unwrap();
            assert_eq!(diffset.segments_count(), segments);
            assert_eq!(apply_diff_copy(&original, &diffset).unwrap(), changed);
        }

        let delivery = |data: Vec<u8>| {
            let account = CommittedAccount {
                pubkey: Pubkey::new_unique(),
                account: Account { data, ..Default::default() },
                remote_slot: 0,
                commit_id: 1,
            };
            let base_account = Account {
                data: original.clone(),
                ..Default::default()
            };
            create_commit_finalize_task(1, false, account, Some(base_account)).delivery
        };
        assert!(matches!(
            delivery(with(1024, &[100])),
            CommitDelivery::DiffInArgs { .. }
        ));
        assert!(matches!(
            delivery(with(1024, &(0..1024).step_by(9).collect::<Vec<_>>())),
            CommitDelivery::StateInArgs
        ));
    }
}
