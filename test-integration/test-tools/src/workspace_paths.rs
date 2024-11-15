use std::path::{Path, PathBuf};

pub fn path_relative_to_workspace(path: &str) -> String {
    let workspace_dir = resolve_workspace_dir();
    let path = Path::new(&workspace_dir).join(path);
    path.to_str().unwrap().to_string()
}

pub fn path_relative_to_manifest(path: &str) -> String {
    let manifest_dir = resolve_manifest_dir();
    let path = Path::new(&manifest_dir).join(path);
    path.to_str().unwrap().to_string()
}

pub fn resolve_manifest_dir() -> PathBuf {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    Path::new(&manifest_dir).to_path_buf()
}

pub fn resolve_workspace_dir() -> PathBuf {
    let manifest_dir = resolve_manifest_dir();
    match manifest_dir.join("..").canonicalize() {
        Ok(path) => path.to_path_buf(),
        Err(e) => panic!("Failed to resolve workspace directory: {:?}", e),
    }
}

#[derive(Debug)]
pub struct TestProgramPaths {
    pub program_path: String,
    pub program_keypair_path: String,
    pub authority_keypair_path: String,
}

impl TestProgramPaths {
    pub fn new(
        program_crate: &str,
        program_dir: &str,
        program_id: &str,
    ) -> Self {
        let program_path = path_relative_to_workspace(&format!(
            "target/deploy/{}.so",
            program_crate
        ));
        let program_keypair_path = path_relative_to_workspace(&format!(
            "programs/{}/keys/{}.json",
            program_dir, program_id
        ));
        let authority_keypair_path = path_relative_to_workspace(&format!(
            "target/deploy/{}-keypair.json",
            program_crate
        ));
        Self {
            program_path,
            program_keypair_path,
            authority_keypair_path,
        }
    }
}
