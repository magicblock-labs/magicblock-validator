#![allow(unused)]
use std::sync::Arc;

use dlp::args::DelegateEphemeralBalanceArgs;
use integration_test_tools::dlp_interface;
use log::*;
use magicblock_chainlink::{
    accounts_bank::mock::AccountsBankStub,
    cloner::Cloner,
    config::{ChainlinkConfig, LifecycleMode},
    fetch_cloner::FetchCloner,
    native_program_accounts,
    remote_account_provider::{
        chain_pubsub_client::ChainPubsubClientImpl,
        chain_rpc_client::ChainRpcClientImpl,
        config::{
            RemoteAccountProviderConfig,
            DEFAULT_SUBSCRIBED_ACCOUNTS_LRU_CAPACITY,
        },
        Endpoint, RemoteAccountProvider,
    },
    submux::SubMuxClient,
    testing::cloner_stub::ClonerStub,
    Chainlink,
};
use program_flexi_counter::state::FlexiCounter;
use solana_account::AccountSharedData;
use solana_pubkey::Pubkey;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_rpc_client_api::config::RpcSendTransactionConfig;
use solana_sdk::{
    commitment_config::CommitmentConfig, native_token::LAMPORTS_PER_SOL,
    signature::Keypair, signer::Signer, transaction::Transaction,
};
use solana_sdk_ids::native_loader;
use tokio::task;

use crate::{programs::send_instructions, sleep_ms};

pub type IxtestChainlink = Chainlink<
    ChainRpcClientImpl,
    SubMuxClient<ChainPubsubClientImpl>,
    AccountsBankStub,
    ClonerStub,
>;

#[derive(Clone)]
pub struct IxtestContext {
    pub rpc_client: Arc<RpcClient>,
    // pub pubsub_client: ChainPubsubClientImpl
    pub chainlink: Arc<IxtestChainlink>,
    pub bank: Arc<AccountsBankStub>,
    pub remote_account_provider: Option<
        Arc<
            RemoteAccountProvider<
                ChainRpcClientImpl,
                SubMuxClient<ChainPubsubClientImpl>,
            >,
        >,
    >,
    pub cloner: Arc<ClonerStub>,
    pub validator_kp: Arc<Keypair>,
}

const RPC_URL: &str = "http://localhost:7799";
pub const TEST_AUTHORITY: [u8; 64] = [
    251, 62, 129, 184, 107, 49, 62, 184, 1, 147, 178, 128, 185, 157, 247, 92,
    56, 158, 145, 53, 51, 226, 202, 96, 178, 248, 195, 133, 133, 237, 237, 146,
    13, 32, 77, 204, 244, 56, 166, 172, 66, 113, 150, 218, 112, 42, 110, 181,
    98, 158, 222, 194, 130, 93, 175, 100, 190, 106, 9, 69, 156, 80, 96, 72,
];
impl IxtestContext {
    pub async fn init() -> Self {
        Self::init_with_config(ChainlinkConfig::default_with_lifecycle_mode(
            LifecycleMode::Ephemeral,
        ))
        .await
    }

    pub async fn init_with_config(config: ChainlinkConfig) -> Self {
        let validator_kp = Keypair::from_bytes(&TEST_AUTHORITY[..]).unwrap();
        let faucet_kp = Keypair::new();

        let commitment = CommitmentConfig::confirmed();
        let lifecycle_mode = LifecycleMode::Ephemeral;
        let bank = Arc::<AccountsBankStub>::default();
        let cloner = Arc::new(ClonerStub::new(bank.clone()));
        let (tx, rx) = tokio::sync::mpsc::channel(100);
        let (fetch_cloner, remote_account_provider) = {
            let endpoints = [Endpoint {
                rpc_url: RPC_URL.to_string(),
                pubsub_url: "ws://localhost:7800".to_string(),
            }];
            // Add all native programs
            let native_programs = native_program_accounts();
            let program_stub = AccountSharedData::new(
                0,
                0,
                &(native_loader::id().to_bytes().into()),
            );
            for pubkey in native_programs {
                cloner
                    .clone_account(pubkey, program_stub.clone())
                    .await
                    .unwrap();
            }
            let remote_account_provider =
                RemoteAccountProvider::try_from_urls_and_config(
                    &endpoints,
                    commitment,
                    tx,
                    &config.remote_account_provider,
                )
                .await;

            match remote_account_provider {
                Ok(Some(remote_account_provider)) => {
                    debug!("Initializing FetchCloner");
                    let provider = Arc::new(remote_account_provider);
                    (
                        Some(FetchCloner::new(
                            &provider,
                            &bank,
                            &cloner,
                            validator_kp.pubkey(),
                            faucet_kp.pubkey(),
                            rx,
                        )),
                        Some(provider),
                    )
                }
                Err(err) => {
                    panic!("Failed to create remote account provider: {err:?}");
                }
                _ => (None, None),
            }
        };
        let chainlink = Chainlink::try_new(
            &bank,
            fetch_cloner,
            validator_kp.pubkey(),
            faucet_kp.pubkey(),
            0,
        )
        .unwrap();

        let rpc_client = IxtestContext::get_rpc_client(commitment);
        Self {
            rpc_client: Arc::new(rpc_client),
            chainlink: Arc::new(chainlink),
            bank,
            remote_account_provider,
            cloner,
            validator_kp: validator_kp.insecure_clone().into(),
        }
    }

    pub fn delegation_record_pubkey(&self, pubkey: &Pubkey) -> Pubkey {
        dlp_interface::delegation_record_pubkey(pubkey)
    }

    pub fn ephemeral_balance_pda_from_payer_pubkey(
        &self,
        payer: &Pubkey,
    ) -> Pubkey {
        dlp_interface::ephemeral_balance_pda_from_payer_pubkey(payer)
    }

    pub fn counter_pda(&self, counter_auth: &Pubkey) -> Pubkey {
        FlexiCounter::pda(counter_auth).0
    }

    pub async fn init_counter(&self, counter_auth: &Keypair) -> &Self {
        use program_flexi_counter::instruction::*;

        self.rpc_client
            .request_airdrop(&counter_auth.pubkey(), 777 * LAMPORTS_PER_SOL)
            .await
            .unwrap();
        debug!("Airdropped to counter auth: {} SOL", 777 * LAMPORTS_PER_SOL);

        let init_counter_ix =
            create_init_ix(counter_auth.pubkey(), "COUNTER".to_string());

        let latest_block_hash =
            self.rpc_client.get_latest_blockhash().await.unwrap();
        self.rpc_client
            .send_and_confirm_transaction_with_spinner_and_config(
                &Transaction::new_signed_with_payer(
                    &[init_counter_ix],
                    Some(&counter_auth.pubkey()),
                    &[&counter_auth],
                    latest_block_hash,
                ),
                CommitmentConfig::confirmed(),
                RpcSendTransactionConfig {
                    skip_preflight: true,
                    ..Default::default()
                },
            )
            .await
            .expect("Failed to init account");
        self
    }
    pub async fn add_accounts(&self, accs: &[(Pubkey, u64)]) {
        let mut joinset = task::JoinSet::new();
        for (pubkey, sol) in accs {
            let rpc_client = self.rpc_client.clone();
            let pubkey = *pubkey;
            let sol = *sol;
            joinset.spawn(async move {
                Self::add_account_impl(&rpc_client, &pubkey, sol).await;
            });
        }
        joinset.join_all().await;
    }

    pub async fn add_account(&self, pubkey: &Pubkey, sol: u64) {
        Self::add_account_impl(&self.rpc_client, pubkey, sol).await;
    }

    async fn add_account_impl(
        rpc_client: &RpcClient,
        pubkey: &Pubkey,
        sol: u64,
    ) {
        let lamports = sol * LAMPORTS_PER_SOL;
        rpc_client
            .request_airdrop(pubkey, lamports)
            .await
            .expect("Failed to airdrop");

        let mut retries = 5;
        loop {
            match rpc_client.get_account(pubkey).await {
                Ok(account) => {
                    if account.lamports >= lamports {
                        break;
                    }
                }
                Err(err) => {
                    if retries < 2 {
                        warn!("{err}");
                    }
                    retries -= 1;
                    if retries == 0 {
                        panic!("Failed to get created account {pubkey}",);
                    }
                }
            }
            sleep_ms(200).await;
        }

        debug!("Airdropped {sol} SOL to {pubkey}");
    }

    pub async fn delegate_counter(&self, counter_auth: &Keypair) -> &Self {
        debug!("Delegating counter account {}", counter_auth.pubkey());
        use program_flexi_counter::instruction::*;

        let delegate_ix = create_delegate_ix(counter_auth.pubkey());

        let latest_block_hash =
            self.rpc_client.get_latest_blockhash().await.unwrap();
        self.rpc_client
            .send_and_confirm_transaction_with_spinner_and_config(
                &Transaction::new_signed_with_payer(
                    &[delegate_ix],
                    Some(&counter_auth.pubkey()),
                    &[&counter_auth],
                    latest_block_hash,
                ),
                CommitmentConfig::confirmed(),
                RpcSendTransactionConfig {
                    skip_preflight: true,
                    ..Default::default()
                },
            )
            .await
            .expect("Failed to delegate account");
        self
    }

    pub async fn undelegate_counter(
        &self,
        counter_auth: &Keypair,
        redelegate: bool,
    ) -> &Self {
        debug!("Undelegating counter account {}", counter_auth.pubkey());
        let counter_pda = self.counter_pda(&counter_auth.pubkey());
        // The committor service will call this in order to have
        // chainlink subscribe to account updates of the counter account
        self.chainlink.undelegation_requested(counter_pda).await;

        // In order to make the account undelegatable we first need to
        // commmit and finalize
        let commit_ix = dlp::instruction_builder::commit_state(
            self.validator_kp.pubkey(),
            counter_pda,
            program_flexi_counter::id(),
            dlp::args::CommitStateArgs {
                nonce: 1,
                lamports: 1_000_000,
                allow_undelegation: true,
                data: vec![0, 1, 0],
            },
        );
        let finalize_ix = dlp::instruction_builder::finalize(
            self.validator_kp.pubkey(),
            counter_pda,
        );
        let undelegate_ix = dlp::instruction_builder::undelegate(
            self.validator_kp.pubkey(),
            counter_pda,
            program_flexi_counter::id(),
            counter_auth.pubkey(),
        );

        // Build instructions and required signers
        let mut ixs = vec![commit_ix, finalize_ix, undelegate_ix];
        let mut signers = vec![&*self.validator_kp];
        if redelegate {
            use program_flexi_counter::instruction::create_delegate_ix;
            let delegate_ix = create_delegate_ix(counter_auth.pubkey());
            ixs.push(delegate_ix);
            signers.push(counter_auth);
        }

        let latest_block_hash =
            self.rpc_client.get_latest_blockhash().await.unwrap();
        self.rpc_client
            .send_and_confirm_transaction_with_spinner_and_config(
                &Transaction::new_signed_with_payer(
                    &ixs,
                    Some(&self.validator_kp.pubkey()),
                    &signers,
                    latest_block_hash,
                ),
                CommitmentConfig::confirmed(),
                RpcSendTransactionConfig {
                    skip_preflight: true,
                    ..Default::default()
                },
            )
            .await
            .expect("Failed to undelegate account");
        self
    }

    pub async fn top_up_ephemeral_fee_balance(
        &self,
        payer: &Keypair,
        sol: u64,
        delegate: bool,
    ) -> (Pubkey, Pubkey) {
        let validator = delegate.then_some(self.validator_kp.pubkey());
        let (sig, ephemeral_balance_pda, deleg_record) =
            dlp_interface::top_up_ephemeral_fee_balance(
                &self.rpc_client,
                payer,
                payer.pubkey(),
                sol,
                validator,
            )
            .await
            .inspect_err(|err| {
                error!(
                    "Topping up balance for {} encountered error:{err:#?}",
                    payer.pubkey()
                );
            })
            .expect("Failed to send and confirm transaction");
        (ephemeral_balance_pda, deleg_record)
    }

    pub fn escrow_pdas(&self, payer: &Pubkey) -> (Pubkey, Pubkey) {
        let ephemeral_balance_pda =
            self.ephemeral_balance_pda_from_payer_pubkey(payer);
        let escrow_deleg_record =
            self.delegation_record_pubkey(&ephemeral_balance_pda);
        (ephemeral_balance_pda, escrow_deleg_record)
    }

    pub async fn get_remote_account(
        &self,
        pubkey: &Pubkey,
    ) -> Option<solana_sdk::account::Account> {
        self.rpc_client.get_account(pubkey).await.ok()
    }

    pub fn get_rpc_client(commitment: CommitmentConfig) -> RpcClient {
        RpcClient::new_with_commitment(RPC_URL.to_string(), commitment)
    }
}
