use std::{str::FromStr, sync::Arc};

use chrono::{Duration, Utc};
use magicblock_aml::{RiskError, RiskService};
use solana_pubkey::Pubkey;
use solana_signature::Signature;
use thiserror::Error;

use crate::{
    auth::token::AuthTokenGenerator,
    types::{LoginRequest, LoginResponse},
};

pub type AuthResult<T> = Result<T, AuthError>;

/// Configuration for the authentication layer of the query filtering service.
#[derive(Debug, Clone)]
pub struct AuthConfig {
    pub jwt_secret: String,
    pub token_expiry_days: i64,
    pub challenge_ttl_seconds: i64,
}

#[derive(Debug, Error)]
pub enum AuthError {
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    ParsePubkey(#[from] solana_pubkey::ParsePubkeyError),
    #[error(transparent)]
    ParseSignature(#[from] solana_signature::ParseSignatureError),
    #[error(transparent)]
    Jwt(#[from] jsonwebtoken::errors::Error),
    #[error("signature verification failed")]
    SignatureVerification,
    #[error("challenge expired")]
    ChallengeExpired,
    #[error("invalid challenge date")]
    InvalidChallengeDate,
    #[error("invalid challenge format")]
    InvalidChallengeFormat,
    #[error("invalid challenge user pubkey")]
    InvalidChallengeUserPubkey,
    #[error(transparent)]
    Risk(#[from] RiskError),
    #[error("token expired")]
    TokenExpired,
}

pub struct AuthService {
    risk: Option<Arc<RiskService>>,
    challenge_ttl_seconds: i64,
    token_generator: AuthTokenGenerator,
}

impl AuthService {
    pub fn new(
        jwt_secret: String,
        token_expiry_days: i64,
        challenge_ttl_seconds: i64,
        risk: Option<Arc<RiskService>>,
    ) -> Self {
        Self {
            risk,
            challenge_ttl_seconds,
            token_generator: AuthTokenGenerator::new(
                jwt_secret,
                token_expiry_days,
            ),
        }
    }

    pub fn generate_challenge(&self, user_pubkey: &str) -> String {
        format!(
            "Login to Query Filtering Service\nTimestamp: {}\nUser: {}",
            Utc::now().timestamp(),
            user_pubkey
        )
    }

    pub fn verify_challenge(
        &self,
        challenge: &str,
        user_pubkey: &str,
    ) -> AuthResult<()> {
        let lines: Vec<&str> = challenge.lines().collect();
        if lines.len() != 3
            || lines[0] != "Login to Query Filtering Service"
            || !lines[1].starts_with("Timestamp: ")
            || !lines[2].starts_with("User: ")
        {
            return Err(AuthError::InvalidChallengeFormat);
        }

        if &lines[2][6..] != user_pubkey {
            return Err(AuthError::InvalidChallengeUserPubkey);
        }

        let timestamp = lines[1][11..]
            .parse::<i64>()
            .map_err(|_| AuthError::InvalidChallengeDate)?;
        let challenge_time = chrono::DateTime::from_timestamp(timestamp, 0)
            .ok_or(AuthError::InvalidChallengeDate)?;
        let now = Utc::now();

        if now.signed_duration_since(challenge_time)
            > Duration::seconds(self.challenge_ttl_seconds)
        {
            return Err(AuthError::ChallengeExpired);
        }

        Ok(())
    }

    pub fn verify_signature(&self, request: &LoginRequest) -> AuthResult<()> {
        let pubkey = Pubkey::from_str(&request.pubkey)?;
        let signature = Signature::from_str(&request.signature)?;

        self.verify_challenge(&request.challenge, &request.pubkey)?;
        if !signature.verify(pubkey.as_ref(), request.challenge.as_bytes()) {
            return Err(AuthError::SignatureVerification);
        }

        Ok(())
    }

    pub async fn login(
        &self,
        request: LoginRequest,
    ) -> AuthResult<LoginResponse> {
        self.verify_signature(&request)?;
        if let Some(risk) = &self.risk {
            risk.check_addresses(vec![request.pubkey.clone()]).await?;
        }
        let token = self.token_generator.generate(&request.pubkey)?;
        Ok(LoginResponse { token })
    }

    pub fn verify_token(&self, token: &str) -> AuthResult<String> {
        let now = Utc::now();
        let claims = self.token_generator.verify(token)?;

        Ok(claims.pubkey)
    }
}

#[cfg(test)]
mod tests {
    use solana_keypair::Keypair;
    use solana_signer::Signer;

    use super::*;

    #[tokio::test]
    async fn login_issues_verifiable_token() {
        let auth = AuthService::new("test-secret".to_owned(), 30, 60, None);
        let keypair = Keypair::new();
        let pubkey = keypair.pubkey().to_string();
        let challenge = auth.generate_challenge(&pubkey);
        let signature = keypair.sign_message(challenge.as_bytes()).to_string();

        let response = auth
            .login(LoginRequest {
                pubkey: pubkey.clone(),
                signature,
                challenge,
            })
            .await
            .unwrap();
        let claims = auth.token_generator.verify(&response.token).unwrap();

        assert_eq!(claims.pubkey, pubkey);
    }

    #[tokio::test]
    async fn login_rejects_challenge_for_other_pubkey() {
        let auth = AuthService::new("test-secret".to_owned(), 30, 60, None);
        let signer = Keypair::new();
        let other = Keypair::new().pubkey().to_string();
        let challenge = auth.generate_challenge(&other);
        let signature = signer.sign_message(challenge.as_bytes()).to_string();

        let err = auth
            .login(LoginRequest {
                pubkey: signer.pubkey().to_string(),
                signature,
                challenge,
            })
            .await
            .unwrap_err();

        assert!(matches!(err, AuthError::InvalidChallengeUserPubkey));
    }
}
