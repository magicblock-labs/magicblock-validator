use std::path::PathBuf;
use std::str::FromStr;
use tempfile::{tempdir, TempDir};

/// Resolves a tmp dir by first considering the value defined in the [env_var_override].
/// If that is not provided it will use the [tempfile] crate in order to create
/// a temporary directory which is dropped as soon as the first tuple item
/// is dropped.
pub fn resolve_tmp_dir(env_var_override: &str) -> (TempDir, PathBuf) {
    let default_tmpdir = tempdir().unwrap();
    let tmpdir_path = std::env::var(env_var_override)
        .map(|s| PathBuf::from_str(&s).unwrap())
        .unwrap_or(default_tmpdir.path().to_path_buf());
    (default_tmpdir, tmpdir_path)
}
