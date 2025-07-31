# MagicBlock Config Macro

A set of macro helpers to keep the config DRY (Don't Repeat Yourself). It contains two attributes meant to be used on struct that need to derive `serde::Deserialize` and `clap::Args`.

## Tests

If the implementation changes, `tests/fixtures/*.expanded.rs` files must be removed. Re-running tests will regenerate them with the implementation changes included.

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
    #[serde(default = "helpers::serde_defaults::url_none")]
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

## `derive(Mergeable)`

This macro will implement the `magicblock_config_helpers::Merge` trait for annotated struct. 

### Example

```rust
use magicblock_config_macro::Mergeable;

#[derive(Default, Mergeable)]
struct SomeOtherConfig {
    field1: u32,
    field2: String,
}

#[derive(Default, Mergeable)]
struct MyConfig {
    field1: u32,
    field2: SomeOtherConfig,
}
```

Will become:
```rust
use magicblock_config_helpers::Merge;

#[derive(Default)]
struct SomeOtherConfig {
    field1: u32,
    field2: String,
}

impl Merge for SomeOtherConfig {
    fn merge(&mut self, other: Self) {
        let default = Self::default();
        if self.field1 == default.field1 {
            self.field1 = other.field1;
        }
        if self.field2 == default.field2 {
            self.field2 = other.field2;
        }
    }
}

#[derive(Default)]
struct MyConfig {
    field1: u32,
    field2: SomeOtherConfig,
}

impl Merge for MyConfig {
    fn merge(&mut self, other: Self) {
        let default = Self::default();
        if self.field1 == default.field1 {
            self.field1 = other.field1;
        }
        self.field2.merge(other.field2);
    }
}
```