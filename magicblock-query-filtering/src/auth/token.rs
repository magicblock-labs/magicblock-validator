use chrono::{Duration, Utc};
use jsonwebtoken::{
    decode, encode, errors::Error as JwtError, DecodingKey, EncodingKey,
    Header, Validation,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Claims {
    pub pubkey: String,
    pub exp: usize,
    pub iat: usize,
}

#[derive(Clone)]
pub struct AuthTokenGenerator {
    encoding_key: EncodingKey,
    decoding_key: DecodingKey,
    pub token_expiry_days: i64,
}

impl AuthTokenGenerator {
    /// Creates a new AuthTokenGenerator.
    ///
    /// # Arguments
    /// * `jwt_secret` - The JWT secret to use for encoding and decoding tokens
    /// * `token_expiry_days` - The number of days the token will be valid for
    ///
    /// # Returns
    /// The new AuthTokenGenerator
    pub fn new(jwt_secret: &str, token_expiry_days: i64) -> Self {
        Self {
            encoding_key: EncodingKey::from_secret(jwt_secret.as_bytes()),
            decoding_key: DecodingKey::from_secret(jwt_secret.as_bytes()),
            token_expiry_days,
        }
    }

    /// Generates a JWT token for the given pubkey.
    ///
    /// # Arguments
    /// * `pubkey` - The pubkey to generate a token for
    ///
    /// # Returns
    /// The generated JWT token
    pub fn generate(&self, pubkey: &str) -> Result<String, JwtError> {
        let now = Utc::now();
        let exp = now + Duration::days(self.token_expiry_days);
        let claims = Claims {
            pubkey: pubkey.to_owned(),
            exp: exp.timestamp() as usize,
            iat: now.timestamp() as usize,
        };

        encode(&Header::default(), &claims, &self.encoding_key)
    }

    /// Verifies a JWT token and returns the claims.
    ///
    /// # Arguments
    /// * `token` - The JWT token to verify
    ///
    /// # Returns
    /// The claims if the token is valid
    pub fn verify(&self, token: &str) -> Result<Claims, JwtError> {
        Ok(
            decode::<Claims>(
                token,
                &self.decoding_key,
                &Validation::default(),
            )?
            .claims,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_and_verify() {
        let generator = AuthTokenGenerator::new("test-secret", 30);
        let token = generator.generate("test-pubkey").unwrap();
        let claims = generator.verify(&token).unwrap();
        assert_eq!(claims.pubkey, "test-pubkey");
    }

    #[test]
    fn test_verify_invalid_token() {
        let generator = AuthTokenGenerator::new("test-secret", 30);
        let token = "invalid-token";
        let result = generator.verify(token);
        assert!(result.is_err());
    }

    #[test]
    fn test_verify_expired_token() {
        let generator = AuthTokenGenerator::new("test-secret", 30);
        let token = generator.generate("test-pubkey").unwrap();
        let claims = generator.verify(&token).unwrap();
        assert_eq!(claims.pubkey, "test-pubkey");
    }

    #[test]
    fn test_verify_invalid_secret() {
        let generator = AuthTokenGenerator::new("valid-secret", 30);
        let wrong_generator = AuthTokenGenerator::new("invalid-secret", 30);
        let token = generator.generate("test-pubkey").unwrap();
        let result = wrong_generator.verify(&token);
        assert!(result.is_err());
    }

    #[test]
    fn test_verify_invalid_expiry() {
        let generator = AuthTokenGenerator::new("test-secret", 30);
        let token = generator.generate("test-pubkey").unwrap();
        let claims = generator.verify(&token).unwrap();
        assert_eq!(claims.pubkey, "test-pubkey");
    }
}
