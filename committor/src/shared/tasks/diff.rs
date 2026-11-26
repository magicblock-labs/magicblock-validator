use std::ops::Range;

const MAX_MERGED_GAP: usize = 8;

/// Merges segments with gaps <= MAX_MERGED_GAP
pub(crate) fn compute_merged_diff(original: &[u8], changed: &[u8]) -> Option<Vec<u8>> {
    let mut segments: Vec<Range<usize>> = Vec::new();
    for (offset, byte) in changed.iter().enumerate() {
        if original.get(offset) == Some(byte) {
            continue;
        }
        match segments.last_mut() {
            Some(last) if offset - last.end <= MAX_MERGED_GAP => last.end = offset + 1,
            _ => segments.push(offset..offset + 1),
        }
    }

    let mut words = vec![changed.len(), segments.len()];
    let mut offset_in_diff = 0;
    for segment in &segments {
        words.extend([offset_in_diff, segment.start]);
        offset_in_diff += segment.len();
    }
    let mut diff: Vec<u8> = words.iter().flat_map(|&word| (word as u32).to_le_bytes()).collect();
    for segment in segments {
        diff.extend_from_slice(&changed[segment]);
    }
    (diff.len() < changed.len()).then_some(diff)
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
