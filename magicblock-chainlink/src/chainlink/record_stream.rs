use std::{
    collections::HashMap,
    ops::{Deref, DerefMut},
    pin::Pin,
    time::Duration,
};

use futures_util::StreamExt;
use helius_laserstream::{
    LaserstreamConfig, LaserstreamError, StreamHandle, client,
    grpc::{
        CommitmentLevel, SlotStatus, SubscribeRequest,
        SubscribeRequestFilterAccounts, SubscribeRequestFilterAccountsFilter,
        SubscribeRequestFilterAccountsFilterMemcmp,
        SubscribeRequestFilterSlots, SubscribeRequestFilterTransactions,
        SubscribeUpdate, SubscribeUpdateAccount, SubscribeUpdateTransaction,
        subscribe_request_filter_accounts_filter::Filter,
        subscribe_request_filter_accounts_filter_memcmp::Data as MemcmpData,
        subscribe_update::UpdateOneof,
    },
};
use tokio::{
    sync::mpsc::{self, Receiver, Sender},
    time,
};

const PUBKEY_LEN: usize = 32;
const DELEGATION_RECORD_DISCRIMINATOR: u64 = 100;
const UNDELEGATE_DISCRIMINATOR: u64 = 3;
const DELEGATE_DISCRIMINATORS: [u64; 2] = [0, 19];
const DISCRIMINATOR_LEN: usize = 8;
const DELEGATION_RECORD_ACCOUNT_INDEX: usize = 6;
const DELEGATE_DELEGATED_ACCOUNT_INDEX: usize = 1;
const DELEGATE_RECORD_ACCOUNT_INDEX: usize = 4;
const UNDELEGATION_REQUEST_DISCRIMINATOR: u64 = 104;
const UNDELEGATION_REQUEST_MIN_LEN: usize = 8 + 32 + 8;
const MAX_PENDING_UPDATES: usize = 8192;
const MAX_RECONNECT_ATTEMPTS: u32 = 16;
const RECONNECT_BASE_DELAY: Duration = Duration::from_secs(1);
const RECONNECT_MAX_DELAY: Duration = Duration::from_secs(60);

/// Replay resumes behind the newest observed slot so updates near a disconnect
/// are not skipped.
const RESUME_SAFETY_MARGIN_SLOTS: u64 = 32;

type PubkeyBytes = [u8; PUBKEY_LEN];
type Slot = u64;
type Laser = Pin<
    Box<
        dyn futures_util::Stream<
                Item = Result<SubscribeUpdate, LaserstreamError>,
            > + Send,
    >,
>;

#[derive(Debug, thiserror::Error)]
pub enum RecordStreamError {
    #[error("record stream connection failed: {0}")]
    Connection(&'static str),
    #[error(transparent)]
    Laserstream(LaserstreamError),
}

#[derive(Debug)]
pub enum RecordStreamUpdate {
    Record {
        record: PubkeyBytes,
        data: Vec<u8>,
        slot: Slot,
    },
    RecordUndelegated {
        record: PubkeyBytes,
        slot: Slot,
    },
    DelegationObserved {
        delegated_account: PubkeyBytes,
        record: PubkeyBytes,
        slot: Slot,
    },
    UndelegationRequested {
        request_pda: PubkeyBytes,
        delegated_account: PubkeyBytes,
        expires_at_slot: Slot,
        slot: Slot,
    },
    SlotAdvanced(Slot),
    SyncInterrupted,
    SyncTerminated,
}

struct LaserStream {
    stream: Laser,
    _handle: Option<StreamHandle>,
}

impl Deref for LaserStream {
    type Target = Laser;

    fn deref(&self) -> &Self::Target {
        &self.stream
    }
}

impl DerefMut for LaserStream {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.stream
    }
}

enum DlpInstruction {
    Delegate {
        delegated_account: PubkeyBytes,
        record: PubkeyBytes,
    },
    Undelegate {
        record: PubkeyBytes,
    },
}

/// Lossless, confirmed DLP record and transaction stream used by Chainlink.
pub struct RecordStream {
    stream: LaserStream,
    config: LaserstreamConfig,
    updates: Sender<RecordStreamUpdate>,
    slot: Slot,
    watermark: Slot,
}

impl RecordStream {
    pub async fn start(
        endpoint: String,
        api_key: String,
    ) -> Result<Receiver<RecordStreamUpdate>, RecordStreamError> {
        let config = LaserstreamConfig {
            api_key,
            endpoint,
            channel_options: Default::default(),
            max_reconnect_attempts: Some(MAX_RECONNECT_ATTEMPTS),
            replay: true,
        };
        let (updates, receiver) = mpsc::channel(MAX_PENDING_UPDATES);
        let stream = Self::connect(config.clone(), None).await?;
        tokio::spawn(
            Self {
                stream,
                config,
                updates,
                slot: 0,
                watermark: 0,
            }
            .run(),
        );
        Ok(receiver)
    }

    async fn run(mut self) {
        loop {
            if self.updates.is_closed() {
                break;
            }
            match self.stream.next().await {
                Some(update) => self.handle_update(update).await,
                None if self.reconnect().await => {}
                None => break,
            }
        }
        let _ = self.updates.send(RecordStreamUpdate::SyncTerminated).await;
    }

    /// Reconnects indefinitely with replay behind the observed slot. A fresh
    /// stream is never trusted after continuity is lost.
    async fn reconnect(&mut self) -> bool {
        tracing::warn!("record stream ended; reconnecting");
        self.watermark = 0;
        if !self.send_interrupted().await {
            return false;
        }
        let mut delay = RECONNECT_BASE_DELAY;
        loop {
            if self.updates.is_closed() {
                return false;
            }
            let resume_slot =
                self.slot.saturating_sub(RESUME_SAFETY_MARGIN_SLOTS).max(1);
            match Self::connect(self.config.clone(), Some(resume_slot)).await {
                Ok(stream) => {
                    self.stream = stream;
                    self.watermark = 0;
                    tracing::info!(
                        from_slot = resume_slot,
                        "record stream reconnected"
                    );
                    return true;
                }
                Err(error) => tracing::warn!(
                    ?error,
                    from_slot = resume_slot,
                    "record stream resume failed"
                ),
            }

            tokio::select! {
                _ = self.updates.closed() => return false,
                _ = time::sleep(delay) => {}
            }
            delay = (delay * 2).min(RECONNECT_MAX_DELAY);
        }
    }

    async fn send_interrupted(&mut self) -> bool {
        self.updates
            .send(RecordStreamUpdate::SyncInterrupted)
            .await
            .is_ok()
    }

    async fn handle_update(
        &mut self,
        result: Result<SubscribeUpdate, LaserstreamError>,
    ) {
        let update = match result {
            Ok(update) => match update.update_oneof {
                Some(update) => update,
                None => return,
            },
            Err(error) => {
                tracing::warn!(%error, "record stream update failed");
                return;
            }
        };

        match update {
            UpdateOneof::Account(account) => {
                self.handle_account_update(account).await
            }
            UpdateOneof::Slot(slot) => {
                if SlotStatus::try_from(slot.status)
                    != Ok(SlotStatus::SlotConfirmed)
                {
                    return;
                }
                // LaserStream guarantees that every confirmed account and
                // transaction update through this slot precedes its confirmed
                // slot notification, making this an exact completeness barrier.
                self.slot = self.slot.max(slot.slot);
                if slot.slot > self.watermark {
                    self.watermark = slot.slot;
                    self.deliver(RecordStreamUpdate::SlotAdvanced(slot.slot))
                        .await;
                }
            }
            UpdateOneof::Transaction(transaction) => {
                self.handle_transaction_update(transaction).await
            }
            _ => {}
        }
    }

    async fn deliver(&mut self, update: RecordStreamUpdate) {
        if self.updates.send(update).await.is_err() {
            tracing::warn!("record stream consumer closed");
        }
    }

    async fn interrupt_on_watermark_violation(&mut self, slot: Slot) {
        if self.watermark > 0 && slot <= self.watermark {
            tracing::warn!(
                slot,
                watermark = self.watermark,
                "record update violated published watermark"
            );
            self.watermark = 0;
            self.deliver(RecordStreamUpdate::SyncInterrupted).await;
        }
    }

    async fn handle_account_update(&mut self, update: SubscribeUpdateAccount) {
        let Some(account) = update.account else {
            return;
        };
        let Ok(pubkey) = PubkeyBytes::try_from(account.pubkey.as_slice())
        else {
            return;
        };
        let event = match parse_undelegation_request(&account.data) {
            Some((delegated_account, expires_at_slot)) => {
                RecordStreamUpdate::UndelegationRequested {
                    request_pda: pubkey,
                    delegated_account,
                    expires_at_slot,
                    slot: update.slot,
                }
            }
            None => RecordStreamUpdate::Record {
                record: pubkey,
                data: account.data,
                slot: update.slot,
            },
        };
        self.interrupt_on_watermark_violation(update.slot).await;
        self.deliver(event).await;
    }

    async fn handle_transaction_update(
        &mut self,
        update: SubscribeUpdateTransaction,
    ) {
        let Some(info) = update.transaction else {
            return;
        };
        let (Some(transaction), Some(meta)) = (info.transaction, info.meta)
        else {
            return;
        };
        if meta.err.is_some() {
            return;
        }
        let Some(message) = transaction.message else {
            return;
        };

        let accounts: Vec<&Vec<u8>> = message
            .account_keys
            .iter()
            .chain(meta.loaded_writable_addresses.iter())
            .chain(meta.loaded_readonly_addresses.iter())
            .collect();
        let delegation_program = dlp_api::id().to_bytes();
        let account_at = |ix_accounts: &[u8], index: usize| {
            ix_accounts
                .get(index)
                .and_then(|&idx| accounts.get(idx as usize))
                .and_then(|bytes| PubkeyBytes::try_from(bytes.as_slice()).ok())
        };
        let parse = |program_id_index: usize,
                     ix_accounts: &[u8],
                     data: &[u8]| {
            let program_id = *accounts.get(program_id_index)?;
            (program_id.as_slice() == delegation_program).then_some(())?;
            let discriminator = u64::from_le_bytes(
                data.get(..DISCRIMINATOR_LEN)?.try_into().ok()?,
            );
            match discriminator {
                UNDELEGATE_DISCRIMINATOR => Some(DlpInstruction::Undelegate {
                    record: account_at(
                        ix_accounts,
                        DELEGATION_RECORD_ACCOUNT_INDEX,
                    )?,
                }),
                discriminator
                    if DELEGATE_DISCRIMINATORS.contains(&discriminator) =>
                {
                    Some(DlpInstruction::Delegate {
                        delegated_account: account_at(
                            ix_accounts,
                            DELEGATE_DELEGATED_ACCOUNT_INDEX,
                        )?,
                        record: account_at(
                            ix_accounts,
                            DELEGATE_RECORD_ACCOUNT_INDEX,
                        )?,
                    })
                }
                _ => None,
            }
        };

        let mut instructions = Vec::new();
        for instruction in &message.instructions {
            instructions.extend(parse(
                instruction.program_id_index as usize,
                &instruction.accounts,
                &instruction.data,
            ));
        }
        for inner in &meta.inner_instructions {
            for instruction in &inner.instructions {
                instructions.extend(parse(
                    instruction.program_id_index as usize,
                    &instruction.accounts,
                    &instruction.data,
                ));
            }
        }

        for instruction in instructions {
            let event = match instruction {
                DlpInstruction::Delegate {
                    delegated_account,
                    record,
                } => RecordStreamUpdate::DelegationObserved {
                    delegated_account,
                    record,
                    slot: update.slot,
                },
                DlpInstruction::Undelegate { record } => {
                    RecordStreamUpdate::RecordUndelegated {
                        record,
                        slot: update.slot,
                    }
                }
            };
            self.interrupt_on_watermark_violation(update.slot).await;
            self.deliver(event).await;
        }
    }

    fn subscribe_request(from_slot: Option<Slot>) -> SubscribeRequest {
        let mut accounts = HashMap::new();
        for (name, discriminator) in [
            ("delegations", DELEGATION_RECORD_DISCRIMINATOR),
            ("undelegation-requests", UNDELEGATION_REQUEST_DISCRIMINATOR),
        ] {
            accounts.insert(
                name.into(),
                SubscribeRequestFilterAccounts {
                    owner: vec![dlp_api::id().to_string()],
                    filters: vec![SubscribeRequestFilterAccountsFilter {
                        filter: Some(Filter::Memcmp(
                            SubscribeRequestFilterAccountsFilterMemcmp {
                                offset: 0,
                                data: Some(MemcmpData::Bytes(
                                    discriminator.to_le_bytes().to_vec(),
                                )),
                            },
                        )),
                    }],
                    ..Default::default()
                },
            );
        }

        let mut transactions = HashMap::new();
        transactions.insert(
            "delegation-instructions".into(),
            SubscribeRequestFilterTransactions {
                account_include: vec![dlp_api::id().to_string()],
                ..Default::default()
            },
        );
        let mut slots = HashMap::new();
        slots.insert(
            "slots".into(),
            SubscribeRequestFilterSlots {
                filter_by_commitment: Some(true),
                ..Default::default()
            },
        );
        SubscribeRequest {
            accounts,
            transactions,
            slots,
            commitment: Some(CommitmentLevel::Confirmed as i32),
            from_slot,
            ..Default::default()
        }
    }

    async fn connect(
        config: LaserstreamConfig,
        from_slot: Option<Slot>,
    ) -> Result<LaserStream, RecordStreamError> {
        let (stream, handle) =
            client::subscribe(config, Self::subscribe_request(from_slot));
        let mut stream = Box::pin(stream);
        let first = time::timeout(Duration::from_secs(5), stream.next())
            .await
            .map_err(|_| {
                RecordStreamError::Connection("health check timed out")
            })?
            .ok_or(RecordStreamError::Connection(
                "stream closed before first update",
            ))?
            .map_err(RecordStreamError::Laserstream)?;
        let stream = Box::pin(
            futures_util::stream::once(std::future::ready(Ok(first)))
                .chain(stream),
        );
        Ok(LaserStream {
            stream,
            _handle: Some(handle),
        })
    }
}

fn parse_undelegation_request(data: &[u8]) -> Option<(PubkeyBytes, Slot)> {
    if data.len() < UNDELEGATION_REQUEST_MIN_LEN
        || data[..DISCRIMINATOR_LEN]
            != UNDELEGATION_REQUEST_DISCRIMINATOR.to_le_bytes()
    {
        return None;
    }
    let delegated_account = PubkeyBytes::try_from(&data[8..40]).ok()?;
    let expires_at_slot = u64::from_le_bytes(data[40..48].try_into().ok()?);
    Some((delegated_account, expires_at_slot))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_stream() -> (RecordStream, Receiver<RecordStreamUpdate>) {
        let (updates, receiver) = mpsc::channel(32);
        (
            RecordStream {
                stream: LaserStream {
                    stream: Box::pin(futures_util::stream::pending()),
                    _handle: None,
                },
                config: LaserstreamConfig {
                    api_key: String::new(),
                    endpoint: String::new(),
                    channel_options: Default::default(),
                    max_reconnect_attempts: Some(1),
                    replay: true,
                },
                updates,
                slot: 0,
                watermark: 0,
            },
            receiver,
        )
    }

    fn delegate_update(
        delegated_account: PubkeyBytes,
        record: PubkeyBytes,
        inner: bool,
        discriminator: u64,
    ) -> SubscribeUpdate {
        use helius_laserstream::{
            grpc::{
                SubscribeUpdateTransaction, SubscribeUpdateTransactionInfo,
            },
            solana::storage::confirmed_block::{
                CompiledInstruction, InnerInstruction, InnerInstructions,
                Message, Transaction, TransactionStatusMeta,
            },
        };
        let accounts = vec![
            vec![9; PUBKEY_LEN],
            delegated_account.to_vec(),
            vec![7; PUBKEY_LEN],
            vec![6; PUBKEY_LEN],
            record.to_vec(),
            vec![5; PUBKEY_LEN],
            dlp_api::id().to_bytes().to_vec(),
        ];
        let data = discriminator.to_le_bytes().to_vec();
        let ix_accounts = vec![0, 1, 2, 3, 4, 5];
        let (instructions, inner_instructions) = if inner {
            (
                vec![],
                vec![InnerInstructions {
                    index: 0,
                    instructions: vec![InnerInstruction {
                        program_id_index: 6,
                        accounts: ix_accounts,
                        data,
                        ..Default::default()
                    }],
                }],
            )
        } else {
            (
                vec![CompiledInstruction {
                    program_id_index: 6,
                    accounts: ix_accounts,
                    data,
                }],
                vec![],
            )
        };
        SubscribeUpdate {
            update_oneof: Some(UpdateOneof::Transaction(
                SubscribeUpdateTransaction {
                    transaction: Some(SubscribeUpdateTransactionInfo {
                        transaction: Some(Transaction {
                            message: Some(Message {
                                account_keys: accounts,
                                instructions,
                                ..Default::default()
                            }),
                            ..Default::default()
                        }),
                        meta: Some(TransactionStatusMeta {
                            inner_instructions,
                            ..Default::default()
                        }),
                        ..Default::default()
                    }),
                    slot: 7,
                },
            )),
            ..Default::default()
        }
    }

    #[test]
    fn record_filter_is_discriminator_based() {
        let request = RecordStream::subscribe_request(Some(42));
        for (name, discriminator) in [
            ("delegations", DELEGATION_RECORD_DISCRIMINATOR),
            ("undelegation-requests", UNDELEGATION_REQUEST_DISCRIMINATOR),
        ] {
            let filter = request.accounts.get(name).unwrap();
            let [filter] = filter.filters.as_slice() else {
                panic!("expected one discriminator filter");
            };
            assert!(matches!(
                filter.filter,
                Some(Filter::Memcmp(ref memcmp))
                    if memcmp.offset == 0
                        && memcmp.data == Some(MemcmpData::Bytes(
                            discriminator.to_le_bytes().to_vec()
                        ))
            ));
        }
        assert_eq!(request.commitment, Some(CommitmentLevel::Confirmed as i32));
        assert_eq!(request.from_slot, Some(42));
        assert_eq!(
            request.transactions["delegation-instructions"].account_include,
            vec![dlp_api::id().to_string()]
        );
        assert_eq!(request.slots["slots"].filter_by_commitment, Some(true));
    }

    #[test]
    fn parses_undelegation_request() {
        let delegated_account = [7; PUBKEY_LEN];
        let mut data =
            UNDELEGATION_REQUEST_DISCRIMINATOR.to_le_bytes().to_vec();
        data.extend_from_slice(&delegated_account);
        data.extend_from_slice(&42u64.to_le_bytes());
        assert_eq!(
            parse_undelegation_request(&data),
            Some((delegated_account, 42))
        );
    }

    #[tokio::test]
    async fn observes_top_level_and_cpi_delegate_variants() {
        let (mut stream, mut updates) = test_stream();
        for (inner, discriminator) in [(false, 0), (true, 19)] {
            stream
                .handle_update(Ok(delegate_update(
                    [1; PUBKEY_LEN],
                    [2; PUBKEY_LEN],
                    inner,
                    discriminator,
                )))
                .await;
            assert!(matches!(
                updates.try_recv(),
                Ok(RecordStreamUpdate::DelegationObserved {
                    delegated_account,
                    record,
                    slot: 7,
                }) if delegated_account == [1; PUBKEY_LEN]
                    && record == [2; PUBKEY_LEN]
            ));
        }
    }

    #[tokio::test]
    async fn resolves_alt_loaded_undelegation_accounts() {
        use helius_laserstream::{
            grpc::{
                SubscribeUpdateTransaction, SubscribeUpdateTransactionInfo,
            },
            solana::storage::confirmed_block::{
                CompiledInstruction, Message, Transaction,
                TransactionStatusMeta,
            },
        };
        let record = [3; PUBKEY_LEN];
        let update = SubscribeUpdate {
            update_oneof: Some(UpdateOneof::Transaction(
                SubscribeUpdateTransaction {
                    transaction: Some(SubscribeUpdateTransactionInfo {
                        transaction: Some(Transaction {
                            message: Some(Message {
                                account_keys: vec![vec![9; PUBKEY_LEN]],
                                instructions: vec![CompiledInstruction {
                                    program_id_index: 2,
                                    accounts: vec![0, 0, 0, 0, 0, 0, 1],
                                    data: UNDELEGATE_DISCRIMINATOR
                                        .to_le_bytes()
                                        .to_vec(),
                                }],
                                ..Default::default()
                            }),
                            ..Default::default()
                        }),
                        meta: Some(TransactionStatusMeta {
                            loaded_writable_addresses: vec![record.to_vec()],
                            loaded_readonly_addresses: vec![
                                dlp_api::id().to_bytes().to_vec(),
                            ],
                            ..Default::default()
                        }),
                        ..Default::default()
                    }),
                    slot: 8,
                },
            )),
            ..Default::default()
        };
        let (mut stream, mut updates) = test_stream();
        stream.handle_update(Ok(update)).await;
        assert!(matches!(
            updates.try_recv(),
            Ok(RecordStreamUpdate::RecordUndelegated {
                record,
                slot: 8,
            }) if record == [3; PUBKEY_LEN]
        ));
    }

    #[tokio::test]
    async fn late_update_interrupts_before_delivery() {
        use helius_laserstream::grpc::{
            SubscribeUpdateAccountInfo, SubscribeUpdateSlot,
        };
        let (mut stream, mut updates) = test_stream();
        stream
            .handle_update(Ok(SubscribeUpdate {
                update_oneof: Some(UpdateOneof::Slot(SubscribeUpdateSlot {
                    slot: 100,
                    status: SlotStatus::SlotConfirmed as i32,
                    ..Default::default()
                })),
                ..Default::default()
            }))
            .await;
        assert!(matches!(
            updates.try_recv(),
            Ok(RecordStreamUpdate::SlotAdvanced(100))
        ));
        stream
            .handle_update(Ok(SubscribeUpdate {
                update_oneof: Some(UpdateOneof::Account(
                    SubscribeUpdateAccount {
                        account: Some(SubscribeUpdateAccountInfo {
                            pubkey: vec![4; PUBKEY_LEN],
                            data: vec![1],
                            ..Default::default()
                        }),
                        slot: 100,
                        ..Default::default()
                    },
                )),
                ..Default::default()
            }))
            .await;
        assert!(matches!(
            updates.try_recv(),
            Ok(RecordStreamUpdate::SyncInterrupted)
        ));
        assert!(matches!(
            updates.try_recv(),
            Ok(RecordStreamUpdate::Record { slot, .. }) if slot == 100
        ));
    }
}
