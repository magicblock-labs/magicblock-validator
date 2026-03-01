use std::{
    fs,
    path::{Path, PathBuf},
};

use serde::Deserialize;

#[derive(Deserialize)]
struct Config {
    #[serde(default)]
    remotes: Vec<String>,
    #[serde(default)]
    aperture: Option<ApertureConfig>,
    #[serde(default)]
    programs: Vec<Program>,
}

#[derive(Deserialize)]
struct ApertureConfig {
    listen: String,
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

fn extract_port_from_listen(listen: &str) -> &str {
    listen.split(':').nth(1).unwrap_or("8899")
}

pub fn config_to_args(
    config_path: &PathBuf,
    program_loader: Option<ProgramLoader>,
) -> Vec<String> {
    let config = parse_config(config_path);
    let program_loader = program_loader.unwrap_or_default();

    let listen = config
        .aperture
        .as_ref()
        .map(|a| a.listen.as_str())
        .unwrap_or("127.0.0.1:8899");
    let port = extract_port_from_listen(listen);

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

        let full_path_to_resolve = config_dir.join(&program.path);
        match fs::canonicalize(&full_path_to_resolve) {
            Ok(path) => {
                args.push(path.to_str().unwrap().to_string());
            }
            Err(e) => {
                let abs_config_dir = fs::canonicalize(config_dir).unwrap_or(config_dir.to_path_buf());
                eprintln!(
                    "Error: Failed to resolve program path.\n\
                     Config Dir: {:?}\n\
                     Relative Path: {:?}\n\
                     Resolution Attempt: {:?}\n\
                     OS Error: {:?}",
                    abs_config_dir,
                    program.path,
                    full_path_to_resolve,
                    e
                );
                
                // List directory contents to aid debugging in CI environments
                if let Some(parent) = full_path_to_resolve.parent() {
                    eprintln!("Directory contents of {:?}:", parent);
                    if let Ok(entries) = fs::read_dir(parent) {
                        for entry in entries.flatten() {
                            eprintln!(" - {:?}", entry.file_name());
                        }
                    } else {
                        eprintln!(" (Unable to read directory)");
                    }
                }
                
                panic!("Program file not found: {:?}", full_path_to_resolve);
            }
        }

        if program_loader == ProgramLoader::UpgradeableProgram {
            if let Some(auth) = program.auth {
                args.push(auth);
            } else {
                args.push("none".to_string());
            }
        }
    }

    // Add the first HTTP/HTTPS remote URL if available
    if let Some(http_remote) =
        config.remotes.iter().find(|r| r.starts_with("http"))
    {
        args.push("--url".into());
        args.push(http_remote.clone());
    }

    args
}

pub fn rpc_port_from_config(config_path: &PathBuf) -> u16 {
    let config = parse_config(config_path);
    let listen = config
        .aperture
        .as_ref()
        .map(|a| a.listen.as_str())
        .unwrap_or("127.0.0.1:8899");
    extract_port_from_listen(listen).parse().unwrap_or(8899)
}
