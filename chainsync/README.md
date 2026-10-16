# MagicBlock ChainSync

Synchronizes Solana base-chain accounts into MagicBlock Engine for local execution.
Request the accounts a transaction needs; ChainSync acquires missing state and keeps
subscribed accounts up to date. Updates are applied directly to Engine, not exposed
as a separate stream for callers to consume.

## Usage

Create one `ChainSync` for an Engine, with at least one HTTP provider and one gRPC
stream, and WebSocket capacity for the accounts you need to track. All providers
must serve the same base chain.

HTTP providers must support confirmed `getMultipleAccounts` with `base64+zstd`
encoding; WebSocket providers must support confirmed account subscriptions with
the same encoding. Yellowstone gRPC streams provide delegation lifecycle events
and delayed redundancy for WebSocket subscriptions.

```no_run
use std::{error::Error, sync::Arc, time::Duration};

use engine::Engine;
use magicblock_chainsync::{
    AccountProperty, ChainSync, ChainSyncAccount, ChainSyncConfig, GrpcStreamConfig,
    WebSocketConfig, WebSocketProvider,
};
use nucleus::shutdown::ShutdownManager;
use solana_pubkey::Pubkey;

async fn synchronize(
    engine: Engine,
    shutdown: &mut ShutdownManager,
    payer: Pubkey,
    account: Pubkey,
) -> Result<Arc<ChainSync>, Box<dyn Error>> {
    let config = ChainSyncConfig {
        aml: None,
        http: vec!["https://rpc.example.com".parse()?],
        websocket: WebSocketConfig(vec![WebSocketProvider {
            url: "wss://rpc.example.com".parse()?,
            max_connections: 4,
            subs_per_connection: 100,
        }]),
        grpc: vec![GrpcStreamConfig {
            endpoint: "https://yellowstone.example.com".parse()?,
            token: None,
            duplication_delay: Duration::from_secs(30 * 60),
        }],
    };
    let chain_sync = ChainSync::new(engine, config, shutdown)?;
    chain_sync.ensure([
        ChainSyncAccount { pubkey: payer, property: AccountProperty::Payer },
        ChainSyncAccount { pubkey: account, property: AccountProperty::Readonly },
    ])
    .await?;
    Ok(chain_sync)
}
```

Replace the example endpoints and capacity limits with your provider settings.
Construction starts background services but does not wait for providers to connect;
an immediate synchronization request can fail if no WebSocket connection is ready.
The services participate in coordinated shutdown through the supplied manager.

## Synchronization flow

`ensure` selects only accounts missing from Engine, rechecking presence under
ordered leases before passing them to `sync`. It leaves resident accounts and
their subscriptions unchanged. `ensure_transaction_accounts` classifies a
transaction's static keys before applying that same missing-only selection.

`sync` consumes selected `SyncTarget` leases, including eligible resident accounts.
The caller establishes replacement eligibility under each lease and may supply a
minimum response context slot. Both entry points use the same fetch and dependency flow;
synchronization itself has no undelegation-specific selection policy. Choose each
target's role according to its use in the transaction:

| Property | Synchronization behavior |
| --- | --- |
| `Payer` | Fetches the account and its delegation record; keeps live updates unless an accepted delegation is found. |
| `Readonly` | Fetches the account with live updates; normalizes executable images and refetches with a companion if it discovers DLP ownership or a Loader V3 program. |
| `Writable` | Checks delegation without subscribing; otherwise refetches with live updates as `Program` if executable, or `Readonly`. |
| `Program` | Acquires an executable image and tracks upgrades, including Loader V3 ProgramData. |

Acquisition proceeds in waves:

1. **Select targets.** Normal callers use `ensure`; lifecycle handling supplies an
   eligible resident lease directly to `sync`. Overlapping missing-only requests
   do not fetch accounts another request has already installed.
2. **Subscribe, then fetch.** Where the role requires live updates, WebSocket
   acknowledgement precedes HTTP acquisition. Batches contain at most 100 addresses,
   including delegation-record and ProgramData companions.
3. **Resolve the image.** Apply the branches below. Accounts needing a companion
   or live updates retain their lease through companion fetches without installing
   the initial image. Existing subscriptions remain active, and missing subscriptions
   are established before refetching. The refetch cannot use an older response context than
   the one that revealed the required acquisition role.
4. **Resolve action dependencies.** After releasing all target leases, delegations
   with post-delegation actions wait while their account and program dependencies
   go through the same acquisition flow. Nested dependencies activate before the
   delegation that needs them.

| Acquired state | Local result |
| --- | --- |
| Ordinary account, with no accepted delegation | Funded HTTP images enter in `ReadOnly` mode; zero-lamport images remain `Uninit`. |
| HTTP `null` | Default `Uninit` account at the response context slot, caching the absence. |
| Program acquisition | Supported loader layouts become a rent-funded, `ReadOnly` ELF image. Loader V3 uses ProgramData under the program's address; other loaders use the program account itself. Unsupported or invalid images fail acquisition. |
| DLP-owned account and record for this Engine's authority | Original owner and delegation slot restored; enters `Delegated` mode, with actions handled after dependencies. |
| DLP-owned account and record with the default authority | Original owner restored; enters confined `Magic` mode with zero lamports, without actions or commit authority. |
| Missing, invalid, or other-authority delegation record | Does not establish local delegation; the fetched account follows ordinary acquisition. |
| Canonical token ATA with a valid eATA delegation to this Engine | Projects the delegated balance onto the ATA, preserving its token program and extensions; enters `Delegated` mode. Without a valid projection, the base ATA follows ordinary acquisition. |
| Raw eATA | Not materialized or kept as an ordinary subscription. Streamed eATA delegations can instead update a matching ATA already present in Engine. |

Accepted delegations stop ordinary mirroring of the local account. For projected
ATAs, local closure is disabled and native-token lamports retain only the rent
reserve; the delegated token balance is not spendable as native lamports.

Both entry points return a `u64` count of HTTP account entries fetched, including
companions, action dependencies, and null results. Later-wave refetches count
again; provider retries do not. Empty inputs return zero; `ensure` also returns zero
when every requested account is resident.
Background lifecycle work and AML requests are not included.

## Keeping accounts up to date

ChainSync separates ordinary mirrors from locally authoritative accounts:

| Path | What it does |
| --- | --- |
| WebSocket account subscription | Supplies confirmed updates for acquired mirrors. |
| Delayed gRPC copy | Supplies the same account updates through one assigned stream after its duplication delay and a filter rebuild. |
| Mandatory gRPC lifecycle filters | Observe DLP-owned accounts, accepted delegation records, undelegation requests, and successful DLP transactions independently of ordinary subscriptions. Every configured stream carries these filters. |

Ordinary non-program updates become `ReadOnly` while funded and `Uninit` at zero
lamports. They do not infer delegation from ownership alone. Program updates are
normalized to ELF; a Loader V3 ProgramData update replaces the local program image,
not a separate local ProgramData account.

Only currently covered subscriptions may apply ordinary updates. Released
subscriptions, obsolete gRPC confirmations, and updates from the wrong stream are
ignored. Engine enforces lifecycle transitions and slot ordering: older images and
same-slot, same-mode duplicates are skipped; ordinary mirrors cannot overwrite a
`Delegated` or `Magic` account. Per-account background application failures are
logged, not returned to an earlier `sync` caller.

## Delegation and undelegation

**Delegation detection** uses two confirmed gRPC account updates: the funded,
DLP-owned application account and its derived delegation-record PDA. Either may
arrive first, but both must arrive on the same stream in the same slot, and the
record's delegation slot must equal that slot. Only this Engine's authority or the
default authority is accepted; unmatched observations are discarded when the
stream changes slots. HTTP acquisition validates fetched account and record images
instead and can restore an existing delegation without observing its creation.

On a matched delegation, ChainSync retires ordinary mirror coverage before
activation. A local delegation restores ownership, acquires action dependencies,
then materializes the account and executes its actions in one Engine transaction.
A confined account is materialized directly, without dependencies, actions, or
rescue. An ordinary eATA delegation activates only through a matching resident
ATA. An eATA with post-delegation actions also acquires its matching ATA through
the normal HTTP projection flow, so shuttle actions do not depend on an earlier
RPC request. Each projection retains a readonly raw eATA with eSPL ownership
restored, no actions, and no ordinary mirror subscriptions. Its balance is not
the live ER token balance, and it can be evicted independently of the projection.
Explicit raw acquisition uses the same policy; undelegated raw accounts are
ordinary subscribed mirrors.

Optional AML assessment checks distinct post-delegation action signers. Rejected
signers, invalid actions, and activation failures enter rescue undelegation unless
local state already supersedes that delegation. Assessment service failures and
dependency-acquisition failures propagate without rescue. Successful rescue
scheduling can make `sync` succeed, but does not mean the intended actions ran or
on-chain undelegation completed.

**Undelegation has two separate signals:**

| Signal | Detection and application |
| --- | --- |
| Request to commit and undelegate | A funded DLP-owned undelegation-request account at its derived PDA identifies the target. ChainSync schedules the current local account only if it is `Delegated` and the request is not older than its delegation slot. Scheduling moves it to `Transient`; repeated requests do not reschedule it. |
| Confirmed undelegation | A successful DLP transaction invokes `Undelegate` or timeout rollback, directly or through CPI. ChainSync refreshes eligible local state through Readonly acquisition at or after the event slot. It retains the current image and mode until acquisition resolves, then installs a subscribed `ReadOnly` mirror or `Uninit` absence. Delegation discovered during fetching uses the usual companion and dependency waves. Older events and still-authoritative `Delegated` and `Magic` accounts are ignored. |

If HTTP acquisition fails, ChainSync attempts to delete the eligible account after
rechecking its mode and slot under a new Engine lease. Transient provider failures
exhaust the HTTP client's retry budget first; malformed responses fail immediately.
Subscription and activation failures do not trigger this deletion fallback.

Raw-eATA requests schedule the matching delegated projection, never the readonly
raw eATA. On release, ChainSync identifies projections from the resident raw image
before refreshing it, so closure of the remote eATA does not hide that identity.
It refreshes raw and eligible projections independently through the same acquisition
flow. When raw is absent locally, its base-chain image is fetched at or after the
event slot; a missing image cannot identify those projections. Transaction-based detection
accepts ordinary `Undelegate` instructions only when their validator matches this
Engine's authority; timeout rollback detection remains authority-unfiltered.
Both paths resolve static message keys only; accounts referenced through address
lookup tables are unsupported.

## Freshness and recovery

An HTTP response's `context_slot` identifies the confirmed bank used for the read,
not the account's last modification slot. Decoded images use this slot until
delegation resolution restores a recorded delegation slot. It is not an Engine
checkpoint or a shared view across requests.

HTTP retries use alternative providers without lowering the minimum context slot.
Separate batches may observe different slots, and a failed `sync` can leave earlier
accounts materialized: synchronization is not an atomic view of the chain or an
all-or-nothing transaction.

If one ordinary update source is lost, the other can continue supplying updates.
Losing the last source removes eligible local mirrors so a later `ensure` can reacquire
them; locally authoritative accounts are preserved. Engine cache eviction also
retires subscriptions before removing an eligible mirror. Explicit unsubscribe
releases coverage but leaves local-state cleanup to its caller.

Reconnection and replay do not guarantee gapless delivery. Engine's persisted
chain slot advances from confirmed gRPC Clock observations, not from completed
account application. HTTP fetches use it as a response freshness floor, not proof that
all earlier updates have been applied. Newly opened gRPC streams request replay
from two slots before that watermark, saturating at zero; ordinary filter refreshes
do not restart replay.

For implementation entry points, see [acquisition](src/acquisition/mod.rs),
[delegation activation](src/delegation/activation.rs),
[live update handling](src/worker/mod.rs), [lifecycle handling](src/worker/lifecycle.rs),
and [gRPC delegation matching](src/grpc/matcher.rs).
