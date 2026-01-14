# MagicBlock Configuration

A robust, layered configuration engine for the MagicBlock Validator.

This crate centralizes the loading, parsing, and validation of configuration settings from multiple sources (CLI, Environment Variables, and TOML files). It ensures type safety, semantic validity, and a strict precedence hierarchy.

## Features

- **Layered Configuration**: seamlessly merges settings from Defaults, TOML files, Environment Variables, and CLI arguments.
- **Strict Precedence**: **CLI > Environment > File > Defaults**. Explicit user intent always wins.
- **Overlay Logic**: CLI arguments act as an "overlay," modifying only the specific fields provided without resetting the rest of the configuration.
- **Type Safety**: uses `serde` for deserialization, supporting complex types like Keypairs, Enums, and URL aliases.
- **Domain Separation**: configuration is modularized into logical sections (Validator, Ledger, AccountsDB, etc.).

## Usage

The core entry point is `MagicBlockParams::try_new`. This method accepts an iterator of arguments (typically from `std::env::args_os`) and builds the final configuration struct.

```rust
use magicblock_config::ValidatorParams;
use std::ffi::OsString;

fn main() -> Result<(), figment::Error> {
    // 1. Collect CLI args
    let args = std::env::args_os();

    // 2. Load and Validate Configuration
    let config = ValidatorParams::try_new(args)?;

    println!("Validator Mode: {:?}", config.lifecycle);
    println!("Listening on: {}", config.listen);

    Ok(())
}
```

## Configuration Hierarchy

The crate resolves configuration values in the following order (highest priority first):

1.  **CLI Arguments**: Explicit flags passed at runtime (e.g., `--basefee 500`).
2.  **Environment Variables**: System environment variables (e.g., `MBV_VALIDATOR__BASEFEE`).
3.  **TOML Configuration File**: A structured file passed via `--config <PATH>`.
4.  **System Defaults**: Hardcoded safe defaults defined in the crate.

## Environment Variables

Environment variables are mapped using the `figment` provider.

- **Prefix**: `MBV_`
- **Separator**: Use a double underscore `__` to denote hierarchy (nesting).
- **Field Names**: Use `UPPER_SNAKE_CASE` for the actual field names.

### Examples

| Struct Field                   | Config Section      | Environment Variable                |
| :----------------------------- | :------------------ | :---------------------------------- |
| `validator.basefee`            | `[validator]`       | `MBV_VALIDATOR__BASEFEE`            |
| `ledger.block_time`            | `[ledger]`          | `MBV_LEDGER__BLOCK_TIME`            |
| `chain_operation.country_code` | `[chain-operation]` | `MBV_CHAIN_OPERATION__COUNTRY_CODE` |

## Configuration File

The validator supports a comprehensive TOML configuration file. You can find a fully documented example in the root of this crate.

🔗 **[View Example Configuration (magicblock.example.toml)](../config.example.toml)**

### Example Snippet

```toml
# magicblock.toml

lifecycle = "offline"
storage = "/data/ledger"

[validator]
basefee = 500
keypair = "9Vo7Tb..."

[accounts-db]
database-size = 104857600 # 100MB
block-size = "block256"
```

## Modules

The configuration is split into domain-specific structs available in `src/config/`:

- **`ValidatorConfig`**: Identity keypair, base fees.
- **`LedgerConfig`**: Block production timing, verification settings.
- **`AccountsDbConfig`**: Snapshotting, indexing, and storage size tuning.
- **`ChainOperationConfig`**: On-chain registration details (Country code, FQDN).
- **`ChainLinkConfig`**: Account cloning settings.
- **`CommitStrategy`**: Compute unit pricing for base chain commits.
- **`TaskSchedulerConfig`**: Task scheduling settings.
- **`CompressionConfig`**: Compressed accounts and interactions settings.

## Testing

This crate includes a comprehensive test suite verifying the precedence logic, overlay behavior, and failure modes.

```bash
cargo test -p magicblock-config
```
