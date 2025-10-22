use std::cmp::min;

use super::{DiffSet, SizeChanged};

///
/// Compute diff for the given original data and the changed data.
///
/// Scenarios:
/// - original.len() == changed.len()
///     - diff is computed by comparing corresponding indices.
///     - bytes comparision
///         original: o1 o2 o3 o4 ... oN  
///          changed: c1 c2 c3 c4 ... cN
/// - original.len() < changed.len()
///     - that implies the account has been reallocated and expanded
///     - bytes comparision
///         original: o1 o2 o3 o4 ... oN  
///          changed: c1 c2 c3 c4 ... cN cN+1 cN+2 ... cN+M
/// - original.len() > changed.len()
///     - that implies the account has been reallocated and shrunk
///     - bytes comparision
///         original: o1 o2 o3 o4 ... oN oN+1 oN+2 ... oN+M
///          changed: c1 c2 c3 c4 ... cN
///     
/// Diff Format:
///
/// Since an account could have modifications at multiple places (i.e imagine modifying
/// multiple fields in a struct), the diff could consist of multiple slices of [u8], one
/// slice for each contiguous change. So we compute these slices of change, and concatenate them
/// to form one big slice. However, in doing so, we lose the lengths of each slice and the
/// position of change in the original data. So the headers in the following format captures all
/// these necessary information:
///
///  - Number of slices: first 4 bytes.  
///  - Then follows offset-pairs that captures offset in concatenated diff as well as in the
///  original account.
///  - Then follows the concatenated diff bytes.
///
/// | # Offset Pairs  | Offset Pair 0 | Offset Pair 1 | ... | Offset Pair N-1 | Concatenated Diff Bytes |
/// | === 4 bytes === | == 8 bytes == | == 8 bytes == | ... | === 8 bytes === | ======  M bytes ======= |
///
/// Offset Pair Format:
///
/// | OffsetInDiffBytes  | OffsetInAccountData |
/// | ==== 4 bytes ===== | ===== 4 bytes ===== |
///
/// where,
///
/// - OffsetInDiffBytes is the index in ConcatenatedDiff indicating the beginning of the slice
/// and the OffsetInDiffBytes from the second pair indicates the end of the slice (for the previous
/// pair).
///   - Note that the OffsetInDiffBytes is relative to the beginning of ConcatenatedDiff, not
///     relative to the beginning of the whole serialized data, that implies the OffsetInDiffBytes
///     in the first pair is always 0.
/// - M is a variable and is the sum of the length of the diff-slices.
///  - M = diff_slices.map(|s| s.len()).sum()
/// - The length of ith slice = OffsetInDiffBytes@(i+1) - OffsetInDiffBytes@i
///
/// Example,
///
/// Suppose we have an account with data field being 100 bytes:
///
///  ----------------------------
///  |   a 100 bytes account    |
///  ----------------------------
///  | M | A | G | I | ... |  C |
///  ----------------------------
///  | 0 | 1 | 2 | 3 | ... | 99 |
///  ----------------------------
///   
/// Also, suppose we have modifications at two locations (u32 and u64), so we have 2 slices of size 4
/// and 8 bytes respectively, and the corresponding indices are the followings (note that these are "indices"
/// in the original account, not the indices in the concatenated diff):
///
///  - | 11 | 12 | 13 | 14 |                     -- 4 bytes
///  - | 71 | 72 | 73 | 74 | 75 | 76 | 77 | 78 | -- 8 bytes
///
/// In this case, the serialized bytes would be:
///
///  ------------------------------------------------
///  | num |   offsets   |  concatenated slices     |      
///  ------------------------------------------------
///  |  2  | 0 11 | 4 71 | 11 12 13 14 71 72 ... 78 |
///  ------------------------------------------------
///                      |  0  1  2  3  4  5 ... 11 |
///                      ----------------------------
///
///
///  - 2         : number of offset pairs
///  - 0 11      : first offset pair (offset in diff, offset in account)
///  - 4 71      : second offset pair (offset in diff, offset in account)
///  - 11 ... 78 : concatenated diff bytes
///
pub fn compute_diff(original: &[u8], changed: &[u8]) -> Vec<u8> {
    // Step 1: Identify contiguous diff slices
    let mut diffs: Vec<(usize, &[u8])> = Vec::new();
    let min_len = min(original.len(), changed.len());

    let mut i = 0;
    while i < min_len {
        if original[i] != changed[i] {
            // start of diff
            let start = i;
            while i < min_len && original[i] != changed[i] {
                i += 1;
            }
            diffs.push((start, &changed[start..i]));
        } else {
            i += 1;
        }
    }

    // Handle expansion/shrinkage
    if changed.len() > original.len() {
        // extra bytes at the end
        diffs.push((original.len(), &changed[original.len()..]));
    } else if changed.len() < original.len() {
        // account shrunk: data truncated
        // (we could note that the diff includes "no data", but to keep consistent,
        // we include no trailing slice)
    }

    if diffs.is_empty() {
        return vec![];
    }

    // Step 2: Serialize according to your spec
    let num_slices = diffs.len() as u32;
    let mut output = Vec::new();

    // number of slices (2 bytes)
    output.extend_from_slice(&num_slices.to_le_bytes());

    // compute header pairs (offset_in_diff, offset_in_account)
    let mut diff_offset = 0u32;
    for (start_in_account, slice) in &diffs {
        output.extend_from_slice(&diff_offset.to_le_bytes());
        output.extend_from_slice(&(*start_in_account as u32).to_le_bytes());
        diff_offset += slice.len() as u32;
    }

    // append concatenated diff bytes
    for (_, slice) in diffs {
        output.extend_from_slice(slice);
    }

    output
}

pub fn detect_size_change(original: &[u8], diff: &[u8]) -> Option<SizeChanged> {
    let diff_set = DiffSet::new(diff);
    diff_set
        .diff_slice_at(diff_set.num_offset_pairs() - 1)
        .and_then(|(slice, offset)| {
            let last_index = offset + slice.len() - 1;
            if last_index >= original.len() {
                Some(SizeChanged::Expanded(last_index + 1))
            } else {
                None
            }
        })
}

pub fn apply_diff_in_place(
    original: &mut [u8],
    diff: &[u8],
) -> Result<(), SizeChanged> {
    if let Some(layout) = detect_size_change(original, diff) {
        return Err(layout);
    }
    apply_diff_impl(original, diff);
    Ok(())
}

pub fn apply_diff(original: &[u8], diff: &[u8]) -> Vec<u8> {
    match detect_size_change(original, diff) {
        Some(SizeChanged::Expanded(new_size)) => {
            let mut applied = Vec::with_capacity(new_size);
            applied.extend_from_slice(original);
            applied.resize(new_size, 0);
            apply_diff_impl(applied.as_mut(), diff);
            applied
        }
        Some(SizeChanged::Shrunk(new_size)) => {
            let mut applied = Vec::with_capacity(new_size);
            applied.extend_from_slice(&original[0..new_size]);
            apply_diff_impl(applied.as_mut(), diff);
            applied
        }
        None => {
            let mut applied = Vec::with_capacity(original.len());
            applied.extend_from_slice(original);
            apply_diff_impl(applied.as_mut(), diff);
            applied
        }
    }
}

/// Apply diff back to reconstruct the changed data (helper).
fn apply_diff_impl(original: &mut [u8], diff: &[u8]) -> Vec<u8> {
    let mut pos = 0;

    // 1. read number of slices
    let num_slices =
        u16::from_le_bytes(diff[pos..pos + 2].try_into().unwrap()) as usize;
    pos += 2;

    // 2. read offset pairs
    let mut offset_pairs = Vec::with_capacity(num_slices);
    for _ in 0..num_slices {
        let offset_in_diff =
            u32::from_le_bytes(diff[pos..pos + 4].try_into().unwrap());
        pos += 4;
        let offset_in_account =
            u32::from_le_bytes(diff[pos..pos + 4].try_into().unwrap());
        pos += 4;
        offset_pairs
            .push((offset_in_diff as usize, offset_in_account as usize));
    }

    // 3. remaining bytes are diff data
    let diff_data = &diff[pos..];

    // 4. reconstruct new data
    let mut result = original.to_vec();

    for (i, (offset_in_diff, offset_in_account)) in
        offset_pairs.iter().enumerate()
    {
        let end_in_diff = if i + 1 < offset_pairs.len() {
            offset_pairs[i + 1].0
        } else {
            diff_data.len()
        };
        let slice = &diff_data[*offset_in_diff..end_in_diff];
        let end_in_account = offset_in_account + slice.len();
        result[*offset_in_account..end_in_account].copy_from_slice(slice);
    }

    result
}

pub fn apply(original: &mut [u8], diff: &[u8]) {
    let diff_set = DiffSet::new(diff);
    let mut index = 0;
    while let Some((diff, offset)) = diff_set.diff_slice_at(index) {
        index += 1;
        original[offset..offset + diff.len()].copy_from_slice(diff);
    }
}

#[cfg(test)]
mod tests {
    use rand::{rngs::StdRng, seq::IteratorRandom, Rng, SeedableRng};

    use crate::diff::apply;

    use super::compute_diff;

    #[test]
    fn test_no_change() {
        let original = [0; 100];
        let diff = compute_diff(&original, &original);

        assert_eq!(diff.len(), 0);
        assert_eq!(diff, Vec::<u8>::new());
    }

    #[test]
    fn test_using_example_data() {
        let original = [0; 100];
        let changed = {
            let mut copy = original;
            // | 11 | 12 | 13 | 14 |
            copy[11..=14].copy_from_slice(&0x01020304u32.to_le_bytes());
            //  | 71 | 72 | 73 | 74 | 75 | 76 | 77 | 78 |
            copy[71..=78].copy_from_slice(&0x0102030405060708u64.to_le_bytes());
            copy
        };

        let actual_diff = compute_diff(&original, &changed);
        let expected_diff = {
            // expected: | 2 | 0 11 | 4 71 | 11 12 13 14 71 72 ... 78 |

            let mut serialized = vec![];

            // 2
            serialized.extend_from_slice(&2u32.to_le_bytes());

            // 0 11
            serialized.extend_from_slice(&0u32.to_le_bytes());
            serialized.extend_from_slice(&11u32.to_le_bytes());

            // 4 71
            serialized.extend_from_slice(&4u32.to_le_bytes());
            serialized.extend_from_slice(&71u32.to_le_bytes());

            // | 11 12 13 14 |
            serialized.extend_from_slice(&0x01020304u32.to_le_bytes());
            //  | 71 72 ... 78
            serialized.extend_from_slice(&0x0102030405060708u64.to_le_bytes());
            serialized
        };

        assert_eq!(actual_diff.len(), 4 + 8 + 8 + (4 + 8));
        assert_eq!(actual_diff, expected_diff);
    }

    #[test]
    fn test_using_large_random_data() {
        // Test Plan:
        // - Use a random account size (between 2 MB and 10 MB).
        // - Mutate it at random, non-overlapping and 8-byte-separated regions.
        // - Use random patch sizes (2–256 bytes).
        // - Verify that apply_diff(original, diff) reproduces changed.
        // - Keep memory use and runtime reasonable.

        const MB: usize = 1024 * 1024;

        // random but reproducible
        let mut rng = StdRng::seed_from_u64(42);

        let original = {
            let account_size: usize = rng.gen_range(2 * MB..10 * MB);
            println!("account_size: {account_size}");
            let mut data = vec![0u8; account_size];
            rng.fill(&mut data[..]);
            data
        };

        let (changed, slices) = {
            let mut copy = original.clone();
            let mut slices = vec![];

            let slab = MB / 2;
            let mut offset_range = 0..slab;

            while offset_range.end < copy.len() {
                let diff_offset = rng.gen_range(offset_range);
                let diff_len = (1..256).choose(&mut rng).unwrap();
                let diff_end = (diff_offset + diff_len).min(copy.len());

                // Overwrite with new random data
                for i in diff_offset..diff_end {
                    let old = copy[i];
                    while old == copy[i] {
                        copy[i] = rng.gen::<u8>();
                    }
                }
                println!("{diff_offset}, {diff_end} => {diff_len}");
                slices
                    .push((diff_offset, copy[diff_offset..diff_end].to_vec()));

                offset_range = (diff_end + 8)..(diff_end + 8 + slab);
            }
            (copy, slices)
        };

        let actual_diff = compute_diff(&original, &changed);
        let expected_diff = {
            let mut diff = vec![];
            diff.extend_from_slice(&(slices.len() as u32).to_le_bytes());

            let mut offset_in_diff = 0u32;
            for (offset_in_account, slice) in slices.iter() {
                diff.extend_from_slice(&offset_in_diff.to_le_bytes());
                diff.extend_from_slice(
                    &(*offset_in_account as u32).to_le_bytes(),
                );
                offset_in_diff += slice.len() as u32;
            }

            for (_, slice) in slices.iter() {
                diff.extend_from_slice(slice);
            }
            diff
        };

        println!("actual_diff len: {}", unsafe {
            *(actual_diff.as_ptr() as *const u32)
        });
        println!("expected_diff len: {}", unsafe {
            *(expected_diff.as_ptr() as *const u32)
        });

        assert_eq!(
            actual_diff.len(),
            expected_diff.len(),
            "number of slices {}",
            slices.len()
        );

        let expected_changed = {
            let mut copy = original;
            apply(&mut copy, &actual_diff);
            copy
        };

        assert_eq!(changed, expected_changed);

        // Optional: apply diff back to verify correctness
        //let reconstructed = apply_diff(&original, &diff);
        //assert_eq!(
        //    reconstructed, changed,
        //    "reconstructed data does not match changed data"
        //);
    }
}
