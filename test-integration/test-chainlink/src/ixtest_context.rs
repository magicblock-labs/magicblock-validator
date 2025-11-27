#![allow(unused)]
use std::sync::Arc;

use borsh::{BorshDeserialize, BorshSerialize};
use compressed_delegation_client::{
    CommitArgs, CommitBuilder, CompressedAccountMeta,
    CompressedDelegationRecord, DelegateArgs as DelegateArgsCpi, FinalizeArgs,
    FinalizeBuilder, PackedAddressTreeInfo, UndelegateArgs, UndelegateBuilder,
};
use dlp::args::DelegateEphemeralBalanceArgs;
use integration_test_tools::dlp_interface;
use light_client::indexer::{
    photon_indexer::PhotonIndexer, AddressWithTree, CompressedAccount, Indexer,
    TreeInfo, ValidityProofWithContext,
};
use light_compressed_account::address::derive_address;
use light_hasher::hash_to_field_size::hashv_to_bn254_field_size_be_const_array;
use light_sdk::instruction::{PackedAccounts, SystemAccountMetaConfig};
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
        photon_client::PhotonClientImpl,
        Endpoint, RemoteAccountProvider,
    },
    submux::SubMuxClient,
    testing::{
        cloner_stub::ClonerStub,
        photon_client_mock::PhotonClientMock,
        utils::{PHOTON_URL, RPC_URL},
    },
    Chainlink,
};
use magicblock_core::compression::derive_cda_from_pda;
use program_flexi_counter::state::FlexiCounter;
use solana_account::AccountSharedData;
use solana_compute_budget_interface::ComputeBudgetInstruction;
use solana_pubkey::Pubkey;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_rpc_client_api::config::{
    RpcSendTransactionConfig, RpcTransactionConfig,
};
use solana_sdk::{
    commitment_config::CommitmentConfig, native_token::LAMPORTS_PER_SOL,
    pubkey, signature::Keypair, signer::Signer, transaction::Transaction,
};
use solana_sdk_ids::{native_loader, system_program};
use solana_transaction_status::{
    option_serializer::OptionSerializer, UiTransactionEncoding,
};
use tokio::task;

use crate::{programs::send_instructions, sleep_ms};

pub type IxtestChainlink = Chainlink<
    ChainRpcClientImpl,
    SubMuxClient<ChainPubsubClientImpl>,
    AccountsBankStub,
    ClonerStub,
    PhotonClientImpl,
>;

#[derive(Clone)]
pub struct IxtestContext {
    pub rpc_client: Arc<RpcClient>,
    pub photon_indexer: Arc<PhotonIndexer>,
    pub chainlink: Arc<IxtestChainlink>,
    pub bank: Arc<AccountsBankStub>,
    pub remote_account_provider: Option<
        Arc<
            RemoteAccountProvider<
                ChainRpcClientImpl,
                SubMuxClient<ChainPubsubClientImpl>,
                PhotonClientImpl,
            >,
        >,
    >,
    pub cloner: Arc<ClonerStub>,
    pub validator_kp: Arc<Keypair>,
}

pub const TEST_AUTHORITY: [u8; 64] = [
    251, 62, 129, 184, 107, 49, 62, 184, 1, 147, 178, 128, 185, 157, 247, 92,
    56, 158, 145, 53, 51, 226, 202, 96, 178, 248, 195, 133, 133, 237, 237, 146,
    13, 32, 77, 204, 244, 56, 166, 172, 66, 113, 150, 218, 112, 42, 110, 181,
    98, 158, 222, 194, 130, 93, 175, 100, 190, 106, 9, 69, 156, 80, 96, 72,
];
const ADDRESS_TREE_PUBKEY: Pubkey =
    pubkey!("amt2kaJA14v3urZbZvnc5v2np8jqvc4Z8zDep5wbtzx");
const OUTPUT_QUEUE_PUBKEY: Pubkey =
    pubkey!("oq1na8gojfdUhsfCpyjNt6h4JaDWtHf1yQj4koBWfto");
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
        let (fetch_cloner, remote_account_provider, photon_indexer) = {
            let endpoints = [
                Endpoint::Rpc {
                    rpc_url: RPC_URL.to_string(),
                    pubsub_url: "ws://localhost:7800".to_string(),
                },
                Endpoint::Compression {
                    url: PHOTON_URL.to_string(),
                },
            ];
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

            let photon_indexer =
                Arc::new(PhotonIndexer::new(PHOTON_URL.to_string(), None));

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
                        photon_indexer,
                    )
                }
                Err(err) => {
                    panic!("Failed to create remote account provider: {err:?}");
                }
                _ => (None, None, photon_indexer),
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
            photon_indexer,
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

    pub async fn delegate_compressed_counter(
        &self,
        counter_auth: &Keypair,
        redelegate: bool,
    ) -> &Self {
        debug!(
            "Delegating compressed counter account {}",
            counter_auth.pubkey()
        );
        use program_flexi_counter::instruction::*;

        let auth = counter_auth.pubkey();
        let pda_seeds = FlexiCounter::seeds(&auth);
        let (pda, bump) = FlexiCounter::pda(&auth);
        let record_address = derive_cda_from_pda(&pda);

        let system_account_meta_config =
            SystemAccountMetaConfig::new(compressed_delegation_client::ID);
        let mut remaining_accounts = PackedAccounts::default();
        remaining_accounts
            .add_system_accounts_v2(system_account_meta_config)
            .unwrap();

        let (
            remaining_accounts_metas,
            validity_proof,
            address_tree_info,
            account_meta,
            output_state_tree_index,
        ) = if redelegate {
            let compressed_delegated_record: CompressedAccount = self
                .photon_indexer
                .get_compressed_account(record_address.to_bytes(), None)
                .await
                .unwrap()
                .value;

            let rpc_result: ValidityProofWithContext = self
                .photon_indexer
                .get_validity_proof(
                    vec![compressed_delegated_record.hash],
                    vec![],
                    None,
                )
                .await
                .unwrap()
                .value;

            let packed_state_tree = rpc_result
                .pack_tree_infos(&mut remaining_accounts)
                .state_trees
                .unwrap();
            let account_meta = CompressedAccountMeta {
                tree_info: packed_state_tree.packed_tree_infos[0],
                address: compressed_delegated_record.address.unwrap(),
                output_state_tree_index: packed_state_tree.output_tree_index,
            };

            let (remaining_accounts_metas, _, _) =
                remaining_accounts.to_account_metas();
            (
                remaining_accounts_metas,
                rpc_result.proof,
                None,
                Some(account_meta),
                packed_state_tree.output_tree_index,
            )
        } else {
            let rpc_result: ValidityProofWithContext = self
                .photon_indexer
                .get_validity_proof(
                    vec![],
                    vec![AddressWithTree {
                        address: record_address.to_bytes(),
                        tree: ADDRESS_TREE_PUBKEY,
                    }],
                    None,
                )
                .await
                .unwrap()
                .value;

            // Insert trees in accounts
            let address_merkle_tree_pubkey_index =
                remaining_accounts.insert_or_get(ADDRESS_TREE_PUBKEY);
            let state_queue_pubkey_index =
                remaining_accounts.insert_or_get(OUTPUT_QUEUE_PUBKEY);

            let packed_address_tree_info = PackedAddressTreeInfo {
                root_index: rpc_result.addresses[0].root_index,
                address_merkle_tree_pubkey_index,
                address_queue_pubkey_index: address_merkle_tree_pubkey_index,
            };

            let (remaining_accounts_metas, _, _) =
                remaining_accounts.to_account_metas();

            (
                remaining_accounts_metas,
                rpc_result.proof,
                Some(packed_address_tree_info),
                None,
                state_queue_pubkey_index,
            )
        };

        let delegate_ix = create_delegate_compressed_ix(
            counter_auth.pubkey(),
            &remaining_accounts_metas,
            DelegateCompressedArgs {
                validator: Some(self.validator_kp.pubkey()),
                validity_proof,
                account_meta,
                address_tree_info,
                output_state_tree_index,
            },
        );

        let latest_block_hash =
            self.rpc_client.get_latest_blockhash().await.unwrap();
        let tx = Transaction::new_signed_with_payer(
            &[
                ComputeBudgetInstruction::set_compute_unit_limit(300_000),
                delegate_ix,
            ],
            Some(&counter_auth.pubkey()),
            &[&counter_auth],
            latest_block_hash,
        );
        debug!("Delegate transaction: {:?}", tx.signatures[0]);
        let res = self
            .rpc_client
            .send_and_confirm_transaction_with_spinner_and_config(
                &tx,
                CommitmentConfig::confirmed(),
                RpcSendTransactionConfig {
                    skip_preflight: true,
                    ..Default::default()
                },
            )
            .await
            .expect("Failed to delegate account");

        // Wait for the indexer to index the account
        sleep_ms(500).await;

        self
    }

    pub async fn undelegate_compressed_counter(
        &self,
        counter_auth: &Keypair,
        redelegate: bool,
    ) -> &Self {
        debug!(
            "Undelegating compressed counter account {}",
            counter_auth.pubkey()
        );
        let counter_pda = self.counter_pda(&counter_auth.pubkey());
        // The committor service will call this in order to have
        // chainlink subscribe to account updates of the counter account
        self.chainlink.undelegation_requested(counter_pda).await;

        // In order to make the account undelegatable we first need to
        // commmit and finalize
        let (pda, bump) = FlexiCounter::pda(&counter_auth.pubkey());
        let record_address = derive_cda_from_pda(&pda);
        let compressed_account = self
            .photon_indexer
            .get_compressed_account(record_address.to_bytes(), None)
            .await
            .unwrap()
            .value;
        let system_account_meta_config =
            SystemAccountMetaConfig::new(compressed_delegation_client::ID);
        let mut remaining_accounts = PackedAccounts::default();
        remaining_accounts
            .add_system_accounts_v2(system_account_meta_config)
            .unwrap();

        let rpc_result = self
            .photon_indexer
            .get_validity_proof(vec![compressed_account.hash], vec![], None)
            .await
            .unwrap()
            .value;

        let packed_tree_accounts = rpc_result
            .pack_tree_infos(&mut remaining_accounts)
            .state_trees
            .unwrap();

        let account_meta = CompressedAccountMeta {
            tree_info: packed_tree_accounts.packed_tree_infos[0],
            address: compressed_account.address.unwrap(),
            output_state_tree_index: packed_tree_accounts.output_tree_index,
        };

        let (remaining_accounts_metas, _, _) =
            remaining_accounts.to_account_metas();

        let commit_ix =
            CommitBuilder::new()
                .validator(self.validator_kp.pubkey())
                .delegated_account(pda)
                .args(CommitArgs {
                    current_compressed_delegated_account_data:
                        compressed_account.data.unwrap().data.to_vec(),
                    new_data: FlexiCounter::new("undecompressed".to_string())
                        .try_to_vec()
                        .unwrap(),
                    account_meta,
                    validity_proof: rpc_result.proof,
                    update_nonce: 1,
                    allow_undelegation: true,
                })
                .add_remaining_accounts(remaining_accounts_metas.as_slice())
                .instruction();

        let latest_block_hash =
            self.rpc_client.get_latest_blockhash().await.unwrap();
        self.rpc_client
            .send_and_confirm_transaction_with_spinner_and_config(
                &Transaction::new_signed_with_payer(
                    &[commit_ix],
                    Some(&self.validator_kp.pubkey()),
                    &[&self.validator_kp],
                    latest_block_hash,
                ),
                CommitmentConfig::confirmed(),
                RpcSendTransactionConfig {
                    skip_preflight: true,
                    ..Default::default()
                },
            )
            .await
            .expect("Failed to commit account");

        // Wait for the indexer to index the account
        sleep_ms(500).await;

        // Finalize
        let compressed_account = self
            .photon_indexer
            .get_compressed_account(record_address.to_bytes(), None)
            .await
            .unwrap()
            .value;
        let system_account_meta_config =
            SystemAccountMetaConfig::new(compressed_delegation_client::ID);
        let mut remaining_accounts = PackedAccounts::default();
        remaining_accounts
            .add_system_accounts_v2(system_account_meta_config)
            .unwrap();

        let rpc_result = self
            .photon_indexer
            .get_validity_proof(vec![compressed_account.hash], vec![], None)
            .await
            .unwrap()
            .value;

        let packed_tree_accounts = rpc_result
            .pack_tree_infos(&mut remaining_accounts)
            .state_trees
            .unwrap();

        let account_meta = CompressedAccountMeta {
            tree_info: packed_tree_accounts.packed_tree_infos[0],
            address: compressed_account.address.unwrap(),
            output_state_tree_index: packed_tree_accounts.output_tree_index,
        };

        let (remaining_accounts_metas, _, _) =
            remaining_accounts.to_account_metas();

        let finalize_ix =
            FinalizeBuilder::new()
                .validator(self.validator_kp.pubkey())
                .delegated_account(pda)
                .args(FinalizeArgs {
                    current_compressed_delegated_account_data:
                        compressed_account.data.unwrap().data,
                    account_meta,
                    validity_proof: rpc_result.proof,
                })
                .add_remaining_accounts(remaining_accounts_metas.as_slice())
                .instruction();

        let latest_block_hash =
            self.rpc_client.get_latest_blockhash().await.unwrap();
        self.rpc_client
            .send_and_confirm_transaction_with_spinner_and_config(
                &Transaction::new_signed_with_payer(
                    &[finalize_ix],
                    Some(&self.validator_kp.pubkey()),
                    &[&self.validator_kp],
                    latest_block_hash,
                ),
                CommitmentConfig::confirmed(),
                RpcSendTransactionConfig {
                    skip_preflight: true,
                    ..Default::default()
                },
            )
            .await
            .expect("Failed to finalize account");

        // Wait for the indexer to index the account
        sleep_ms(500).await;

        let compressed_account = self
            .photon_indexer
            .get_compressed_account(record_address.to_bytes(), None)
            .await
            .unwrap()
            .value;
        let rpc_result = self
            .photon_indexer
            .get_validity_proof(vec![compressed_account.hash], vec![], None)
            .await
            .unwrap()
            .value;

        let system_account_meta_config =
            SystemAccountMetaConfig::new(compressed_delegation_client::ID);
        let mut remaining_accounts = PackedAccounts::default();
        remaining_accounts
            .add_system_accounts_v2(system_account_meta_config)
            .unwrap();

        let packed_state_tree = rpc_result
            .pack_tree_infos(&mut remaining_accounts)
            .state_trees
            .unwrap();
        let account_meta = CompressedAccountMeta {
            tree_info: packed_state_tree.packed_tree_infos[0],
            address: compressed_account.address.unwrap(),
            output_state_tree_index: packed_state_tree.output_tree_index,
        };

        let (remaining_accounts_metas, _, _) =
            remaining_accounts.to_account_metas();

        let undelegate_ix = UndelegateBuilder::new()
            .payer(counter_auth.pubkey())
            .delegated_account(counter_pda)
            .owner_program(program_flexi_counter::ID)
            .system_program(system_program::ID)
            .args(UndelegateArgs {
                validity_proof: rpc_result.proof,
                delegation_record_account_meta: account_meta,
                compressed_delegated_record:
                    CompressedDelegationRecord::try_from_slice(
                        &compressed_account.data.clone().unwrap().data,
                    )
                    .unwrap(),
            })
            .add_remaining_accounts(remaining_accounts_metas.as_slice())
            .instruction();
        let latest_block_hash =
            self.rpc_client.get_latest_blockhash().await.unwrap();
        let tx = Transaction::new_signed_with_payer(
            &[
                ComputeBudgetInstruction::set_compute_unit_limit(250_000),
                undelegate_ix,
            ],
            Some(&counter_auth.pubkey()),
            &[&counter_auth],
            latest_block_hash,
        );
        debug!("Undelegate transaction: {:?}", tx.signatures[0]);
        self.rpc_client
            .send_and_confirm_transaction_with_spinner_and_config(
                &tx,
                CommitmentConfig::confirmed(),
                RpcSendTransactionConfig {
                    skip_preflight: true,
                    ..Default::default()
                },
            )
            .await
            .expect("Failed to undelegate account");

        // Build instructions and required signers
        if redelegate {
            // Wait for the indexer to index the account
            sleep_ms(500).await;

            self.delegate_compressed_counter(counter_auth, true).await
        } else {
            self
        }
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
    ) -> Option<solana_account::Account> {
        self.rpc_client.get_account(pubkey).await.ok()
    }

    pub fn get_rpc_client(commitment: CommitmentConfig) -> RpcClient {
        RpcClient::new_with_commitment(RPC_URL.to_string(), commitment)
    }
}
