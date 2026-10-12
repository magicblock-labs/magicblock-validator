//! Provider-independent account images and canonical delegation/ATA fixtures.

use std::sync::Arc;

use base64::{Engine, engine::general_purpose::STANDARD};
use dlp_api::{pda::delegation_record_pda_from_delegated_account, state::DelegationRecord};
use serde_json::{Value, json};
use solana_account::{AccountBuilder, StateFlags, testkit::delegated_account};
use solana_instruction::Instruction;
use solana_program_option::COption;
use solana_program_pack::Pack;
use solana_pubkey::Pubkey;
use solana_rent::Rent;
use spl_associated_token_account_interface::address::get_associated_token_address_with_program_id;
use spl_token_2022::state::{Account as TokenAccount, AccountState};
use yellowstone_grpc_geyser::plugin::message::{Message, MessageAccount, MessageAccountInfo};

use super::{
    ACCOUNT_DATA, DELEGATED_TOKEN_BALANCE, NATIVE_RESERVE, TOKEN_BALANCE, TOKEN_LAMPORTS,
    V42_INITIAL,
};

/// One provider-independent account image for HTTP responses, notifications, and Geyser messages.
#[derive(Clone)]
pub(crate) struct Account {
    pub(crate) key: Pubkey,
    pub(crate) owner: Pubkey,
    pub(crate) lamports: u64,
    pub(crate) data: Vec<u8>,
    pub(crate) executable: bool,
}

impl Account {
    /// Non-executable, rent-funded image with one byte of recognizable data.
    pub(crate) fn new(key: Pubkey) -> Self {
        Self {
            key,
            owner: Pubkey::default(),
            lamports: 1_000_000,
            data: ACCOUNT_DATA.to_vec(),
            executable: false,
        }
    }

    /// Shares an Engine testkit image across the HTTP, WebSocket, and plugin encoders.
    pub(crate) fn from_builder(key: Pubkey, image: &AccountBuilder) -> Self {
        let image = image.read();
        Self {
            key,
            owner: image.owner(),
            lamports: image.lamports(),
            data: image.data().to_vec(),
            executable: image.flags().contains(StateFlags::EXECUTABLE),
        }
    }

    /// Confirmed RPC account encoding shared by HTTP fetches and WebSocket notifications.
    pub(crate) fn rpc(&self) -> Value {
        let compressed = zstd::stream::encode_all(self.data.as_slice(), 0).unwrap();
        json!({"owner": self.owner.to_string(), "lamports": self.lamports,
            "executable": self.executable, "data": [STANDARD.encode(compressed), "base64+zstd"]})
    }

    /// Plugin input image with a transaction signature, eligible for DLP account filters.
    pub(crate) fn geyser(&self, slot: u64) -> Message {
        Message::Account(Arc::new(MessageAccount {
            account: MessageAccountInfo {
                pubkey: self.key,
                owner: self.owner,
                lamports: self.lamports,
                data: self.data.clone().into(),
                executable: self.executable,
                rent_epoch: 0,
                write_version: slot,
                txn_signature: Some(Default::default()),
                pre_encoded: Default::default(),
            },
            slot,
            is_startup: false,
            created_at: Default::default(),
        }))
    }
}

/// Application and canonical record images shared by all provider encoders.
pub(crate) fn delegation(
    key: Pubkey,
    owner: Pubkey,
    authority: Pubkey,
    slot: u64,
    actions: &[Instruction],
) -> (Account, Account) {
    use dlp_api::compact::ClearText;
    let mut account = Account::new(key);
    account.owner = dlp_api::id();
    account.data = V42_INITIAL.to_le_bytes().to_vec();
    let metadata = DelegationRecord {
        owner,
        authority,
        delegation_slot: slot,
        lamports: account.lamports,
        commit_frequency_ms: 1,
    };
    let mut record = Account::new(delegation_record_pda_from_delegated_account(&key));
    record.owner = dlp_api::id();
    record.data = encode_record(&metadata);
    if !actions.is_empty() {
        record.data.extend(borsh::to_vec(&actions.to_vec().cleartext()).unwrap());
    }
    (account, record)
}

/// Canonical record header shared by ordinary and eATA delegation fixtures.
pub(crate) fn encode_record(metadata: &DelegationRecord) -> Vec<u8> {
    let mut data = vec![0; DelegationRecord::size_with_discriminator()];
    metadata.to_bytes_with_discriminator(&mut data).unwrap();
    data
}

/// Canonical, DLP-owned request image accepted by the confirmed account filter.
pub(crate) fn undelegation_request(key: Pubkey) -> Account {
    use dlp_api::{
        pda::undelegation_request_pda_from_delegated_account, state::UndelegationRequest,
    };
    let request = UndelegationRequest {
        delegated_account: key,
        expires_at_slot: 1000,
    };
    let mut account = Account::new(undelegation_request_pda_from_delegated_account(&key));
    account.owner = dlp_api::id();
    account.data.resize(UndelegationRequest::size_with_discriminator(), 0);
    request.to_bytes_with_discriminator(&mut account.data).unwrap();
    account
}

/// Canonical token account and the eATA delegation that projects onto it.
pub(crate) struct Ata {
    pub(crate) ata: Pubkey,
    /// Ordinary token image, including extensions and native reserve when requested.
    pub(crate) base: AccountBuilder,
    pub(crate) eata: Pubkey,
    /// Restored eATA image; provider encoders use DLP ownership instead.
    pub(crate) delegated: AccountBuilder,
    pub(crate) record: Vec<u8>,
}

impl Ata {
    /// Builds matching canonical identities, balances, and authority metadata.
    pub(crate) fn new(program: Pubkey, native: bool, authority: Pubkey, slot: u64) -> Self {
        let owner = Pubkey::new_unique();
        let mint = if !native {
            Pubkey::new_unique()
        } else if program == spl_token::id() {
            spl_token::native_mint::id()
        } else {
            spl_token_2022::native_mint::id()
        };
        let eata_program: Pubkey = "SPLxh1LVZzEkX99H6rqYizhytLWPZVV296zyYDPagv2".parse().unwrap();
        let (eata, bump) =
            Pubkey::find_program_address(&[owner.as_ref(), mint.as_ref()], &eata_program);
        let ata = get_associated_token_address_with_program_id(&owner, &mint, &program);
        let token = TokenAccount {
            owner,
            mint,
            amount: TOKEN_BALANCE,
            state: AccountState::Initialized,
            is_native: if native { COption::Some(NATIVE_RESERVE) } else { COption::None },
            ..Default::default()
        };
        let mut data = vec![0; TokenAccount::LEN];
        token.pack_into_slice(&mut data);
        if program == spl_token_2022::id() {
            // Account type followed by an ImmutableOwner TLV (tag 7, zero-length payload).
            data.extend([2, 7, 0, 0, 0]);
        }
        let lamports = if native { NATIVE_RESERVE + TOKEN_BALANCE } else { TOKEN_LAMPORTS };
        let base = AccountBuilder::default().owner(program).lamports(lamports).data(data);
        let mut data = owner.to_bytes().to_vec();
        data.extend(mint.to_bytes());
        data.extend(DELEGATED_TOKEN_BALANCE.to_le_bytes());
        data.extend([bump, 0, 0, 0, 0, 0, 0, 0]);
        let raw_lamports = Rent::default().minimum_balance(data.len());
        let delegated = delegated_account(raw_lamports, data, eata_program).slot(slot);
        let metadata = DelegationRecord {
            owner: eata_program,
            authority,
            delegation_slot: slot,
            lamports: raw_lamports,
            commit_frequency_ms: 1,
        };
        let record = encode_record(&metadata);
        Self {
            ata,
            base,
            eata,
            delegated,
            record,
        }
    }

    /// DLP-owned application and canonical record images for HTTP or the plugin.
    pub(crate) fn delegation(&self) -> [Account; 2] {
        let mut account = Account::from_builder(self.eata, &self.delegated);
        account.owner = dlp_api::id();
        let record = Account {
            owner: dlp_api::id(),
            data: self.record.clone(),
            ..Account::new(delegation_record_pda_from_delegated_account(&self.eata))
        };
        [account, record]
    }
}
