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
