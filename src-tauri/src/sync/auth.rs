use super::crypto::AuthenticatedPrincipal;
use jsonwebtoken::jwk::JwkSet;
use jsonwebtoken::{decode, decode_header, Algorithm, DecodingKey, Validation};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::RwLock;

#[derive(Clone)]
pub struct WorkOsVerifier {
    issuer: String,
    client_id: String,
    client: reqwest::Client,
    jwks: Arc<RwLock<Option<CachedJwks>>>,
    validated: Arc<RwLock<HashMap<[u8; 32], CachedPrincipal>>>,
}

struct CachedJwks {
    fetched_at: Instant,
    keys: JwkSet,
}

struct CachedPrincipal {
    cached_at: Instant,
    expires_at: u64,
    principal: AuthenticatedPrincipal,
}

impl WorkOsVerifier {
    pub fn new(issuer: String, client_id: String) -> Result<Self, AuthError> {
        if !issuer.starts_with("https://") || client_id.trim().is_empty() {
            return Err(AuthError::Configuration);
        }
        let client = reqwest::Client::builder()
            .https_only(true)
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(|_| AuthError::Configuration)?;
        Ok(Self {
            issuer: issuer.trim_end_matches('/').into(),
            client_id,
            client,
            jwks: Arc::new(RwLock::new(None)),
            validated: Arc::new(RwLock::new(HashMap::new())),
        })
    }

    pub async fn verify(&self, token: &str) -> Result<AuthenticatedPrincipal, AuthError> {
        if token.len() > 16_384 || token.matches('.').count() != 2 {
            return Err(AuthError::InvalidToken);
        }
        let digest: [u8; 32] = Sha256::digest(token.as_bytes()).into();
        let now = now_seconds();
        if let Some(cached) = self.validated.read().await.get(&digest) {
            if cached.expires_at > now && cached.cached_at.elapsed() < Duration::from_secs(5 * 60) {
                return Ok(cached.principal.clone());
            }
        }
        let header = decode_header(token).map_err(|_| AuthError::InvalidToken)?;
        if header.alg != Algorithm::RS256 {
            return Err(AuthError::InvalidToken);
        }
        let kid = header.kid.ok_or(AuthError::InvalidToken)?;
        let mut keys = self.keys(false).await?;
        let mut jwk = keys.find(&kid);
        if jwk.is_none() {
            keys = self.keys(true).await?;
            jwk = keys.find(&kid);
        }
        let key = DecodingKey::from_jwk(jwk.ok_or(AuthError::InvalidToken)?)
            .map_err(|_| AuthError::InvalidToken)?;
        let mut validation = Validation::new(Algorithm::RS256);
        validation.set_issuer(&[&self.issuer]);
        validation.set_audience(&[&self.client_id]);
        validation.set_required_spec_claims(&["exp", "iss", "sub", "aud"]);
        let claims = decode::<Claims>(token, &key, &validation)
            .map_err(|_| AuthError::InvalidToken)?
            .claims;
        if claims.sub.is_empty() {
            return Err(AuthError::InvalidToken);
        }
        let principal = AuthenticatedPrincipal {
            subject: claims.sub,
            organization_id: claims.org_id,
        };
        let mut validated = self.validated.write().await;
        validated.retain(|_, value| value.expires_at > now);
        if validated.len() >= 128 {
            validated.clear();
        }
        validated.insert(
            digest,
            CachedPrincipal {
                cached_at: Instant::now(),
                expires_at: claims.exp,
                principal: principal.clone(),
            },
        );
        Ok(principal)
    }

    async fn keys(&self, force: bool) -> Result<JwkSet, AuthError> {
        if !force {
            let cache = self.jwks.read().await;
            if let Some(cache) = cache.as_ref() {
                if cache.fetched_at.elapsed() < Duration::from_secs(15 * 60) {
                    return Ok(cache.keys.clone());
                }
            }
        }
        let endpoint = format!("{}/oauth2/jwks", self.issuer);
        let keys: JwkSet = self
            .client
            .get(endpoint)
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(|_| AuthError::JwksUnavailable)?
            .json()
            .await
            .map_err(|_| AuthError::JwksUnavailable)?;
        *self.jwks.write().await = Some(CachedJwks {
            fetched_at: Instant::now(),
            keys: keys.clone(),
        });
        Ok(keys)
    }
}

#[derive(Clone, Deserialize)]
struct Claims {
    sub: String,
    org_id: Option<String>,
    exp: u64,
}

fn now_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthError {
    Configuration,
    InvalidToken,
    JwksUnavailable,
}
