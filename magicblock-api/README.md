# magicblock-api

Provides the API used to initialize, start, and stop the validator runtime.

## Usage

`MagicValidator` is created from `ValidatorParams` (from `magicblock-config`).
The canonical config example is the workspace-level file:

- [`../config.example.toml`](../config.example.toml)

The current initialization flow is asynchronous:

1. Build `ValidatorParams` (for example via `ValidatorParams::try_new(args)`).
2. Create the runtime with `MagicValidator::try_from_config(config).await`.
3. Start services with `magic_validator.start().await`.
4. Stop gracefully with `magic_validator.stop().await`.

## Example

```rust
use magicblock_api::MagicValidator;
use magicblock_config::ValidatorParams;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = std::env::args_os();
    let config = ValidatorParams::try_new(args)?;

    let mut magic_validator = MagicValidator::try_from_config(config).await?;
    magic_validator.start().await?;

    // ... application lifecycle ...

    magic_validator.stop().await;
    Ok(())
```
