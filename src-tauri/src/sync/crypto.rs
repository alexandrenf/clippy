use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chacha20poly1305::aead::{Aead, KeyInit, OsRng};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use hkdf::Hkdf;
use rand_core::RngCore;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::time::{SystemTime, UNIX_EPOCH};
use subtle::ConstantTimeEq;
use x25519_dalek::{PublicKey, StaticSecret};

const KEYCHAIN_SERVICE: &str = "app.clippy.desktop.sync";
const KEYCHAIN_ACCOUNT_PREFIX: &str = "workspace-key:";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PairingOffer {
    pub version: u8,
    pub workspace_id: String,
    pub tunnel_url: String,
    pub workos_issuer: String,
    pub workos_audience: String,
    pub mac_public_key: String,
    pub one_time_token: String,
    pub expires_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PairingResponse {
    pub phone_public_key: String,
    pub one_time_token: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthenticatedPrincipal {
    pub subject: String,
    pub organization_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PairingGrant {
    pub mac_public_key: String,
    pub phone_public_key: String,
    pub sealed_workspace: SealedEnvelope,
}

pub struct PendingPairing {
    pub offer: PairingOffer,
    secret: StaticSecret,
    workspace_key: WorkspaceKey,
    owner: AuthenticatedPrincipal,
}

impl PendingPairing {
    pub fn new(
        workspace_id: String,
        tunnel_url: String,
        workos_issuer: String,
        workos_audience: String,
        workspace_key: WorkspaceKey,
        owner: AuthenticatedPrincipal,
        ttl_ms: u64,
    ) -> Self {
        let secret = StaticSecret::random_from_rng(OsRng);
        let public = PublicKey::from(&secret);
        let mut token = [0_u8; 32];
        OsRng.fill_bytes(&mut token);
        Self {
            offer: PairingOffer {
                version: 1,
                workspace_id,
                tunnel_url,
                workos_issuer,
                workos_audience,
                mac_public_key: URL_SAFE_NO_PAD.encode(public.as_bytes()),
                one_time_token: URL_SAFE_NO_PAD.encode(token),
                expires_at_ms: now_ms().saturating_add(ttl_ms),
            },
            secret,
            workspace_key,
            owner,
        }
    }

    /// Wraps the existing workspace key for one authenticated peer. The
    /// ephemeral X25519 result is never itself used as the workspace key, so
    /// pairing additional devices cannot strand existing peers.
    pub fn complete(
        self,
        response: &PairingResponse,
        principal: &AuthenticatedPrincipal,
    ) -> Result<PairingGrant, CryptoError> {
        self.validate_response(response, principal)?;
        let phone_key = decode_fixed::<32>(&response.phone_public_key)?;
        let phone_public = PublicKey::from(phone_key);
        let shared = self.secret.diffie_hellman(&phone_public);
        let wrap_key = derive_pairing_wrap_key(
            shared.as_bytes(),
            self.offer.one_time_token.as_bytes(),
            self.offer.workspace_id.as_bytes(),
        )?;
        let aad = pairing_aad(&self.offer, &response.phone_public_key, principal);
        let payload = PairingGrantPayload {
            workspace_id: self.offer.workspace_id.clone(),
            workspace_key: self.workspace_key.encode(),
            authorized_subject: principal.subject.clone(),
            organization_id: principal.organization_id.clone(),
        };
        let plaintext = serde_json::to_vec(&payload).map_err(|_| CryptoError::InvalidEncoding)?;
        Ok(PairingGrant {
            mac_public_key: self.offer.mac_public_key,
            phone_public_key: response.phone_public_key.clone(),
            sealed_workspace: seal(&wrap_key, &plaintext, &aad)?,
        })
    }

    pub fn validate_response(
        &self,
        response: &PairingResponse,
        principal: &AuthenticatedPrincipal,
    ) -> Result<(), CryptoError> {
        if now_ms() > self.offer.expires_at_ms {
            return Err(CryptoError::ExpiredPairing);
        }
        if !constant_time_eq(
            self.offer.one_time_token.as_bytes(),
            response.one_time_token.as_bytes(),
        ) {
            return Err(CryptoError::InvalidPairingToken);
        }
        if !principal_matches(&self.owner, principal) {
            return Err(CryptoError::PrincipalMismatch);
        }
        decode_fixed::<32>(&response.phone_public_key)?;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PairingGrantPayload {
    workspace_id: String,
    workspace_key: String,
    authorized_subject: String,
    organization_id: Option<String>,
}

/// Phone-side counterpart used by tests and by the wire specification.
pub fn unwrap_pairing_grant(
    phone_secret: &StaticSecret,
    offer: &PairingOffer,
    grant: &PairingGrant,
    principal: &AuthenticatedPrincipal,
) -> Result<WorkspaceKey, CryptoError> {
    if grant.mac_public_key != offer.mac_public_key {
        return Err(CryptoError::AuthenticationFailed);
    }
    let mac_public = PublicKey::from(decode_fixed::<32>(&grant.mac_public_key)?);
    let shared = phone_secret.diffie_hellman(&mac_public);
    let wrap_key = derive_pairing_wrap_key(
        shared.as_bytes(),
        offer.one_time_token.as_bytes(),
        offer.workspace_id.as_bytes(),
    )?;
    let aad = pairing_aad(offer, &grant.phone_public_key, principal);
    let plaintext = open(&wrap_key, &grant.sealed_workspace, &aad)?;
    let payload: PairingGrantPayload =
        serde_json::from_slice(&plaintext).map_err(|_| CryptoError::InvalidEncoding)?;
    if payload.workspace_id != offer.workspace_id
        || !constant_time_eq(
            payload.authorized_subject.as_bytes(),
            principal.subject.as_bytes(),
        )
        || payload.organization_id != principal.organization_id
    {
        return Err(CryptoError::PrincipalMismatch);
    }
    WorkspaceKey::decode(&payload.workspace_key)
}

#[derive(Clone)]
pub struct WorkspaceKey([u8; 32]);

impl WorkspaceKey {
    pub fn random() -> Self {
        let mut bytes = [0_u8; 32];
        OsRng.fill_bytes(&mut bytes);
        Self(bytes)
    }

    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub fn encode(&self) -> String {
        URL_SAFE_NO_PAD.encode(self.0)
    }

    pub fn decode(value: &str) -> Result<Self, CryptoError> {
        Ok(Self(decode_fixed(value)?))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SealedEnvelope {
    pub version: u8,
    pub nonce: String,
    pub ciphertext: String,
}

pub fn seal(
    key: &WorkspaceKey,
    plaintext: &[u8],
    aad: &[u8],
) -> Result<SealedEnvelope, CryptoError> {
    let cipher = ChaCha20Poly1305::new(key.as_bytes().into());
    let mut nonce_bytes = [0_u8; 12];
    OsRng.fill_bytes(&mut nonce_bytes);
    let ciphertext = cipher
        .encrypt(
            Nonce::from_slice(&nonce_bytes),
            chacha20poly1305::aead::Payload {
                msg: plaintext,
                aad,
            },
        )
        .map_err(|_| CryptoError::EncryptionFailed)?;
    Ok(SealedEnvelope {
        version: 1,
        nonce: URL_SAFE_NO_PAD.encode(nonce_bytes),
        ciphertext: URL_SAFE_NO_PAD.encode(ciphertext),
    })
}

pub fn open(
    key: &WorkspaceKey,
    envelope: &SealedEnvelope,
    aad: &[u8],
) -> Result<Vec<u8>, CryptoError> {
    if envelope.version != 1 {
        return Err(CryptoError::UnsupportedVersion);
    }
    let nonce = decode_fixed::<12>(&envelope.nonce)?;
    let ciphertext = URL_SAFE_NO_PAD
        .decode(&envelope.ciphertext)
        .map_err(|_| CryptoError::InvalidEncoding)?;
    ChaCha20Poly1305::new(key.as_bytes().into())
        .decrypt(
            Nonce::from_slice(&nonce),
            chacha20poly1305::aead::Payload {
                msg: &ciphertext,
                aad,
            },
        )
        .map_err(|_| CryptoError::AuthenticationFailed)
}

fn derive_pairing_wrap_key(
    shared_secret: &[u8],
    one_time_token: &[u8],
    workspace_id: &[u8],
) -> Result<WorkspaceKey, CryptoError> {
    let hkdf = Hkdf::<Sha256>::new(Some(one_time_token), shared_secret);
    let mut key = [0_u8; 32];
    let mut info = b"clippy-sync-pairing-wrap-key-v1:".to_vec();
    info.extend_from_slice(workspace_id);
    hkdf.expand(&info, &mut key)
        .map_err(|_| CryptoError::KeyDerivationFailed)?;
    Ok(WorkspaceKey(key))
}

fn pairing_aad(
    offer: &PairingOffer,
    phone_public_key: &str,
    principal: &AuthenticatedPrincipal,
) -> Vec<u8> {
    // Length-prefixing avoids ambiguous concatenations without leaking secrets.
    let fields = [
        offer.version.to_string(),
        offer.workspace_id.clone(),
        offer.tunnel_url.clone(),
        offer.workos_issuer.clone(),
        offer.workos_audience.clone(),
        offer.expires_at_ms.to_string(),
        offer.mac_public_key.clone(),
        phone_public_key.to_string(),
        principal.subject.clone(),
        principal.organization_id.clone().unwrap_or_default(),
    ];
    let mut aad = Vec::new();
    for field in fields {
        aad.extend_from_slice(&(field.len() as u32).to_be_bytes());
        aad.extend_from_slice(field.as_bytes());
    }
    aad
}

fn principal_matches(left: &AuthenticatedPrincipal, right: &AuthenticatedPrincipal) -> bool {
    constant_time_eq(left.subject.as_bytes(), right.subject.as_bytes())
        && match (&left.organization_id, &right.organization_id) {
            (Some(left), Some(right)) => constant_time_eq(left.as_bytes(), right.as_bytes()),
            (None, None) => true,
            _ => false,
        }
}

#[cfg(target_os = "macos")]
pub fn store_workspace_key(workspace_id: &str, key: &WorkspaceKey) -> Result<(), CryptoError> {
    security_framework::passwords::set_generic_password(
        KEYCHAIN_SERVICE,
        &format!("{KEYCHAIN_ACCOUNT_PREFIX}{workspace_id}"),
        key.encode().as_bytes(),
    )
    .map_err(|_| CryptoError::KeychainFailed)
}

#[cfg(target_os = "macos")]
pub fn load_workspace_key(workspace_id: &str) -> Result<Option<WorkspaceKey>, CryptoError> {
    match security_framework::passwords::get_generic_password(
        KEYCHAIN_SERVICE,
        &format!("{KEYCHAIN_ACCOUNT_PREFIX}{workspace_id}"),
    ) {
        Ok(value) => {
            let encoded = std::str::from_utf8(&value).map_err(|_| CryptoError::InvalidEncoding)?;
            WorkspaceKey::decode(encoded).map(Some)
        }
        Err(error) if error.code() == -25300 => Ok(None), // errSecItemNotFound
        Err(_) => Err(CryptoError::KeychainFailed),
    }
}

fn decode_fixed<const N: usize>(value: &str) -> Result<[u8; N], CryptoError> {
    let bytes = URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| CryptoError::InvalidEncoding)?;
    bytes.try_into().map_err(|_| CryptoError::InvalidEncoding)
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    left.len() == right.len() && bool::from(left.ct_eq(right))
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CryptoError {
    InvalidEncoding,
    InvalidPairingToken,
    ExpiredPairing,
    KeyDerivationFailed,
    EncryptionFailed,
    AuthenticationFailed,
    UnsupportedVersion,
    KeychainFailed,
    PrincipalMismatch,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sealed_payload_rejects_tampering_and_wrong_context() {
        let key = WorkspaceKey::from_bytes([7; 32]);
        let sealed = seal(&key, b"private clipboard", b"workspace-1").unwrap();
        assert_eq!(
            open(&key, &sealed, b"workspace-1").unwrap(),
            b"private clipboard"
        );
        assert_eq!(
            open(&key, &sealed, b"workspace-2"),
            Err(CryptoError::AuthenticationFailed)
        );
    }

    #[test]
    fn pairing_multiple_phones_preserves_one_workspace_key() {
        let workspace_key = WorkspaceKey::from_bytes([42; 32]);
        let principal = AuthenticatedPrincipal {
            subject: "user_123".into(),
            organization_id: Some("org_123".into()),
        };

        fn pair_phone(
            workspace_key: WorkspaceKey,
            principal: &AuthenticatedPrincipal,
        ) -> WorkspaceKey {
            let pairing = PendingPairing::new(
                "workspace".into(),
                "https://sync.example.com".into(),
                "https://issuer.example.com".into(),
                "client_test".into(),
                workspace_key,
                principal.clone(),
                60_000,
            );
            let offer = pairing.offer.clone();
            let phone_secret = StaticSecret::random_from_rng(OsRng);
            let phone_public = PublicKey::from(&phone_secret);
            let response = PairingResponse {
                phone_public_key: URL_SAFE_NO_PAD.encode(phone_public.as_bytes()),
                one_time_token: pairing.offer.one_time_token.clone(),
            };
            let grant = pairing.complete(&response, principal).unwrap();
            unwrap_pairing_grant(&phone_secret, &offer, &grant, principal).unwrap()
        }

        let phone_one = pair_phone(workspace_key.clone(), &principal);
        let phone_two = pair_phone(workspace_key.clone(), &principal);
        assert_eq!(phone_one.as_bytes(), workspace_key.as_bytes());
        assert_eq!(phone_two.as_bytes(), workspace_key.as_bytes());
    }

    #[test]
    fn pairing_grant_binds_every_routing_and_identity_field() {
        let principal = AuthenticatedPrincipal {
            subject: "user_123".into(),
            organization_id: Some("org_123".into()),
        };
        let pairing = PendingPairing::new(
            "workspace".into(),
            "https://sync.example.com".into(),
            "https://issuer.example.com".into(),
            "client_test".into(),
            WorkspaceKey::from_bytes([9; 32]),
            principal.clone(),
            60_000,
        );
        let offer = pairing.offer.clone();
        let phone_secret = StaticSecret::random_from_rng(OsRng);
        let phone_public = PublicKey::from(&phone_secret);
        let response = PairingResponse {
            phone_public_key: URL_SAFE_NO_PAD.encode(phone_public.as_bytes()),
            one_time_token: offer.one_time_token.clone(),
        };
        let grant = pairing.complete(&response, &principal).unwrap();

        let mut tampered = offer.clone();
        tampered.tunnel_url = "https://attacker.example".into();
        assert!(unwrap_pairing_grant(&phone_secret, &tampered, &grant, &principal).is_err());
        let mut tampered = offer.clone();
        tampered.workos_issuer = "https://attacker.example".into();
        assert!(unwrap_pairing_grant(&phone_secret, &tampered, &grant, &principal).is_err());
        let mut tampered = offer.clone();
        tampered.workos_audience = "client_attacker".into();
        assert!(unwrap_pairing_grant(&phone_secret, &tampered, &grant, &principal).is_err());
        let mut tampered = offer;
        tampered.expires_at_ms = tampered.expires_at_ms.saturating_add(1);
        assert!(unwrap_pairing_grant(&phone_secret, &tampered, &grant, &principal).is_err());
    }
}
