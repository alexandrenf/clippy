use super::auth::WorkOsVerifier;
use super::config::{
    Environment, PRODUCTION_WORKOS_AUDIENCE, PRODUCTION_WORKOS_ISSUER, STAGING_WORKOS_AUDIENCE,
    STAGING_WORKOS_ISSUER,
};
use super::connect::{DpopKey, EnvironmentIdentity, OkpPublicJwk};
use rusqlite::{params, Connection};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use url::Url;
use uuid::Uuid;

const KEYCHAIN_SERVICE: &str = "app.clippy.desktop.sync";
const AUTH_KEYCHAIN_SERVICE: &str = "app.clippy.desktop.auth.v2";

pub async fn link(environment: Environment, db_path: &Path, name: &str) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() || name.len() > 120 || name.chars().any(char::is_control) {
        return Err("Device name must contain 1 to 120 printable characters".into());
    }
    let config = LinkConfig::load(environment)?;
    let access_token = load_keychain_string_from(
        AUTH_KEYCHAIN_SERVICE,
        &format!("workos:{}:access-token", environment.as_str()),
    )?
    .ok_or_else(|| "Sign in to Clippy before connecting sync".to_string())?;
    let principal =
        WorkOsVerifier::new(config.workos_issuer.clone(), config.workos_audience.clone())
            .map_err(|_| "WorkOS configuration is invalid".to_string())?
            .verify(&access_token)
            .await
            .map_err(|_| "Clippy sign-in expired; sign in again".to_string())?;
    let dpop = DpopKey::load_or_create(environment.as_str(), "relay")
        .map_err(|_| "Could not load the relay device key".to_string())?;
    let identity = EnvironmentIdentity::load_or_create(environment.as_str())
        .map_err(|_| "Could not load the environment signing key".to_string())?;
    let environment_id = pending_or_linked_environment_id(environment)?;
    let client = reqwest::Client::builder()
        .https_only(true)
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(40))
        .build()
        .map_err(|_| "Could not create the relay client".to_string())?;

    let challenge_url = config.endpoint("/v1/environments/link/challenge")?;
    let challenge: LinkChallenge = workos_request(
        &client,
        &dpop,
        &access_token,
        reqwest::Method::POST,
        challenge_url,
        Some(&serde_json::json!({"environment_id": environment_id, "name": name})),
    )
    .await?;
    let issued_at = now_seconds()?;
    let public_jwk = identity.public_jwk();
    let signed = LinkProof {
        challenge: &challenge.challenge,
        challenge_id: &challenge.challenge_id,
        environment_id: &environment_id,
        environment_public_jwk: &public_jwk,
        issued_at,
        name,
    };
    let signature = identity
        .sign_canonical(&signed)
        .map_err(|_| "Could not sign the relay challenge".to_string())?;
    let link_url = config.endpoint("/v1/environments/link")?;
    let response: LinkResponse = workos_request(
        &client,
        &dpop,
        &access_token,
        reqwest::Method::POST,
        link_url,
        Some(&serde_json::json!({
            "challenge_id": challenge.challenge_id,
            "environment_id": environment_id,
            "name": name,
            "environment_public_jwk": public_jwk,
            "issued_at": issued_at,
            "signature": signature,
        })),
    )
    .await?;
    validate_response(&config, &environment_id, &response)?;
    persist_link(
        environment,
        db_path,
        &config,
        &environment_id,
        &principal.subject,
        principal.organization_id.as_deref(),
        &response,
    )?;
    Ok(response.endpoint.http_base_url)
}

async fn workos_request<T: DeserializeOwned>(
    client: &reqwest::Client,
    dpop: &DpopKey,
    access_token: &str,
    method: reqwest::Method,
    url: Url,
    body: Option<&Value>,
) -> Result<T, String> {
    let proof = dpop
        .proof(method.as_str(), url.as_str(), Some(access_token))
        .map_err(|_| "Could not create the DPoP proof".to_string())?;
    let mut request = client
        .request(method, url)
        .header("authorization", format!("Bearer {access_token}"))
        .header("dpop", proof);
    if let Some(body) = body {
        request = request.json(body);
    }
    let response = request
        .send()
        .await
        .map_err(|_| "Could not reach the Clippy relay".to_string())?;
    if !response.status().is_success() {
        let status = response.status();
        let message = response
            .json::<RelayError>()
            .await
            .ok()
            .map(|value| value.error.message)
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "request failed".into());
        return Err(format!("Clippy relay returned HTTP {status}: {message}"));
    }
    response
        .json()
        .await
        .map_err(|_| "Clippy relay returned an invalid response".to_string())
}

struct LinkConfig {
    relay: Url,
    endpoint_suffix: &'static str,
    workos_issuer: String,
    workos_audience: String,
}

impl LinkConfig {
    fn load(environment: Environment) -> Result<Self, String> {
        let (relay, endpoint_suffix, workos_issuer, workos_audience) = match environment {
            Environment::Staging => (
                std::env::var("CLIPPY_STAGING_RELAY_BASE_URL")
                    .unwrap_or_else(|_| "https://relay-staging.saudecomalex.com".into()),
                "clippy-staging.saudecomalex.com",
                STAGING_WORKOS_ISSUER,
                STAGING_WORKOS_AUDIENCE,
            ),
            Environment::Production => (
                std::env::var("CLIPPY_PRODUCTION_RELAY_BASE_URL")
                    .unwrap_or_else(|_| "https://relay.saudecomalex.com".into()),
                "clippy.saudecomalex.com",
                PRODUCTION_WORKOS_ISSUER,
                PRODUCTION_WORKOS_AUDIENCE,
            ),
        };
        let relay = Url::parse(&relay).map_err(|_| "Relay URL is invalid".to_string())?;
        if relay.scheme() != "https"
            || relay.host_str().is_none()
            || relay.query().is_some()
            || relay.fragment().is_some()
        {
            return Err("Relay URL must be an HTTPS origin".into());
        }
        Ok(Self {
            relay,
            endpoint_suffix,
            workos_issuer: workos_issuer.into(),
            workos_audience: workos_audience.into(),
        })
    }

    fn endpoint(&self, path: &str) -> Result<Url, String> {
        self.relay
            .join(path.trim_start_matches('/'))
            .map_err(|_| "Relay endpoint is invalid".to_string())
    }
}

fn validate_response(
    config: &LinkConfig,
    expected_environment_id: &str,
    response: &LinkResponse,
) -> Result<(), String> {
    if response.environment.id != expected_environment_id
        || response.environment.environment_id != expected_environment_id
        || response.environment.workspace_id != expected_environment_id
        || response.environment.endpoint != response.endpoint
        || response.environment.hostname != response.runtime.hostname
        || response.runtime.tunnel_id.is_empty()
        || response.runtime.connector_token.is_empty()
    {
        return Err("Relay returned a mismatched environment allocation".into());
    }
    let http = validate_managed_url(
        &response.endpoint.http_base_url,
        "https",
        config.endpoint_suffix,
    )?;
    let websocket = validate_managed_url(
        &response.endpoint.ws_base_url,
        "wss",
        config.endpoint_suffix,
    )?;
    if http.host_str() != websocket.host_str() {
        return Err("Relay returned inconsistent HTTP and WebSocket endpoints".into());
    }
    let jwk = &response.runtime.relay_signing_public_jwk;
    if jwk.kty != "OKP" || jwk.crv != "Ed25519" || jwk.x.is_empty() {
        return Err("Relay returned an invalid public signing key".into());
    }
    Ok(())
}

fn validate_managed_url(raw: &str, scheme: &str, suffix: &str) -> Result<Url, String> {
    let url = Url::parse(raw).map_err(|_| "Relay returned an invalid managed URL".to_string())?;
    let host = url
        .host_str()
        .ok_or_else(|| "Managed endpoint has no hostname".to_string())?;
    let suffix = suffix.to_ascii_lowercase();
    let host = host.to_ascii_lowercase();
    if url.scheme() != scheme
        || (host != suffix && !host.ends_with(&format!(".{suffix}")))
        || url.username() != ""
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || (url.path() != "/" && !url.path().is_empty())
    {
        return Err("Managed endpoint is outside the allowed Clippy hostname".into());
    }
    Ok(url)
}

fn persist_link(
    environment: Environment,
    db_path: &Path,
    config: &LinkConfig,
    environment_id: &str,
    owner_subject: &str,
    owner_organization: Option<&str>,
    response: &LinkResponse,
) -> Result<(), String> {
    let prefix = format!("connect:{}", environment.as_str());
    keychain_set(
        &format!("{prefix}:relay-issuer"),
        config.relay.as_str().trim_end_matches('/').as_bytes(),
    )?;
    keychain_set(
        &format!("{prefix}:relay-signing-public-jwk"),
        &serde_json::to_vec(&response.runtime.relay_signing_public_jwk)
            .map_err(|_| "Could not encode relay trust".to_string())?,
    )?;
    keychain_set(
        &format!("{prefix}:endpoint"),
        &serde_json::to_vec(&response.endpoint)
            .map_err(|_| "Could not encode managed endpoint".to_string())?,
    )?;
    keychain_set(&format!("{prefix}:owner-sub"), owner_subject.as_bytes())?;
    if let Some(organization) = owner_organization.filter(|value| !value.is_empty()) {
        keychain_set(&format!("{prefix}:owner-org"), organization.as_bytes())?;
    } else {
        keychain_delete(&format!("{prefix}:owner-org"))?;
    }
    keychain_set(
        &format!("cloudflare-tunnel:{}", environment.as_str()),
        response.runtime.connector_token.as_bytes(),
    )?;
    let connection =
        Connection::open(db_path).map_err(|_| "Could not open Clippy settings".to_string())?;
    let transaction = connection
        .unchecked_transaction()
        .map_err(|_| "Could not update Clippy settings".to_string())?;
    transaction
        .execute(
            "INSERT INTO settings(key,value) VALUES('sync_environment',?1)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![environment.as_str()],
        )
        .map_err(|_| "Could not save the sync environment".to_string())?;
    transaction
        .execute(
            "INSERT INTO settings(key,value) VALUES(?1,?2)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![
                format!("sync_workspace_id:{}", environment.as_str()),
                environment_id
            ],
        )
        .map_err(|_| "Could not save the workspace identity".to_string())?;
    transaction
        .commit()
        .map_err(|_| "Could not commit Clippy settings".to_string())?;
    keychain_set(
        &format!("{prefix}:environment-id"),
        environment_id.as_bytes(),
    )?;
    keychain_delete(&format!("{prefix}:pending-environment-id"))?;
    Ok(())
}

fn pending_or_linked_environment_id(environment: Environment) -> Result<String, String> {
    let prefix = format!("connect:{}", environment.as_str());
    for account in [
        format!("{prefix}:environment-id"),
        format!("{prefix}:pending-environment-id"),
    ] {
        if let Some(value) = load_keychain_string(&account)? {
            Uuid::parse_str(&value)
                .map_err(|_| "Stored environment identity is invalid".to_string())?;
            return Ok(value);
        }
    }
    let value = Uuid::new_v4().to_string();
    keychain_set(
        &format!("{prefix}:pending-environment-id"),
        value.as_bytes(),
    )?;
    Ok(value)
}

fn load_keychain_string(account: &str) -> Result<Option<String>, String> {
    load_keychain_string_from(KEYCHAIN_SERVICE, account)
}

fn load_keychain_string_from(service: &str, account: &str) -> Result<Option<String>, String> {
    match security_framework::passwords::get_generic_password(service, account) {
        Ok(value) => String::from_utf8(value)
            .map(Some)
            .map_err(|_| "Stored Keychain value is invalid".to_string()),
        Err(error) if error.code() == -25300 => Ok(None),
        Err(_) => Err("Could not read Clippy credentials from Keychain".into()),
    }
}

fn keychain_set(account: &str, value: &[u8]) -> Result<(), String> {
    security_framework::passwords::set_generic_password(KEYCHAIN_SERVICE, account, value)
        .map_err(|_| "Could not store Clippy credentials in Keychain".to_string())
}

fn keychain_delete(account: &str) -> Result<(), String> {
    match security_framework::passwords::delete_generic_password(KEYCHAIN_SERVICE, account) {
        Ok(()) => Ok(()),
        Err(error) if error.code() == -25300 => Ok(()),
        Err(_) => Err("Could not update Clippy credentials in Keychain".into()),
    }
}

fn now_seconds() -> Result<u64, String> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_secs())
        .map_err(|_| "System clock is invalid".to_string())
}

#[derive(Deserialize)]
struct LinkChallenge {
    challenge_id: String,
    challenge: String,
}

#[derive(Serialize)]
struct LinkProof<'a> {
    challenge: &'a str,
    challenge_id: &'a str,
    environment_id: &'a str,
    environment_public_jwk: &'a OkpPublicJwk,
    issued_at: u64,
    name: &'a str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Endpoint {
    http_base_url: String,
    ws_base_url: String,
}

#[derive(Deserialize)]
struct LinkResponse {
    environment: RelayEnvironment,
    endpoint: Endpoint,
    runtime: LinkRuntime,
}

#[derive(Deserialize)]
struct RelayEnvironment {
    id: String,
    environment_id: String,
    workspace_id: String,
    endpoint: Endpoint,
    hostname: String,
}

#[derive(Deserialize)]
struct LinkRuntime {
    tunnel_id: String,
    hostname: String,
    connector_token: String,
    relay_signing_public_jwk: RelayPublicJwk,
}

#[derive(Serialize, Deserialize)]
struct RelayPublicJwk {
    kty: String,
    crv: String,
    x: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    alg: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    kid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    r#use: Option<String>,
}

#[derive(Deserialize)]
struct RelayError {
    error: RelayErrorBody,
}

#[derive(Deserialize)]
struct RelayErrorBody {
    message: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn managed_endpoint_rejects_suffix_confusion() {
        assert!(validate_managed_url(
            "https://evilclippy.saudecomalex.com.attacker.test",
            "https",
            "clippy.saudecomalex.com"
        )
        .is_err());
    }
}
