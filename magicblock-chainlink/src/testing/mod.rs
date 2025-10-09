#[cfg(any(test, feature = "dev-context"))]
pub mod accounts;
#[cfg(any(test, feature = "dev-context"))]
pub mod chain_pubsub;
#[cfg(any(test, feature = "dev-context"))]
pub mod cloner_stub;
#[cfg(any(test, feature = "dev-context"))]
pub mod deleg;
#[cfg(any(test, feature = "dev-context"))]
pub mod rpc_client_mock;
#[cfg(any(test, feature = "dev-context"))]
pub mod utils;

#[cfg(any(test, feature = "dev-context"))]
pub use utils::init_logger;

#[macro_export]
macro_rules! assert_subscribed {
    ($provider:expr, $pubkeys:expr) => {{
        for pubkey in $pubkeys {
            assert!(
                $provider.is_watching(pubkey),
                "Expected {} to be subscribed",
                pubkey
            );
        }
    }};
}

#[macro_export]
macro_rules! assert_not_subscribed {
    ($provider:expr, $pubkeys:expr) => {{
        for pubkey in $pubkeys {
            assert!(
                !$provider.is_watching(pubkey),
                "Expected {} to not be subscribed",
                pubkey
            );
        }
    }};
}

#[macro_export]
macro_rules! assert_subscribed_without_delegation_record {
    ($provider:expr, $pubkeys:expr) => {{
        for pubkey in $pubkeys {
            let deleg_record_pubkey =
                ::dlp::pda::delegation_record_pda_from_delegated_account(&pubkey);
            assert!(
                $provider.is_watching(pubkey),
                "Expected {} to be subscribed",
                pubkey
            );
            assert!(
                !$provider.is_watching(&deleg_record_pubkey),
                "Expected {} to not be subscribed since it is a delegation record",
                deleg_record_pubkey
            );
        }
    }};
}

#[macro_export]
macro_rules! assert_subscribed_without_loaderv3_program_data_account {
    ($provider:expr, $pubkeys:expr) => {{
        for pubkey in $pubkeys {
            let program_data_account_pubkey =
                $crate::remote_account_provider::program_account::get_loaderv3_get_program_data_address(pubkey);
            assert!(
                $provider.is_watching(pubkey),
                "Expected {} to be subscribed",
                pubkey
            );
            assert!(
                !$provider.is_watching(&program_data_account_pubkey),
                "Expected {} to not be subscribed since it is a program data account",
                program_data_account_pubkey
            );
        }
    }};
}

#[macro_export]
macro_rules! assert_cloned_as_undelegated {
    ($cloner:expr, $pubkeys:expr) => {{
        for pubkey in $pubkeys {
            let account = $cloner
                .get_account(pubkey)
                .expect(&format!("Expected account {} to be cloned", pubkey));
            assert!(
                !account.delegated(),
                "Expected account {} to be undelegated",
                pubkey
            );
        }
    }};
    ($cloner:expr, $pubkeys:expr, $slot:expr) => {{
        for pubkey in $pubkeys {
            let account = $cloner
                .get_account(pubkey)
                .expect(&format!("Expected account {} to be cloned", pubkey));
            assert!(
                !account.delegated(),
                "Expected account {} to be undelegated",
                pubkey
            );
            assert_eq!(
                account.remote_slot(),
                $slot,
                "Expected account {} to have remote slot {}",
                pubkey,
                $slot
            );
        }
    }};
    ($cloner:expr, $pubkeys:expr, $slot:expr, $owner:expr) => {{
        use solana_account::ReadableAccount;
        for pubkey in $pubkeys {
            let account = $cloner
                .get_account(pubkey)
                .expect(&format!("Expected account {} to be cloned", pubkey));
            assert!(
                !account.delegated(),
                "Expected account {} to be undelegated",
                pubkey
            );
            assert_eq!(
                account.remote_slot(),
                $slot,
                "Expected account {} to have remote slot {}",
                pubkey,
                $slot
            );
            assert_eq!(
                account.owner(),
                &$owner,
                "Expected account {} to have owner {}",
                pubkey,
                $owner
            );
        }
    }};
}

#[macro_export]
macro_rules! assert_cloned_as_delegated {
    ($cloner:expr, $pubkeys:expr) => {{
        for pubkey in $pubkeys {
            let account = $cloner
                .get_account(pubkey)
                .expect(&format!("Expected account {} to be cloned", pubkey));
            assert!(
                account.delegated(),
                "Expected account {} to be delegated",
                pubkey
            );
        }
    }};
    ($cloner:expr, $pubkeys:expr, $slot:expr) => {{
        for pubkey in $pubkeys {
            let account = $cloner
                .get_account(pubkey)
                .expect(&format!("Expected account {} to be cloned", pubkey));
            assert!(
                account.delegated(),
                "Expected account {} to be delegated",
                pubkey
            );
            assert_eq!(
                account.remote_slot(),
                $slot,
                "Expected account {} to have remote slot {}",
                pubkey,
                $slot
            );
        }
    }};
    ($cloner:expr, $pubkeys:expr, $slot:expr, $owner:expr) => {{
        use solana_account::ReadableAccount;
        for pubkey in $pubkeys {
            let account = $cloner
                .get_account(pubkey)
                .expect(&format!("Expected account {} to be cloned", pubkey));
            assert!(
                account.delegated(),
                "Expected account {} to be delegated",
                pubkey
            );
            assert_eq!(
                account.remote_slot(),
                $slot,
                "Expected account {} to have remote slot {}",
                pubkey,
                $slot
            );
            assert_eq!(
                account.owner(),
                &$owner,
                "Expected account {} to have owner {}",
                pubkey,
                $owner
            );
        }
    }};
}

#[macro_export]
macro_rules! assert_not_cloned {
    ($cloner:expr, $pubkeys:expr) => {{
        for pubkey in $pubkeys {
            assert!(
                $cloner.get_account(pubkey).is_none(),
                "Expected account {} to not be cloned",
                pubkey
            );
        }
    }};
}

#[macro_export]
macro_rules! assert_cloned_as_empty_placeholder {
    ($cloner:expr, $pubkeys:expr) => {{
        use solana_account::ReadableAccount;
        for pubkey in $pubkeys {
            let account = $cloner
                .get_account(pubkey)
                .expect(&format!("Expected account {} to be cloned", pubkey));
            assert_eq!(
                account.lamports(),
                0,
                "Expected account {} to have 0 lamports",
                pubkey
            );
            assert!(
                account.data().is_empty(),
                "Expected account {} to have no data",
                pubkey
            );
            assert_eq!(
                account.owner(),
                &::solana_sdk::system_program::id(),
                "Expected account {} to be owned by system program",
                pubkey
            );
        }
    }};
    ($cloner:expr, $pubkeys:expr, $slot:expr) => {{}};
}

#[macro_export]
macro_rules! assert_remain_undelegating {
    ($cloner:expr, $pubkeys:expr, $slot:expr) => {{
        use solana_account::ReadableAccount;
        for pubkey in $pubkeys {
            let account = $cloner
                .get_account(pubkey)
                .expect(&format!("Expected account {} to be cloned", pubkey));
            assert_eq!(
                account.remote_slot(),
                $slot,
                "Expected account {} to have remote slot {}",
                pubkey,
                $slot
            );
            assert_eq!(
                account.owner(),
                &dlp::id(),
                "Expected account {} to remain owned by the delegation program",
                pubkey,
            );
        }
    }};
}

#[macro_export]
macro_rules! assert_not_found {
    ($fetch_and_clone_res:expr, $pubkeys:expr) => {{
        for pubkey in $pubkeys {
            assert!(
                $fetch_and_clone_res
                    .not_found_on_chain
                    .iter()
                    .map(|(pk, _)| pk)
                    .collect::<Vec<_>>()
                    .contains(&pubkey),
                "Expected {} to be in not_found_on_chain, got {:?}",
                pubkey,
                $fetch_and_clone_res.not_found_on_chain
            );
        }
    }};
}

// -----------------
// Loaded Programs
// -----------------
#[macro_export]
macro_rules! assert_loaded_program {
    ($cloner:expr, $program_id:expr, $auth:expr, $loader:expr, $loader_status:expr) => {{
        let loaded_program = $cloner
            .get_cloned_program($program_id)
            .expect(&format!("Expected program {} to be loaded", $program_id));
        assert_eq!(loaded_program.program_id, *$program_id);
        assert_eq!(loaded_program.authority, *$auth);
        assert_eq!(loaded_program.loader, $loader);
        assert_eq!(loaded_program.loader_status, $loader_status);
        loaded_program
    }};
}
#[macro_export]
macro_rules! assert_loaded_program_with_size {
    ($cloner:expr, $program_id:expr, $auth:expr, $loader:expr, $loader_status:expr, $size:expr) => {{
        let loaded_program = $crate::assert_loaded_program!(
            $cloner,
            $program_id,
            $auth,
            $loader,
            $loader_status
        );
        // Program size may vary a bit
        const DEVIATION: usize = 200;
        let actual_size = loaded_program.program_data.len();
        let min = $size - DEVIATION;
        let max = $size + DEVIATION;
        assert!(
            actual_size >= min && actual_size <= max,
            "Expected program {} to have size around {}, got {}",
            $program_id,
            $size,
            actual_size
        );
        loaded_program
    }};
}

#[macro_export]
macro_rules! assert_loaded_program_with_min_size {
    ($cloner:expr, $program_id:expr, $auth:expr, $loader:expr, $loader_status:expr, $size:expr) => {{
        let loaded_program = $crate::assert_loaded_program!(
            $cloner,
            $program_id,
            $auth,
            $loader,
            $loader_status
        );
        assert!(loaded_program.program_data.len() >= $size);
    }};
}
