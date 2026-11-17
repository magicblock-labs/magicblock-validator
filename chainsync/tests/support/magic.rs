use engine::testkit::{Pacing, TestEngine};
use keeper::testkit::{Dirs, keeper_builder};
use magic_program_api::{MAGIC_CONTEXT_PUBKEY, id};
use program::{MagicContext, magicblock_processor::Entrypoint};
use solana_account::{AccountBuilder, AccountMode, ReadableAccount};
use solana_keypair::Keypair;
use solana_program_runtime::solana_sbpf::program::BuiltinFunctionDefinition;

/// Actual host scheduling state, independent of test-side event or transaction counters.
pub(crate) fn intents(engine: &TestEngine) -> usize {
    let account = engine.get_account(MAGIC_CONTEXT_PUBKEY).unwrap();
    MagicContext::deserialize(account.data()).unwrap().scheduled_base_intents.len()
}

/// Installs the host's real Magic builtin into the existing externally paced Engine testkit.
pub(crate) async fn engine() -> TestEngine {
    let authority = Keypair::new();
    let dirs = Dirs::default();
    let mut builder = keeper_builder(&dirs);
    builder.authority = authority.into();
    builder.builtins.insert(id(), (Entrypoint::vm, Entrypoint::codegen));
    builder.possibly_missing_accounts.insert(
        MAGIC_CONTEXT_PUBKEY,
        AccountBuilder::default()
            .owner(id())
            .mode(AccountMode::Magic)
            .data(vec![0; MagicContext::SIZE])
            .build(),
    );
    TestEngine::from_builder(dirs, builder, Pacing::External).await
}
