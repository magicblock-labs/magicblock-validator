use std::sync::Arc;

use log::*;
use magicblock_committor_program::ChangedAccountMeta;
use solana_pubkey::Pubkey;
use solana_sdk::{clock::Slot, signature::Signature};

use crate::{
    error::CommitAccountError,
    persist::{CommitStatus, CommitStatusSignatures, CommitStrategy},
    CommitInfo,
};

#[derive(Debug, Clone)]
pub struct CommitSignatures {
    /// The signature of the transaction processing the commit
    pub process_signature: Signature,
    /// The signature of the transaction finalizing the commit.
    /// If the account was not finalized or it failed then this is `None`.
    /// If the finalize instruction was part of the process transaction then
    /// this signature is the same as [Self::process_signature].
    pub finalize_signature: Option<Signature>,
    /// The signature of the transaction undelegating the committed accounts
    /// if so requested.
    /// If the account was not undelegated or it failed the this is `None`.
    /// NOTE: this can be removed if we decide to perform the undelegation
    ///       step as part of the finalize instruction in the delegation program
    pub undelegate_signature: Option<Signature>,
}

impl CommitSignatures {
    pub fn process_only(process_signature: Signature) -> Self {
        Self {
            process_signature,
            finalize_signature: None,
            undelegate_signature: None,
        }
    }
}

impl From<CommitSignatures> for CommitStatusSignatures {
    fn from(commit_signatures: CommitSignatures) -> Self {
        Self {
            process_signature: commit_signatures.process_signature,
            finalize_signature: commit_signatures.finalize_signature,
            undelegate_signature: commit_signatures.undelegate_signature,
        }
    }
}

#[derive(Debug)]
pub enum CommitStage {
    /// This account was part of a changeset that could not be split into
    /// args only/args with lookup table or buffered changesets.
    /// The commit for this account needs to be restarted from scratch.
    SplittingChangesets((ChangedAccountMeta, Slot, bool)),

    /// This account was part of a changeset for which we could not obtain the
    /// latest on chain blockhash when trying to commit them via args.
    /// The commit for this account needs to be restarted from scratch.
    GettingLatestBlockhash((ChangedAccountMeta, Slot, bool, CommitStrategy)),

    /// No part of the commit pipeline succeeded.
    /// The commit for this account needs to be restarted from scratch.
    Failed((CommitInfo, CommitStrategy)),

    /// The buffer and chunks account were initialized, but could either not
    /// be retrieved or deserialized. It is recommended to fully re-initialize
    /// them on retry.
    BufferAndChunkPartiallyInitialized((CommitInfo, CommitStrategy)),

    /// The buffer and chunks accounts were initialized and all data was
    /// written to them (for data accounts).
    /// This means on retry we can skip that step and just try to process
    /// these buffers to complete the commit.
    /// This stage is returned in the following scenarios:
    /// - the commit could not be processed
    /// - another account in the same bundle failed to fully initialize
    ///   the buffer and chunks accounts and thus the bundle could not be
    ///   processed
    BufferAndChunkFullyInitialized((CommitInfo, CommitStrategy)),

    /// The commit is part of a bundle that contains too many commits to be included
    /// in a single transaction. Thus we cannot commit any of them.
    /// The max amount of accounts we can commit and process as part of a single
    /// transaction is [crate::max_per_transaction::MAX_COMMIT_STATE_AND_CLOSE_PER_TRANSACTION].
    /// These commits were prepared, which means the buffer and chunk accounts were fully
    /// initialized, but then this issue was detected.
    PartOfTooLargeBundleToProcess(CommitInfo),

    /// Failed to create the lookup table required for this commit.
    /// This happens when the table mania system cannot ensure that the lookup
    /// table exists for the pubkeys needed by this commit.
    CouldNotCreateLookupTable(
        (CommitInfo, CommitStrategy, Option<CommitSignatures>),
    ),

    /// The commit was properly initialized and added to a chunk of instructions to process
    /// commits via a transaction. For large commits the buffer and chunk accounts were properly
    /// prepared and haven't been closed.
    /// However that transaction failed.
    FailedProcess((CommitInfo, CommitStrategy, Option<CommitSignatures>)),

    /// The commit was properly processed but the finalize instructions didn't fit into a single
    /// transaction.
    /// This should never happen since otherwise the [CommitStage::PartOfTooLargeBundleToProcess]
    /// would have been returned as the bundle would have been too large to process in the
    /// first place.
    PartOfTooLargeBundleToFinalize(CommitInfo),

    /// The commit was properly processed but the requested finalize transaction failed.
    FailedFinalize((CommitInfo, CommitStrategy, CommitSignatures)),

    /// The commit was properly processed but the requested undelegation transaction failed.
    FailedUndelegate((CommitInfo, CommitStrategy, CommitSignatures)),

    /// All stages of the commit pipeline for this account succeeded
    /// and we don't have to retry any of them.
    /// This means the commit was processed and if so requested also finalized.
    /// We are done committing this account.
    Succeeded((CommitInfo, CommitStrategy, CommitSignatures)),
}

impl From<CommitAccountError> for CommitStage {
    fn from(err: CommitAccountError) -> Self {
        use CommitAccountError::*;
        macro_rules! ci {
            ($ci:ident) => {
                Arc::<CommitInfo>::unwrap_or_clone($ci)
            };
        }

        match err {
            InitBufferAndChunkAccounts(err, commit_info, commit_strategy) => {
                warn!("Init buffer and chunks accounts failed: {:?}", err);
                Self::Failed((*commit_info, commit_strategy))
            }
            GetChunksAccount(err, commit_info, commit_strategy) => {
                warn!("Get chunks account failed: {:?}", err);
                Self::BufferAndChunkPartiallyInitialized((
                    ci!(commit_info),
                    commit_strategy,
                ))
            }
            DeserializeChunksAccount(err, commit_info, commit_strategy) => {
                warn!("Deserialize chunks account failed: {:?}", err);
                Self::BufferAndChunkPartiallyInitialized((
                    ci!(commit_info),
                    commit_strategy,
                ))
            }
            ReallocBufferRanOutOfRetries(err, commit_info, commit_strategy) => {
                warn!("Realloc buffer ran out of retries: {:?}", err);
                Self::BufferAndChunkPartiallyInitialized((
                    ci!(commit_info),
                    commit_strategy,
                ))
            }
            WriteChunksRanOutOfRetries(err, commit_info, commit_strategy) => {
                warn!("Write chunks ran out of retries: {:?}", err);
                Self::BufferAndChunkPartiallyInitialized((
                    ci!(commit_info),
                    commit_strategy,
                ))
            }
        }
    }
}

pub enum CommitMetadata<'a> {
    CommitInfo(&'a CommitInfo),
    ChangedAccountMeta((&'a ChangedAccountMeta, Slot, bool)),
}

impl<'a> From<&'a CommitInfo> for CommitMetadata<'a> {
    fn from(commit_info: &'a CommitInfo) -> Self {
        Self::CommitInfo(commit_info)
    }
}

impl CommitMetadata<'_> {
    pub fn pubkey(&self) -> Pubkey {
        use CommitMetadata::*;
        match self {
            CommitInfo(ci) => ci.pubkey(),
            ChangedAccountMeta((cm, _, _)) => cm.pubkey,
        }
    }

    pub fn commit_state(&self) -> Option<Pubkey> {
        use CommitMetadata::*;
        match self {
            CommitInfo(ci) => ci.commit_state(),
            ChangedAccountMeta((_, _, _)) => None,
        }
    }

    pub fn bundle_id(&self) -> u64 {
        use CommitMetadata::*;
        match self {
            CommitInfo(ci) => ci.bundle_id(),
            ChangedAccountMeta((cm, _, _)) => cm.bundle_id,
        }
    }
}

impl CommitStage {
    pub fn commit_metadata(&self) -> CommitMetadata<'_> {
        use CommitStage::*;
        match self {
            SplittingChangesets((cm, slot, undelegate)) => {
                CommitMetadata::ChangedAccountMeta((cm, *slot, *undelegate))
            }
            GettingLatestBlockhash((cm, slot, undelegate, _)) => {
                CommitMetadata::ChangedAccountMeta((cm, *slot, *undelegate))
            }
            Failed((ci, _))
            | BufferAndChunkPartiallyInitialized((ci, _))
            | BufferAndChunkFullyInitialized((ci, _))
            | PartOfTooLargeBundleToProcess(ci)
            | CouldNotCreateLookupTable((ci, _))
            | FailedProcess((ci, _, _))
            | PartOfTooLargeBundleToFinalize(ci)
            | FailedFinalize((ci, _, _))
            | FailedUndelegate((ci, _, _))
            | Succeeded((ci, _, _)) => CommitMetadata::from(ci),
        }
    }

    pub fn commit_strategy(&self) -> CommitStrategy {
        use CommitStage::*;
        match self {
            SplittingChangesets((_, _, _)) => CommitStrategy::Undetermined,

            // For the below two the only strategy that would possibly have worked is the one
            // allowing most accounts per bundle, thus we return that as the assumed strategy
            PartOfTooLargeBundleToProcess(_)
            | PartOfTooLargeBundleToFinalize(_) => {
                CommitStrategy::FromBufferWithLookupTable
            }

            GettingLatestBlockhash((_, _, _, strategy))
            | Failed((_, strategy))
            | BufferAndChunkPartiallyInitialized((_, strategy))
            | BufferAndChunkFullyInitialized((_, strategy))
            | CouldNotCreateLookupTable((_, strategy))
            | FailedProcess((_, strategy, _))
            | FailedFinalize((_, strategy, _))
            | FailedUndelegate((_, strategy, _))
            | Succeeded((_, strategy, _)) => *strategy,
        }
    }

    pub fn commit_status(&self) -> CommitStatus {
        use CommitStage::*;
        match self {
            SplittingChangesets((meta, _, _))
            | GettingLatestBlockhash((meta, _, _, _)) => {
                CommitStatus::Failed(meta.bundle_id)
            }
            Failed((ci, _)) => CommitStatus::Failed(ci.bundle_id()),
            BufferAndChunkPartiallyInitialized((ci, _)) => {
                CommitStatus::BufferAndChunkPartiallyInitialized(ci.bundle_id())
            }
            BufferAndChunkFullyInitialized((ci, _)) => {
                CommitStatus::BufferAndChunkFullyInitialized(ci.bundle_id())
            }
            PartOfTooLargeBundleToProcess(ci)
            // NOTE: the below cannot occur if the above didn't, so we can merge them
            //       here
            | PartOfTooLargeBundleToFinalize(ci) => {
                CommitStatus::PartOfTooLargeBundleToProcess(ci.bundle_id())
            }
            CouldNotCreateLookupTable((ci, _)) => {
                CommitStatus::CouldNotCreateLookupTable(ci.bundle_id())
            }
            FailedProcess((ci, strategy, sigs)) => CommitStatus::FailedProcess((
                ci.bundle_id(),
                *strategy,
                sigs.as_ref().cloned().map(CommitStatusSignatures::from),
            )),
            FailedFinalize((ci, strategy, sigs)) => CommitStatus::FailedFinalize((
                ci.bundle_id(),
                *strategy,
                CommitStatusSignatures::from(sigs.clone()),
            )),
            FailedUndelegate((ci, strategy, sigs)) => CommitStatus::FailedUndelegate((
                ci.bundle_id(),
                *strategy,
                CommitStatusSignatures::from(sigs.clone()),
            )),
            Succeeded((ci, strategy, sigs)) => CommitStatus::Succeeded((
                ci.bundle_id(),
                *strategy,
                CommitStatusSignatures::from(sigs.clone()),
            )),
        }
    }

    pub fn commit_infos(commit_stages: &[Self]) -> Vec<CommitMetadata<'_>> {
        commit_stages.iter().map(Self::commit_metadata).collect()
    }

    /// Pubkey of the committed account
    pub fn pubkey(&self) -> Pubkey {
        self.commit_metadata().pubkey()
    }

    /// Pubkey of the account holding the state we commit until the commit is finalized
    pub fn commit_state(&self) -> Option<Pubkey> {
        self.commit_metadata().commit_state()
    }

    /// Returns `true` if we need to init the chunks and buffer accounts when we
    /// retry committing this account
    pub fn needs_accounts_init(&self) -> bool {
        use CommitStage::*;
        matches!(self, Failed(_) | BufferAndChunkPartiallyInitialized(_))
    }

    /// Returns `true` if we need to complete writing data to the buffer account
    /// when we retry committing this account
    pub fn needs_accounts_write(&self) -> bool {
        use CommitStage::*;
        self.needs_accounts_init()
            || matches!(self, BufferAndChunkFullyInitialized(_))
    }

    /// Returns `true` if we need to process the buffer account in order to apply
    /// the commit when we retry committing this account
    pub fn needs_process(&self) -> bool {
        use CommitStage::*;
        self.needs_accounts_write()
            || matches!(
                self,
                PartOfTooLargeBundleToProcess(_) | FailedProcess(_)
            )
    }

    /// Returns `true` if we need to rerun the finalize transaction when we retry
    /// committing this account
    pub fn needs_finalize(&self) -> bool {
        use CommitStage::*;
        self.needs_process()
            || matches!(
                self,
                PartOfTooLargeBundleToFinalize(_) | FailedFinalize(_)
            )
    }

    /// Returns `true` if the commit was successfully processed and the account
    /// was undelegated as part of the commit
    pub fn is_successfully_undelegated(&self) -> bool {
        use CommitStage::*;
        match self {
            Succeeded((ci, _, _)) => ci.undelegate(),
            _ => false,
        }
    }
}
