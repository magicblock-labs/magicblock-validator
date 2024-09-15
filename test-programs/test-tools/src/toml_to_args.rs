use serde::Deserialize;
use std::{fs, path::Path};

#[derive(Deserialize)]
struct Config {
    rpc: Rpc,
    program: Vec<Program>,
}

#[derive(Deserialize)]
struct Rpc {
    port: u16,
}

#[derive(Deserialize)]
struct Program {
    id: String,
    path: String,
}

pub fn config_to_args(config_path: &str) -> Vec<String> {
    let config_file =
        fs::read_to_string(config_path).expect("Failed to read config file");
    let config: Config =
        toml::from_str(&config_file).expect("Failed to parse config file");

    let mut args = vec![
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
        args.push("--bpf-program".to_string());
        args.push(program.id);

        let resolved_full_config_path =
            config_dir.join(program.path).canonicalize().unwrap();
        args.push(resolved_full_config_path.to_str().unwrap().to_string());
    }

    args
}
