use std::{
    collections::HashMap,
    ops::{Deref, DerefMut},
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use dlp_api::pda::undelegation_request_pda_from_delegated_account;
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
use magicblock_metrics::metrics;
use solana_pubkey::Pubkey;
use solana_system_interface::MAX_PERMITTED_DATA_LENGTH;
use solana_transaction_status_client_types::{
    UiConfirmedBlock, UiInstruction, option_serializer::OptionSerializer,
};
use tokio::{
    sync::{
        Notify, OwnedSemaphorePermit, Semaphore,
        mpsc::{self, Receiver, Sender},
    },
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
const MAX_PENDING_PAYLOAD_BYTES: usize = 64 * 1024 * 1024;
const MAX_RECONNECT_ATTEMPTS: u32 = 16;
const MAX_REPLAY_RECONNECT_FAILURES: usize = 5;
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

impl RecordStreamUpdate {
    fn payload_bytes(&self) -> usize {
        match self {
            Self::Record { data, .. } => data.len(),
            _ => 0,
        }
    }
}

pub(super) struct RecordStreamMessage {
    update: RecordStreamUpdate,
    epoch: u64,
    _payload_permit: Option<OwnedSemaphorePermit>,
}

impl RecordStreamMessage {
    #[cfg(test)]
    pub(super) fn into_update(self) -> RecordStreamUpdate {
        self.update
    }

    pub(super) fn into_parts(self) -> (RecordStreamUpdate, u64) {
        (self.update, self.epoch)
    }
}

pub(super) struct RecordStreamReceiver {
    pub(super) updates: Receiver<RecordStreamMessage>,
    pub(super) continuity_epoch: Arc<AtomicU64>,
    pub(super) replay_recovery: Arc<ReplayRecoveryState>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct ReplayRecoveryRange {
    pub(super) after_slot: Slot,
    pub(super) through_slot: Slot,
}

impl ReplayRecoveryRange {
    pub(super) fn merged(self, other: Self) -> Self {
        Self {
            after_slot: self.after_slot.min(other.after_slot),
            through_slot: self.through_slot.max(other.through_slot),
        }
    }
}

#[derive(Debug, Default)]
pub(super) struct ReplayRecoveryState {
    pending: Mutex<Option<ReplayRecoveryRange>>,
    notify: Notify,
}

impl ReplayRecoveryState {
    pub(super) fn request(&self, range: ReplayRecoveryRange) {
        if range.through_slot <= range.after_slot {
            return;
        }
        let mut pending = self.pending.lock().expect("replay recovery lock");
        *pending = Some(
            pending
                .take()
                .map_or(range, |current| current.merged(range)),
        );
        self.notify.notify_one();
    }

    pub(super) async fn notified(&self) {
        self.notify.notified().await;
    }

    pub(super) fn take_pending(&self) -> Option<ReplayRecoveryRange> {
        self.pending.lock().expect("replay recovery lock").take()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct RecoveredDelegation {
    pub(super) delegated_account: Pubkey,
    pub(super) record: Pubkey,
    pub(super) slot: Slot,
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
    updates: Sender<RecordStreamMessage>,
    payload_budget: Arc<Semaphore>,
    continuity_epoch: Arc<AtomicU64>,
    replay_recovery: Arc<ReplayRecoveryState>,
    slot: Slot,
    last_confirmed_slot: Slot,
    watermark: Slot,
}

impl RecordStream {
    pub async fn start(
        endpoint: String,
        api_key: String,
    ) -> Result<RecordStreamReceiver, RecordStreamError> {
        let config = LaserstreamConfig {
            api_key,
            endpoint,
            channel_options: Default::default(),
            max_reconnect_attempts: Some(MAX_RECONNECT_ATTEMPTS),
            replay: true,
        };
        let (updates, receiver) = mpsc::channel(MAX_PENDING_UPDATES);
        let continuity_epoch = Arc::new(AtomicU64::new(0));
        let replay_recovery = Arc::new(ReplayRecoveryState::default());
        let (stream, confirmed_slot) =
            Self::connect(config.clone(), None).await?;
        replay_recovery.request(ReplayRecoveryRange {
            after_slot: 0,
            through_slot: confirmed_slot,
        });
        tokio::spawn(
            Self {
                stream,
                config,
                updates,
                payload_budget: Arc::new(Semaphore::new(
                    MAX_PENDING_PAYLOAD_BYTES,
                )),
                continuity_epoch: Arc::clone(&continuity_epoch),
                replay_recovery: Arc::clone(&replay_recovery),
                slot: 0,
                last_confirmed_slot: 0,
                watermark: 0,
            }
            .run(),
        );
        Ok(RecordStreamReceiver {
            updates: receiver,
            continuity_epoch,
            replay_recovery,
        })
    }

    async fn run(mut self) {
        loop {
            if self.updates.is_closed() {
                break;
            }
            match self.stream.next().await {
                Some(update) => {
                    if !self.handle_update(update).await
                        && !self.reconnect().await
                    {
                        break;
                    }
                }
                None => {
                    if !self.interrupt().await || !self.reconnect().await {
                        break;
                    }
                }
            }
        }
        self.invalidate_continuity();
        self.deliver(RecordStreamUpdate::SyncTerminated).await;
    }

    /// Reconnects with replay behind the observed slot. If that anchor is no
    /// longer retained, a fresh confirmed barrier requests RPC reconciliation.
    async fn reconnect(&mut self) -> bool {
        tracing::warn!("record stream continuity lost; reconnecting");
        let mut delay = RECONNECT_BASE_DELAY;
        let mut replay_failures = 0usize;
        loop {
            if self.updates.is_closed() {
                return false;
            }
            let from_slot = reconnect_from_slot(self.slot, replay_failures);
            match Self::connect(self.config.clone(), from_slot).await {
                Ok((stream, confirmed_slot)) => {
                    self.stream = stream;
                    self.watermark = 0;
                    if let Some(from_slot) = from_slot {
                        tracing::info!(from_slot, "record stream reconnected");
                    } else {
                        let range = ReplayRecoveryRange {
                            after_slot: self.last_confirmed_slot,
                            through_slot: confirmed_slot,
                        };
                        self.replay_recovery.request(range);
                        tracing::warn!(
                            after_slot = range.after_slot,
                            through_slot = range.through_slot,
                            "record stream replay anchor expired; requesting RPC recovery"
                        );
                    }
                    return true;
                }
                Err(error) => tracing::warn!(
                    ?error,
                    from_slot,
                    "record stream resume failed"
                ),
            }
            replay_failures = replay_failures.saturating_add(1);

            tokio::select! {
                _ = self.updates.closed() => return false,
                _ = time::sleep(delay) => {}
            }
            delay = (delay * 2).min(RECONNECT_MAX_DELAY);
        }
    }

    async fn interrupt(&mut self) -> bool {
        self.invalidate_continuity();
        self.enqueue(RecordStreamUpdate::SyncInterrupted).await
    }

    fn invalidate_continuity(&mut self) {
        self.watermark = 0;
        self.continuity_epoch.fetch_add(1, Ordering::AcqRel);
        metrics::set_record_mirror_live(false);
    }

    async fn handle_update(
        &mut self,
        result: Result<SubscribeUpdate, LaserstreamError>,
    ) -> bool {
        let mut update = match result {
            Ok(update) => match update.update_oneof {
                Some(update) => update,
                None => return true,
            },
            Err(error) => {
                tracing::warn!(%error, "record stream update failed");
                let _ = self.interrupt().await;
                return false;
            }
        };
        sanitize_account_update_payload(&mut update);

        match update {
            UpdateOneof::Account(account) => {
                self.slot = self.slot.max(account.slot);
                let violated =
                    self.interrupt_on_watermark_violation(account.slot).await;
                self.handle_account_update(account).await;
                !violated
            }
            UpdateOneof::Slot(slot) => {
                if SlotStatus::try_from(slot.status)
                    != Ok(SlotStatus::SlotConfirmed)
                {
                    return true;
                }
                // LaserStream guarantees that every confirmed account and
                // transaction update through this slot precedes its confirmed
                // slot notification, making this an exact completeness barrier.
                self.slot = self.slot.max(slot.slot);
                if slot.slot > self.watermark {
                    self.watermark = slot.slot;
                    self.last_confirmed_slot =
                        self.last_confirmed_slot.max(slot.slot);
                    self.deliver(RecordStreamUpdate::SlotAdvanced(slot.slot))
                        .await;
                }
                true
            }
            UpdateOneof::Transaction(transaction) => {
                self.slot = self.slot.max(transaction.slot);
                let violated = self
                    .interrupt_on_watermark_violation(transaction.slot)
                    .await;
                self.handle_transaction_update(transaction).await;
                !violated
            }
            _ => true,
        }
    }

    async fn enqueue(&mut self, update: RecordStreamUpdate) -> bool {
        let payload_bytes = update.payload_bytes();
        let payload_permit = if payload_bytes == 0 {
            None
        } else {
            let Ok(payload_bytes) = u32::try_from(payload_bytes) else {
                return false;
            };
            match self
                .payload_budget
                .clone()
                .acquire_many_owned(payload_bytes)
                .await
            {
                Ok(permit) => Some(permit),
                Err(_) => return false,
            }
        };
        self.updates
            .send(RecordStreamMessage {
                update,
                epoch: self.continuity_epoch.load(Ordering::Acquire),
                _payload_permit: payload_permit,
            })
            .await
            .is_ok()
    }

    async fn deliver(&mut self, update: RecordStreamUpdate) {
        if !self.enqueue(update).await {
            tracing::warn!("record stream consumer closed");
        }
    }

    async fn interrupt_on_watermark_violation(&mut self, slot: Slot) -> bool {
        if self.watermark > 0 && slot <= self.watermark {
            tracing::warn!(
                slot,
                watermark = self.watermark,
                "record update violated published watermark"
            );
            // Rewind replay to the interval whose ordering guarantee failed.
            self.slot = slot;
            self.last_confirmed_slot =
                self.last_confirmed_slot.min(slot.saturating_sub(1));
            self.invalidate_continuity();
            self.deliver(RecordStreamUpdate::SyncInterrupted).await;
            true
        } else {
            false
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
        let event = if account.data.get(..DISCRIMINATOR_LEN)
            == Some(UNDELEGATION_REQUEST_DISCRIMINATOR.to_le_bytes().as_slice())
        {
            let Some((delegated_account, expires_at_slot)) =
                parse_undelegation_request(&account.data)
            else {
                tracing::warn!(
                    request_pda = %Pubkey::new_from_array(pubkey),
                    data_len = account.data.len(),
                    "ignoring malformed undelegation request account update"
                );
                return;
            };
            let delegated_account_pubkey =
                Pubkey::new_from_array(delegated_account);
            if undelegation_request_pda_from_delegated_account(
                &delegated_account_pubkey,
            ) != Pubkey::new_from_array(pubkey)
            {
                tracing::warn!(
                    request_pda = %Pubkey::new_from_array(pubkey),
                    delegated_account = %delegated_account_pubkey,
                    "ignoring undelegation request account update with invalid PDA"
                );
                return;
            }
            RecordStreamUpdate::UndelegationRequested {
                request_pda: pubkey,
                delegated_account,
                expires_at_slot,
                slot: update.slot,
            }
        } else {
            RecordStreamUpdate::Record {
                record: pubkey,
                data: account.data,
                slot: update.slot,
            }
        };
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

        let accounts = message
            .account_keys
            .iter()
            .chain(meta.loaded_writable_addresses.iter())
            .chain(meta.loaded_readonly_addresses.iter())
            .map(|bytes| PubkeyBytes::try_from(bytes.as_slice()).ok())
            .collect::<Vec<_>>();

        let mut instructions = Vec::new();
        for instruction in &message.instructions {
            instructions.extend(parse_dlp_instruction(
                &accounts,
                instruction.program_id_index as usize,
                &instruction.accounts,
                &instruction.data,
            ));
        }
        for inner in &meta.inner_instructions {
            for instruction in &inner.instructions {
                instructions.extend(parse_dlp_instruction(
                    &accounts,
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
    ) -> Result<(LaserStream, Slot), RecordStreamError> {
        let (stream, handle) =
            client::subscribe(config, Self::subscribe_request(from_slot));
        let mut stream: Laser = Box::pin(stream);
        let (initial_updates, confirmed_slot) = time::timeout(
            Duration::from_secs(5),
            buffer_until_confirmed_slot(&mut stream),
        )
        .await
        .map_err(|_| {
            RecordStreamError::Connection(
                "confirmed-slot health check timed out",
            )
        })??;
        let stream = Box::pin(
            futures_util::stream::iter(initial_updates.into_iter().map(Ok))
                .chain(stream),
        );
        Ok((
            LaserStream {
                stream,
                _handle: Some(handle),
            },
            confirmed_slot,
        ))
    }
}

fn parse_dlp_instruction(
    accounts: &[Option<PubkeyBytes>],
    program_id_index: usize,
    ix_accounts: &[u8],
    data: &[u8],
) -> Option<DlpInstruction> {
    let program_id = accounts.get(program_id_index).copied().flatten()?;
    (program_id == dlp_api::id().to_bytes()).then_some(())?;
    let account_at = |index: usize| {
        ix_accounts
            .get(index)
            .and_then(|&idx| accounts.get(idx as usize))
            .copied()
            .flatten()
    };
    let discriminator =
        u64::from_le_bytes(data.get(..DISCRIMINATOR_LEN)?.try_into().ok()?);
    match discriminator {
        UNDELEGATE_DISCRIMINATOR => Some(DlpInstruction::Undelegate {
            record: account_at(DELEGATION_RECORD_ACCOUNT_INDEX)?,
        }),
        discriminator if DELEGATE_DISCRIMINATORS.contains(&discriminator) => {
            Some(DlpInstruction::Delegate {
                delegated_account: account_at(
                    DELEGATE_DELEGATED_ACCOUNT_INDEX,
                )?,
                record: account_at(DELEGATE_RECORD_ACCOUNT_INDEX)?,
            })
        }
        _ => None,
    }
}

pub(super) fn recover_delegations_from_block(
    block: &UiConfirmedBlock,
    slot: Slot,
) -> Vec<RecoveredDelegation> {
    let mut recovered = Vec::new();
    for encoded in block.transactions.iter().flatten() {
        let Some(meta) = encoded.meta.as_ref() else {
            continue;
        };
        if meta.err.is_some() {
            continue;
        }
        let Some(transaction) = encoded.transaction.decode() else {
            continue;
        };
        let mut accounts = transaction
            .message
            .static_account_keys()
            .iter()
            .map(|pubkey| Some(pubkey.to_bytes()))
            .collect::<Vec<_>>();
        if let OptionSerializer::Some(loaded) = &meta.loaded_addresses {
            accounts.extend(
                loaded.writable.iter().chain(loaded.readonly.iter()).map(
                    |pubkey| {
                        pubkey.parse::<Pubkey>().ok().map(|key| key.to_bytes())
                    },
                ),
            );
        }

        for instruction in transaction.message.instructions() {
            if let Some(DlpInstruction::Delegate {
                delegated_account,
                record,
            }) = parse_dlp_instruction(
                &accounts,
                instruction.program_id_index as usize,
                &instruction.accounts,
                &instruction.data,
            ) {
                recovered.push(RecoveredDelegation {
                    delegated_account: Pubkey::new_from_array(
                        delegated_account,
                    ),
                    record: Pubkey::new_from_array(record),
                    slot,
                });
            }
        }

        let OptionSerializer::Some(inner_instructions) =
            &meta.inner_instructions
        else {
            continue;
        };
        for inner in inner_instructions {
            for instruction in &inner.instructions {
                let UiInstruction::Compiled(instruction) = instruction else {
                    continue;
                };
                let Ok(data) = bs58::decode(&instruction.data).into_vec()
                else {
                    continue;
                };
                if let Some(DlpInstruction::Delegate {
                    delegated_account,
                    record,
                }) = parse_dlp_instruction(
                    &accounts,
                    instruction.program_id_index as usize,
                    &instruction.accounts,
                    &data,
                ) {
                    recovered.push(RecoveredDelegation {
                        delegated_account: Pubkey::new_from_array(
                            delegated_account,
                        ),
                        record: Pubkey::new_from_array(record),
                        slot,
                    });
                }
            }
        }
    }
    recovered
}

fn reconnect_from_slot(slot: Slot, replay_failures: usize) -> Option<Slot> {
    (replay_failures < MAX_REPLAY_RECONNECT_FAILURES)
        .then(|| slot.saturating_sub(RESUME_SAFETY_MARGIN_SLOTS).max(1))
}

async fn buffer_until_confirmed_slot(
    stream: &mut Laser,
) -> Result<(Vec<SubscribeUpdate>, Slot), RecordStreamError> {
    let mut initial_updates = Vec::new();
    let mut pending_payload_bytes = 0usize;
    loop {
        let mut update = stream
            .next()
            .await
            .ok_or(RecordStreamError::Connection(
                "stream closed before confirmed-slot barrier",
            ))?
            .map_err(RecordStreamError::Laserstream)?;
        if let Some(update) = update.update_oneof.as_mut() {
            pending_payload_bytes = pending_payload_bytes
                .saturating_add(sanitize_account_update_payload(update));
            if pending_payload_bytes > MAX_PENDING_PAYLOAD_BYTES {
                return Err(RecordStreamError::Connection(
                    "confirmed-slot barrier exceeded payload budget",
                ));
            }
        }
        let confirmed_slot = match update.update_oneof.as_ref() {
            Some(UpdateOneof::Slot(slot))
                if SlotStatus::try_from(slot.status)
                    == Ok(SlotStatus::SlotConfirmed) =>
            {
                Some(slot.slot)
            }
            _ => None,
        };
        initial_updates.push(update);
        if let Some(confirmed_slot) = confirmed_slot {
            return Ok((initial_updates, confirmed_slot));
        }
        if initial_updates.len() >= MAX_PENDING_UPDATES {
            return Err(RecordStreamError::Connection(
                "confirmed-slot barrier exceeded startup buffer",
            ));
        }
    }
}

fn sanitize_account_update_payload(update: &mut UpdateOneof) -> usize {
    let UpdateOneof::Account(update) = update else {
        return 0;
    };
    let Some(account) = update.account.as_mut() else {
        return 0;
    };
    if account.data.len() > MAX_PERMITTED_DATA_LENGTH as usize {
        tracing::warn!(
            data_len = account.data.len(),
            "record stream account exceeded the Solana data limit; requiring RPC confirmation"
        );
        account.data.clear();
    }
    account.data.len()
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

    fn test_stream() -> (RecordStream, Receiver<RecordStreamMessage>) {
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
                payload_budget: Arc::new(Semaphore::new(
                    MAX_PENDING_PAYLOAD_BYTES,
                )),
                continuity_epoch: Arc::new(AtomicU64::new(0)),
                replay_recovery: Arc::new(ReplayRecoveryState::default()),
                slot: 0,
                last_confirmed_slot: 0,
                watermark: 0,
            },
            receiver,
        )
    }

    fn try_recv_update(
        updates: &mut Receiver<RecordStreamMessage>,
    ) -> Result<RecordStreamUpdate, mpsc::error::TryRecvError> {
        updates.try_recv().map(RecordStreamMessage::into_update)
    }

    async fn recv_update(
        updates: &mut Receiver<RecordStreamMessage>,
    ) -> Option<RecordStreamUpdate> {
        updates.recv().await.map(RecordStreamMessage::into_update)
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
    fn reconnect_drops_expired_replay_anchor() {
        assert_eq!(reconnect_from_slot(100, 0), Some(68));
        assert_eq!(reconnect_from_slot(10, 4), Some(1));
        assert_eq!(reconnect_from_slot(100, 5), None);
        assert_eq!(reconnect_from_slot(100, usize::MAX), None);
    }

    #[tokio::test]
    async fn pending_recovery_ranges_merge_without_loss() {
        let recovery = ReplayRecoveryState::default();
        recovery.request(ReplayRecoveryRange {
            after_slot: 20,
            through_slot: 30,
        });
        recovery.request(ReplayRecoveryRange {
            after_slot: 40,
            through_slot: 50,
        });

        recovery.notified().await;
        assert_eq!(
            recovery.take_pending(),
            Some(ReplayRecoveryRange {
                after_slot: 20,
                through_slot: 50,
            })
        );
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
    async fn rejects_undelegation_request_with_noncanonical_pda() {
        use helius_laserstream::grpc::SubscribeUpdateAccountInfo;

        let delegated_account = Pubkey::new_unique();
        let mut data =
            UNDELEGATION_REQUEST_DISCRIMINATOR.to_le_bytes().to_vec();
        data.extend_from_slice(delegated_account.as_ref());
        data.extend_from_slice(&42u64.to_le_bytes());
        let (mut stream, mut updates) = test_stream();

        stream
            .handle_account_update(SubscribeUpdateAccount {
                account: Some(SubscribeUpdateAccountInfo {
                    pubkey: Pubkey::new_unique().to_bytes().to_vec(),
                    data,
                    ..Default::default()
                }),
                slot: 7,
                ..Default::default()
            })
            .await;

        assert!(matches!(
            updates.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
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
                try_recv_update(&mut updates),
                Ok(RecordStreamUpdate::DelegationObserved {
                    delegated_account,
                    record,
                    slot: 7,
                }) if delegated_account == [1; PUBKEY_LEN]
                    && record == [2; PUBKEY_LEN]
            ));
        }
        assert_eq!(stream.slot, 7);
    }

    #[test]
    fn recovers_cpi_delegation_with_alt_accounts_from_block() {
        use solana_hash::Hash;
        use solana_message::{
            MessageHeader, VersionedMessage,
            compiled_instruction::CompiledInstruction,
            v0::{LoadedAddresses, Message, MessageAddressTableLookup},
        };
        use solana_signature::Signature;
        use solana_transaction::versioned::VersionedTransaction;
        use solana_transaction_status::Encodable;
        use solana_transaction_status_client_types::{
            EncodedTransactionWithStatusMeta, InnerInstruction,
            InnerInstructions, TransactionStatusMeta, UiTransactionEncoding,
        };

        let delegated_account = Pubkey::new_unique();
        let record = Pubkey::new_unique();
        let transaction = VersionedTransaction {
            signatures: vec![Signature::default()],
            message: VersionedMessage::V0(Message {
                header: MessageHeader {
                    num_required_signatures: 1,
                    num_readonly_signed_accounts: 0,
                    num_readonly_unsigned_accounts: 1,
                },
                account_keys: vec![Pubkey::new_unique(), dlp_api::id()],
                recent_blockhash: Hash::default(),
                instructions: vec![],
                address_table_lookups: vec![MessageAddressTableLookup {
                    account_key: Pubkey::new_unique(),
                    writable_indexes: vec![0, 1],
                    readonly_indexes: vec![],
                }],
            }),
        };
        let meta = TransactionStatusMeta {
            inner_instructions: Some(vec![InnerInstructions {
                index: 0,
                instructions: vec![InnerInstruction {
                    instruction: CompiledInstruction {
                        program_id_index: 1,
                        accounts: vec![0, 2, 0, 0, 3],
                        data: 0u64.to_le_bytes().to_vec(),
                    },
                    stack_height: Some(2),
                }],
            }]),
            loaded_addresses: LoadedAddresses {
                writable: vec![delegated_account, record],
                readonly: vec![],
            },
            ..Default::default()
        };
        let block = UiConfirmedBlock {
            previous_blockhash: String::new(),
            blockhash: String::new(),
            parent_slot: 41,
            transactions: Some(vec![EncodedTransactionWithStatusMeta {
                transaction: transaction.encode(UiTransactionEncoding::Base64),
                meta: Some(meta.into()),
                version: None,
            }]),
            signatures: None,
            rewards: None,
            num_reward_partitions: None,
            block_time: None,
            block_height: None,
        };

        assert_eq!(
            recover_delegations_from_block(&block, 42),
            vec![RecoveredDelegation {
                delegated_account,
                record,
                slot: 42,
            }]
        );
    }

    #[tokio::test]
    async fn startup_requires_confirmed_slot_replay_anchor() {
        use helius_laserstream::grpc::SubscribeUpdateSlot;

        let mut source: Laser = Box::pin(futures_util::stream::iter([
            Ok(delegate_update([1; PUBKEY_LEN], [2; PUBKEY_LEN], false, 0)),
            Ok(SubscribeUpdate {
                update_oneof: Some(UpdateOneof::Slot(SubscribeUpdateSlot {
                    slot: 8,
                    status: SlotStatus::SlotConfirmed as i32,
                    ..Default::default()
                })),
                ..Default::default()
            }),
        ]));
        let (buffered, confirmed_slot) =
            buffer_until_confirmed_slot(&mut source).await.unwrap();
        assert_eq!(buffered.len(), 2);
        assert_eq!(confirmed_slot, 8);

        let mut source_without_barrier: Laser =
            Box::pin(futures_util::stream::iter([Ok(delegate_update(
                [1; PUBKEY_LEN],
                [2; PUBKEY_LEN],
                false,
                0,
            ))]));
        assert!(matches!(
            buffer_until_confirmed_slot(&mut source_without_barrier).await,
            Err(RecordStreamError::Connection(
                "stream closed before confirmed-slot barrier"
            ))
        ));
    }

    #[tokio::test]
    async fn pending_record_payload_preserves_action_bytes() {
        use dlp_api::state::DelegationRecord;
        use helius_laserstream::grpc::SubscribeUpdateAccountInfo;

        let payload_len = DelegationRecord::size_with_discriminator() + 64;
        let (mut stream, mut updates) = test_stream();
        stream
            .handle_update(Ok(SubscribeUpdate {
                update_oneof: Some(UpdateOneof::Account(
                    SubscribeUpdateAccount {
                        account: Some(SubscribeUpdateAccountInfo {
                            pubkey: vec![4; PUBKEY_LEN],
                            data: vec![0; payload_len],
                            ..Default::default()
                        }),
                        slot: 7,
                        ..Default::default()
                    },
                )),
                ..Default::default()
            }))
            .await;

        assert!(matches!(
            recv_update(&mut updates).await,
            Some(RecordStreamUpdate::Record { data, .. })
                if data.len() == payload_len
        ));
    }

    #[tokio::test]
    async fn pending_record_payload_budget_applies_backpressure() {
        let (mut stream, mut updates) = test_stream();
        stream.payload_budget = Arc::new(Semaphore::new(4));
        let record = |slot| RecordStreamUpdate::Record {
            record: [slot as u8; PUBKEY_LEN],
            data: vec![0; 4],
            slot,
        };

        stream.deliver(record(1)).await;
        let blocked_delivery = stream.deliver(record(2));
        tokio::pin!(blocked_delivery);
        assert!(
            tokio::time::timeout(
                Duration::from_millis(10),
                &mut blocked_delivery,
            )
            .await
            .is_err()
        );

        drop(updates.recv().await.unwrap());
        tokio::time::timeout(Duration::from_secs(1), blocked_delivery)
            .await
            .unwrap();
        assert!(updates.recv().await.is_some());
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
            try_recv_update(&mut updates),
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
            try_recv_update(&mut updates),
            Ok(RecordStreamUpdate::SlotAdvanced(100))
        ));
        assert!(
            !stream
                .handle_update(Ok(SubscribeUpdate {
                    update_oneof: Some(UpdateOneof::Account(
                        SubscribeUpdateAccount {
                            account: Some(SubscribeUpdateAccountInfo {
                                pubkey: vec![4; PUBKEY_LEN],
                                data: vec![1],
                                ..Default::default()
                            }),
                            slot: 40,
                            ..Default::default()
                        },
                    )),
                    ..Default::default()
                }))
                .await
        );
        assert!(matches!(
            try_recv_update(&mut updates),
            Ok(RecordStreamUpdate::SyncInterrupted)
        ));
        assert!(matches!(
            try_recv_update(&mut updates),
            Ok(RecordStreamUpdate::Record { slot, .. }) if slot == 40
        ));
        assert_eq!(stream.slot, 40);
        assert_eq!(stream.last_confirmed_slot, 39);
    }

    #[tokio::test]
    async fn stream_error_interrupts_before_reconnect() {
        let (mut stream, mut updates) = test_stream();
        stream.watermark = 100;

        assert!(
            !stream
                .handle_update(Err(LaserstreamError::ConnectionError(
                    "test".into()
                )))
                .await
        );
        assert_eq!(stream.watermark, 0);
        assert!(matches!(
            try_recv_update(&mut updates),
            Ok(RecordStreamUpdate::SyncInterrupted)
        ));
    }

    #[tokio::test]
    async fn continuity_invalidates_before_control_enqueue() {
        let (mut stream, _updates) = test_stream();
        for slot in 0..32 {
            stream.deliver(RecordStreamUpdate::SlotAdvanced(slot)).await;
        }
        let continuity_epoch = Arc::clone(&stream.continuity_epoch);
        let interrupt = stream.interrupt();
        tokio::pin!(interrupt);

        assert!(
            tokio::time::timeout(Duration::from_millis(10), &mut interrupt)
                .await
                .is_err()
        );
        assert_eq!(continuity_epoch.load(Ordering::Acquire), 1);
    }
}
