# Removed `magicblock-accounts`

`magicblock-accounts` has been removed from the workspace.

This crate used to contain account-manager and scheduled-commit glue, but that
ownership is obsolete:

- local account storage is owned by `magicblock-accounts-db`;
- account synchronization and base-layer account observation are owned by
  `magicblock-chainlink` and `magicblock-account-cloner`;
- DLP `UndelegationRequest` processing is owned by
  `magicblock-services::undelegation_request_service`;
- scheduled intent acceptance and base-layer execution are owned by
  `magicblock-committor-service`.

Do not add new code or dependencies to `magicblock-accounts`. If future work
appears to need this crate, first choose the current owner above and update this
note only if the crate is intentionally reintroduced.

For routing:

- DLP request subscription/polling/scheduling: see
  `.agents/context/crates/magicblock-services.md`;
- account cloning and delegation state: see
  `.agents/context/crates/magicblock-chainlink.md` and
  `.agents/context/crates/magicblock-account-cloner.md`;
- commit/undelegate execution and recovery: see
  `.agents/context/crates/magicblock-committor-service.md`.
