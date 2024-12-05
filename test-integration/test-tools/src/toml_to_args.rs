use std::{
    fs,
    path::{Path, PathBuf},
};

use serde::Deserialize;

#[derive(Deserialize)]
struct Config {
    accounts: RemoteConfig,
    #[serde(default)]
    rpc: Rpc,
    #[serde(default)]
    program: Vec<Program>,
}

#[derive(Deserialize)]
struct RemoteConfig {
    remote: String,
}

#[derive(Deserialize)]
struct Rpc {
    port: u16,
}

impl Default for Rpc {
    fn default() -> Self {
        Rpc { port: 8899 }
    }
}

#[derive(Deserialize)]
struct Program {
    id: String,
    path: String,
}

fn parse_config(config_path: &PathBuf) -> Config {
    let config_toml =
        fs::read_to_string(config_path).expect("Failed to read config file");
    toml::from_str(&config_toml).expect("Failed to parse config file")
}

#[derive(Default, PartialEq, Eq)]
pub enum ProgramLoader {
    #[default]
    UpgradeableProgram,
    BpfProgram,
}

pub fn config_to_args(
    config_path: &PathBuf,
    program_loader: Option<ProgramLoader>,
) -> Vec<String> {
    let config = parse_config(config_path);
    let program_loader = program_loader.unwrap_or_default();

    let mut args = vec![
        "--log".to_string(),
        "--rpc-port".to_string(),
        config.rpc.port.to_string(),
        "-r".to_string(),
        "--limit-ledger-size".to_string(),
        "10000".to_string(),
    ];

    let config_dir = Path::new(config_path)
        .parent()
        .expect("Failed to get parent directory of config file");

    for program in config.program {
        match program.path.as_str() {
            "<remote>" => {
                args.push("--clone".into());
                args.push(program.id);
            }
            path => {
                if program_loader == ProgramLoader::UpgradeableProgram {
                    args.push("--upgradeable-program".to_string());
                } else {
                    args.push("--bpf-program".to_string());
                }

                args.push(program.id);

                let resolved_full_config_path =
                    config_dir.join(path).canonicalize().unwrap();
                args.push(
                    resolved_full_config_path.to_str().unwrap().to_string(),
                );

                if program_loader == ProgramLoader::UpgradeableProgram {
                    args.push("none".to_string());
                }
            }
        }
    }
    args.push("--url".into());
    args.push(config.accounts.remote);

    args
}

pub fn rpc_port_from_config(config_path: &PathBuf) -> u16 {
    let config = parse_config(config_path);
    config.rpc.port
}
