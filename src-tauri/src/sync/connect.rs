use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use ed25519_dalek::{Signature as Ed25519Signature, Signer, SigningKey, VerifyingKey};
use jsonwebtoken::jwk::{AlgorithmParameters, EllipticCurve, Jwk, ThumbprintHash};
use jsonwebtoken::{decode, decode_header, Algorithm, DecodingKey, Validation};
use p256::ecdsa::{Signature as P256Signature, SigningKey as P256SigningKey};
use rand_core::{OsRng, RngCore};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use subtle::ConstantTimeEq;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use url::Url;
use uuid::Uuid;

use super::crypto::AuthenticatedPrincipal;

const KEYCHAIN_SERVICE: &str = "app.clippy.desktop.sync";
const BOOTSTRAP_TTL_SECONDS: u64 = 120;
const ACCESS_TOKEN_TTL_SECONDS: u64 = 60 * 60;
const WEBSOCKET_TICKET_TTL_SECONDS: u64 = 5 * 60;
const DPOP_CLOCK_SKEW_SECONDS: i64 = 5 * 60;
const MAX_PROOF_BYTES: usize = 16 * 1024;

#[derive(Clone)]
pub struct EnvironmentIdentity {
    signing: SigningKey,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OkpPublicJwk {
    pub kty: String,
    pub crv: String,
    pub x: String,
}

impl EnvironmentIdentity {
    pub fn load_or_create(environment: &str) -> Result<Self, ConnectError> {
        require_environment(environment)?;
        let account = format!("connect:{environment}:environment-signing-key");
        let signing = match keychain_get(&account)? {
            Some(value) => {
                let bytes: [u8; 32] = value
                    .try_into()
                    .map_err(|_| ConnectError::InvalidKeyMaterial)?;
                SigningKey::from_bytes(&bytes)
            }
            None => {
                let signing = SigningKey::generate(&mut OsRng);
                keychain_set(&account, signing.as_bytes())?;
                signing
            }
        };
        Ok(Self { signing })
    }

    #[cfg(test)]
    fn from_bytes(bytes: [u8; 32]) -> Self {
        Self {
            signing: SigningKey::from_bytes(&bytes),
        }
    }

    pub fn public_jwk(&self) -> OkpPublicJwk {
        OkpPublicJwk {
            kty: "OKP".into(),
            crv: "Ed25519".into(),
            x: URL_SAFE_NO_PAD.encode(self.signing.verifying_key().as_bytes()),
        }
    }

    pub fn sign_canonical<T: Serialize>(&self, value: &T) -> Result<String, ConnectError> {
        let encoded = serde_jcs::to_vec(value).map_err(|_| ConnectError::InvalidEncoding)?;
        Ok(URL_SAFE_NO_PAD.encode(self.signing.sign(&encoded).to_bytes()))
    }

    pub fn verify_canonical<T: Serialize>(
        jwk: &OkpPublicJwk,
        value: &T,
        signature: &str,
    ) -> Result<(), ConnectError> {
        let key = verifying_key(jwk)?;
        let signature = decode_fixed::<64>(signature)?;
        let signature = Ed25519Signature::from_bytes(&signature);
        let encoded = serde_jcs::to_vec(value).map_err(|_| ConnectError::InvalidEncoding)?;
        key.verify_strict(&encoded, &signature)
            .map_err(|_| ConnectError::InvalidSignature)
    }
}

fn verifying_key(jwk: &OkpPublicJwk) -> Result<VerifyingKey, ConnectError> {
    if jwk.kty != "OKP" || jwk.crv != "Ed25519" {
        return Err(ConnectError::InvalidKeyMaterial);
    }
    VerifyingKey::from_bytes(&decode_fixed::<32>(&jwk.x)?)
        .map_err(|_| ConnectError::InvalidKeyMaterial)
}

#[derive(Clone)]
pub struct DpopKey {
    signing: P256SigningKey,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EcPublicJwk {
    pub kty: String,
    pub crv: String,
    pub x: String,
    pub y: String,
}

#[derive(Serialize)]
struct DpopHeader<'a> {
    typ: &'static str,
    alg: &'static str,
    jwk: &'a EcPublicJwk,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DpopClaims {
    htm: String,
    htu: String,
    iat: i64,
    jti: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ath: Option<String>,
}

impl DpopKey {
    pub fn load_or_create(environment: &str, label: &str) -> Result<Self, ConnectError> {
        require_environment(environment)?;
        if label.is_empty()
            || !label
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            return Err(ConnectError::InvalidKeyMaterial);
        }
        let account = format!("connect:{environment}:dpop:{label}");
        let signing = match keychain_get(&account)? {
            Some(value) => {
                let bytes: [u8; 32] = value
                    .try_into()
                    .map_err(|_| ConnectError::InvalidKeyMaterial)?;
                P256SigningKey::from_bytes((&bytes).into())
                    .map_err(|_| ConnectError::InvalidKeyMaterial)?
            }
            None => {
                let signing = P256SigningKey::random(&mut OsRng);
                keychain_set(&account, signing.to_bytes().as_slice())?;
                signing
            }
        };
        Ok(Self { signing })
    }

    #[cfg(test)]
    fn random() -> Self {
        Self {
            signing: P256SigningKey::random(&mut OsRng),
        }
    }

    pub fn public_jwk(&self) -> EcPublicJwk {
        let point = self.signing.verifying_key().to_encoded_point(false);
        EcPublicJwk {
            kty: "EC".into(),
            crv: "P-256".into(),
            x: URL_SAFE_NO_PAD.encode(point.x().expect("P-256 x coordinate")),
            y: URL_SAFE_NO_PAD.encode(point.y().expect("P-256 y coordinate")),
        }
    }

    pub fn thumbprint(&self) -> Result<String, ConnectError> {
        let jwk: Jwk = serde_json::from_value(
            serde_json::to_value(self.public_jwk()).map_err(|_| ConnectError::InvalidEncoding)?,
        )
        .map_err(|_| ConnectError::InvalidKeyMaterial)?;
        Ok(jwk.thumbprint(ThumbprintHash::SHA256))
    }

    pub fn proof(
        &self,
        method: &str,
        url: &str,
        access_token: Option<&str>,
    ) -> Result<String, ConnectError> {
        let htu = normalize_htu(url)?;
        let method = normalize_method(method)?;
        let claims = DpopClaims {
            htm: method,
            htu,
            iat: now_seconds() as i64,
            jti: Uuid::new_v4().to_string(),
            ath: access_token.map(token_hash),
        };
        let header = DpopHeader {
            typ: "dpop+jwt",
            alg: "ES256",
            jwk: &self.public_jwk(),
        };
        let encoded_header = URL_SAFE_NO_PAD
            .encode(serde_json::to_vec(&header).map_err(|_| ConnectError::InvalidEncoding)?);
        let encoded_claims = URL_SAFE_NO_PAD
            .encode(serde_json::to_vec(&claims).map_err(|_| ConnectError::InvalidEncoding)?);
        let signing_input = format!("{encoded_header}.{encoded_claims}");
        let signature: P256Signature = self.signing.sign(signing_input.as_bytes());
        Ok(format!(
            "{signing_input}.{}",
            URL_SAFE_NO_PAD.encode(signature.to_bytes())
        ))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedDpop {
    pub thumbprint: String,
    pub jti: String,
}

pub fn verify_dpop(
    proof: &str,
    method: &str,
    url: &str,
    access_token: Option<&str>,
) -> Result<VerifiedDpop, ConnectError> {
    if proof.len() > MAX_PROOF_BYTES || proof.matches('.').count() != 2 {
        return Err(ConnectError::InvalidDpopProof);
    }
    let header = decode_header(proof).map_err(|_| ConnectError::InvalidDpopProof)?;
    if header.alg != Algorithm::ES256 || header.typ.as_deref() != Some("dpop+jwt") {
        return Err(ConnectError::InvalidDpopProof);
    }
    let jwk = header.jwk.ok_or(ConnectError::InvalidDpopProof)?;
    match &jwk.algorithm {
        AlgorithmParameters::EllipticCurve(parameters)
            if parameters.curve == EllipticCurve::P256 => {}
        _ => return Err(ConnectError::InvalidDpopProof),
    }
    let key = DecodingKey::from_jwk(&jwk).map_err(|_| ConnectError::InvalidDpopProof)?;
    let mut validation = Validation::new(Algorithm::ES256);
    validation.required_spec_claims.clear();
    validation.validate_exp = false;
    validation.validate_aud = false;
    let claims = decode::<DpopClaims>(proof, &key, &validation)
        .map_err(|_| ConnectError::InvalidDpopProof)?
        .claims;
    if claims.htm != normalize_method(method)? || claims.htu != normalize_htu(url)? {
        return Err(ConnectError::InvalidDpopProof);
    }
    let now = now_seconds() as i64;
    if claims.iat < now - DPOP_CLOCK_SKEW_SECONDS || claims.iat > now + DPOP_CLOCK_SKEW_SECONDS {
        return Err(ConnectError::InvalidDpopProof);
    }
    if claims.jti.len() > 128 || Uuid::parse_str(&claims.jti).is_err() {
        return Err(ConnectError::InvalidDpopProof);
    }
    match (access_token, claims.ath.as_deref()) {
        (Some(token), Some(ath)) if secure_eq(ath.as_bytes(), token_hash(token).as_bytes()) => {}
        (None, None) => {}
        _ => return Err(ConnectError::InvalidDpopProof),
    }
    Ok(VerifiedDpop {
        thumbprint: jwk.thumbprint(ThumbprintHash::SHA256),
        jti: claims.jti,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelayMintClaims {
    pub iss: String,
    pub aud: String,
    pub sub: String,
    pub org_id: Option<String>,
    pub environment_id: String,
    pub endpoint: RelayEndpoint,
    pub client_jkt: String,
    pub client_nonce: String,
    pub generation: u64,
    pub jti: String,
    pub iat: u64,
    pub exp: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelayEndpoint {
    pub http_base_url: String,
    pub ws_base_url: String,
}

#[derive(Clone)]
pub struct RelayProofVerifier {
    issuer: String,
    environment_id: String,
    key: DecodingKey,
}

impl RelayProofVerifier {
    pub fn new(
        issuer: String,
        environment_id: String,
        public_jwk: &str,
    ) -> Result<Self, ConnectError> {
        let issuer = normalize_https_origin(&issuer)?;
        let jwk: Jwk =
            serde_json::from_str(public_jwk).map_err(|_| ConnectError::InvalidKeyMaterial)?;
        match &jwk.algorithm {
            AlgorithmParameters::OctetKeyPair(parameters)
                if parameters.curve == EllipticCurve::Ed25519 => {}
            _ => return Err(ConnectError::InvalidKeyMaterial),
        }
        let key = DecodingKey::from_jwk(&jwk).map_err(|_| ConnectError::InvalidKeyMaterial)?;
        Ok(Self {
            issuer,
            environment_id,
            key,
        })
    }

    pub fn verify(
        &self,
        proof: &str,
        owner: &AuthenticatedPrincipal,
        endpoint: &RelayEndpoint,
    ) -> Result<RelayMintClaims, ConnectError> {
        if proof.len() > MAX_PROOF_BYTES || proof.matches('.').count() != 2 {
            return Err(ConnectError::InvalidRelayProof);
        }
        let header = decode_header(proof).map_err(|_| ConnectError::InvalidRelayProof)?;
        if header.alg != Algorithm::EdDSA {
            return Err(ConnectError::InvalidRelayProof);
        }
        let audience = format!("clippy-env:{}", self.environment_id);
        let mut validation = Validation::new(Algorithm::EdDSA);
        validation.set_issuer(&[&self.issuer]);
        validation.set_audience(&[&audience]);
        validation.set_required_spec_claims(&["exp", "iat", "iss", "aud", "sub"]);
        let claims = decode::<RelayMintClaims>(proof, &self.key, &validation)
            .map_err(|_| ConnectError::InvalidRelayProof)?
            .claims;
        let claimed_endpoint = normalize_relay_endpoint(&claims.endpoint)
            .map_err(|_| ConnectError::InvalidRelayProof)?;
        let expected_endpoint =
            normalize_relay_endpoint(endpoint).map_err(|_| ConnectError::InvalidRelayProof)?;
        if claims.environment_id != self.environment_id
            || claimed_endpoint != expected_endpoint
            || !principal_matches(
                owner,
                &AuthenticatedPrincipal {
                    subject: claims.sub.clone(),
                    organization_id: claims.org_id.clone(),
                },
            )
            || decode_fixed::<32>(&claims.client_jkt).is_err()
            || decode_fixed::<32>(&claims.client_nonce).is_err()
            || Uuid::parse_str(&claims.jti).is_err()
            || claims.generation == 0
        {
            return Err(ConnectError::InvalidRelayProof);
        }
        Ok(claims)
    }
}

#[derive(Clone)]
pub struct ConnectSessionStore {
    state: Arc<Mutex<SessionState>>,
}

#[derive(Default)]
struct SessionState {
    bootstraps: HashMap<[u8; 32], BootstrapRecord>,
    access_tokens: HashMap<[u8; 32], AccessRecord>,
    websocket_tickets: HashMap<[u8; 32], TicketRecord>,
    relay_jtis: HashMap<String, u64>,
    dpop_jtis: HashMap<String, u64>,
}

#[derive(Clone)]
struct BootstrapRecord {
    expires_at: u64,
    principal: AuthenticatedPrincipal,
    client_jkt: String,
}

#[derive(Clone)]
struct AccessRecord {
    expires_at: u64,
    principal: AuthenticatedPrincipal,
    client_jkt: String,
}

#[derive(Clone)]
struct TicketRecord {
    expires_at: u64,
    principal: AuthenticatedPrincipal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MintResponse {
    pub environment_id: String,
    pub bootstrap_credential: String,
    pub expires_at: String,
    pub client_jkt: String,
    pub client_nonce: String,
    pub signature: String,
}

#[derive(Serialize)]
struct MintResponseUnsigned<'a> {
    environment_id: &'a str,
    bootstrap_credential: &'a str,
    expires_at: &'a str,
    client_jkt: &'a str,
    client_nonce: &'a str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenResponse {
    pub access_token: String,
    pub token_type: String,
    pub expires_in: u64,
    pub scope: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebsocketTicketResponse {
    pub ws_ticket: String,
    pub expires_in: u64,
}

impl ConnectSessionStore {
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(SessionState::default())),
        }
    }

    pub fn mint_from_relay(
        &self,
        proof: &str,
        verifier: &RelayProofVerifier,
        identity: &EnvironmentIdentity,
        owner: &AuthenticatedPrincipal,
        endpoint: &RelayEndpoint,
    ) -> Result<MintResponse, ConnectError> {
        let claims = verifier.verify(proof, owner, endpoint)?;
        let now = now_seconds();
        let mut state = self.state.lock().map_err(|_| ConnectError::Busy)?;
        prune(&mut state, now);
        if state.relay_jtis.contains_key(&claims.jti) {
            return Err(ConnectError::Replay);
        }
        state.relay_jtis.insert(claims.jti, claims.exp);
        let bootstrap_credential = random_token();
        let expires_at_seconds = now.saturating_add(BOOTSTRAP_TTL_SECONDS);
        let expires_at = format_timestamp(expires_at_seconds)?;
        state.bootstraps.insert(
            token_digest(&bootstrap_credential),
            BootstrapRecord {
                expires_at: expires_at_seconds,
                principal: owner.clone(),
                client_jkt: claims.client_jkt.clone(),
            },
        );
        let unsigned = MintResponseUnsigned {
            environment_id: &claims.environment_id,
            bootstrap_credential: &bootstrap_credential,
            expires_at: &expires_at,
            client_jkt: &claims.client_jkt,
            client_nonce: &claims.client_nonce,
        };
        let signature = identity.sign_canonical(&unsigned)?;
        Ok(MintResponse {
            environment_id: claims.environment_id,
            bootstrap_credential,
            expires_at,
            client_jkt: claims.client_jkt,
            client_nonce: claims.client_nonce,
            signature,
        })
    }

    pub fn exchange_bootstrap(
        &self,
        credential: &str,
        proof: &str,
        method: &str,
        url: &str,
    ) -> Result<(TokenResponse, AuthenticatedPrincipal), ConnectError> {
        if !valid_random_token(credential) {
            return Err(ConnectError::InvalidCredential);
        }
        let now = now_seconds();
        let digest = token_digest(credential);
        let mut state = self.state.lock().map_err(|_| ConnectError::Busy)?;
        prune(&mut state, now);
        let record = state
            .bootstraps
            .get(&digest)
            .cloned()
            .ok_or(ConnectError::InvalidCredential)?;
        let verified = verify_dpop(proof, method, url, Some(credential))?;
        consume_dpop_jti(&mut state, &verified.jti, now)?;
        if !secure_eq(verified.thumbprint.as_bytes(), record.client_jkt.as_bytes()) {
            return Err(ConnectError::InvalidDpopProof);
        }
        state.bootstraps.remove(&digest);
        let access_token = random_token();
        state.access_tokens.insert(
            token_digest(&access_token),
            AccessRecord {
                expires_at: now.saturating_add(ACCESS_TOKEN_TTL_SECONDS),
                principal: record.principal.clone(),
                client_jkt: record.client_jkt,
            },
        );
        Ok((
            TokenResponse {
                access_token,
                token_type: "DPoP".into(),
                expires_in: ACCESS_TOKEN_TTL_SECONDS,
                scope: "sync:read sync:write files:read files:write".into(),
            },
            record.principal,
        ))
    }

    pub fn authorize(
        &self,
        access_token: &str,
        proof: &str,
        method: &str,
        url: &str,
    ) -> Result<AuthenticatedPrincipal, ConnectError> {
        if !valid_random_token(access_token) {
            return Err(ConnectError::InvalidCredential);
        }
        let now = now_seconds();
        let mut state = self.state.lock().map_err(|_| ConnectError::Busy)?;
        prune(&mut state, now);
        let record = state
            .access_tokens
            .get(&token_digest(access_token))
            .cloned()
            .ok_or(ConnectError::InvalidCredential)?;
        let verified = verify_dpop(proof, method, url, Some(access_token))?;
        consume_dpop_jti(&mut state, &verified.jti, now)?;
        if !secure_eq(verified.thumbprint.as_bytes(), record.client_jkt.as_bytes()) {
            return Err(ConnectError::InvalidDpopProof);
        }
        Ok(record.principal)
    }

    pub fn issue_websocket_ticket(
        &self,
        access_token: &str,
        proof: &str,
        method: &str,
        url: &str,
    ) -> Result<(WebsocketTicketResponse, AuthenticatedPrincipal), ConnectError> {
        let principal = self.authorize(access_token, proof, method, url)?;
        let ticket = random_token();
        let mut state = self.state.lock().map_err(|_| ConnectError::Busy)?;
        state.websocket_tickets.insert(
            token_digest(&ticket),
            TicketRecord {
                expires_at: now_seconds().saturating_add(WEBSOCKET_TICKET_TTL_SECONDS),
                principal: principal.clone(),
            },
        );
        Ok((
            WebsocketTicketResponse {
                ws_ticket: ticket,
                expires_in: WEBSOCKET_TICKET_TTL_SECONDS,
            },
            principal,
        ))
    }

    pub fn consume_websocket_ticket(
        &self,
        ticket: &str,
    ) -> Result<AuthenticatedPrincipal, ConnectError> {
        if !valid_random_token(ticket) {
            return Err(ConnectError::InvalidCredential);
        }
        let now = now_seconds();
        let mut state = self.state.lock().map_err(|_| ConnectError::Busy)?;
        prune(&mut state, now);
        state
            .websocket_tickets
            .remove(&token_digest(ticket))
            .map(|record| record.principal)
            .ok_or(ConnectError::InvalidCredential)
    }
}

impl Default for ConnectSessionStore {
    fn default() -> Self {
        Self::new()
    }
}

fn consume_dpop_jti(state: &mut SessionState, jti: &str, now: u64) -> Result<(), ConnectError> {
    if state.dpop_jtis.contains_key(jti) {
        return Err(ConnectError::Replay);
    }
    state.dpop_jtis.insert(
        jti.to_string(),
        now.saturating_add(DPOP_CLOCK_SKEW_SECONDS as u64),
    );
    Ok(())
}

fn prune(state: &mut SessionState, now: u64) {
    state.bootstraps.retain(|_, value| value.expires_at > now);
    state
        .access_tokens
        .retain(|_, value| value.expires_at > now);
    state
        .websocket_tickets
        .retain(|_, value| value.expires_at > now);
    state.relay_jtis.retain(|_, expires_at| *expires_at > now);
    state.dpop_jtis.retain(|_, expires_at| *expires_at > now);
}

fn token_hash(token: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(token.as_bytes()))
}

fn token_digest(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

fn random_token() -> String {
    let mut value = [0_u8; 32];
    OsRng.fill_bytes(&mut value);
    URL_SAFE_NO_PAD.encode(value)
}

fn valid_random_token(value: &str) -> bool {
    value.len() == 43
        && URL_SAFE_NO_PAD
            .decode(value)
            .is_ok_and(|decoded| decoded.len() == 32)
}

fn format_timestamp(epoch_seconds: u64) -> Result<String, ConnectError> {
    OffsetDateTime::from_unix_timestamp(epoch_seconds as i64)
        .map_err(|_| ConnectError::InvalidEncoding)?
        .format(&Rfc3339)
        .map_err(|_| ConnectError::InvalidEncoding)
}

fn normalize_method(method: &str) -> Result<String, ConnectError> {
    let method = method.trim().to_ascii_uppercase();
    if method.is_empty() || !method.bytes().all(|byte| byte.is_ascii_uppercase()) {
        return Err(ConnectError::InvalidDpopProof);
    }
    Ok(method)
}

fn normalize_htu(value: &str) -> Result<String, ConnectError> {
    let mut url = Url::parse(value).map_err(|_| ConnectError::InvalidDpopProof)?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || url.username() != ""
        || url.password().is_some()
    {
        return Err(ConnectError::InvalidDpopProof);
    }
    url.set_query(None);
    url.set_fragment(None);
    Ok(url.to_string())
}

fn normalize_relay_endpoint(endpoint: &RelayEndpoint) -> Result<RelayEndpoint, ConnectError> {
    let http = normalize_base_url(&endpoint.http_base_url, "https")?;
    let ws = normalize_base_url(&endpoint.ws_base_url, "wss")?;
    let http_url = Url::parse(&http).map_err(|_| ConnectError::InvalidConfiguration)?;
    let ws_url = Url::parse(&ws).map_err(|_| ConnectError::InvalidConfiguration)?;
    if http_url.host_str() != ws_url.host_str() {
        return Err(ConnectError::InvalidConfiguration);
    }
    Ok(RelayEndpoint {
        http_base_url: http,
        ws_base_url: ws,
    })
}

fn normalize_base_url(value: &str, scheme: &str) -> Result<String, ConnectError> {
    let url = Url::parse(value).map_err(|_| ConnectError::InvalidConfiguration)?;
    if url.scheme() != scheme
        || url.host_str().is_none()
        || url.username() != ""
        || url.password().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(ConnectError::InvalidConfiguration);
    }
    Ok(url.as_str().trim_end_matches('/').to_string())
}

fn normalize_https_origin(value: &str) -> Result<String, ConnectError> {
    let url = Url::parse(value).map_err(|_| ConnectError::InvalidConfiguration)?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || url.username() != ""
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/"
    {
        return Err(ConnectError::InvalidConfiguration);
    }
    Ok(url.as_str().trim_end_matches('/').to_string())
}

fn principal_matches(left: &AuthenticatedPrincipal, right: &AuthenticatedPrincipal) -> bool {
    secure_eq(left.subject.as_bytes(), right.subject.as_bytes())
        && match (&left.organization_id, &right.organization_id) {
            (Some(left), Some(right)) => secure_eq(left.as_bytes(), right.as_bytes()),
            (None, None) => true,
            _ => false,
        }
}

fn secure_eq(left: &[u8], right: &[u8]) -> bool {
    left.len() == right.len() && bool::from(left.ct_eq(right))
}

fn decode_fixed<const N: usize>(value: &str) -> Result<[u8; N], ConnectError> {
    URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| ConnectError::InvalidEncoding)?
        .try_into()
        .map_err(|_| ConnectError::InvalidEncoding)
}

fn require_environment(environment: &str) -> Result<(), ConnectError> {
    if matches!(environment, "staging" | "production") {
        Ok(())
    } else {
        Err(ConnectError::InvalidConfiguration)
    }
}

fn now_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

#[cfg(target_os = "macos")]
fn keychain_get(account: &str) -> Result<Option<Vec<u8>>, ConnectError> {
    match security_framework::passwords::get_generic_password(KEYCHAIN_SERVICE, account) {
        Ok(value) => Ok(Some(value)),
        Err(error) if error.code() == -25300 => Ok(None),
        Err(_) => Err(ConnectError::KeychainFailed),
    }
}

#[cfg(not(target_os = "macos"))]
fn keychain_get(_account: &str) -> Result<Option<Vec<u8>>, ConnectError> {
    Err(ConnectError::KeychainFailed)
}

#[cfg(target_os = "macos")]
fn keychain_set(account: &str, value: &[u8]) -> Result<(), ConnectError> {
    security_framework::passwords::set_generic_password(KEYCHAIN_SERVICE, account, value)
        .map_err(|_| ConnectError::KeychainFailed)
}

#[cfg(not(target_os = "macos"))]
fn keychain_set(_account: &str, _value: &[u8]) -> Result<(), ConnectError> {
    Err(ConnectError::KeychainFailed)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectError {
    InvalidConfiguration,
    InvalidKeyMaterial,
    InvalidEncoding,
    InvalidSignature,
    InvalidRelayProof,
    InvalidDpopProof,
    InvalidCredential,
    Replay,
    KeychainFailed,
    Busy,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn environment_signature_covers_canonical_payload() {
        let identity = EnvironmentIdentity::from_bytes([7; 32]);
        let value = serde_json::json!({"z": 1, "a": {"y": 2, "b": 3}});
        let signature = identity.sign_canonical(&value).unwrap();
        EnvironmentIdentity::verify_canonical(&identity.public_jwk(), &value, &signature).unwrap();
        assert_eq!(
            EnvironmentIdentity::verify_canonical(
                &identity.public_jwk(),
                &serde_json::json!({"z": 2, "a": {"y": 2, "b": 3}}),
                &signature,
            ),
            Err(ConnectError::InvalidSignature)
        );
    }

    #[test]
    fn dpop_proof_is_bound_to_method_url_and_access_token() {
        let key = DpopKey::random();
        let url = "https://relay.clippy.saudecomalex.com/v1/environments";
        let proof = key.proof("GET", url, Some("access-token")).unwrap();
        let verified = verify_dpop(&proof, "GET", url, Some("access-token")).unwrap();
        assert_eq!(verified.thumbprint, key.thumbprint().unwrap());
        assert_eq!(
            verify_dpop(&proof, "POST", url, Some("access-token")),
            Err(ConnectError::InvalidDpopProof)
        );
        assert_eq!(
            verify_dpop(&proof, "GET", url, Some("other-token")),
            Err(ConnectError::InvalidDpopProof)
        );
        let query_proof = key
            .proof(
                "GET",
                "https://relay.clippy.saudecomalex.com/v1/environments?cursor=secret",
                Some("access-token"),
            )
            .unwrap();
        assert!(verify_dpop(&query_proof, "GET", url, Some("access-token")).is_ok());
    }

    #[test]
    fn bootstrap_and_access_tokens_are_one_use_and_proof_bound() {
        let store = ConnectSessionStore::new();
        let key = DpopKey::random();
        let principal = AuthenticatedPrincipal {
            subject: "user_123".into(),
            organization_id: None,
        };
        let bootstrap = random_token();
        store.state.lock().unwrap().bootstraps.insert(
            token_digest(&bootstrap),
            BootstrapRecord {
                expires_at: now_seconds() + 120,
                principal: principal.clone(),
                client_jkt: key.thumbprint().unwrap(),
            },
        );
        let url = "https://prod-test.clippy.saudecomalex.com/v1/connect/token";
        let proof = key.proof("POST", url, Some(&bootstrap)).unwrap();
        let (token, owner) = store
            .exchange_bootstrap(&bootstrap, &proof, "POST", url)
            .unwrap();
        assert_eq!(owner, principal);
        assert_eq!(token.token_type, "DPoP");
        let second_proof = key.proof("POST", url, Some(&bootstrap)).unwrap();
        assert_eq!(
            store.exchange_bootstrap(&bootstrap, &second_proof, "POST", url),
            Err(ConnectError::InvalidCredential)
        );

        let exchange_url = "https://prod-test.clippy.saudecomalex.com/v1/sync/exchange";
        let access_proof = key
            .proof("POST", exchange_url, Some(&token.access_token))
            .unwrap();
        assert_eq!(
            store
                .authorize(&token.access_token, &access_proof, "POST", exchange_url)
                .unwrap(),
            principal
        );
        assert_eq!(
            store.authorize(&token.access_token, &access_proof, "POST", exchange_url),
            Err(ConnectError::Replay)
        );

        let ticket_url = "https://prod-test.clippy.saudecomalex.com/v1/connect/websocket-ticket";
        let ticket_proof = key
            .proof("POST", ticket_url, Some(&token.access_token))
            .unwrap();
        let (ticket, ticket_owner) = store
            .issue_websocket_ticket(&token.access_token, &ticket_proof, "POST", ticket_url)
            .unwrap();
        assert_eq!(ticket_owner, principal);
        assert_eq!(
            store.consume_websocket_ticket(&ticket.ws_ticket).unwrap(),
            principal
        );
        assert_eq!(
            store.consume_websocket_ticket(&ticket.ws_ticket),
            Err(ConnectError::InvalidCredential)
        );
    }

    #[test]
    fn relay_mint_binds_canonical_http_and_websocket_endpoint() {
        let relay_identity = EnvironmentIdentity::from_bytes([9; 32]);
        let environment_identity = EnvironmentIdentity::from_bytes([7; 32]);
        let environment_id = Uuid::new_v4().to_string();
        let issuer = "https://relay.example.com";
        let endpoint = RelayEndpoint {
            http_base_url: "https://device.clippy.saudecomalex.com".into(),
            ws_base_url: "wss://device.clippy.saudecomalex.com".into(),
        };
        let principal = AuthenticatedPrincipal {
            subject: "user_123".into(),
            organization_id: Some("org_123".into()),
        };
        let client = DpopKey::random();
        let claims = RelayMintClaims {
            iss: issuer.into(),
            aud: format!("clippy-env:{environment_id}"),
            sub: principal.subject.clone(),
            org_id: principal.organization_id.clone(),
            environment_id: environment_id.clone(),
            endpoint: endpoint.clone(),
            client_jkt: client.thumbprint().unwrap(),
            client_nonce: random_token(),
            generation: 1,
            jti: Uuid::new_v4().to_string(),
            iat: now_seconds(),
            exp: now_seconds() + 60,
        };
        let proof = relay_jwt(&relay_identity, &claims);
        let public_jwk = serde_json::to_string(&relay_identity.public_jwk()).unwrap();
        let verifier = RelayProofVerifier::new(issuer.into(), environment_id, &public_jwk).unwrap();
        let store = ConnectSessionStore::new();
        let response = store
            .mint_from_relay(
                &proof,
                &verifier,
                &environment_identity,
                &principal,
                &endpoint,
            )
            .unwrap();
        let unsigned = MintResponseUnsigned {
            environment_id: &response.environment_id,
            bootstrap_credential: &response.bootstrap_credential,
            expires_at: &response.expires_at,
            client_jkt: &response.client_jkt,
            client_nonce: &response.client_nonce,
        };
        EnvironmentIdentity::verify_canonical(
            &environment_identity.public_jwk(),
            &unsigned,
            &response.signature,
        )
        .unwrap();

        let mismatched = RelayEndpoint {
            ws_base_url: "wss://other.clippy.saudecomalex.com".into(),
            ..endpoint
        };
        assert_eq!(
            verifier.verify(&proof, &principal, &mismatched),
            Err(ConnectError::InvalidRelayProof)
        );
        assert_eq!(
            store.mint_from_relay(
                &proof,
                &verifier,
                &environment_identity,
                &principal,
                &mismatched,
            ),
            Err(ConnectError::InvalidRelayProof)
        );
    }

    fn relay_jwt(identity: &EnvironmentIdentity, claims: &RelayMintClaims) -> String {
        let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"EdDSA","typ":"JWT"}"#);
        let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(claims).unwrap());
        let signing_input = format!("{header}.{payload}");
        let signature = identity.signing.sign(signing_input.as_bytes());
        format!(
            "{signing_input}.{}",
            URL_SAFE_NO_PAD.encode(signature.to_bytes())
        )
    }
}
