use std::collections::{HashMap, HashSet};

use borsh::{BorshDeserialize, BorshSerialize};
use solana_account::{Account, AccountSharedData, ReadableAccount};
use solana_program::clock::Slot;
use solana_pubkey::Pubkey;

use super::{
    changeset_chunks::{ChangesetChunks, ChangesetChunksIter},
    chunks::Chunks,
};

// -----------------
// ChangedAccount
// -----------------
pub type ChangedBundle = Vec<(Pubkey, ChangedAccount)>;

#[derive(BorshSerialize, BorshDeserialize, PartialEq, Eq, Clone, Debug)]
pub enum ChangedAccount {
    Full {
        lamports: u64,
        data: Vec<u8>,
        /// The original owner of the delegated account on chain
        owner: Pubkey,
        /// This id will be the same for accounts that need to be committed together atomically
        /// For single commit accounts it is still set for consistency
        bundle_id: u64,
    },
    // NOTE: placeholder for later without breaking existing
    //       buffers
    Diff,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ChangedAccountMeta {
    /// The on chain and ephemeral address of the delegated account
    pub pubkey: Pubkey,
    /// The lamports the account holds in the ephemeral
    pub lamports: u64,
    /// The original owner of the delegated account on chain
    pub owner: Pubkey,
    /// This id will be the same for accounts that need to be committed together atomically
    /// For single commit accounts it is still set for consistency
    pub bundle_id: u64,
}

impl From<(&Pubkey, &ChangedAccount)> for ChangedAccountMeta {
    fn from((pubkey, changed_account): (&Pubkey, &ChangedAccount)) -> Self {
        match changed_account {
            ChangedAccount::Full {
                lamports,
                owner,
                bundle_id,
                ..
            } => Self {
                pubkey: *pubkey,
                lamports: *lamports,
                owner: *owner,
                bundle_id: *bundle_id,
            },
            ChangedAccount::Diff => {
                unreachable!("We don't yet support account diffs")
            }
        }
    }
}

impl From<(Account, u64)> for ChangedAccount {
    fn from((account, bundle_id): (Account, u64)) -> Self {
        Self::Full {
            lamports: account.lamports,
            // NOTE: the owner of the account in the ephemeral is set to the original account owner
            owner: account.owner,
            data: account.data,
            bundle_id,
        }
    }
}

impl From<(AccountSharedData, u64)> for ChangedAccount {
    fn from((value, bundle_id): (AccountSharedData, u64)) -> Self {
        Self::Full {
            lamports: value.lamports(),
            owner: *value.owner(),
            data: value.data().to_vec(),
            bundle_id,
        }
    }
}

impl ChangedAccount {
    pub(crate) fn into_inner(self) -> (u64, Pubkey, Vec<u8>, u64) {
        use ChangedAccount::*;
        match self {
            Full {
                lamports,
                owner,
                data,
                bundle_id,
            } => (lamports, owner, data, bundle_id),
            Diff => unreachable!("We don't yet support account diffs"),
        }
    }

    pub fn lamports(&self) -> u64 {
        match self {
            Self::Full { lamports, .. } => *lamports,
            Self::Diff => unreachable!("We don't yet support account diffs"),
        }
    }

    pub fn data(&self) -> &[u8] {
        match self {
            Self::Full { data, .. } => data,
            Self::Diff => unreachable!("We don't yet support account diffs"),
        }
    }

    pub fn owner(&self) -> Pubkey {
        match self {
            Self::Full { owner, .. } => *owner,
            Self::Diff => unreachable!("We don't yet support account diffs"),
        }
    }

    pub fn bundle_id(&self) -> u64 {
        match self {
            Self::Full { bundle_id, .. } => *bundle_id,
            Self::Diff => unreachable!("We don't yet support account diffs"),
        }
    }
}

// -----------------
// ChangeSet
// -----------------

/// This is data structure which holds the account changes to commit to chain.
/// Locally it will be filled with the changes to commit.
/// On chain it is initialized as empty at first and then is filled from the
/// local changeset via multiple transactions.
/// A related [Chunks] account is used in order to track which changes have been
/// applied successfully.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Changeset {
    /// The accounts that should be updated
    pub accounts: HashMap<Pubkey, ChangedAccount>,
    /// The ephemeral slot at which those changes were requested
    pub slot: Slot,
    /// The accounts that should be undelegated after they were committed
    pub accounts_to_undelegate: HashSet<Pubkey>,
}

/// The meta data of the changeset which can be used to capture information about
/// the changeset before transferring ownership. Createing this metadata is
/// a lot cheaper than copying the entire changeset which includes the accounts data.
/// Thus it can be used to capture information to include with error responses.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ChangesetMeta {
    /// The accounts that should be updated
    pub accounts: Vec<ChangedAccountMeta>,
    /// The ephemeral slot at which those changes were requested
    pub slot: Slot,
    /// The accounts that should be undelegated after they were committed
    pub accounts_to_undelegate: HashSet<Pubkey>,
}

impl ChangesetMeta {
    /// Separates information per account including the following:
    /// - account commit metadata
    /// - slot at which commit was requested
    /// - if the account should be undelegated after it was committed
    pub fn into_account_infos(self) -> Vec<(ChangedAccountMeta, Slot, bool)> {
        self.accounts
            .into_iter()
            .map(|account| {
                let undelegate =
                    self.accounts_to_undelegate.contains(&account.pubkey);
                (account, self.slot, undelegate)
            })
            .collect()
    }
}

impl From<&Changeset> for ChangesetMeta {
    fn from(changeset: &Changeset) -> Self {
        let accounts = changeset
            .accounts
            .iter()
            .map(ChangedAccountMeta::from)
            .collect();
        Self {
            accounts,
            slot: changeset.slot,
            accounts_to_undelegate: changeset.accounts_to_undelegate.clone(),
        }
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ChangesetBundles {
    /// The bundles, each of which needs to be committed atomically
    pub bundles: Vec<ChangedBundle>,
    /// The ephemeral slot at which those changes were requested
    pub slot: Slot,
    /// The accounts that should be undelegated after they were committed
    pub accounts_to_undelegate: HashSet<Pubkey>,
}

impl Changeset {
    /// Adds an account to the change set.
    /// If it already exists, it will be replaced, thus the caller needs
    /// to ensure that conflicting changes are added in the right order, i.e.
    /// the last update needs to be added last.
    ///
    /// - **pubkey** public key of the account
    /// - **account** account to add
    ///
    /// *returns* true if the account was already present and was replaced
    pub fn add<T: Into<ChangedAccount>>(
        &mut self,
        pubkey: Pubkey,
        account: T,
    ) -> bool {
        self.accounts.insert(pubkey, account.into()).is_some()
    }

    /// This method should be called for all accounts that we want to
    /// undelegate after committing them.
    pub fn request_undelegation(&mut self, pubkey: Pubkey) {
        self.accounts_to_undelegate.insert(pubkey);
    }

    /// When we're ready to commit this changeset we convert it into
    /// a [CommitableChangeSet] which allows to commit the changes in chunks.
    pub fn into_committables(self, chunk_size: u16) -> Vec<CommitableAccount> {
        self.accounts
            .into_iter()
            .map(|(pubkey, acc)| {
                let (lamports, owner, data, bundle_id) = acc.into_inner();
                CommitableAccount::new(
                    pubkey,
                    owner,
                    data,
                    lamports,
                    chunk_size,
                    self.slot,
                    self.accounts_to_undelegate.contains(&pubkey),
                    bundle_id,
                )
            })
            .collect::<Vec<_>>()
    }

    pub fn account_keys(&self) -> Vec<&Pubkey> {
        self.accounts.keys().collect()
    }

    pub fn undelegate_keys(&self) -> Vec<&Pubkey> {
        self.accounts_to_undelegate.iter().collect()
    }

    pub fn owners(&self) -> HashMap<Pubkey, Pubkey> {
        self.accounts
            .iter()
            .map(|(pubkey, account)| (*pubkey, account.owner()))
            .collect()
    }

    /// Splits the accounts into bundles that need to be committed together
    /// keeping each bundle as small as possible.
    /// Accounts without a bundle id each get their own bundle here.
    /// The return value returns info about accounts needing to be delegated and
    /// the slot at which the changeset was created.
    pub fn into_small_changeset_bundles(self) -> ChangesetBundles {
        let mut bundles: HashMap<u64, ChangedBundle> = HashMap::new();
        let accounts_to_undelegate = self.accounts_to_undelegate;
        let slot = self.slot;
        for (pubkey, account) in self.accounts.into_iter() {
            bundles
                .entry(account.bundle_id())
                .or_default()
                .push((pubkey, account));
        }
        let bundles = bundles.into_values().collect::<Vec<_>>();

        ChangesetBundles {
            bundles,
            slot,
            accounts_to_undelegate,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.accounts.is_empty()
    }

    pub fn len(&self) -> usize {
        self.accounts.len()
    }

    pub fn overlaps(change_sets: &[&Self]) -> Vec<Pubkey> {
        let mut overlapping = HashSet::new();
        for change_set in change_sets {
            for (pubkey, _) in change_set.accounts.iter() {
                if overlapping.contains(pubkey) {
                    continue;
                }
                for other_change_set in change_sets {
                    if other_change_set == change_set {
                        continue;
                    }
                    if other_change_set.accounts.contains_key(pubkey) {
                        overlapping.insert(*pubkey);
                    }
                }
            }
        }
        overlapping.into_iter().collect()
    }

    pub fn contains(&self, pubkey: &Pubkey) -> bool {
        self.accounts.contains_key(pubkey)
    }
}

// -----------------
// CommitableChangeSet
// -----------------
/// There is one committable per account that we are trying to commit
#[derive(Debug)]
pub struct CommitableAccount {
    /// The on chain address of the account
    pub pubkey: Pubkey,
    /// The original owner of the delegated account on chain
    pub delegated_account_owner: Pubkey,
    /// The account data to commit
    pub data: Vec<u8>,
    /// The lamports that the account holds in the ephemeral
    pub lamports: u64,
    /// Keep track of which part of the account data has been committed
    chunks: Chunks,
    /// The size of each data chunk that we send to fill the buffer
    chunk_size: u16,
    /// The ephemeral slot at which those changes were requested
    pub slot: Slot,
    /// If we also undelegate the account after committing it
    pub undelegate: bool,
    /// This id will be the same for accounts that need to be committed together atomically
    /// For single commit accounts it is still set for consistency
    pub bundle_id: u64,
}

impl CommitableAccount {
    #[allow(clippy::too_many_arguments)] // internal API
    pub(crate) fn new(
        pubkey: Pubkey,
        delegated_account_owner: Pubkey,
        data: Vec<u8>,
        lamports: u64,
        chunk_size: u16,
        slot: Slot,
        undelegate: bool,
        bundle_id: u64,
    ) -> Self {
        let len = data.len();
        let chunk_count = if chunk_size == 0 {
            // Special case for when the commit info is handled without chunking
            1
        } else {
            let count = len / chunk_size as usize;
            if len % chunk_size as usize > 0 {
                count + 1
            } else {
                count
            }
        };
        Self {
            pubkey,
            delegated_account_owner,
            data,
            lamports,
            chunk_size,
            chunks: Chunks::new(chunk_count, chunk_size),
            slot,
            undelegate,
            bundle_id,
        }
    }

    /// Iterates all chunks of data no matter if they were committed or not.
    /// Thus only use this the very first time when trying to commit all chunks.
    pub fn iter_all(&self) -> ChangesetChunksIter<'_> {
        ChangesetChunks::new(&self.chunks, self.chunk_size).iter(&self.data)
    }

    /// Iterates all chunks of data that have not been committed yet.
    /// Use this to discover chunks that failed to commit.
    pub fn iter_missing(&self) -> ChangesetChunksIter<'_> {
        ChangesetChunks::new(&self.chunks, self.chunk_size)
            .iter_missing(&self.data)
    }

    /// When all chunks were committed we query the chain to see which commits
    /// actually landed.
    /// We then update the chunks here in order to allow to retry the missing
    /// chunks via [Self::iter_missing].
    pub fn set_chunks(&mut self, chunks: Chunks) {
        self.chunks = chunks;
    }

    /// The total size of the data that we will commit.
    /// Use this to initialize the empty account on chain.
    pub fn size(&self) -> usize {
        self.data.len()
    }

    pub fn chunk_size(&self) -> u16 {
        self.chunk_size
    }

    pub fn chunk_count(&self) -> usize {
        self.chunks.count()
    }

    pub fn has_data(&self) -> bool {
        !self.data.is_empty()
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_committing_changeset() {
        let mut changeset = Changeset::default();
        let pubkey = Pubkey::new_unique();
        let owner = Pubkey::new_unique();
        let changed_account = ChangedAccount::Full {
            lamports: 5_000,
            owner,
            data: vec![5; 547],
            bundle_id: 1,
        };
        changeset.add(pubkey, changed_account.clone());

        // The below results in a buffer of 547 bytes and we split it into 14 chunks
        let commitable = &mut changeset.into_committables(547 / 14)[0];
        eprintln!("SIZE: {}", commitable.size());
        assert_eq!(commitable.chunk_size(), 39);
        assert_eq!(commitable.chunk_count(), 15);
        assert_eq!(commitable.iter_all().count(), 15);

        // 1. Try to commit all chunks into a buffer simulating that some fail
        let mut tgt_buf = vec![0u8; commitable.size()];
        let mut chunks =
            Chunks::new(commitable.chunk_count(), commitable.chunk_size());

        for chunk in commitable.iter_all() {
            let idx = chunk.chunk_idx();
            // Skip the some chunks to simulate transactions not landing
            if idx == 7 || idx == 8 || idx == 12 {
                continue;
            }

            chunks.set_idx(idx as usize);

            let start = chunk.offset;
            for (i, d) in chunk.data_chunk.into_iter().enumerate() {
                tgt_buf[start as usize + i] = d;
            }
        }

        // 2. Update the chunks we were able to commit
        //    We will get this updated data from chain as each commit landing will
        //    also update the chunks account
        commitable.set_chunks(chunks.clone());
        assert_eq!(commitable.iter_missing().count(), 3);

        // 3. Retry the missing chunks
        for chunk in commitable.iter_missing() {
            chunks.set_idx(chunk.chunk_idx() as usize);

            let start = chunk.offset;
            for (i, d) in chunk.data_chunk.into_iter().enumerate() {
                tgt_buf[start as usize + i] = d;
            }
        }

        commitable.set_chunks(chunks);
        assert_eq!(commitable.iter_missing().count(), 0);

        // 4. Ensure that the entire account data was committed
        let (_, _, data, _) = changed_account.into_inner();
        assert_eq!(tgt_buf, data);
    }
}
