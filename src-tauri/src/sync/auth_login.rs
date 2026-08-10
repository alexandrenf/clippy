use super::auth::WorkOsVerifier;
use super::config::{
    Environment, PRODUCTION_WORKOS_AUDIENCE, PRODUCTION_WORKOS_CLIENT_ID, PRODUCTION_WORKOS_ISSUER,
    STAGING_WORKOS_AUDIENCE, STAGING_WORKOS_CLIENT_ID, STAGING_WORKOS_ISSUER,
};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use jsonwebtoken::jwk::JwkSet;
use jsonwebtoken::{decode, decode_header, Algorithm, DecodingKey, Validation};
use rand_core::{OsRng, RngCore};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;
use subtle::ConstantTimeEq;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::{timeout, Instant};
use url::Url;

const CALLBACK: &str = "http://127.0.0.1:49834/auth/callback";
const CALLBACK_TIMEOUT: Duration = Duration::from_secs(15 * 60);

pub async fn sign_in(environment: Environment, db_path: &Path) -> Result<(), String> {
    let config = LoginConfig::load(environment)?;
    let listener = TcpListener::bind("127.0.0.1:49834")
        .await
        .map_err(|_| "Another Clippy sign-in is already in progress".to_string())?;
    let verifier = random_urlsafe(32);
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let state = random_urlsafe(32);
    let nonce = random_urlsafe(32);
    let authorize_url = config.authorize_url(&challenge, &state, &nonce)?;
    open::that_detached(authorize_url.as_str())
        .map_err(|_| "Could not open AuthKit in your browser".to_string())?;

    let code = receive_callback(&listener, &state).await?;
    let tokens = exchange_code(&config, &code, &verifier).await?;
    let workos = WorkOsVerifier::new(config.issuer.to_string(), config.audience.clone())
        .map_err(|_| "WorkOS configuration is invalid".to_string())?;
    let principal = workos.verify(&tokens.access_token).await.map_err(|error| {
        eprintln!("clippy AuthKit access-token verification failed: {error:?}");
        "AuthKit returned an invalid access token".to_string()
    })?;
    let id_subject = verify_id_token(&config, &tokens.id_token, &nonce).await?;
    if !secure_eq(principal.subject.as_bytes(), id_subject.as_bytes()) {
        return Err("AuthKit token identities did not match".into());
    }

    store_session(
        db_path,
        environment,
        &StoredOAuthSession {
            access_token: tokens.access_token,
            id_token: tokens.id_token,
            refresh_token: tokens.refresh_token,
        },
    )?;
    Ok(())
}

pub async fn is_signed_in(environment: Environment, db_path: &Path) -> bool {
    let config = match LoginConfig::load(environment) {
        Ok(config) => config,
        Err(_) => return false,
    };
    let session = match load_session(db_path, environment) {
        Some(session) => session,
        None => return false,
    };
    let verifier = match WorkOsVerifier::new(config.issuer.to_string(), config.audience) {
        Ok(verifier) => verifier,
        Err(_) => return false,
    };
    if verifier.verify(&session.access_token).await.is_ok() {
        true
    } else if access_token_expires_soon(&session.access_token) {
        // A rejected refresh removes the unusable local session. Network and
        // service failures intentionally preserve it so an offline launch does
        // not look like a logout or force another browser sign-in.
        let _ = refresh_access_token(environment, db_path).await;
        load_session(db_path, environment).is_some()
    } else {
        true
    }
}

pub fn access_token(environment: Environment, db_path: &Path) -> Result<String, String> {
    load_session(db_path, environment)
        .map(|session| session.access_token)
        .ok_or_else(|| "Sign in from Clippy Settings before enabling sync".to_string())
}

pub fn sign_out(environment: Environment, db_path: &Path) {
    delete_session(db_path, environment);
}

pub async fn refresh_access_token(
    environment: Environment,
    db_path: &Path,
) -> Result<String, String> {
    let config = LoginConfig::load(environment)?;
    let mut session = load_session(db_path, environment)
        .ok_or_else(|| "Desktop sign-in expired; sign in again from Clippy Settings".to_string())?;
    let refresh_token = session
        .refresh_token
        .as_deref()
        .ok_or_else(|| "Desktop sign-in expired; sign in again from Clippy Settings".to_string())?;
    let endpoint = config
        .issuer
        .join("oauth2/token")
        .map_err(|_| "AuthKit token endpoint is invalid".to_string())?;
    let response = post_token_form(
        &endpoint,
        &[
            ("client_id", config.client_id.as_str()),
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
        ],
    )
    .await
    .map_err(|_| "Could not refresh desktop sign-in; Clippy will retry".to_string())?;
    if matches!(response.status, 400 | 401 | 403) {
        delete_session(db_path, environment);
        return Err("Desktop sign-in expired; sign in again from Clippy Settings".into());
    }
    if response.status != 200 {
        return Err(format!(
            "Could not refresh desktop sign-in; Clippy will retry (HTTP {})",
            response.status
        ));
    }
    let tokens: RefreshTokenSet = serde_json::from_slice(&response.body)
        .map_err(|_| "AuthKit returned an invalid refresh response".to_string())?;
    let verifier = WorkOsVerifier::new(config.issuer.to_string(), config.audience)
        .map_err(|_| "WorkOS configuration is invalid".to_string())?;
    verifier
        .verify(&tokens.access_token)
        .await
        .map_err(|_| "AuthKit returned an invalid access token".to_string())?;
    session.access_token = tokens.access_token.clone();
    if let Some(refresh_token) = tokens.refresh_token {
        session.refresh_token = Some(refresh_token);
    }
    store_session(db_path, environment, &session)?;
    Ok(tokens.access_token)
}

pub fn access_token_expires_soon(token: &str) -> bool {
    let expires_at = token
        .split('.')
        .nth(1)
        .and_then(|payload| URL_SAFE_NO_PAD.decode(payload).ok())
        .and_then(|payload| serde_json::from_slice::<AccessExpiry>(&payload).ok())
        .map(|claims| claims.exp);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(u64::MAX);
    expires_at.is_none_or(|expires_at| expires_at <= now.saturating_add(60))
}

struct LoginConfig {
    issuer: Url,
    client_id: String,
    audience: String,
}

impl LoginConfig {
    fn load(environment: Environment) -> Result<Self, String> {
        let (issuer, client_id, audience) = match environment {
            Environment::Staging => (
                std::env::var("CLIPPY_STAGING_WORKOS_ISSUER")
                    .unwrap_or_else(|_| STAGING_WORKOS_ISSUER.into()),
                std::env::var("CLIPPY_STAGING_WORKOS_CLIENT_ID")
                    .unwrap_or_else(|_| STAGING_WORKOS_CLIENT_ID.into()),
                std::env::var("CLIPPY_STAGING_WORKOS_AUDIENCE")
                    .unwrap_or_else(|_| STAGING_WORKOS_AUDIENCE.into()),
            ),
            Environment::Production => (
                std::env::var("CLIPPY_PRODUCTION_WORKOS_ISSUER")
                    .unwrap_or_else(|_| PRODUCTION_WORKOS_ISSUER.into()),
                std::env::var("CLIPPY_PRODUCTION_WORKOS_CLIENT_ID")
                    .unwrap_or_else(|_| PRODUCTION_WORKOS_CLIENT_ID.into()),
                std::env::var("CLIPPY_PRODUCTION_WORKOS_AUDIENCE")
                    .unwrap_or_else(|_| PRODUCTION_WORKOS_AUDIENCE.into()),
            ),
        };
        let issuer = Url::parse(&issuer).map_err(|_| "WorkOS issuer is invalid".to_string())?;
        if issuer.scheme() != "https"
            || issuer.host_str().is_none()
            || client_id.is_empty()
            || audience.is_empty()
        {
            return Err("WorkOS configuration is invalid".into());
        }
        Ok(Self {
            issuer,
            client_id,
            audience,
        })
    }

    fn authorize_url(&self, challenge: &str, state: &str, nonce: &str) -> Result<Url, String> {
        let mut url = self
            .issuer
            .join("oauth2/authorize")
            .map_err(|_| "AuthKit authorization endpoint is invalid".to_string())?;
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

async fn receive_callback(listener: &TcpListener, expected_state: &str) -> Result<String, String> {
    let deadline = Instant::now() + CALLBACK_TIMEOUT;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err("Sign-in timed out after 15 minutes".into());
        }
        let (mut stream, _) = timeout(remaining, listener.accept())
            .await
            .map_err(|_| "Sign-in timed out after 15 minutes".to_string())?
            .map_err(|_| "Could not receive the AuthKit redirect".to_string())?;
        match parse_callback(&mut stream, expected_state).await {
            Ok(code) => {
                respond(
                    &mut stream,
                    200,
                    "Clippy is signed in. You can close this tab.",
                )
                .await;
                return Ok(code);
            }
            Err(error) => {
                respond(&mut stream, 400, "Clippy could not verify this sign-in.").await;
                // Loopback ports can receive speculative browser connections or
                // local probes. Only AuthKit's explicit OAuth error is terminal;
                // malformed and wrong-state requests must not cancel the real flow.
                if error.starts_with("AuthKit authorization failed: ") {
                    return Err(error);
                }
            }
        }
    }
}

async fn parse_callback(stream: &mut TcpStream, expected_state: &str) -> Result<String, String> {
    let mut buffer = [0_u8; 8192];
    let read = timeout(Duration::from_secs(3), stream.read(&mut buffer))
        .await
        .map_err(|_| "AuthKit redirect timed out".to_string())?
        .map_err(|_| "Could not read the AuthKit redirect".to_string())?;
    let request = std::str::from_utf8(&buffer[..read])
        .map_err(|_| "AuthKit redirect was invalid".to_string())?;
    let target = request
        .lines()
        .next()
        .and_then(|line| line.strip_prefix("GET "))
        .and_then(|line| line.split_whitespace().next())
        .ok_or_else(|| "AuthKit redirect was invalid".to_string())?;
    parse_callback_target(target, expected_state)
}

fn parse_callback_target(target: &str, expected_state: &str) -> Result<String, String> {
    let url = Url::parse(&format!("http://127.0.0.1:49834{target}"))
        .map_err(|_| "AuthKit redirect URL was invalid".to_string())?;
    if url.path() != "/auth/callback" {
        return Err("AuthKit returned an unexpected callback path".into());
    }
    let values: HashMap<_, _> = url.query_pairs().into_owned().collect();
    if let Some(error) = values.get("error") {
        return Err(format!("AuthKit authorization failed: {error}"));
    }
    let state = values
        .get("state")
        .ok_or_else(|| "AuthKit callback state is missing".to_string())?;
    if !secure_eq(state.as_bytes(), expected_state.as_bytes()) {
        return Err("AuthKit callback state did not match".into());
    }
    values
        .get("code")
        .filter(|code| !code.is_empty())
        .cloned()
        .ok_or_else(|| "AuthKit authorization code is missing".to_string())
}

async fn respond(stream: &mut TcpStream, status: u16, message: &str) {
    let body = format!(
        "<!doctype html><meta charset=utf-8><title>Clippy</title><style>body{{font:16px system-ui;padding:48px;color:#243129;background:#f5f2eb}}</style><p>{message}</p>"
    );
    let response = format!(
        "HTTP/1.1 {status} {}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        if status == 200 { "OK" } else { "Bad Request" },
        body.len()
    );
    let _ = stream.write_all(response.as_bytes()).await;
}

async fn exchange_code(
    config: &LoginConfig,
    code: &str,
    verifier: &str,
) -> Result<TokenSet, String> {
    let endpoint = config
        .issuer
        .join("oauth2/token")
        .map_err(|_| "AuthKit token endpoint is invalid".to_string())?;
    let response = post_token_form(
        &endpoint,
        &[
            ("client_id", config.client_id.as_str()),
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", CALLBACK),
            ("code_verifier", verifier),
        ],
    )
    .await
    .map_err(|_| "AuthKit token exchange failed".to_string())?;
    if response.status != 200 {
        return Err("AuthKit token exchange failed".into());
    }
    serde_json::from_slice(&response.body)
        .map_err(|_| "AuthKit returned an invalid token response".to_string())
}

struct TokenHttpResponse {
    status: u16,
    body: Vec<u8>,
}

async fn post_token_form(
    endpoint: &Url,
    fields: &[(&str, &str)],
) -> Result<TokenHttpResponse, String> {
    let body = {
        let mut serializer = url::form_urlencoded::Serializer::new(String::new());
        for (name, value) in fields {
            serializer.append_pair(name, value);
        }
        serializer.finish()
    };

    #[cfg(target_os = "macos")]
    {
        let endpoint = endpoint.as_str().to_string();
        return tokio::task::spawn_blocking(move || post_token_form_with_curl(&endpoint, &body))
            .await
            .map_err(|_| "AuthKit request task failed".to_string())?;
    }

    #[cfg(not(target_os = "macos"))]
    {
        let response = reqwest::Client::builder()
            .https_only(true)
            .timeout(Duration::from_secs(20))
            .build()
            .map_err(|_| "Could not create the AuthKit client".to_string())?
            .post(endpoint.clone())
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded",
            )
            .header(reqwest::header::ACCEPT, "application/json")
            .body(body)
            .send()
            .await
            .map_err(|_| "AuthKit request failed".to_string())?;
        let status = response.status().as_u16();
        let body = response
            .bytes()
            .await
            .map_err(|_| "AuthKit response failed".to_string())?;
        if body.len() > 1_048_576 {
            return Err("AuthKit response was too large".into());
        }
        Ok(TokenHttpResponse {
            status,
            body: body.to_vec(),
        })
    }
}

#[cfg(target_os = "macos")]
fn post_token_form_with_curl(endpoint: &str, body: &str) -> Result<TokenHttpResponse, String> {
    use std::io::Write;
    use std::process::{Command, Stdio};

    let mut child = Command::new("/usr/bin/curl")
        .args([
            "--silent",
            "--show-error",
            "--connect-timeout",
            "10",
            "--max-time",
            "20",
            "--max-filesize",
            "1048576",
            "--request",
            "POST",
            "--header",
            "Content-Type: application/x-www-form-urlencoded",
            "--header",
            "Accept: application/json",
            "--data-binary",
            "@-",
            "--output",
            "-",
            "--write-out",
            "\n%{http_code}",
            endpoint,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| "Could not start the macOS HTTPS client".to_string())?;
    child
        .stdin
        .take()
        .ok_or_else(|| "Could not open the macOS HTTPS client".to_string())?
        .write_all(body.as_bytes())
        .map_err(|_| "Could not send the AuthKit request".to_string())?;
    let output = child
        .wait_with_output()
        .map_err(|_| "Could not finish the AuthKit request".to_string())?;
    if !output.status.success() {
        return Err("AuthKit request failed".into());
    }
    parse_curl_token_response(output.stdout)
}

#[cfg(target_os = "macos")]
fn parse_curl_token_response(mut output: Vec<u8>) -> Result<TokenHttpResponse, String> {
    let split = output
        .iter()
        .rposition(|byte| *byte == b'\n')
        .ok_or_else(|| "AuthKit response status was missing".to_string())?;
    let status = std::str::from_utf8(&output[split + 1..])
        .ok()
        .and_then(|value| value.parse::<u16>().ok())
        .filter(|status| (100..=599).contains(status))
        .ok_or_else(|| "AuthKit response status was invalid".to_string())?;
    output.truncate(split);
    Ok(TokenHttpResponse {
        status,
        body: output,
    })
}

async fn verify_id_token(
    config: &LoginConfig,
    token: &str,
    expected_nonce: &str,
) -> Result<String, String> {
    let endpoint = config
        .issuer
        .join("oauth2/jwks")
        .map_err(|_| "AuthKit signing-key endpoint is invalid".to_string())?;
    let jwks: JwkSet = super::auth::fetch_jwks(endpoint.as_str())
        .await
        .map_err(|_| "Could not retrieve AuthKit signing keys".to_string())?;
    let header = decode_header(token).map_err(|_| "ID token header was invalid".to_string())?;
    if header.alg != Algorithm::RS256 {
        return Err("ID token used an unexpected signing algorithm".into());
    }
    let key = DecodingKey::from_jwk(
        jwks.find(
            header
                .kid
                .as_deref()
                .ok_or_else(|| "ID token key id is missing".to_string())?,
        )
        .ok_or_else(|| "ID token signing key was not found".to_string())?,
    )
    .map_err(|_| "ID token signing key was invalid".to_string())?;
    let mut validation = Validation::new(Algorithm::RS256);
    validation.set_audience(&[&config.client_id]);
    validation.set_issuer(&[config.issuer.as_str().trim_end_matches('/')]);
    validation.set_required_spec_claims(&["exp", "iss", "aud", "sub"]);
    let claims = decode::<IdClaims>(token, &key, &validation)
        .map_err(|_| "ID token signature or claims were invalid".to_string())?
        .claims;
    if !secure_eq(claims.nonce.as_bytes(), expected_nonce.as_bytes()) || claims.sub.is_empty() {
        return Err("ID token nonce did not match".into());
    }
    Ok(claims.sub)
}

fn random_urlsafe(bytes: usize) -> String {
    let mut value = vec![0_u8; bytes];
    OsRng.fill_bytes(&mut value);
    URL_SAFE_NO_PAD.encode(value)
}

fn secure_eq(left: &[u8], right: &[u8]) -> bool {
    left.len() == right.len() && bool::from(left.ct_eq(right))
}

fn session_key(environment: Environment) -> String {
    format!("oauth-session:{}", environment.as_str())
}

fn store_session(
    db_path: &Path,
    environment: Environment,
    session: &StoredOAuthSession,
) -> Result<(), String> {
    if session.access_token.is_empty()
        || session.id_token.is_empty()
        || session.access_token.len() > 65_536
        || session.id_token.len() > 65_536
        || session
            .refresh_token
            .as_ref()
            .is_some_and(|token| token.is_empty() || token.len() > 65_536)
    {
        return Err("AuthKit returned invalid session credentials".into());
    }
    let value = serde_json::to_string(session)
        .map_err(|_| "Could not save the desktop sign-in".to_string())?;
    let connection = Connection::open(db_path)
        .map_err(|_| "Could not open Clippy's private local database".to_string())?;
    crate::db::set_setting(&connection, &session_key(environment), &value)
        .map_err(|_| "Could not save the desktop sign-in".to_string())
}

fn load_session(db_path: &Path, environment: Environment) -> Option<StoredOAuthSession> {
    let connection = Connection::open(db_path).ok()?;
    let value = crate::db::get_setting(&connection, &session_key(environment))?;
    if value.len() > 200_000 {
        return None;
    }
    let session: StoredOAuthSession = serde_json::from_str(&value).ok()?;
    (!session.access_token.is_empty() && !session.id_token.is_empty()).then_some(session)
}

fn delete_session(db_path: &Path, environment: Environment) {
    if let Ok(connection) = Connection::open(db_path) {
        let _ = connection.execute(
            "DELETE FROM settings WHERE key = ?1",
            params![session_key(environment)],
        );
    }
}

#[derive(Clone, Deserialize, Serialize)]
struct StoredOAuthSession {
    access_token: String,
    id_token: String,
    refresh_token: Option<String>,
}

#[derive(Deserialize)]
struct TokenSet {
    access_token: String,
    id_token: String,
    refresh_token: Option<String>,
}

#[derive(Deserialize)]
struct RefreshTokenSet {
    access_token: String,
    refresh_token: Option<String>,
}

#[derive(Deserialize)]
struct AccessExpiry {
    exp: u64,
}

#[derive(Deserialize)]
struct IdClaims {
    sub: String,
    nonce: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_forged_callback_state() {
        assert_eq!(
            parse_callback_target("/auth/callback?code=x&state=wrong", "expected"),
            Err("AuthKit callback state did not match".into())
        );
    }

    #[test]
    fn access_token_refreshes_before_expiry_or_when_malformed() {
        let future = format!(
            "header.{}.signature",
            URL_SAFE_NO_PAD.encode(br#"{"exp":4000000000}"#)
        );
        let expired = format!(
            "header.{}.signature",
            URL_SAFE_NO_PAD.encode(br#"{"exp":1}"#)
        );
        assert!(!access_token_expires_soon(&future));
        assert!(access_token_expires_soon(&expired));
        assert!(access_token_expires_soon("not-a-jwt"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn parses_the_bounded_curl_token_response_without_exposing_its_body() {
        let response =
            parse_curl_token_response(b"{\"access_token\":\"secret\"}\n200".to_vec()).unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(response.body, br#"{"access_token":"secret"}"#);
        assert!(parse_curl_token_response(b"missing-status".to_vec()).is_err());
    }

    #[test]
    fn oauth_session_persists_in_the_private_database_without_keychain() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("clippy.db");
        crate::db::init(&path).unwrap();
        let expected = StoredOAuthSession {
            access_token: "access".into(),
            id_token: "identity".into(),
            refresh_token: Some("refresh".into()),
        };

        store_session(&path, Environment::Production, &expected).unwrap();
        let restored = load_session(&path, Environment::Production).unwrap();

        assert_eq!(restored.access_token, expected.access_token);
        assert_eq!(restored.id_token, expected.id_token);
        assert_eq!(restored.refresh_token, expected.refresh_token);
        assert!(load_session(&path, Environment::Staging).is_none());

        sign_out(Environment::Production, &path);
        assert!(load_session(&path, Environment::Production).is_none());
    }

    #[tokio::test]
    async fn stray_loopback_request_does_not_cancel_login() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let callback = tokio::spawn(async move { receive_callback(&listener, "expected").await });

        let mut stray = TcpStream::connect(address).await.unwrap();
        stray
            .write_all(b"GET /favicon.ico HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n")
            .await
            .unwrap();
        let mut response = Vec::new();
        stray.read_to_end(&mut response).await.unwrap();
        assert!(response.starts_with(b"HTTP/1.1 400"));
        if callback.is_finished() {
            panic!(
                "callback listener stopped after stray request: {:?}",
                callback.await.unwrap()
            );
        }

        let mut valid = TcpStream::connect(address).await.unwrap();
        valid
            .write_all(
                b"GET /auth/callback?code=valid&state=expected HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
            )
            .await
            .unwrap();

        assert_eq!(callback.await.unwrap(), Ok("valid".into()));
    }

}
