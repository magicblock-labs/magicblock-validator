# magicblock-api

Provides a highlevel API which allows to initialize and startup all pieces of the validator.

## Usage

Provide a `MagicValidatorConfig` which includes the following:

- `validator_config: MagicBlockConfig` which indicates how the validator should be configured,
see [this default config toml](../magicblock-config/tests/fixtures/02_defaults.toml) for more
info
- `ledger: Option<Ledger>` if you want to control the ledger location, otherwise it is placed
in a temporary folder
- `init_geyser_service_config: InitGeyserServiceConfig` which can be used to control behavior
of the built in geyser plugin as well as add more plugins

Provide that config along with a `Keypair` to act as the validator's identity and authority in
order to create a `MagicValidator` instance which you can then `start`.

## Example

From Luzid which embeds this validator:

```rust
// The config is provided and Luzid provides ledger and identity keypair
// as well as adding its own plugin
config.ledger = Some(get_ledger(&app_data_dir).unwrap());
config.init_geyser_service_config.add_plugin(
    "Luzid Geyser Plugin".to_string(),
    geyser_plugin::get_plugin(
        account_update_handler,
        transaction_update_handler,
    ),
);

let magic_validator =
    MagicValidator::try_from_config(config, identity_keypair)?;
// ...
```
