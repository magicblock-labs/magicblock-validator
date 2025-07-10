# MagicBlock Config Macro

A set a macro helpers to keep the config DRY (Don't Repeat Yourself). It contains two attributes meant to be used on struct that need to derive `serde::Deserialize` and `clap::Args`.

## `clap_prefix`

This macro will update existing `#[arg]` (or create one if needed) and add a `long`, `name`, and `env` constructed from the prefix and the name of each field in annotated struct.

Use the `#[derive_env_var]` attribute on field where you want to create the environment variable.

### Example

```rust
#[clap_prefix("rpc")]
#[derive(clap::Args)]
struct RpcConfig {
    addr: IpAddr,
    #[derive_env_var]
    port: u16,
}
```

Will become:

```rust
#[derive(clap::Args)]
struct RpcConfig {
    #[arg(long = "rpc-addr", name = "rpc-addr")]
    rpc_addr: IpAddr,
    #[arg(long = "rpc-port", name = "rpc-port", env = "RPC_PORT")]
    rpc_port: u16,
}
```

## `clap_from_serde`

This macro will look for `#[serde]` attributes in each field of the annotated struct and will output the corresponding `#[arg]` or `#[command]` attribute.

Use `#[clap_from_serde_skip]` on field where you don't want the processing.

### Example

```rust
#[clap_from_serde]
struct RpcConfig {
    #[serde(deserialize_with = "deserialize_ip_addr")]
    addr: IpAddr,
    #[serde(default = "helpers::serde_defaults::bool_true")]
    enabled: bool,
    #[clap_from_serde_skip]
    #[serde(default = "helpers::serde_default::url_none")]
    url: Option<Url>,
    #[serde(flatten)]
    config: SomeOtherConfig,
}
```

Will become:

```rust
struct RpcConfig {
    #[serde(deserialize_with = "deserialize_ip_addr")]
    #[arg(default_value_t = clap_deserialize_ip_addr)]
    addr: IpAddr,
    #[serde(default = "helpers::serde_defaults::bool_true")]
    #[arg(default_value = helpers::serde_defaults::bool_true())]
    enabled: bool,
    #[serde(default)]
    url: Option<Url>,
    #[serde(flatten)]
    #[command(flatten)]
    config: SomeOtherConfig,
}
```