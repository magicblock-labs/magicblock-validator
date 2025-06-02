use std::str::FromStr;

use clap::Parser;
use magicblock_api::EphemeralConfig;
use magicblock_config::ProgramConfig;
use solana_sdk::pubkey::Pubkey;

use super::{
    AccountsArgs, GeyserGrpcArgs, LedgerConfigArgs, MetricsConfigArgs, RpcArgs,
    ValidatorConfigArgs,
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
        mut config: EphemeralConfig,
    ) -> Result<EphemeralConfig, String> {
        self.accounts.merge_with_config(&mut config)?;
        self.rpc.merge_with_config(&mut config);
        self.geyser_grpc.merge_with_config(&mut config);
        self.validator.merge_with_config(&mut config);
        self.ledger.merge_with_config(&mut config);
        self.metrics.merge_with_config(&mut config);

        config.programs.extend(self.programs.clone());

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
