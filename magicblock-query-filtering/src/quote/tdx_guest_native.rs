use std::{
    fs::{self, File},
    io::{self, ErrorKind, Read, Write},
    path::{Path, PathBuf},
};

use sha2::{Digest, Sha256};
use thiserror::Error;

pub const CONFIGFS_TSM_REPORT_PATH: &str = "/sys/kernel/config/tsm/report";

#[derive(Error, Debug)]
pub enum TdxGuestError {
    #[error(
        "TSM report interface missing ({0}); need kernel configfs-tsm and tdx_guest provider"
    )]
    NoDevice(&'static str),
    #[error("TDX quote: {0}")]
    Quote(String),
    #[error(transparent)]
    Io(#[from] io::Error),
}

pub fn tdx_guest_device_present() -> bool {
    Path::new(CONFIGFS_TSM_REPORT_PATH).is_dir()
}

fn quote_dir_name(input: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input);
    let hash = hasher.finalize();
    hash.iter().map(|b| format!("{:02x}", b)).collect()
}

fn trim_newline(s: &mut String) {
    if s.ends_with('\n') {
        s.pop();
        if s.ends_with('\r') {
            s.pop();
        }
    }
}

fn read_generation(quote_path: &Path) -> Result<u32, TdxGuestError> {
    let mut s = fs::read_to_string(quote_path.join("generation"))?;
    trim_newline(&mut s);
    s.parse()
        .map_err(|_| TdxGuestError::Quote("invalid generation counter".into()))
}

pub fn get_tdx_quote(report_data: [u8; 64]) -> Result<Vec<u8>, TdxGuestError> {
    let base = Path::new(CONFIGFS_TSM_REPORT_PATH);
    if !base.is_dir() {
        return Err(TdxGuestError::NoDevice(CONFIGFS_TSM_REPORT_PATH));
    }

    let name = quote_dir_name(&report_data);
    let quote_path: PathBuf = base.join(&name);

    fs::create_dir(&quote_path).or_else(|e| {
        if e.kind() == ErrorKind::AlreadyExists {
            Ok(())
        } else {
            Err(e)
        }
    })?;

    let mut provider = fs::read_to_string(quote_path.join("provider"))?;
    trim_newline(&mut provider);
    if provider != "tdx_guest" {
        return Err(TdxGuestError::Quote(format!(
            "TSM provider is {provider:?}, expected tdx_guest"
        )));
    }

    let mut expected_generation = read_generation(&quote_path)?;

    let mut inblob = File::create(quote_path.join("inblob"))?;
    inblob.write_all(&report_data)?;
    inblob.flush()?;
    drop(inblob);

    expected_generation = expected_generation
        .checked_add(1)
        .ok_or_else(|| TdxGuestError::Quote("generation overflow".into()))?;

    let mut out = Vec::new();
    File::open(quote_path.join("outblob"))?.read_to_end(&mut out)?;

    let actual = read_generation(&quote_path)?;
    if actual != expected_generation {
        return Err(TdxGuestError::Quote(format!(
            "quote generation mismatch: expected {expected_generation}, got {actual}"
        )));
    }

    if out.is_empty() {
        return Err(TdxGuestError::Quote(
            "empty quote (check QGS/tdx-qgs and permissions)".into(),
        ));
    }

    Ok(out)
}
