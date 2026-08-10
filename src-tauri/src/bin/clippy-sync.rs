use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use jsonwebtoken::jwk::JwkSet;
use jsonwebtoken::{decode, decode_header, Algorithm, DecodingKey, Validation};
use rand_core::{OsRng, RngCore};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::env;
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::time::{Duration, Instant};
use subtle::ConstantTimeEq;
use url::Url;
use uuid::Uuid;

#[path = "../sync/connect.rs"]
mod connect;
#[path = "../sync/crypto.rs"]
mod crypto;

use connect::{DpopKey, EnvironmentIdentity, OkpPublicJwk};

const CALLBACK: &str = "http://127.0.0.1:49834/auth/callback";
const CALLBACK_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const KEYCHAIN_SERVICE: &str = "app.clippy.desktop.sync";
const STAGING_ISSUER: &str = "https://fashionable-machine-85-staging.authkit.app";
const STAGING_CLIENT_ID: &str = "client_01KZMNQXBXWT2A807NZCE6V2HV";
const PRODUCTION_ISSUER: &str = "https://brave-mermaid-84.authkit.app";
const PRODUCTION_CLIENT_ID: &str = "client_01KZMNK73NWS9NDAPC3T54S2PE";
const STAGING_RELAY: &str = "https://relay-staging.saudecomalex.com";
const PRODUCTION_RELAY: &str = "https://relay.saudecomalex.com";
const APP_ID: &str = "app.clippy.desktop";

fn main() {
    if let Err(error) = run() {
        eprintln!("clippy-sync: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let args: Vec<String> = env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("login") => login(required_environment(&args)?),
        Some("link") => link(
            required_environment(&args)?,
            option_value(&args, "--name").unwrap_or_else(|| "Clippy on this Mac".into()),
        ),
        Some("status") => status(required_environment(&args)?),
        Some("unlink") => unlink(
            required_environment(&args)?,
            args.iter().any(|argument| argument == "--delete-tunnel"),
        ),
        _ => Err(
            "usage: clippy-sync login|link|status|unlink --environment staging|production [--name NAME] [--delete-tunnel]"
                .into(),
        ),
    }
}

fn login(environment: &str) -> Result<(), String> {
    let config = LoginConfig::load(environment)?;

    let listener = TcpListener::bind("127.0.0.1:49834")
        .map_err(|_| "callback port 49834 is unavailable".to_string())?;
    listener
        .set_nonblocking(true)
        .map_err(|_| "could not configure callback listener".to_string())?;

    let verifier = random_urlsafe(32);
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let state = random_urlsafe(32);
    let nonce = random_urlsafe(32);
    let authorize_url = config.authorize_url(&challenge, &state, &nonce)?;

    println!("Opening AuthKit for {} login…", config.name);
    open::that_detached(authorize_url.as_str())
        .map_err(|_| "could not open the system browser".to_string())?;
    let code = receive_callback(&listener, &state)?;
    let tokens = exchange_code(&config, &code, &verifier)?;
    let access_claims = verify_token(&config, &tokens.access_token, None)?;
    let id_claims = verify_token(&config, &tokens.id_token, Some(&nonce))?;
    if !secure_eq(access_claims.sub.as_bytes(), id_claims.sub.as_bytes()) {
        return Err("access and ID token subjects do not match".into());
    }

    store_token(environment, "access-token", tokens.access_token.as_bytes())?;
    store_token(environment, "id-token", tokens.id_token.as_bytes())?;
    if let Some(refresh_token) = tokens.refresh_token {
        store_token(environment, "refresh-token", refresh_token.as_bytes())?;
    }
    println!("Clippy Sync login succeeded for {}.", config.name);
    Ok(())
}

fn required_environment(args: &[String]) -> Result<&str, String> {
    option_value_ref(args, "--environment")
        .ok_or_else(|| "--environment staging|production is required".to_string())
}

fn option_value(args: &[String], flag: &str) -> Option<String> {
    option_value_ref(args, flag).map(ToOwned::to_owned)
}

fn option_value_ref<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.windows(2)
        .find(|pair| pair[0] == flag)
        .map(|pair| pair[1].as_str())
}

struct LoginConfig {
    name: &'static str,
    issuer: Url,
    client_id: String,
}

impl LoginConfig {
    fn load(environment: &str) -> Result<Self, String> {
        let (name, issuer, client_id) = match environment {
            "staging" => (
                "Staging",
                env::var("CLIPPY_STAGING_WORKOS_ISSUER").unwrap_or_else(|_| STAGING_ISSUER.into()),
                env::var("CLIPPY_STAGING_WORKOS_CLIENT_ID")
                    .unwrap_or_else(|_| STAGING_CLIENT_ID.into()),
            ),
            "production" => (
                "Production",
                env::var("CLIPPY_PRODUCTION_WORKOS_ISSUER")
                    .unwrap_or_else(|_| PRODUCTION_ISSUER.into()),
                env::var("CLIPPY_PRODUCTION_WORKOS_CLIENT_ID")
                    .unwrap_or_else(|_| PRODUCTION_CLIENT_ID.into()),
            ),
            _ => return Err("environment must be staging or production".into()),
        };
        let issuer = Url::parse(&issuer).map_err(|_| "invalid WorkOS issuer".to_string())?;
        if issuer.scheme() != "https" || issuer.host_str().is_none() {
            return Err("WorkOS issuer must be HTTPS".into());
        }
        if client_id.trim().is_empty() {
            return Err("WorkOS public client ID is missing".into());
        }
        Ok(Self {
            name,
            issuer,
            client_id,
        })
    }

    fn authorize_url(&self, challenge: &str, state: &str, nonce: &str) -> Result<Url, String> {
        let mut url = self
            .issuer
            .join("oauth2/authorize")
            .map_err(|_| "invalid authorization endpoint".to_string())?;
        url.query_pairs_mut()
            .append_pair("client_id", &self.client_id)
            .append_pair("redirect_uri", CALLBACK)
            .append_pair("response_type", "code")
            .append_pair("scope", "openid profile email offline_access")
            .append_pair("code_challenge", challenge)
            .append_pair("code_challenge_method", "S256")
            .append_pair("state", state)
            .append_pair("nonce", nonce);
        Ok(url)
    }
}

struct RelayConfig {
    base_url: Url,
    endpoint_suffix: &'static str,
}

impl RelayConfig {
    fn load(environment: &str) -> Result<Self, String> {
        let (default_url, endpoint_suffix, override_name) = match environment {
            "staging" => (
                STAGING_RELAY,
                "clippy-staging.saudecomalex.com",
                "CLIPPY_STAGING_RELAY_BASE_URL",
            ),
            "production" => (
                PRODUCTION_RELAY,
                "clippy.saudecomalex.com",
                "CLIPPY_PRODUCTION_RELAY_BASE_URL",
            ),
            _ => return Err("environment must be staging or production".into()),
        };
        let raw = env::var(override_name).unwrap_or_else(|_| default_url.into());
        let base_url = Url::parse(&raw).map_err(|_| "relay URL is invalid".to_string())?;
        if base_url.scheme() != "https"
            || base_url.host_str().is_none()
            || base_url.query().is_some()
            || base_url.fragment().is_some()
        {
            return Err("relay URL must be an HTTPS origin".into());
        }
        Ok(Self {
            base_url,
            endpoint_suffix,
        })
    }

    fn endpoint(&self, path: &str) -> Result<Url, String> {
        self.base_url
            .join(path.trim_start_matches('/'))
            .map_err(|_| "relay endpoint is invalid".to_string())
    }
}

fn link(environment: &str, name: String) -> Result<(), String> {
    let name = name.trim();
    if name.is_empty() || name.len() > 120 || name.chars().any(char::is_control) {
        return Err("--name must contain 1 to 120 printable characters".into());
    }
    let login = LoginConfig::load(environment)?;
    let relay = RelayConfig::load(environment)?;
    let access_token =
        load_token(environment, "access-token")?.ok_or_else(|| login_required(environment))?;
    let claims =
        verify_token(&login, &access_token, None).map_err(|_| login_required(environment))?;
    let dpop = DpopKey::load_or_create(environment, "relay")
        .map_err(|_| "could not load the relay device key".to_string())?;
    let identity = EnvironmentIdentity::load_or_create(environment)
        .map_err(|_| "could not load the environment signing key".to_string())?;
    let environment_id = pending_or_linked_environment_id(environment)?;
    let client = https_client()?;

    let challenge_url = relay.endpoint("/v1/environments/link/challenge")?;
    let challenge: LinkChallenge = workos_request(
        &client,
        &dpop,
        &access_token,
        reqwest::Method::POST,
        challenge_url,
        Some(&serde_json::json!({
            "environment_id": environment_id,
            "name": name,
        })),
    )?;
    if challenge.challenge_id.is_empty() || challenge.challenge.is_empty() {
        return Err("relay returned an invalid link challenge".into());
    }
    let issued_at = unix_seconds()?;
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
        .map_err(|_| "could not sign the link challenge".to_string())?;
    let link_url = relay.endpoint("/v1/environments/link")?;
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
    )?;
    validate_link_response(&relay, &environment_id, &response)?;
    persist_link(environment, &relay, &environment_id, &claims, &response)?;
    println!(
        "Clippy linked {} at {}.",
        if environment == "production" {
            "Production"
        } else {
            "Staging"
        },
        response.endpoint.http_base_url
    );
    Ok(())
}

fn status(environment: &str) -> Result<(), String> {
    let relay = RelayConfig::load(environment)?;
    let environment_id = linked_environment_id(environment)?;
    let (client, dpop, relay_token) = relay_session(environment, &relay)?;
    let url = relay.endpoint(&format!("/v1/environments/{}/status", environment_id))?;
    let response: StatusResponse = relay_request(
        &client,
        &dpop,
        &relay_token.access_token,
        reqwest::Method::GET,
        url,
        None,
    )?;
    if response.environment.environment_id != environment_id {
        return Err("relay returned a different environment identity".into());
    }
    println!(
        "{}: {} ({})",
        response.environment.name,
        response.environment.status,
        response.environment.endpoint.http_base_url
    );
    Ok(())
}

fn unlink(environment: &str, delete_tunnel: bool) -> Result<(), String> {
    let relay = RelayConfig::load(environment)?;
    let environment_id = linked_environment_id(environment)?;
    let (client, dpop, relay_token) = relay_session(environment, &relay)?;
    if delete_tunnel {
        let url = relay.endpoint(&format!("/v1/environments/{}/tunnel", environment_id))?;
        relay_empty_request(
            &client,
            &dpop,
            &relay_token.access_token,
            reqwest::Method::DELETE,
            url,
            None,
        )?;
    } else {
        let url = relay.endpoint(&format!("/v1/environments/{}", environment_id))?;
        relay_empty_request(
            &client,
            &dpop,
            &relay_token.access_token,
            reqwest::Method::DELETE,
            url,
            None,
        )?;
    }
    clear_link(environment, &environment_id)?;
    println!(
        "Clippy unlinked {}{}.",
        environment,
        if delete_tunnel {
            " and deleted its managed tunnel"
        } else {
            " (the managed tunnel was retained for recovery)"
        }
    );
    Ok(())
}

fn relay_session(
    environment: &str,
    relay: &RelayConfig,
) -> Result<(reqwest::blocking::Client, DpopKey, RelayToken), String> {
    let login = LoginConfig::load(environment)?;
    let access_token =
        load_token(environment, "access-token")?.ok_or_else(|| login_required(environment))?;
    verify_token(&login, &access_token, None).map_err(|_| login_required(environment))?;
    let dpop = DpopKey::load_or_create(environment, "relay")
        .map_err(|_| "could not load the relay device key".to_string())?;
    let client = https_client()?;
    let url = relay.endpoint("/v1/auth/token")?;
    let token: RelayToken = workos_request(
        &client,
        &dpop,
        &access_token,
        reqwest::Method::POST,
        url,
        Some(&serde_json::json!({})),
    )?;
    if token.token_type != "DPoP"
        || token.scope != "relay:environments"
        || token.cnf.jkt
            != dpop
                .thumbprint()
                .map_err(|_| "could not verify the relay device key".to_string())?
    {
        return Err("relay returned a token for a different device key".into());
    }
    Ok((client, dpop, token))
}

fn workos_request<T: for<'de> Deserialize<'de>>(
    client: &reqwest::blocking::Client,
    dpop: &DpopKey,
    access_token: &str,
    method: reqwest::Method,
    url: Url,
    body: Option<&Value>,
) -> Result<T, String> {
    let proof = dpop
        .proof(method.as_str(), url.as_str(), Some(access_token))
        .map_err(|_| "could not create the DPoP proof".to_string())?;
    let mut request = client
        .request(method, url)
        .header("authorization", format!("Bearer {access_token}"))
        .header("dpop", proof);
    if let Some(body) = body {
        request = request.json(body);
    }
    parse_json_response(request.send())
}

fn relay_request<T: for<'de> Deserialize<'de>>(
    client: &reqwest::blocking::Client,
    dpop: &DpopKey,
    access_token: &str,
    method: reqwest::Method,
    url: Url,
    body: Option<&Value>,
) -> Result<T, String> {
    let proof = dpop
        .proof(method.as_str(), url.as_str(), Some(access_token))
        .map_err(|_| "could not create the DPoP proof".to_string())?;
    let mut request = client
        .request(method, url)
        .header("authorization", format!("DPoP {access_token}"))
        .header("dpop", proof);
    if let Some(body) = body {
        request = request.json(body);
    }
    parse_json_response(request.send())
}

fn relay_empty_request(
    client: &reqwest::blocking::Client,
    dpop: &DpopKey,
    access_token: &str,
    method: reqwest::Method,
    url: Url,
    body: Option<&Value>,
) -> Result<(), String> {
    let proof = dpop
        .proof(method.as_str(), url.as_str(), Some(access_token))
        .map_err(|_| "could not create the DPoP proof".to_string())?;
    let mut request = client
        .request(method, url)
        .header("authorization", format!("DPoP {access_token}"))
        .header("dpop", proof);
    if let Some(body) = body {
        request = request.json(body);
    }
    let response = request
        .send()
        .map_err(|_| "could not reach the Clippy relay".to_string())?;
    if response.status().is_success() {
        Ok(())
    } else {
        Err(relay_http_error(response))
    }
}

fn parse_json_response<T: for<'de> Deserialize<'de>>(
    response: Result<reqwest::blocking::Response, reqwest::Error>,
) -> Result<T, String> {
    let response = response.map_err(|_| "could not reach the Clippy relay".to_string())?;
    if !response.status().is_success() {
        return Err(relay_http_error(response));
    }
    response
        .json()
        .map_err(|_| "relay returned an invalid response".to_string())
}

fn relay_http_error(response: reqwest::blocking::Response) -> String {
    let status = response.status();
    let message = response
        .json::<RelayError>()
        .ok()
        .map(|value| value.error.message)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "request failed".into());
    format!("relay returned HTTP {status}: {message}")
}

fn https_client() -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .https_only(true)
        .build()
        .map_err(|_| "could not create the HTTPS client".to_string())
}

fn validate_link_response(
    relay: &RelayConfig,
    expected_environment_id: &str,
    response: &LinkResponse,
) -> Result<(), String> {
    if response.environment.environment_id != expected_environment_id
        || response.environment.workspace_id != expected_environment_id
        || response.environment.id != expected_environment_id
        || response.runtime.tunnel_id.is_empty()
        || response.runtime.connector_token.is_empty()
        || response.runtime.hostname != response.environment.hostname
    {
        return Err("relay returned a mismatched environment allocation".into());
    }
    let http = validate_managed_url(
        &response.endpoint.http_base_url,
        "https",
        relay.endpoint_suffix,
    )?;
    let websocket =
        validate_managed_url(&response.endpoint.ws_base_url, "wss", relay.endpoint_suffix)?;
    if http.host_str() != websocket.host_str()
        || http.port_or_known_default() != Some(443)
        || websocket.port_or_known_default() != Some(443)
        || response.environment.endpoint != response.endpoint
    {
        return Err("relay returned inconsistent managed endpoints".into());
    }
    let relay_jwk = serde_json::to_value(&response.runtime.relay_signing_public_jwk)
        .map_err(|_| "relay signing key was invalid".to_string())?;
    if response.runtime.relay_signing_public_jwk.kty != "OKP"
        || response.runtime.relay_signing_public_jwk.crv != "Ed25519"
        || response.runtime.relay_signing_public_jwk.x.is_empty()
        || relay_jwk.get("d").is_some()
    {
        return Err("relay returned an invalid public signing key".into());
    }
    Ok(())
}

fn validate_managed_url(raw: &str, scheme: &str, suffix: &str) -> Result<Url, String> {
    let url = Url::parse(raw).map_err(|_| "relay returned an invalid managed URL".to_string())?;
    let host = url
        .host_str()
        .ok_or_else(|| "managed endpoint has no hostname".to_string())?;
    if url.scheme() != scheme
        || (!host.eq_ignore_ascii_case(suffix)
            && !host
                .to_ascii_lowercase()
                .ends_with(&format!(".{}", suffix.to_ascii_lowercase())))
        || url.username() != ""
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || (url.path() != "/" && !url.path().is_empty())
    {
        return Err("managed endpoint is outside the allowed Clippy hostname".into());
    }
    Ok(url)
}

fn persist_link(
    environment: &str,
    relay: &RelayConfig,
    environment_id: &str,
    claims: &Claims,
    response: &LinkResponse,
) -> Result<(), String> {
    let prefix = format!("connect:{environment}");
    let endpoint = serde_json::to_vec(&response.endpoint)
        .map_err(|_| "could not encode the managed endpoint".to_string())?;
    let signing_jwk = serde_json::to_vec(&response.runtime.relay_signing_public_jwk)
        .map_err(|_| "could not encode the relay signing key".to_string())?;
    store_keychain(
        &format!("{prefix}:relay-issuer"),
        relay.base_url.as_str().trim_end_matches('/').as_bytes(),
    )?;
    store_keychain(&format!("{prefix}:relay-signing-public-jwk"), &signing_jwk)?;
    store_keychain(&format!("{prefix}:endpoint"), &endpoint)?;
    store_keychain(&format!("{prefix}:owner-sub"), claims.sub.as_bytes())?;
    match claims.org_id.as_deref().filter(|value| !value.is_empty()) {
        Some(organization) => {
            store_keychain(&format!("{prefix}:owner-org"), organization.as_bytes())?
        }
        None => delete_keychain(&format!("{prefix}:owner-org"))?,
    }
    store_keychain(
        &format!("cloudflare-tunnel:{environment}"),
        response.runtime.connector_token.as_bytes(),
    )?;
    persist_workspace_settings(environment, environment_id)?;
    // This is the commit marker. Runtime config ignores all linked fields
    // until the matching environment id exists.
    store_keychain(
        &format!("{prefix}:environment-id"),
        environment_id.as_bytes(),
    )?;
    delete_keychain(&format!("{prefix}:pending-environment-id"))?;
    Ok(())
}

fn clear_link(environment: &str, environment_id: &str) -> Result<(), String> {
    let prefix = format!("connect:{environment}");
    // Preserve the environment identity so a normal unlink can reactivate the
    // same allocation and workspace key without silently forking local data.
    store_keychain(
        &format!("{prefix}:pending-environment-id"),
        environment_id.as_bytes(),
    )?;
    for account in [
        format!("{prefix}:environment-id"),
        format!("{prefix}:relay-issuer"),
        format!("{prefix}:relay-signing-public-jwk"),
        format!("{prefix}:endpoint"),
        format!("{prefix}:owner-sub"),
        format!("{prefix}:owner-org"),
        format!("cloudflare-tunnel:{environment}"),
    ] {
        delete_keychain(&account)?;
    }
    clear_workspace_setting(environment)?;
    Ok(())
}

fn pending_or_linked_environment_id(environment: &str) -> Result<String, String> {
    let prefix = format!("connect:{environment}");
    for account in [
        format!("{prefix}:environment-id"),
        format!("{prefix}:pending-environment-id"),
    ] {
        if let Some(value) = load_keychain_string(&account)? {
            if Uuid::parse_str(&value).is_ok() {
                return Ok(value);
            }
            return Err("stored environment identity is invalid".into());
        }
    }
    let value = Uuid::new_v4().to_string();
    store_keychain(
        &format!("{prefix}:pending-environment-id"),
        value.as_bytes(),
    )?;
    Ok(value)
}

fn linked_environment_id(environment: &str) -> Result<String, String> {
    let account = format!("connect:{environment}:environment-id");
    let value = load_keychain_string(&account)?.ok_or_else(|| {
        format!("{environment} is not linked; run `clippy-sync link --environment {environment}`")
    })?;
    Uuid::parse_str(&value).map_err(|_| "stored environment identity is invalid".to_string())?;
    Ok(value)
}

fn persist_workspace_settings(environment: &str, environment_id: &str) -> Result<(), String> {
    let path = app_database_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|_| "could not create Clippy data directory".to_string())?;
    }
    let connection =
        Connection::open(path).map_err(|_| "could not open Clippy settings".to_string())?;
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS settings(key TEXT PRIMARY KEY, value TEXT NOT NULL);",
        )
        .map_err(|_| "could not initialize Clippy settings".to_string())?;
    let transaction = connection
        .unchecked_transaction()
        .map_err(|_| "could not update Clippy settings".to_string())?;
    transaction
        .execute(
            "INSERT INTO settings(key,value) VALUES('sync_environment',?1)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![environment],
        )
        .map_err(|_| "could not save the selected sync environment".to_string())?;
    transaction
        .execute(
            "INSERT INTO settings(key,value) VALUES(?1,?2)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![format!("sync_workspace_id:{environment}"), environment_id],
        )
        .map_err(|_| "could not save the workspace identity".to_string())?;
    transaction
        .commit()
        .map_err(|_| "could not commit Clippy settings".to_string())
}

fn clear_workspace_setting(environment: &str) -> Result<(), String> {
    let path = app_database_path()?;
    if !path.exists() {
        return Ok(());
    }
    let connection =
        Connection::open(path).map_err(|_| "could not open Clippy settings".to_string())?;
    connection
        .execute(
            "DELETE FROM settings WHERE key = ?1",
            params![format!("sync_workspace_id:{environment}")],
        )
        .map_err(|_| "could not clear the workspace setting".to_string())?;
    Ok(())
}

fn app_database_path() -> Result<PathBuf, String> {
    let home = env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .ok_or_else(|| "HOME is unavailable".to_string())?;
    Ok(home
        .join("Library")
        .join("Application Support")
        .join(APP_ID)
        .join("clippy.db"))
}

fn load_token(environment: &str, kind: &str) -> Result<Option<String>, String> {
    load_keychain_string(&format!("workos:{environment}:{kind}"))
}

fn load_keychain_string(account: &str) -> Result<Option<String>, String> {
    match security_framework::passwords::get_generic_password(KEYCHAIN_SERVICE, account) {
        Ok(value) => String::from_utf8(value)
            .map(Some)
            .map_err(|_| "stored Keychain value is invalid".to_string()),
        Err(error) if error.code() == -25300 => Ok(None),
        Err(_) => Err("could not read credentials from Keychain".into()),
    }
}

fn store_keychain(account: &str, value: &[u8]) -> Result<(), String> {
    security_framework::passwords::set_generic_password(KEYCHAIN_SERVICE, account, value)
        .map_err(|_| "could not store credentials in Keychain".to_string())
}

fn delete_keychain(account: &str) -> Result<(), String> {
    match security_framework::passwords::delete_generic_password(KEYCHAIN_SERVICE, account) {
        Ok(()) => Ok(()),
        Err(error) if error.code() == -25300 => Ok(()),
        Err(_) => Err("could not remove credentials from Keychain".into()),
    }
}

fn login_required(environment: &str) -> String {
    format!("login is missing or expired; run `clippy-sync login --environment {environment}`")
}

fn unix_seconds() -> Result<u64, String> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_secs())
        .map_err(|_| "system clock is invalid".to_string())
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
    name: String,
    status: String,
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
struct StatusResponse {
    environment: RelayEnvironment,
}

#[derive(Deserialize)]
struct RelayToken {
    access_token: String,
    token_type: String,
    scope: String,
    cnf: Confirmation,
}

#[derive(Deserialize)]
struct Confirmation {
    jkt: String,
}

#[derive(Deserialize)]
struct RelayError {
    error: RelayErrorBody,
}

#[derive(Deserialize)]
struct RelayErrorBody {
    message: String,
}

fn receive_callback(listener: &TcpListener, expected_state: &str) -> Result<String, String> {
    let deadline = Instant::now() + CALLBACK_TIMEOUT;
    while Instant::now() < deadline {
        match listener.accept() {
            Ok((mut stream, _)) => {
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .map_err(|_| "callback timeout setup failed".to_string())?;
                match parse_callback(&mut stream, expected_state) {
                    Ok(code) => {
                        respond(
                            &mut stream,
                            200,
                            "Authentication received. You can close this tab.",
                        );
                        return Ok(code);
                    }
                    Err(error) => {
                        respond(&mut stream, 400, "Authentication could not be verified.");
                        if error.starts_with("AuthKit returned ") {
                            return Err(error);
                        }
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(_) => return Err("callback listener failed".into()),
        }
    }
    Err("login timed out after 15 minutes".into())
}

fn parse_callback(stream: &mut TcpStream, expected_state: &str) -> Result<String, String> {
    let mut buffer = [0_u8; 8192];
    let read = stream
        .read(&mut buffer)
        .map_err(|_| "could not read callback".to_string())?;
    let request =
        std::str::from_utf8(&buffer[..read]).map_err(|_| "invalid callback request".to_string())?;
    let target = request
        .lines()
        .next()
        .and_then(|line| line.strip_prefix("GET "))
        .and_then(|line| line.split_whitespace().next())
        .ok_or_else(|| "invalid callback request".to_string())?;
    parse_callback_target(target, expected_state)
}

fn parse_callback_target(target: &str, expected_state: &str) -> Result<String, String> {
    let url = Url::parse(&format!("http://127.0.0.1:49834{target}"))
        .map_err(|_| "invalid callback URL".to_string())?;
    if url.path() != "/auth/callback" {
        return Err("unexpected callback path".into());
    }
    let values: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
    if let Some(error) = values.get("error") {
        return Err(format!("AuthKit returned {error}"));
    }
    let returned_state = values
        .get("state")
        .ok_or_else(|| "callback state is missing".to_string())?;
    if !secure_eq(returned_state.as_bytes(), expected_state.as_bytes()) {
        return Err("callback state did not match".into());
    }
    values
        .get("code")
        .filter(|code| !code.is_empty())
        .cloned()
        .ok_or_else(|| "authorization code is missing".to_string())
}

fn respond(stream: &mut TcpStream, status: u16, message: &str) {
    let body =
        format!("<!doctype html><meta charset=utf-8><title>Clippy Sync</title><p>{message}</p>");
    let response = format!(
        "HTTP/1.1 {status} {}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        if status == 200 { "OK" } else { "Bad Request" },
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
}

fn exchange_code(config: &LoginConfig, code: &str, verifier: &str) -> Result<TokenSet, String> {
    let endpoint = config
        .issuer
        .join("oauth2/token")
        .map_err(|_| "invalid token endpoint".to_string())?;
    let response = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|_| "could not create HTTPS client".to_string())?
        .post(endpoint)
        .form(&[
            ("client_id", config.client_id.as_str()),
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", CALLBACK),
            ("code_verifier", verifier),
        ])
        .send()
        .map_err(|_| "token exchange failed".to_string())?;
    if !response.status().is_success() {
        return Err(format!(
            "token exchange returned HTTP {}",
            response.status()
        ));
    }
    response
        .json()
        .map_err(|_| "token response was invalid".to_string())
}

fn verify_token(
    config: &LoginConfig,
    token: &str,
    expected_nonce: Option<&str>,
) -> Result<Claims, String> {
    let jwks_url = config
        .issuer
        .join("oauth2/jwks")
        .map_err(|_| "invalid JWKS endpoint".to_string())?;
    let jwks: JwkSet = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|_| "could not create HTTPS client".to_string())?
        .get(jwks_url)
        .send()
        .and_then(reqwest::blocking::Response::error_for_status)
        .map_err(|_| "could not retrieve WorkOS signing keys".to_string())?
        .json()
        .map_err(|_| "WorkOS signing keys were invalid".to_string())?;
    let header = decode_header(token).map_err(|_| "token header was invalid".to_string())?;
    if header.alg != Algorithm::RS256 {
        return Err("unexpected token signing algorithm".into());
    }
    let kid = header
        .kid
        .ok_or_else(|| "token key ID is missing".to_string())?;
    let jwk = jwks
        .find(&kid)
        .ok_or_else(|| "token signing key was not found".to_string())?;
    let key = DecodingKey::from_jwk(jwk).map_err(|_| "unsupported signing key".to_string())?;
    let mut validation = Validation::new(Algorithm::RS256);
    validation.set_issuer(&[config.issuer.as_str().trim_end_matches('/')]);
    validation.validate_aud = false;
    validation.set_required_spec_claims(&["exp", "iss", "sub"]);
    let claims = decode::<Claims>(token, &key, &validation)
        .map_err(|_| "token signature or claims were invalid".to_string())?
        .claims;
    if let Some(expected) = expected_nonce {
        if claims
            .aud
            .as_ref()
            .map(|audience| audience.contains(&config.client_id))
            != Some(true)
        {
            return Err("ID token audience did not match this application".into());
        }
        let actual = claims
            .nonce
            .as_deref()
            .ok_or_else(|| "ID token nonce is missing".to_string())?;
        if !secure_eq(actual.as_bytes(), expected.as_bytes()) {
            return Err("ID token nonce did not match".into());
        }
    } else if claims.client_id.as_deref() != Some(config.client_id.as_str()) {
        return Err("access token belongs to another application".into());
    }
    Ok(claims)
}

fn store_token(environment: &str, kind: &str, value: &[u8]) -> Result<(), String> {
    security_framework::passwords::set_generic_password(
        KEYCHAIN_SERVICE,
        &format!("workos:{environment}:{kind}"),
        value,
    )
    .map_err(|_| "could not store credentials in Keychain".to_string())
}

fn random_urlsafe(bytes: usize) -> String {
    let mut value = vec![0_u8; bytes];
    OsRng.fill_bytes(&mut value);
    URL_SAFE_NO_PAD.encode(value)
}

fn secure_eq(left: &[u8], right: &[u8]) -> bool {
    left.len() == right.len() && bool::from(left.ct_eq(right))
}

#[derive(Deserialize)]
struct TokenSet {
    access_token: String,
    id_token: String,
    refresh_token: Option<String>,
}

#[derive(Clone, Deserialize)]
struct Claims {
    sub: String,
    nonce: Option<String>,
    org_id: Option<String>,
    aud: Option<Audience>,
    client_id: Option<String>,
}

#[derive(Clone, Deserialize)]
#[serde(untagged)]
enum Audience {
    One(String),
    Many(Vec<String>),
}

impl Audience {
    fn contains(&self, expected: &str) -> bool {
        match self {
            Self::One(value) => value == expected,
            Self::Many(values) => values.iter().any(|value| value == expected),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_forged_callback_state() {
        assert_eq!(
            parse_callback_target("/auth/callback?code=x&state=wrong", "expected"),
            Err("callback state did not match".into())
        );
    }
}
