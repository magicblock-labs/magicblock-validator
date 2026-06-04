use std::{
    collections::{HashMap, HashSet},
    future::Future,
    sync::Arc,
    time::{Duration, Instant},
};

use solana_pubkey::Pubkey;
use tokio::sync::{watch, Mutex};

use crate::error::{TableManiaError, TableManiaResult};

const REMOTE_READINESS_SUCCESS_TTL: Duration = Duration::from_millis(750);

#[derive(Debug, Clone)]
pub(crate) struct MatchingTableReadiness {
    pub(crate) local_keys: HashSet<Pubkey>,
    pub(crate) latest_update_sent_at: Option<Instant>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RemoteReadinessTarget {
    pub(crate) wall_clock_deadline: Option<Instant>,
}

pub(crate) type RemoteReadinessAddresses = HashMap<Pubkey, Vec<Pubkey>>;
type SharedRemoteReadinessResult =
    Arc<Result<RemoteReadinessAddresses, Arc<TableManiaError>>>;
type RemoteReadinessReceiver =
    watch::Receiver<Option<SharedRemoteReadinessResult>>;

#[derive(Debug, Clone, PartialEq, Eq)]
struct RemoteReadinessTableRequirement {
    table_address: Pubkey,
    required_pubkeys: Vec<Pubkey>,
    latest_update_sent_at: Option<Instant>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RemoteReadinessWaiterKey {
    tables: Vec<RemoteReadinessTableRequirement>,
}

struct ActiveRemoteReadinessWaiter {
    key: RemoteReadinessWaiterKey,
    receiver: RemoteReadinessReceiver,
}

struct CachedRemoteReadinessResult {
    key: RemoteReadinessWaiterKey,
    remote_tables: RemoteReadinessAddresses,
    expires_at: Instant,
}

#[derive(Default)]
struct RemoteReadinessWaiterState {
    active: Vec<ActiveRemoteReadinessWaiter>,
    successful: Vec<CachedRemoteReadinessResult>,
}

#[derive(Default)]
pub(crate) struct RemoteReadinessWaiters {
    state: Mutex<RemoteReadinessWaiterState>,
}

impl RemoteReadinessWaiterKey {
    fn new(matching_tables: &HashMap<Pubkey, MatchingTableReadiness>) -> Self {
        let mut tables = matching_tables
            .iter()
            .map(|(table_address, readiness)| {
                let mut required_pubkeys =
                    readiness.local_keys.iter().copied().collect::<Vec<_>>();
                required_pubkeys.sort_unstable();
                RemoteReadinessTableRequirement {
                    table_address: *table_address,
                    required_pubkeys,
                    latest_update_sent_at: readiness.latest_update_sent_at,
                }
            })
            .collect::<Vec<_>>();
        tables.sort_unstable_by_key(|requirement| requirement.table_address);
        Self { tables }
    }

    fn is_satisfied_by(&self, other: &Self) -> bool {
        self.tables.iter().all(|requirement| {
            other
                .tables
                .iter()
                .find(|other_requirement| {
                    other_requirement.table_address == requirement.table_address
                })
                .is_some_and(|other_requirement| {
                    requirement.required_pubkeys.iter().all(|pk| {
                        other_requirement.required_pubkeys.contains(pk)
                    }) && match requirement.latest_update_sent_at {
                        Some(required_update) => {
                            other_requirement.latest_update_sent_at.is_some_and(
                                |other_update| other_update >= required_update,
                            )
                        }
                        None => true,
                    }
                })
        })
    }
}

impl RemoteReadinessWaiterState {
    fn prune_expired_successes(&mut self, now: Instant) {
        self.successful.retain(|result| result.expires_at > now);
    }

    fn find_success(
        &self,
        key: &RemoteReadinessWaiterKey,
    ) -> Option<RemoteReadinessAddresses> {
        self.successful
            .iter()
            .find(|result| key.is_satisfied_by(&result.key))
            .map(|result| result.remote_tables.clone())
    }

    fn find_active(
        &mut self,
        key: &RemoteReadinessWaiterKey,
    ) -> Option<RemoteReadinessReceiver> {
        let mut index = 0;
        while index < self.active.len() {
            let waiter = &self.active[index];
            if !Self::receiver_is_live_or_ready(&waiter.receiver) {
                self.active.remove(index);
                continue;
            }
            if key.is_satisfied_by(&waiter.key) {
                return Some(waiter.receiver.clone());
            }
            index += 1;
        }
        None
    }

    fn receiver_is_live_or_ready(receiver: &RemoteReadinessReceiver) -> bool {
        receiver.borrow().is_some() || receiver.has_changed().is_ok()
    }
}

impl RemoteReadinessWaiters {
    pub(crate) async fn wait_or_spawn<F, Fut>(
        self: &Arc<Self>,
        matching_tables: &HashMap<Pubkey, MatchingTableReadiness>,
        wait_for_remote_table_match: Duration,
        spawn_future: F,
    ) -> TableManiaResult<RemoteReadinessAddresses>
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = TableManiaResult<RemoteReadinessAddresses>>
            + Send
            + 'static,
    {
        let caller_deadline = Instant::now() + wait_for_remote_table_match;
        let key = RemoteReadinessWaiterKey::new(matching_tables);
        let receiver = {
            let mut state = self.state.lock().await;
            state.prune_expired_successes(Instant::now());
            if let Some(remote_tables) = state.find_success(&key) {
                return Ok(remote_tables);
            }
            if let Some(receiver) = state.find_active(&key) {
                receiver
            } else {
                let (sender, receiver) = watch::channel(None);
                state.active.push(ActiveRemoteReadinessWaiter {
                    key: key.clone(),
                    receiver: receiver.clone(),
                });
                let waiters = self.clone();
                tokio::spawn(async move {
                    let result =
                        Arc::new(spawn_future().await.map_err(Arc::new));
                    {
                        let mut state = waiters.state.lock().await;
                        state.active.retain(|waiter| waiter.key != key);
                        if let Ok(remote_tables) = result.as_ref() {
                            state.successful.push(
                                CachedRemoteReadinessResult {
                                    key,
                                    remote_tables: remote_tables.clone(),
                                    expires_at: Instant::now()
                                        + REMOTE_READINESS_SUCCESS_TTL,
                                },
                            );
                        }
                    }
                    let _ = sender.send(Some(result));
                });
                receiver
            }
        };

        Self::wait_for_result_until(receiver, caller_deadline).await
    }

    async fn wait_for_result_until(
        receiver: RemoteReadinessReceiver,
        caller_deadline: Instant,
    ) -> TableManiaResult<RemoteReadinessAddresses> {
        match tokio::time::timeout_at(
            tokio::time::Instant::from_std(caller_deadline),
            Self::wait_for_result(receiver),
        )
        .await
        {
            Ok(result) => result,
            Err(_) => {
                Err(TableManiaError::TimedOutWaitingForRemoteTablesToUpdate(
                    "shared remote readiness waiter timed out".to_string(),
                ))
            }
        }
    }

    async fn wait_for_result(
        mut receiver: RemoteReadinessReceiver,
    ) -> TableManiaResult<RemoteReadinessAddresses> {
        loop {
            if let Some(result) = receiver.borrow().as_ref() {
                return match result.as_ref() {
                    Ok(remote_tables) => Ok(remote_tables.clone()),
                    Err(err) => {
                        Err(TableManiaError::SharedRemoteReadinessFailure(
                            err.clone(),
                        ))
                    }
                };
            }
            if receiver.changed().await.is_err() {
                return Err(TableManiaError::SharedRemoteReadinessFailure(
                    Arc::new(
                        TableManiaError::TimedOutWaitingForRemoteTablesToUpdate(
                            "remote readiness waiter closed before result"
                                .to_string(),
                        ),
                    ),
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tokio::time::sleep;

    use super::*;

    fn readiness(
        pubkeys: impl IntoIterator<Item = Pubkey>,
        latest_update_sent_at: Option<Instant>,
    ) -> MatchingTableReadiness {
        MatchingTableReadiness {
            local_keys: pubkeys.into_iter().collect(),
            latest_update_sent_at,
        }
    }

    fn key_for(
        entries: impl IntoIterator<Item = (Pubkey, Vec<Pubkey>, Option<Instant>)>,
    ) -> RemoteReadinessWaiterKey {
        let matching_tables = entries
            .into_iter()
            .map(|(table, pubkeys, latest_update_sent_at)| {
                (table, readiness(pubkeys, latest_update_sent_at))
            })
            .collect::<HashMap<_, _>>();
        RemoteReadinessWaiterKey::new(&matching_tables)
    }

    fn matching_tables_for(
        entries: impl IntoIterator<Item = (Pubkey, Vec<Pubkey>, Option<Instant>)>,
    ) -> HashMap<Pubkey, MatchingTableReadiness> {
        entries
            .into_iter()
            .map(|(table, pubkeys, latest_update_sent_at)| {
                (table, readiness(pubkeys, latest_update_sent_at))
            })
            .collect()
    }

    #[test]
    fn remote_readiness_key_sorts_tables_and_pubkeys() {
        let table_a = Pubkey::new_unique();
        let table_b = Pubkey::new_unique();
        let pk_a = Pubkey::new_unique();
        let pk_b = Pubkey::new_unique();
        let pk_c = Pubkey::new_unique();

        let key = key_for([
            (table_b, vec![pk_c], None),
            (table_a, vec![pk_b, pk_a], None),
        ]);

        let mut expected_tables = vec![table_a, table_b];
        expected_tables.sort_unstable();
        assert_eq!(
            key.tables
                .iter()
                .map(|requirement| requirement.table_address)
                .collect::<Vec<_>>(),
            expected_tables
        );
        for requirement in key.tables {
            let mut sorted_pubkeys = requirement.required_pubkeys.clone();
            sorted_pubkeys.sort_unstable();
            assert_eq!(requirement.required_pubkeys, sorted_pubkeys);
        }
    }

    #[test]
    fn remote_readiness_key_accepts_compatible_superset() {
        let table = Pubkey::new_unique();
        let pk_a = Pubkey::new_unique();
        let pk_b = Pubkey::new_unique();
        let base = Instant::now();
        let subset = key_for([(table, vec![pk_a], Some(base))]);
        let superset = key_for([(
            table,
            vec![pk_a, pk_b],
            Some(base + Duration::from_millis(1)),
        )]);

        assert!(subset.is_satisfied_by(&superset));
    }

    #[test]
    fn remote_readiness_key_rejects_missing_table() {
        let requested =
            key_for([(Pubkey::new_unique(), vec![Pubkey::new_unique()], None)]);
        let other =
            key_for([(Pubkey::new_unique(), vec![Pubkey::new_unique()], None)]);

        assert!(!requested.is_satisfied_by(&other));
    }

    #[test]
    fn remote_readiness_key_rejects_missing_pubkey() {
        let table = Pubkey::new_unique();
        let pk_a = Pubkey::new_unique();
        let pk_b = Pubkey::new_unique();
        let requested = key_for([(table, vec![pk_a, pk_b], None)]);
        let other = key_for([(table, vec![pk_a], None)]);

        assert!(!requested.is_satisfied_by(&other));
    }

    #[test]
    fn remote_readiness_key_rejects_older_update() {
        let table = Pubkey::new_unique();
        let pk = Pubkey::new_unique();
        let base = Instant::now();
        let requested = key_for([(table, vec![pk], Some(base))]);
        let older =
            key_for([(table, vec![pk], Some(base - Duration::from_millis(1)))]);

        assert!(!requested.is_satisfied_by(&older));
    }

    #[tokio::test]
    async fn remote_readiness_waiters_share_active_waiter() {
        let waiters = Arc::new(RemoteReadinessWaiters::default());
        let table = Pubkey::new_unique();
        let pk = Pubkey::new_unique();
        let matching_tables = matching_tables_for([(table, vec![pk], None)]);
        let calls = Arc::new(AtomicUsize::new(0));

        let first_waiters = waiters.clone();
        let first_matching_tables = matching_tables.clone();
        let first_calls = calls.clone();
        let first = tokio::spawn(async move {
            first_waiters
                .wait_or_spawn(
                    &first_matching_tables,
                    Duration::from_secs(1),
                    move || async move {
                        first_calls.fetch_add(1, Ordering::SeqCst);
                        sleep(Duration::from_millis(50)).await;
                        Ok(HashMap::from([(table, vec![pk])]))
                    },
                )
                .await
        });

        sleep(Duration::from_millis(10)).await;
        let second = waiters
            .wait_or_spawn(&matching_tables, Duration::from_secs(1), || async {
                panic!("compatible active waiter should be reused")
            })
            .await
            .unwrap();
        let first = first.await.unwrap().unwrap();

        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(first, second);
    }

    #[tokio::test]
    async fn remote_readiness_waiters_reuse_success_cache() {
        let waiters = Arc::new(RemoteReadinessWaiters::default());
        let table = Pubkey::new_unique();
        let pk = Pubkey::new_unique();
        let matching_tables = matching_tables_for([(table, vec![pk], None)]);
        let calls = Arc::new(AtomicUsize::new(0));

        let first_calls = calls.clone();
        waiters
            .wait_or_spawn(
                &matching_tables,
                Duration::from_secs(1),
                move || async move {
                    first_calls.fetch_add(1, Ordering::SeqCst);
                    Ok(HashMap::from([(table, vec![pk])]))
                },
            )
            .await
            .unwrap();
        waiters
            .wait_or_spawn(&matching_tables, Duration::from_secs(1), || async {
                panic!("compatible success should be reused")
            })
            .await
            .unwrap();

        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn remote_readiness_waiters_do_not_cache_failure() {
        let waiters = Arc::new(RemoteReadinessWaiters::default());
        let table = Pubkey::new_unique();
        let pk = Pubkey::new_unique();
        let matching_tables = matching_tables_for([(table, vec![pk], None)]);
        let calls = Arc::new(AtomicUsize::new(0));

        let first_calls = calls.clone();
        let first = waiters
            .wait_or_spawn(
                &matching_tables,
                Duration::from_secs(1),
                move || async move {
                    first_calls.fetch_add(1, Ordering::SeqCst);
                    Err(
                        TableManiaError::TimedOutWaitingForRemoteTablesToUpdate(
                            "expected failure".to_string(),
                        ),
                    )
                },
            )
            .await;
        assert!(matches!(
            first,
            Err(TableManiaError::SharedRemoteReadinessFailure(_))
        ));

        let second_calls = calls.clone();
        waiters
            .wait_or_spawn(
                &matching_tables,
                Duration::from_secs(1),
                move || async move {
                    second_calls.fetch_add(1, Ordering::SeqCst);
                    Ok(HashMap::from([(table, vec![pk])]))
                },
            )
            .await
            .unwrap();

        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn remote_readiness_waiters_share_identical_batch_requests() {
        let waiters = Arc::new(RemoteReadinessWaiters::default());
        let table = Pubkey::new_unique();
        let pk = Pubkey::new_unique();
        let matching_tables = matching_tables_for([(table, vec![pk], None)]);
        let calls = Arc::new(AtomicUsize::new(0));

        let first_waiters = waiters.clone();
        let first_matching_tables = matching_tables.clone();
        let first_calls = calls.clone();
        let first = tokio::spawn(async move {
            first_waiters
                .wait_or_spawn(
                    &first_matching_tables,
                    Duration::from_secs(1),
                    move || async move {
                        first_calls.fetch_add(1, Ordering::SeqCst);
                        sleep(Duration::from_millis(50)).await;
                        Ok(HashMap::from([(table, vec![pk])]))
                    },
                )
                .await
        });

        sleep(Duration::from_millis(10)).await;
        let second = waiters
            .wait_or_spawn(&matching_tables, Duration::from_secs(1), || async {
                panic!("identical active waiter should be reused")
            })
            .await
            .unwrap();
        let first = first.await.unwrap().unwrap();

        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(first, second);
    }

    #[tokio::test]
    async fn remote_readiness_waiters_share_subset_batch_requests() {
        let waiters = Arc::new(RemoteReadinessWaiters::default());
        let table = Pubkey::new_unique();
        let pk_a = Pubkey::new_unique();
        let pk_b = Pubkey::new_unique();
        let superset = matching_tables_for([(table, vec![pk_a, pk_b], None)]);
        let subset = matching_tables_for([(table, vec![pk_a], None)]);
        let calls = Arc::new(AtomicUsize::new(0));

        let first_waiters = waiters.clone();
        let first_calls = calls.clone();
        let first = tokio::spawn(async move {
            first_waiters
                .wait_or_spawn(
                    &superset,
                    Duration::from_secs(1),
                    move || async move {
                        first_calls.fetch_add(1, Ordering::SeqCst);
                        sleep(Duration::from_millis(50)).await;
                        Ok(HashMap::from([(table, vec![pk_a, pk_b])]))
                    },
                )
                .await
        });

        sleep(Duration::from_millis(10)).await;
        let second = waiters
            .wait_or_spawn(&subset, Duration::from_secs(1), || async {
                panic!("subset request should reuse compatible superset")
            })
            .await
            .unwrap();
        let first = first.await.unwrap().unwrap();

        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(first, second);
    }

    #[tokio::test]
    async fn remote_readiness_waiters_do_not_share_disjoint_pubkeys() {
        let waiters = Arc::new(RemoteReadinessWaiters::default());
        let table = Pubkey::new_unique();
        let pk_a = Pubkey::new_unique();
        let pk_b = Pubkey::new_unique();
        let key_a = matching_tables_for([(table, vec![pk_a], None)]);
        let key_b = matching_tables_for([(table, vec![pk_b], None)]);
        let calls = Arc::new(AtomicUsize::new(0));

        let first_waiters = waiters.clone();
        let first_calls = calls.clone();
        let first = tokio::spawn(async move {
            first_waiters
                .wait_or_spawn(
                    &key_a,
                    Duration::from_secs(1),
                    move || async move {
                        first_calls.fetch_add(1, Ordering::SeqCst);
                        sleep(Duration::from_millis(50)).await;
                        Ok(HashMap::from([(table, vec![pk_a])]))
                    },
                )
                .await
        });

        sleep(Duration::from_millis(10)).await;
        let second_calls = calls.clone();
        let second = waiters
            .wait_or_spawn(&key_b, Duration::from_secs(1), move || async move {
                second_calls.fetch_add(1, Ordering::SeqCst);
                Ok(HashMap::from([(table, vec![pk_b])]))
            })
            .await
            .unwrap();
        let first = first.await.unwrap().unwrap();

        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(first, HashMap::from([(table, vec![pk_a])]));
        assert_eq!(second, HashMap::from([(table, vec![pk_b])]));
    }

    #[tokio::test]
    async fn remote_readiness_waiters_share_only_true_table_supersets() {
        let waiters = Arc::new(RemoteReadinessWaiters::default());
        let table_a = Pubkey::new_unique();
        let table_b = Pubkey::new_unique();
        let pk_a = Pubkey::new_unique();
        let pk_b = Pubkey::new_unique();
        let superset = matching_tables_for([
            (table_a, vec![pk_a], None),
            (table_b, vec![pk_b], None),
        ]);
        let subset = matching_tables_for([(table_a, vec![pk_a], None)]);
        let incompatible = matching_tables_for([(table_b, vec![pk_a], None)]);
        let calls = Arc::new(AtomicUsize::new(0));

        let first_waiters = waiters.clone();
        let first_calls = calls.clone();
        let first = tokio::spawn(async move {
            first_waiters
                .wait_or_spawn(
                    &superset,
                    Duration::from_secs(1),
                    move || async move {
                        first_calls.fetch_add(1, Ordering::SeqCst);
                        sleep(Duration::from_millis(50)).await;
                        Ok(HashMap::from([
                            (table_a, vec![pk_a]),
                            (table_b, vec![pk_b]),
                        ]))
                    },
                )
                .await
        });

        sleep(Duration::from_millis(10)).await;
        waiters
            .wait_or_spawn(&subset, Duration::from_secs(1), || async {
                panic!("table subset should reuse table superset")
            })
            .await
            .unwrap();

        let incompatible_calls = calls.clone();
        waiters
            .wait_or_spawn(
                &incompatible,
                Duration::from_secs(1),
                move || async move {
                    incompatible_calls.fetch_add(1, Ordering::SeqCst);
                    Ok(HashMap::from([(table_b, vec![pk_a])]))
                },
            )
            .await
            .unwrap();
        first.await.unwrap().unwrap();

        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn remote_readiness_waiters_expire_success_cache_after_ttl() {
        let waiters = Arc::new(RemoteReadinessWaiters::default());
        let table = Pubkey::new_unique();
        let pk = Pubkey::new_unique();
        let matching_tables = matching_tables_for([(table, vec![pk], None)]);
        let calls = Arc::new(AtomicUsize::new(0));

        let first_calls = calls.clone();
        waiters
            .wait_or_spawn(
                &matching_tables,
                Duration::from_secs(1),
                move || async move {
                    first_calls.fetch_add(1, Ordering::SeqCst);
                    Ok(HashMap::from([(table, vec![pk])]))
                },
            )
            .await
            .unwrap();

        sleep(REMOTE_READINESS_SUCCESS_TTL + Duration::from_millis(25)).await;

        let second_calls = calls.clone();
        waiters
            .wait_or_spawn(
                &matching_tables,
                Duration::from_secs(1),
                move || async move {
                    second_calls.fetch_add(1, Ordering::SeqCst);
                    Ok(HashMap::from([(table, vec![pk])]))
                },
            )
            .await
            .unwrap();

        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn remote_readiness_waiters_prune_closed_active_waiters() {
        let waiters = Arc::new(RemoteReadinessWaiters::default());
        let table = Pubkey::new_unique();
        let pk = Pubkey::new_unique();
        let matching_tables = matching_tables_for([(table, vec![pk], None)]);
        let key = RemoteReadinessWaiterKey::new(&matching_tables);
        let (_sender, receiver) = watch::channel(None);
        drop(_sender);
        waiters
            .state
            .lock()
            .await
            .active
            .push(ActiveRemoteReadinessWaiter { key, receiver });
        let calls = Arc::new(AtomicUsize::new(0));

        let new_calls = calls.clone();
        let remote_tables = waiters
            .wait_or_spawn(
                &matching_tables,
                Duration::from_secs(1),
                move || async move {
                    new_calls.fetch_add(1, Ordering::SeqCst);
                    Ok(HashMap::from([(table, vec![pk])]))
                },
            )
            .await
            .unwrap();

        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(remote_tables, HashMap::from([(table, vec![pk])]));
        assert!(waiters.state.lock().await.active.is_empty());
    }

    #[tokio::test]
    async fn remote_readiness_waiters_do_not_cancel_when_awaiter_drops() {
        let waiters = Arc::new(RemoteReadinessWaiters::default());
        let table = Pubkey::new_unique();
        let pk = Pubkey::new_unique();
        let matching_tables = matching_tables_for([(table, vec![pk], None)]);
        let calls = Arc::new(AtomicUsize::new(0));

        let first_waiters = waiters.clone();
        let first_matching_tables = matching_tables.clone();
        let first_calls = calls.clone();
        let first = tokio::spawn(async move {
            first_waiters
                .wait_or_spawn(
                    &first_matching_tables,
                    Duration::from_secs(1),
                    move || async move {
                        first_calls.fetch_add(1, Ordering::SeqCst);
                        sleep(Duration::from_millis(50)).await;
                        Ok(HashMap::from([(table, vec![pk])]))
                    },
                )
                .await
        });

        sleep(Duration::from_millis(10)).await;
        first.abort();

        let second = waiters
            .wait_or_spawn(&matching_tables, Duration::from_secs(1), || async {
                panic!("shared task should still be active after awaiter drop")
            })
            .await
            .unwrap();

        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(second, HashMap::from([(table, vec![pk])]));
    }
}
