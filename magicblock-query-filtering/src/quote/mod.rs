use std::sync::OnceLock;

use base64::{prelude::BASE64_STANDARD, Engine};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use sha2::{Digest, Sha256, Sha512};
use thiserror::Error;
use tokio::task::spawn_blocking;

use crate::types::{FastQuoteResponse, QuoteResponse};

mod tdx_guest_native;

pub type QuoteResult<T> = Result<T, QuoteError>;

#[derive(Debug, Error)]
pub enum QuoteError {
    #[error("challenge must decode to 64 bytes")]
    InvalidChallenge,
    #[error(transparent)]
    TdxGuest(#[from] tdx_guest_native::TdxGuestError),
    #[error(transparent)]
    Base64(#[from] base64::DecodeError),
    #[error(transparent)]
    Join(#[from] tokio::task::JoinError),
}

pub struct AttestedKeyMaterial {
    pub quote: Vec<u8>,
    pub verifying_key: VerifyingKey,
    pub signing_key: SigningKey,
    pub report_data_sha256: [u8; 32],
}

static ATTESTED: OnceLock<AttestedKeyMaterial> = OnceLock::new();

fn decode_challenge(challenge: &str) -> QuoteResult<[u8; 64]> {
    let challenge_bytes = BASE64_STANDARD.decode(challenge)?;
    challenge_bytes
        .try_into()
        .map_err(|_| QuoteError::InvalidChallenge)
}

fn init_attested_key_blocking() -> QuoteResult<AttestedKeyMaterial> {
    let signing_key = SigningKey::from_bytes(&rand::random::<[u8; 32]>());
    let verifying_key = signing_key.verifying_key();

    let mut report_data = [0u8; 64];
    let digest = Sha512::digest(verifying_key.as_bytes());
    report_data.copy_from_slice(&digest);
    let report_data_sha256: [u8; 32] = Sha256::digest(report_data).into();

    let quote = tdx_guest_native::get_tdx_quote(report_data)?;

    Ok(AttestedKeyMaterial {
        quote,
        verifying_key,
        signing_key,
        report_data_sha256,
    })
}

async fn init_attested_key() -> QuoteResult<&'static AttestedKeyMaterial> {
    if let Some(material) = ATTESTED.get() {
        return Ok(material);
    }
    if !tdx_guest_native::tdx_guest_device_present() {
        return Err(tdx_guest_native::TdxGuestError::NoDevice(
            tdx_guest_native::CONFIGFS_TSM_REPORT_PATH,
        )
        .into());
    }
    let material = spawn_blocking(init_attested_key_blocking).await??;
    Ok(ATTESTED.get_or_init(|| material))
}

pub async fn quote(challenge: &str) -> QuoteResult<QuoteResponse> {
    if !tdx_guest_native::tdx_guest_device_present() {
        return Err(tdx_guest_native::TdxGuestError::NoDevice(
            tdx_guest_native::CONFIGFS_TSM_REPORT_PATH,
        )
        .into());
    }

    let report_data = decode_challenge(challenge)?;
    let quote =
        spawn_blocking(move || tdx_guest_native::get_tdx_quote(report_data))
            .await??;

    Ok(QuoteResponse {
        quote: BASE64_STANDARD.encode(quote),
    })
}

pub async fn fast_quote(challenge: &str) -> QuoteResult<FastQuoteResponse> {
    let challenge_bytes = BASE64_STANDARD.decode(challenge)?;
    let key_material = init_attested_key().await?;
    let sig: Signature = key_material.signing_key.sign(&challenge_bytes);

    Ok(FastQuoteResponse {
        quote: BASE64_STANDARD.encode(&key_material.quote),
        report_data_sha256: BASE64_STANDARD
            .encode(key_material.report_data_sha256),
        pubkey: BASE64_STANDARD.encode(key_material.verifying_key.as_bytes()),
        challenge: challenge.to_owned(),
        signature: BASE64_STANDARD.encode(sig.to_bytes()),
    })
}
