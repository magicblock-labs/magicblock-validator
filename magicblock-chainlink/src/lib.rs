#![allow(clippy::result_large_err)]
pub mod accounts_bank;
pub mod chainlink;
pub mod cloner;
pub mod remote_account_provider;
pub mod submux;

use std::ops::Add;

pub use chainlink::*;
pub use magicblock_metrics::metrics::AccountFetchOrigin;
mod filters;

#[cfg(any(test, feature = "dev-context"))]
pub mod testing;

#[tokio::test]
async fn test_trtr() {
    // The issue:
    // 1. We need to clone inner ArcMutex to hold locks
    // We can't have reference as that would mean blocking outer Mutex for func duration
    // 2. When we clone we have a problem - the entry may get evicted from LruCache
    // That would lead to race conditions:
    // New thread comes in, sees no entry for pubkey, creates new ArcMutex instance
    // Other thread will overwrite it

    // Goal: we still need to cleanup space ut without race conditions

    // That means - we can't use LruCache directly
    // We need to use HashMap and clean post factum

    // When can we insert and clean?
    // Cleaning:
    // We clean after execution.

    // Alg for fetch next
    // 1. lock outer mutex
    // 2. get inner mutexes
    //      a. exists
    //          i. value == u64::MAX,  clone move to_request
    //          ii. value != u64::MAX, clone it into ready group
    //      b. absent - insert in HashMap + clone into to_request
    // 3. update LruCache. // Here some locked keys could be evicted. TODO: update or maybe not/
    // 4. drop outer mutex
    // 5. lock.await inner mutexes
    // 6. request data for uninitialized ones
    // 7. On failure
    //      a. lock outer
    //      b. TODO: ensure safety for uninitialized, check if locked
    // 8. On success - lock outer
    // 9. Update locked inner values
    // 10. compile result
    // 11. get pubkey in LRU
    //      a. exists - continue
    //      b. !exists - push
    //          i. None returned - continue
    //          ii. Some(el) - if not in use remove from hashmap

    // Problem wuth 2.b
    // Once inserted in map it could be accessed, even tho it is uninitialized
    // That would mean that we can't just remove it on failure
    // If it is cloned by someone - we need to leave it alone

    // The scenario we're trying to cover - requests exceed capacity
    // Key getting before RPC finished - means that so many values pushed that our key got evicted
    // We could insert it back into LRU or clean it upo
    // We need cleanup logic. The last active function has to cleanup
    // If it isn't in Lru & key NotInUse - cleanup
    // If it isn't in Lru & key InUse - keep, do nothing it will be cleaned up by occupying party
    // !!!WITH THIS we don't need to care of evicted keys on first outer lock,
    //      If it is in use the working thread will cleanup itself

    // Problem:
    // If we don't insert in the beggining in LRU, then if key absent in the end
    // We can't know if it was evicted or not
    // What is cleanup logic in that case?
    // If we insert at the end and Some key evicted, this key is in use
    // If we don't dispose of it the other thread won't be able

    // OPTION 1
    // Handle LRU at the END get pubkey in LRU
    //      a. exists - continue, no need to cleanup
    //      b. !exists - push
    //          i. None returned - continue, no need to cleanup
    //          ii. Evicted(el) && NotInUse - remove from hashmap
    //          iii. Evicted(el) && InUse - continue??
    // Evicted key at the end has to be cleaned up by someone
    // if ii - excess dealt with right away
    // if iii - will be dealt with by some running instance, since it will be inserted in LRU at some point

    // OPTION 2
    // Handle LRU at the BEGGINING
    //      LRU.get
    //      a. exists - continue, no need to cleanup
    //      b. !exists - push
    //          i. None returned - continue, no need to cleanup
    //          ii. Evicted(el) && NotInUse - remove from hashmap
    //          iii. Evicted(el) && InUse - continue
    // What to do at the END?
    //      LRU.peek()
    //      a. exists - continue, no need to cleanup
    //      b. !exists
    //          i. InUse - continue
    //          ii. NotInUse - cleanup

    // We need to define how LRU works
    // I come and place request
    // Not to repeat work someone may cache it so If i come again the could just give cached request
    // But while they make my order many other people came and asked other things

    use std::{
        num::NonZeroUsize,
        sync::{Arc, Mutex},
    };

    use lru::LruCache;
    use tokio::sync::{Mutex as TMutex, MutexGuard, OwnedMutexGuard};

    struct Locker<'a> {
        val: &'a i32,
        m: Arc<TMutex<u64>>,
    };

    impl<'a> Locker<'a> {
        pub fn new(val: &'a i32, m: Arc<TMutex<u64>>) -> Self {
            Self { val, m }
        }

        pub async fn lock<'s>(&'s self) -> MutexGuard<'s, u64> {
            self.m.lock().await
        }
    }

    impl<'a> Drop for Locker<'a> {
        fn drop(&mut self) {
            println!("lpol");
        }
    }

    let val = 1;
    let m = Arc::new(TMutex::new(10));
    let locker = Locker::new(&val, m);
    let mut asd = locker.lock().await;
    *asd = 1;
    assert_eq!(*asd, 1);
    drop(asd);
    drop(locker);

    println!("{}", val);
}
