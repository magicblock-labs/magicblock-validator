use std::{
    fs,
    path::{Path, PathBuf},
    str::FromStr,
};

use magicblock_config::types::{resolve_url, RemoteKind};
use serde::Deserialize;

#[derive(Deserialize)]
struct Config {
    #[serde(default)]
    remote: Vec<RemoteConfig>,
    listen: String,
    #[serde(default)]
    programs: Vec<Program>,
}

#[derive(Deserialize, Clone)]
struct RemoteConfig {
    kind: String,
    url: String,
}

impl RemoteConfig {
    /// Returns the URL for this remote, resolving aliases based on kind.
    fn url(&self) -> String {
        // Convert string kind to RemoteKind enum
        let kind = match self.kind.as_str() {
            "rpc" => RemoteKind::Rpc,
            "websocket" => RemoteKind::Websocket,
            "grpc" => RemoteKind::Grpc,
            // Default to rpc for unknown kinds
            _ => RemoteKind::Rpc,
        };
        // Use the production resolve_url function from magicblock-config
        resolve_url(kind, &self.url)
    }
}

#[derive(Deserialize)]
struct Program {
    id: String,
    path: String,
    auth: Option<String>,
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

    let port = config.listen.split(':').nth(1).unwrap_or("8899");

    let mut args = vec![
        "--log".to_string(),
        "--rpc-port".to_string(),
        port.to_string(),
        "-r".to_string(),
        "--limit-ledger-size".to_string(),
        "10000".to_string(),
    ];

    let config_dir = Path::new(config_path)
        .parent()
        .expect("Failed to get parent directory of config file");

    for program in config.programs {
        if program_loader == ProgramLoader::UpgradeableProgram {
            args.push("--upgradeable-program".to_string());
        } else {
            args.push("--bpf-program".to_string());
        }

        args.push(program.id);

        let resolved_full_config_path =
            config_dir.join(&program.path).canonicalize().unwrap();
        args.push(resolved_full_config_path.to_str().unwrap().to_string());

        if program_loader == ProgramLoader::UpgradeableProgram {
            if let Some(auth) = program.auth {
                args.push(auth);
            } else {
                args.push("none".to_string());
            }
        }
    }

    // Add the first RPC remote URL if available
    if let Some(rpc_remote) = config.remote.iter().find(|r| r.kind == "rpc") {
        args.push("--url".into());
        args.push(rpc_remote.url());
    }

    args
}

pub fn rpc_port_from_config(config_path: &PathBuf) -> u16 {
    let config = parse_config(config_path);
    config
        .listen
        .split(':')
        .nth(1)
        .and_then(|p| p.parse().ok())
        .unwrap_or(8899)
}
