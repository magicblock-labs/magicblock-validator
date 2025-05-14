use magicblock_committor_service::ChangesetCommittor;

#[derive(Default)]
pub struct ChangesetCommittorStub {}

impl ChangesetCommittor for ChangesetCommittorStub {
    fn commit_changeset(
        &self,
        _changeset: magicblock_committor_service::Changeset,
        _ephemeral_blockhash: solana_sdk::hash::Hash,
        _finalize: bool,
    ) -> tokio::sync::oneshot::Receiver<Option<String>> {
        unimplemented!("Not called during tests")
    }

    fn get_commit_statuses(
        &self,
        _reqid: String,
    ) -> tokio::sync::oneshot::Receiver<
        magicblock_committor_service::error::CommittorServiceResult<
            Vec<magicblock_committor_service::persist::CommitStatusRow>,
        >,
    > {
        unimplemented!("Not called during tests")
    }

    fn get_bundle_signatures(
        &self,
        _bundle_id: u64,
    ) -> tokio::sync::oneshot::Receiver<
        magicblock_committor_service::error::CommittorServiceResult<
            Option<magicblock_committor_service::persist::BundleSignatureRow>,
        >,
    > {
        unimplemented!("Not called during tests")
    }

    fn reserve_pubkeys_for_committee(
        &self,
        _committee: magicblock_program::Pubkey,
        _owner: magicblock_program::Pubkey,
    ) -> tokio::sync::oneshot::Receiver<
        magicblock_committor_service::error::CommittorServiceResult<()>,
    > {
        unimplemented!("Not called during tests")
    }
}
