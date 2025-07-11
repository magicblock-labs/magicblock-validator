mod clap_from_serde;
mod clap_prefix;
use clap_from_serde::*;
use clap_prefix::*;
use proc_macro::TokenStream;
use quote::quote;
use syn::{parse_macro_input, visit_mut::VisitMut, ItemStruct};

/// Prefixes the fields of the annotated struct with the given prefix.
///
/// This macro is used to prefix the fields of a struct with a given prefix.
/// It is used to create a more readable CLI interface.
///
/// # Example
/// ```
/// use std::net::IpAddr;
/// use magicblock_config_macro::clap_prefix;
///  
/// #[clap_prefix("rpc")]
/// #[derive(serde::Deserialize, serde::Serialize, clap::Args)]
/// struct RpcConfig {
///     addr: IpAddr,
///     #[derive_env_var]
///     port: u16,
/// }
/// ```
///
/// Will become:
/// ```
/// use std::net::IpAddr;
///
/// #[derive(serde::Deserialize, serde::Serialize, clap::Args)]
/// struct RpcConfig {
///     #[arg(long = "rpc-addr", name = "rpc-addr")]
///     rpc_addr: IpAddr,
///     #[arg(long = "rpc-port", name = "rpc-port", env = "RPC_PORT")]
///     rpc_port: u16,
/// }
/// ```
#[proc_macro_attribute]
pub fn clap_prefix(attr: TokenStream, item: TokenStream) -> TokenStream {
    let mut input = parse_macro_input!(item as ItemStruct);
    let mut compile_errors = Vec::new();

    let mut clap_prefix = ClapPrefix::new(attr);
    clap_prefix.visit_fields_mut(&mut input.fields);
    compile_errors.extend(clap_prefix.compile_errors);

    let errors = quote! { #(#compile_errors)* };
    quote! {
        #errors
        #input
    }
    .into()
}

/// Converts serde attributes on the field of the annotated struct into clap attributes.
///
/// For each function "deserialize_XXX" passed as `deserialize_with`, a function
/// `fn clap_deserialize_XXX(&str) -> Result<T, String>` must be created!.
///
/// # Example
/// ```
/// use std::net::IpAddr;
/// use serde::{Deserialize, Deserializer};
/// use clap::Args;
/// use magicblock_config_macro::clap_from_serde;
///
/// fn clap_deserialize_addr(s: &str) -> Result<::std::net::IpAddr, String> {
///     s.parse().map_err(|err| format!("Invalid IP address: {err}"))
/// }
///
/// fn deserialize_addr<'de, D>(
///     deserializer: D,
/// ) -> Result<::std::net::IpAddr, D::Error>
/// where
///     D: ::serde::Deserializer<'de>,
/// {
///     let s =
///         <String as ::serde::Deserialize>::deserialize(deserializer)?;
///     s.parse().map_err(serde::de::Error::custom)
/// }
///
/// fn bool_true() -> bool {
///     true
/// }
///
/// #[derive(serde::Deserialize, serde::Serialize, clap::Args)]
/// struct SomeOtherConfig {
///     #[serde(deserialize_with = "deserialize_addr")]
///     inner_addr: IpAddr,
/// }
///
/// #[clap_from_serde]
/// #[derive(serde::Deserialize, serde::Serialize, clap::Args)]
/// struct RpcConfig {
///     #[serde(deserialize_with = "deserialize_addr")]
///     addr: IpAddr,
///     #[serde(default = "bool_true")]
///     enabled: bool,
///     #[serde(flatten)]
///     config: SomeOtherConfig,
/// }
/// ```
///
/// Will become:
/// ```
/// use std::net::IpAddr;
/// use serde::{Deserialize, Deserializer};
///
/// fn clap_deserialize_addr(s: &str) -> Result<::std::net::IpAddr, String> {
///     s.parse().map_err(|err| format!("Invalid IP address: {err}"))
/// }
///
/// fn deserialize_addr<'de, D>(
///     deserializer: D,
/// ) -> Result<::std::net::IpAddr, D::Error>
/// where
///     D: ::serde::Deserializer<'de>,
/// {
///     let s =
///         <String as ::serde::Deserialize>::deserialize(deserializer)?;
///     s.parse().map_err(serde::de::Error::custom)
/// }
///
/// fn bool_true() -> bool {
///     true
/// }
///
/// #[derive(serde::Deserialize, serde::Serialize, clap::Args)]
/// struct SomeOtherConfig {
///     #[serde(deserialize_with = "deserialize_addr")]
///     inner_addr: IpAddr,
/// }
///
/// #[derive(serde::Deserialize, serde::Serialize, clap::Args)]
/// struct RpcConfig {
///     #[serde(deserialize_with = "deserialize_addr")]
///     #[arg(value_parser = clap_deserialize_addr)]
///     addr: IpAddr,
///     #[serde(default = "bool_true")]
///     #[arg(default_value_t = bool_true())]
///     enabled: bool,
///     #[serde(flatten)]
///     #[command(flatten)]
///     config: SomeOtherConfig,
/// }
/// ```
#[proc_macro_attribute]
pub fn clap_from_serde(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let mut input = parse_macro_input!(item as ItemStruct);

    let mut clap_from_serde = ClapFromSerde::new();
    clap_from_serde.visit_fields_mut(&mut input.fields);

    quote! {
        #input
    }
    .into()
}
