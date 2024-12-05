use std::{fs, path::Path};

pub fn remove_directory_contents_if_exists(
    dir: &Path,
) -> Result<(), std::io::Error> {
    if !dir.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        if entry.metadata()?.is_dir() {
            fs::remove_dir_all(entry.path())?
        } else {
            fs::remove_file(entry.path())?
        }
    }
    Ok(())
}
