use std::collections::HashSet;

use magicblock_committor_program::{ChangedBundle, Changeset};
use solana_pubkey::Pubkey;
use solana_sdk::clock::Slot;

use crate::{
    error::{CommittorServiceError, CommittorServiceResult},
    transactions::{
        commit_tx_report, CommitTxReport, MAX_ENCODED_TRANSACTION_SIZE,
    },
};

/// These are the commit strategies we can use to commit a changeset in order
/// of preference. We use lookup tables only as last resort since they are
/// slow to prepare.
#[derive(Debug)]
pub enum CommitBundleStrategy {
    ArgsIncludeFinalize(ChangedBundle),
    Args(ChangedBundle),
    FromBuffer(ChangedBundle),
    ArgsIncludeFinalizeWithLookupTable(ChangedBundle),
    ArgsWithLookupTable(ChangedBundle),
    FromBufferWithLookupTable(ChangedBundle),
}

impl TryFrom<(ChangedBundle, bool)> for CommitBundleStrategy {
    type Error = CommittorServiceError;

    /// Try to find the fastest/efficient commit strategy for the given bundle.
    /// Order of preference:
    /// 1. [CommitBundleStrategy::ArgsIncludeFinalize]
    /// 2. [CommitBundleStrategy::Args]
    /// 3. [CommitBundleStrategy::FromBuffer]
    /// 4. [CommitBundleStrategy::ArgsIncludeFinalizeWithLookupTable]
    /// 5. [CommitBundleStrategy::ArgsWithLookupTable]
    /// 6. [CommitBundleStrategy::FromBufferWithLookupTable]
    fn try_from(
        (bundle, finalize): (ChangedBundle, bool),
    ) -> Result<Self, Self::Error> {
        let CommitTxReport {
            size_args_including_finalize,
            size_args,
            fits_buffer,
            size_args_with_lookup_including_finalize,
            size_args_with_lookup,
            fits_buffer_using_lookup,
        } = commit_tx_report(&bundle, finalize)?;
        // Try to combine process and finalize if finalize is true
        if let Some(size_including_finalize) = size_args_including_finalize {
            if size_including_finalize < MAX_ENCODED_TRANSACTION_SIZE {
                return Ok(CommitBundleStrategy::ArgsIncludeFinalize(bundle));
            }
        }
        // Next still using args but with separate finalize if needed
        if size_args < MAX_ENCODED_TRANSACTION_SIZE {
            return Ok(CommitBundleStrategy::Args(bundle));
        }

        // Last option to avoid lookup tables
        if fits_buffer {
            return Ok(CommitBundleStrategy::FromBuffer(bundle));
        }

        // All the below use lookup tables and will be a lot slower

        // Combining finalize and process
        if let Some(size_with_lookup_including_finalize) =
            size_args_with_lookup_including_finalize
        {
            if size_with_lookup_including_finalize
                < MAX_ENCODED_TRANSACTION_SIZE
            {
                return Ok(
                    CommitBundleStrategy::ArgsIncludeFinalizeWithLookupTable(
                        bundle,
                    ),
                );
            }
        }
        // Using lookup tables but separate finalize
        if let Some(size_with_lookup) = size_args_with_lookup {
            if size_with_lookup < MAX_ENCODED_TRANSACTION_SIZE {
                return Ok(CommitBundleStrategy::ArgsWithLookupTable(bundle));
            }
        }

        // Worst case try to use a buffer with lookup tables
        if fits_buffer_using_lookup {
            return Ok(CommitBundleStrategy::FromBufferWithLookupTable(bundle));
        }

        // If none of the strategies work then we need to error
        let bundle_id = bundle
            .first()
            .map(|(_, acc)| acc.bundle_id())
            .unwrap_or_default();
        Err(CommittorServiceError::CouldNotFindCommitStrategyForBundle(
            bundle_id,
        ))
    }
}

#[derive(Debug)]
pub struct SplitChangesets {
    /// These changesets can be committed in one processing step, passing account data as args
    pub args_changesets: Vec<Changeset>,
    /// These changesets can be committed in one processing step, passing account data as args
    /// and the finalize instruction fits into the same transaction
    pub args_including_finalize_changesets: Vec<Changeset>,
    /// These changesets can be committed in one processing step, passing account data as args
    /// but needs to use lookup tables for the accounts
    pub args_with_lookup_changesets: Vec<Changeset>,
    /// These changesets can be committed in one processing step, passing account data as args
    /// and the finalize instruction fits into the same transaction.
    /// It needs to use lookup tables for the accounts.
    pub args_including_finalize_with_lookup_changesets: Vec<Changeset>,
    /// These changesets need to be committed in two steps:
    /// 1. Prepare the buffer account
    /// 2. Process the buffer account
    pub from_buffer_changesets: Vec<Changeset>,
    /// These changesets need to be committed in three steps:
    /// 1. Prepare the buffer account
    /// 2. Prepare lookup table
    /// 3. Process the buffer account
    pub from_buffer_with_lookup_changesets: Vec<Changeset>,
}

pub fn split_changesets_by_commit_strategy(
    changeset: Changeset,
    finalize: bool,
) -> CommittorServiceResult<SplitChangesets> {
    fn add_to_changesets(
        slot: Slot,
        changesets: &mut Vec<Changeset>,
        accounts_to_undelegate: &HashSet<Pubkey>,
        bundle: ChangedBundle,
    ) {
        let mut changeset = Changeset {
            slot,
            ..Default::default()
        };
        for (pubkey, acc) in bundle {
            changeset.add(pubkey, acc);
            if accounts_to_undelegate.contains(&pubkey) {
                changeset.accounts_to_undelegate.insert(pubkey);
            }
        }
        changesets.push(changeset);
    }

    let mut args_changeset = vec![];
    let mut args_including_finalize_changeset = vec![];
    let mut args_with_lookup_changeset = vec![];
    let mut args_including_finalize_with_lookup_changeset = vec![];
    let mut from_buffer_changeset = vec![];
    let mut from_buffer_with_lookup_changeset = vec![];

    let accounts_to_undelegate = changeset.accounts_to_undelegate.clone();
    let slot = changeset.slot;
    let changeset_bundles = changeset.into_small_changeset_bundles();
    for bundle in changeset_bundles.bundles.into_iter() {
        let commit_strategy =
            CommitBundleStrategy::try_from((bundle, finalize))?;
        match commit_strategy {
            CommitBundleStrategy::Args(bundle) => {
                add_to_changesets(
                    slot,
                    &mut args_changeset,
                    &accounts_to_undelegate,
                    bundle,
                );
            }
            CommitBundleStrategy::ArgsIncludeFinalize(bundle) => {
                add_to_changesets(
                    slot,
                    &mut args_including_finalize_changeset,
                    &accounts_to_undelegate,
                    bundle,
                );
            }
            CommitBundleStrategy::ArgsWithLookupTable(bundle) => {
                add_to_changesets(
                    slot,
                    &mut args_with_lookup_changeset,
                    &accounts_to_undelegate,
                    bundle,
                );
            }
            CommitBundleStrategy::ArgsIncludeFinalizeWithLookupTable(
                bundle,
            ) => {
                add_to_changesets(
                    slot,
                    &mut args_including_finalize_with_lookup_changeset,
                    &accounts_to_undelegate,
                    bundle,
                );
            }
            CommitBundleStrategy::FromBuffer(bundle) => {
                add_to_changesets(
                    slot,
                    &mut from_buffer_changeset,
                    &accounts_to_undelegate,
                    bundle,
                );
            }
            CommitBundleStrategy::FromBufferWithLookupTable(bundle) => {
                add_to_changesets(
                    slot,
                    &mut from_buffer_with_lookup_changeset,
                    &accounts_to_undelegate,
                    bundle,
                );
            }
        }
    }

    Ok(SplitChangesets {
        args_changesets: args_changeset,
        args_including_finalize_changesets: args_including_finalize_changeset,
        args_with_lookup_changesets: args_with_lookup_changeset,
        args_including_finalize_with_lookup_changesets:
            args_including_finalize_with_lookup_changeset,
        from_buffer_changesets: from_buffer_changeset,
        from_buffer_with_lookup_changesets: from_buffer_with_lookup_changeset,
    })
}

#[cfg(test)]
mod test {
    use log::*;
    use magicblock_committor_program::ChangedAccount;
    use solana_sdk::pubkey::Pubkey;

    use super::*;

    fn init_logger() {
        let _ = env_logger::builder()
            .format_timestamp(None)
            .is_test(true)
            .try_init();
    }

    fn add_changed_account(
        changeset: &mut Changeset,
        size: usize,
        bundle_id: u64,
        undelegate: bool,
    ) -> Pubkey {
        let pubkey = Pubkey::new_unique();
        changeset.add(
            pubkey,
            ChangedAccount::Full {
                data: vec![1; size],
                owner: Pubkey::new_unique(),
                lamports: 0,
                bundle_id,
            },
        );
        if undelegate {
            changeset.accounts_to_undelegate.insert(pubkey);
        }
        pubkey
    }

    struct ChangesetCounts {
        args_changeset_count: usize,
        args_including_finalize_changeset_count: usize,
        args_with_lookup_changeset_count: usize,
        args_including_finalize_with_lookup_changeset_count: usize,
        from_buffer_changeset_count: usize,
        from_buffer_with_lookup_changeset_count: usize,
    }

    macro_rules! changeset_counts {
        ($split_changesets:ident) => {
            ChangesetCounts {
                args_changeset_count: $split_changesets
                    .args_changesets
                    .iter()
                    .map(|cs| cs.len())
                    .sum::<usize>(),
                args_including_finalize_changeset_count: $split_changesets
                    .args_including_finalize_changesets
                    .iter()
                    .map(|cs| cs.len())
                    .sum::<usize>(),
                args_with_lookup_changeset_count: $split_changesets
                    .args_with_lookup_changesets
                    .iter()
                    .map(|cs| cs.len())
                    .sum::<usize>(),
                args_including_finalize_with_lookup_changeset_count:
                    $split_changesets
                        .args_including_finalize_with_lookup_changesets
                        .iter()
                        .map(|cs| cs.len())
                        .sum::<usize>(),
                from_buffer_changeset_count: $split_changesets
                    .from_buffer_changesets
                    .iter()
                    .map(|cs| cs.len())
                    .sum::<usize>(),
                from_buffer_with_lookup_changeset_count: $split_changesets
                    .from_buffer_with_lookup_changesets
                    .iter()
                    .map(|cs| cs.len())
                    .sum::<usize>(),
            }
        };
    }

    macro_rules! debug_counts {
        ($label:expr, $changeset:ident, $split_changesets:ident) => {
            let ChangesetCounts {
                args_changeset_count,
                args_including_finalize_changeset_count,
                args_with_lookup_changeset_count,
                args_including_finalize_with_lookup_changeset_count,
                from_buffer_changeset_count,
                from_buffer_with_lookup_changeset_count,
            } = changeset_counts!($split_changesets);
            debug!(
                "{}: ({}) {{
args_changeset:                                 {}
args_including_finalize_changeset:              {}
args_with_lookup_changeset:                     {}
args_including_finalize_with_lookup_changeset:  {}
from_buffer_changeset:                          {}
from_buffer_with_lookup_changeset:              {}
}}",
                $label,
                $changeset.accounts.len(),
                args_changeset_count,
                args_including_finalize_changeset_count,
                args_with_lookup_changeset_count,
                args_including_finalize_with_lookup_changeset_count,
                from_buffer_changeset_count,
                from_buffer_with_lookup_changeset_count,
            );
        };
    }

    macro_rules! assert_accounts_sum_matches {
        ($changeset:ident, $split_changesets:ident) => {
            let ChangesetCounts {
                args_changeset_count,
                args_including_finalize_changeset_count,
                args_with_lookup_changeset_count,
                args_including_finalize_with_lookup_changeset_count,
                from_buffer_changeset_count,
                from_buffer_with_lookup_changeset_count,
            } = changeset_counts!($split_changesets);
            assert_eq!(
                args_changeset_count
                    + args_including_finalize_changeset_count
                    + args_with_lookup_changeset_count
                    + args_including_finalize_with_lookup_changeset_count
                    + from_buffer_changeset_count
                    + from_buffer_with_lookup_changeset_count,
                $changeset.len()
            );
        };
    }

    macro_rules! assert_undelegate_sum_matches {
        ($changeset:ident, $split_changesets:ident) => {

            let (args_changeset_accounts_to_undelegate_count,
                args_including_finalize_changeset_accounts_to_undelegate_count,
                args_with_lookup_changeset_accounts_to_undelegate_count,
                args_including_finalize_with_lookup_changeset_accounts_to_undelegate_count,
                from_buffer_changeset_accounts_to_undelegate_count,
                from_buffer_with_lookup_changeset_accounts_to_undelegate_count) =(
                    $split_changesets
                        .args_changesets
                        .iter()
                        .map(|cs| cs.accounts_to_undelegate.len())
                        .sum::<usize>(),
                    $split_changesets
                        .args_including_finalize_changesets
                        .iter()
                        .map(|cs| cs.accounts_to_undelegate.len())
                        .sum::<usize>(),
                    $split_changesets
                        .args_with_lookup_changesets
                        .iter()
                        .map(|cs| cs.accounts_to_undelegate.len())
                        .sum::<usize>(),
                    $split_changesets
                        .args_including_finalize_with_lookup_changesets
                        .iter()
                        .map(|cs| cs.accounts_to_undelegate.len())
                        .sum::<usize>(),
                    $split_changesets
                        .from_buffer_changesets
                        .iter()
                        .map(|cs| cs.accounts_to_undelegate.len())
                        .sum::<usize>(),
                    $split_changesets
                        .from_buffer_with_lookup_changesets
                        .iter()
                        .map(|cs| cs.accounts_to_undelegate.len())
                        .sum::<usize>(),
            );

            assert_eq!(
                args_changeset_accounts_to_undelegate_count
                    + args_including_finalize_changeset_accounts_to_undelegate_count
                    + args_with_lookup_changeset_accounts_to_undelegate_count
                    + args_including_finalize_with_lookup_changeset_accounts_to_undelegate_count
                    + from_buffer_changeset_accounts_to_undelegate_count
                    + from_buffer_with_lookup_changeset_accounts_to_undelegate_count,
                $changeset.accounts_to_undelegate.len()
            );
        };
    }
    #[test]
    fn test_split_small_changesets_by_commit_strategy() {
        init_logger();

        // Setup a changeset with different bundle/account sizes
        let mut changeset = Changeset {
            slot: 1,
            ..Default::default()
        };

        let bundle_id = 1111;

        // 2 accounts bundle that can be handled via args
        for idx in 1..=2 {
            add_changed_account(&mut changeset, 10, bundle_id, idx % 2 == 0);
        }

        // 8 accounts bundle that needs lookup
        for idx in 1..=8 {
            add_changed_account(
                &mut changeset,
                10,
                bundle_id * 10,
                idx % 2 == 0,
            );
        }

        // No Finalize
        let split_changesets =
            split_changesets_by_commit_strategy(changeset.clone(), false)
                .unwrap();
        let ChangesetCounts {
            args_changeset_count,
            args_with_lookup_changeset_count,
            ..
        } = changeset_counts!(split_changesets);

        debug_counts!("No Finalize", changeset, split_changesets);
        assert_accounts_sum_matches!(changeset, split_changesets);
        assert_undelegate_sum_matches!(changeset, split_changesets);
        assert_eq!(args_changeset_count, 2,);
        assert_eq!(args_with_lookup_changeset_count, 8,);

        // Finalize
        let split_changesets =
            split_changesets_by_commit_strategy(changeset.clone(), true)
                .unwrap();
        let ChangesetCounts {
            args_including_finalize_changeset_count,
            args_including_finalize_with_lookup_changeset_count,
            ..
        } = changeset_counts!(split_changesets);

        debug_counts!("Finalize", changeset, split_changesets);
        assert_accounts_sum_matches!(changeset, split_changesets);
        assert_undelegate_sum_matches!(changeset, split_changesets);

        assert_eq!(args_including_finalize_changeset_count, 2,);
        assert_eq!(args_including_finalize_with_lookup_changeset_count, 8,);
    }

    #[test]
    fn test_split_medium_changesets_by_commit_strategy() {
        init_logger();

        // Setup a changeset with different bundle/account sizes
        let mut changeset = Changeset {
            slot: 1,
            ..Default::default()
        };
        let bundle_id = 2222;

        // 2 accounts bundle that can be handled via args and include the finalize instructions
        for idx in 1..=2 {
            add_changed_account(&mut changeset, 80, bundle_id, idx % 2 == 0);
        }

        // 2 accounts bundle that can be handled via args, but cannot include finalize due
        // to the size of the data
        for idx in 1..=2 {
            add_changed_account(
                &mut changeset,
                100,
                bundle_id + 1,
                idx % 2 == 0,
            );
        }

        // 3 accounts bundle that needs lookup buffer due to overall args size
        for idx in 1..=3 {
            add_changed_account(
                &mut changeset,
                100,
                bundle_id + 3,
                idx % 2 == 0,
            );
        }

        // No Finalize
        let split_changesets =
            split_changesets_by_commit_strategy(changeset.clone(), false)
                .unwrap();
        let ChangesetCounts {
            args_changeset_count,
            from_buffer_changeset_count,
            ..
        } = changeset_counts!(split_changesets);
        debug_counts!("No Finalize", changeset, split_changesets);
        assert_accounts_sum_matches!(changeset, split_changesets);
        assert_undelegate_sum_matches!(changeset, split_changesets);

        assert_eq!(args_changeset_count, 4,);
        assert_eq!(from_buffer_changeset_count, 3,);

        // Finalize
        let split_changesets =
            split_changesets_by_commit_strategy(changeset.clone(), true)
                .unwrap();
        let ChangesetCounts {
            args_changeset_count,
            args_including_finalize_changeset_count,
            from_buffer_changeset_count,
            ..
        } = changeset_counts!(split_changesets);
        debug_counts!("Finalize", changeset, split_changesets);
        assert_accounts_sum_matches!(changeset, split_changesets);
        assert_undelegate_sum_matches!(changeset, split_changesets);

        assert_eq!(args_changeset_count, 2,);
        assert_eq!(args_including_finalize_changeset_count, 2,);
        assert_eq!(from_buffer_changeset_count, 3,);
    }

    #[test]
    fn test_split_large_changesets_by_commit_strategy() {
        init_logger();

        // Setup a changeset with different bundle/account sizes
        let mut changeset = Changeset {
            slot: 1,
            ..Default::default()
        };

        let bundle_id = 3333;

        // 5 accounts bundle that needs to be handled via lookup (buffer)
        for idx in 1..=5 {
            add_changed_account(&mut changeset, 400, bundle_id, idx % 2 == 0);
        }

        // 2 accounts bundle that can be handled without lookup (buffer)
        for idx in 1..=2 {
            add_changed_account(
                &mut changeset,
                600,
                bundle_id * 10,
                idx % 2 == 0,
            );
        }

        // No Finalize
        let split_changesets =
            split_changesets_by_commit_strategy(changeset.clone(), false)
                .unwrap();
        let ChangesetCounts {
            from_buffer_changeset_count,
            from_buffer_with_lookup_changeset_count,
            ..
        } = changeset_counts!(split_changesets);
        debug_counts!("No Finalize", changeset, split_changesets);
        assert_accounts_sum_matches!(changeset, split_changesets);
        assert_undelegate_sum_matches!(changeset, split_changesets);

        assert_eq!(from_buffer_changeset_count, 2,);
        assert_eq!(from_buffer_with_lookup_changeset_count, 5,);

        // Finalize
        let split_changesets =
            split_changesets_by_commit_strategy(changeset.clone(), true)
                .unwrap();
        let ChangesetCounts {
            from_buffer_changeset_count,
            from_buffer_with_lookup_changeset_count,
            ..
        } = changeset_counts!(split_changesets);
        debug_counts!("Finalize", changeset, split_changesets);
        assert_accounts_sum_matches!(changeset, split_changesets);
        assert_undelegate_sum_matches!(changeset, split_changesets);

        assert_eq!(from_buffer_changeset_count, 2,);
        assert_eq!(from_buffer_with_lookup_changeset_count, 5,);
    }

    #[test]
    fn test_split_different_size_changesets_by_commit_strategy() {
        // Combining the different changeset sizes we already test above into one changeset to
        // split
        init_logger();

        // Setup a changeset with different bundle/account sizes
        let mut changeset = Changeset {
            slot: 1,
            ..Default::default()
        };

        // Small sized bundles
        {
            let bundle_id = 1111;

            // 2 accounts bundle that can be handled via args
            for idx in 1..=2 {
                add_changed_account(
                    &mut changeset,
                    10,
                    bundle_id,
                    idx % 2 == 0,
                );
            }

            // 8 accounts bundle that needs lookup
            for idx in 1..=8 {
                add_changed_account(
                    &mut changeset,
                    10,
                    bundle_id * 10,
                    idx % 2 == 0,
                );
            }
        };

        // Medium sized bundles
        {
            let bundle_id = 2222;

            // 2 accounts bundle that can be handled via args
            for idx in 1..=2 {
                add_changed_account(
                    &mut changeset,
                    100,
                    bundle_id,
                    idx % 2 == 0,
                );
            }
        };

        // Large sized bundles
        {
            let bundle_id = 3333;

            // 5 accounts bundle that needs to be handled via lookup (buffer)
            for idx in 1..=5 {
                add_changed_account(
                    &mut changeset,
                    400,
                    bundle_id,
                    idx % 2 == 0,
                );
            }

            // 2 accounts bundle that can be handled without lookup (buffer)
            for idx in 1..=2 {
                add_changed_account(
                    &mut changeset,
                    600,
                    bundle_id * 10,
                    idx % 2 == 0,
                );
            }
        };

        // No Finalize
        {
            let split_changesets =
                split_changesets_by_commit_strategy(changeset.clone(), false)
                    .unwrap();
            let ChangesetCounts {
                args_changeset_count,
                args_with_lookup_changeset_count,
                from_buffer_changeset_count,
                from_buffer_with_lookup_changeset_count,
                ..
            } = changeset_counts!(split_changesets);

            debug_counts!("No Finalize", changeset, split_changesets);
            assert_accounts_sum_matches!(changeset, split_changesets);
            assert_undelegate_sum_matches!(changeset, split_changesets);

            assert_eq!(args_changeset_count, 4);
            assert_eq!(args_with_lookup_changeset_count, 8);
            assert_eq!(from_buffer_changeset_count, 2);
            assert_eq!(from_buffer_with_lookup_changeset_count, 5);
        }

        // Finalize
        {
            let split_changesets =
                split_changesets_by_commit_strategy(changeset.clone(), true)
                    .unwrap();
            let ChangesetCounts {
                args_changeset_count,
                args_including_finalize_changeset_count,
                args_including_finalize_with_lookup_changeset_count,
                from_buffer_changeset_count,
                from_buffer_with_lookup_changeset_count,
                ..
            } = changeset_counts!(split_changesets);

            debug_counts!("Finalize", changeset, split_changesets);
            assert_accounts_sum_matches!(changeset, split_changesets);
            assert_undelegate_sum_matches!(changeset, split_changesets);

            assert_eq!(args_changeset_count, 2);
            assert_eq!(args_including_finalize_changeset_count, 2);
            assert_eq!(args_including_finalize_with_lookup_changeset_count, 8);
            assert_eq!(from_buffer_changeset_count, 2);
            assert_eq!(from_buffer_with_lookup_changeset_count, 5);
        }
    }
}
