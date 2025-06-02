use std::str::FromStr;

use clap::Parser;
use magicblock_api::EphemeralConfig;
use magicblock_config::{AllowedProgram, ProgramConfig, RemoteConfig};
use solana_sdk::pubkey::Pubkey;
use url::Url;

use super::{
    AccountsArgs, GeyserGrpcArgs, LedgerConfigArgs, MetricsConfigArgs,
    RemoteConfigArg, RpcArgs, ValidatorConfigArgs,
};

#[derive(Parser, Debug)]
pub(crate) struct ConfigArgs {
    #[command(flatten)]
    pub accounts: AccountsArgs,
    #[command(flatten)]
    pub rpc: RpcArgs,
    #[command(flatten)]
    pub geyser_grpc: GeyserGrpcArgs,
    #[command(flatten)]
    pub validator: ValidatorConfigArgs,
    #[command(flatten)]
    pub ledger: LedgerConfigArgs,
    /// List of programs to add on startup. Format: <program_id>:<program_path>
    #[arg(long, value_parser = program_config_parser)]
    pub programs: Vec<ProgramConfig>,
    #[command(flatten)]
    pub metrics: MetricsConfigArgs,
}

impl ConfigArgs {
    pub fn override_config(
        &self,
        config: EphemeralConfig,
    ) -> Result<EphemeralConfig, String> {
        let mut config = config.clone();

        if let Some(remote) = self.accounts.remote {
            config.accounts.remote = match remote {
                RemoteConfigArg::Devnet => RemoteConfig::Devnet,
                RemoteConfigArg::Mainnet => RemoteConfig::Mainnet,
                RemoteConfigArg::Testnet => RemoteConfig::Testnet,
                RemoteConfigArg::Development => RemoteConfig::Development,
            };
        } else if let Some(remote_custom) = &self.accounts.remote_custom {
            let url = Url::from_str(remote_custom).map_err(|e| {
                format!("Failed to parse URL {}: {}", remote_custom, e)
            })?;

            if let Some(remote_custom_with_ws) =
                &self.accounts.remote_custom_with_ws
            {
                let url_ws =
                    Url::from_str(remote_custom_with_ws).map_err(|e| {
                        format!(
                            "Failed to parse URL {}: {}",
                            remote_custom_with_ws, e
                        )
                    })?;
                config.accounts.remote =
                    RemoteConfig::CustomWithWs(url, url_ws);
            } else {
                config.accounts.remote = RemoteConfig::Custom(url);
            }
        }

        config.accounts.lifecycle = self.accounts.lifecycle.into();
        config.accounts.commit = self.accounts.commit.into();
        config.accounts.payer = self.accounts.payer.into();
        config.accounts.allowed_programs = self
            .accounts
            .allowed_programs
            .clone()
            .into_iter()
            .map(|id| AllowedProgram {
                id: Pubkey::from_str_const(&id),
            })
            .collect();
        config.accounts.db = self.accounts.db.into();

        config.rpc = self.rpc.into();
        config.geyser_grpc = self.geyser_grpc.into();
        config.validator = self.validator.clone().into();
        config.ledger = self.ledger.clone().into();
        config.programs = self.programs.clone();
        Ok(config)
    }
}

fn program_config_parser(s: &str) -> Result<ProgramConfig, String> {
    let parts: Vec<String> =
        s.split(':').map(|part| part.to_string()).collect();
    let [id, path] = parts.as_slice() else {
        return Err(format!("Invalid program config: {}", s));
    };
    let id = Pubkey::from_str(id)
        .map_err(|e| format!("Invalid program id {}: {}", id, e))?;

    Ok(ProgramConfig {
        id,
        path: path.clone(),
    })
}
