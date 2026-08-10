use super::auth::WorkOsVerifier;
use super::connect::{
    ConnectSessionStore, EnvironmentIdentity, MintResponse, RelayEndpoint, RelayProofVerifier,
    TokenResponse, WebsocketTicketResponse,
};
use super::crypto::{self, AuthenticatedPrincipal, PairingGrant, PairingResponse, PendingPairing};
use super::model::SyncPayload;
use super::status::{self, SyncState, SyncStatus};
use super::store;
use axum::body::Body;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::DefaultBodyLimit;
use axum::extract::{ConnectInfo, Path, Query, State};
use axum::http::{header, HeaderMap, Method, Response, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, head, post};
use axum::{Json, Router};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use subtle::ConstantTimeEq;
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::broadcast;
use uuid::Uuid;

const MAX_ENVELOPE_BYTES: usize = 8 * 1024 * 1024;
const MAX_HTTP_BODY_BYTES: usize = 12 * 1024 * 1024;
const MAX_MISSING_HASHES: usize = 1_024;

pub struct OriginState {
    pub workspace_id: String,
    pub db_path: PathBuf,
    pub chunks_dir: PathBuf,
    pub attachments_dir: PathBuf,
    pub key: crypto::WorkspaceKey,
    pub owner: AuthenticatedPrincipal,
    pub verifier: Option<WorkOsVerifier>,
    pub direct: Option<DirectSessionOrigin>,
    pub legacy_bearer_migration: bool,
    pub pending_pairing: Mutex<Option<PendingPairing>>,
    pub events: broadcast::Sender<()>,
    connected_peers: AtomicUsize,
    app: AppHandle,
    rate: Mutex<HashMap<String, RateWindow>>,
}

#[derive(Clone)]
pub struct DirectSessionOrigin {
    pub sessions: ConnectSessionStore,
    pub relay_verifier: RelayProofVerifier,
    pub identity: EnvironmentIdentity,
    pub endpoint: RelayEndpoint,
}

struct RateWindow {
    started: Instant,
    requests: u32,
}

impl OriginState {
    pub fn new(
        workspace_id: String,
        db_path: PathBuf,
        chunks_dir: PathBuf,
        attachments_dir: PathBuf,
        key: crypto::WorkspaceKey,
        owner: AuthenticatedPrincipal,
        verifier: Option<WorkOsVerifier>,
        direct: Option<DirectSessionOrigin>,
        legacy_bearer_migration: bool,
        app: AppHandle,
    ) -> Arc<Self> {
        let (events, _) = broadcast::channel(32);
        Arc::new(Self {
            workspace_id,
            db_path,
            chunks_dir,
            attachments_dir,
            key,
            owner,
            verifier,
            direct,
            legacy_bearer_migration,
            pending_pairing: Mutex::new(None),
            events,
            connected_peers: AtomicUsize::new(0),
            app,
            rate: Mutex::new(HashMap::new()),
        })
    }

    fn allow_request(&self, bucket: String, limit: u32) -> bool {
        allow_bucket(&self.rate, &bucket, limit)
    }

    fn unauthorized(&self, client: &str) -> ApiError {
        if self.allow_request(format!("unauth:{client}"), 120) {
            ApiError::Unauthorized
        } else {
            ApiError::TooManyRequests
        }
    }

    async fn authorize_http(
        &self,
        headers: &HeaderMap,
        address: SocketAddr,
        method: &Method,
        path: &str,
    ) -> Result<AuthenticatedPrincipal, ApiError> {
        let client = client_key(headers, address);
        let authorization = headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        let principal = if let Some(access_token) = authorization.strip_prefix("DPoP ") {
            let direct = self
                .direct
                .as_ref()
                .ok_or_else(|| self.unauthorized(&client))?;
            let proof = headers
                .get("dpop")
                .and_then(|value| value.to_str().ok())
                .ok_or_else(|| self.unauthorized(&client))?;
            let url = format!(
                "{}{}",
                direct.endpoint.http_base_url.trim_end_matches('/'),
                path
            );
            direct
                .sessions
                .authorize(access_token, proof, method.as_str(), &url)
                .map_err(|_| self.unauthorized(&client))?
        } else if let Some(access_token) = authorization.strip_prefix("Bearer ") {
            // Migration-only compatibility for pre-link clients. A linked
            // origin never enables this branch and DPoP is always evaluated
            // first, so bearer can neither override nor downgrade a session.
            if !self.legacy_bearer_migration {
                return Err(self.unauthorized(&client));
            }
            self.verifier
                .as_ref()
                .ok_or_else(|| self.unauthorized(&client))?
                .verify(access_token)
                .await
                .map_err(|_| self.unauthorized(&client))?
        } else {
            return Err(self.unauthorized(&client));
        };
        if !same_principal(&principal, &self.owner) {
            return Err(ApiError::Forbidden);
        }
        // A maximum-size attachment uses 256 one-MiB chunks, so the valid
        // principal bucket intentionally permits a complete resumable burst.
        if !self.allow_request(format!("auth:{client}:{}", principal.subject), 4_096) {
            return Err(ApiError::TooManyRequests);
        }
        Ok(principal)
    }

    fn authorize_ticket(
        &self,
        ticket: &str,
        headers: &HeaderMap,
        address: SocketAddr,
    ) -> Result<AuthenticatedPrincipal, ApiError> {
        let direct = self.direct.as_ref().ok_or(ApiError::Unauthorized)?;
        let client = client_key(headers, address);
        let principal = direct
            .sessions
            .consume_websocket_ticket(ticket)
            .map_err(|_| self.unauthorized(&client))?;
        if !same_principal(&principal, &self.owner) {
            return Err(ApiError::Forbidden);
        }
        if !self.allow_request(format!("auth:{client}:{}", principal.subject), 4_096) {
            return Err(ApiError::TooManyRequests);
        }
        Ok(principal)
    }
}

fn allow_bucket(windows: &Mutex<HashMap<String, RateWindow>>, bucket: &str, limit: u32) -> bool {
    windows
        .lock()
        .map(|mut rates| {
            rates.retain(|_, window| window.started.elapsed() < Duration::from_secs(120));
            let rate = rates.entry(bucket.to_string()).or_insert(RateWindow {
                started: Instant::now(),
                requests: 0,
            });
            if rate.started.elapsed() >= Duration::from_secs(60) {
                rate.started = Instant::now();
                rate.requests = 0;
            }
            rate.requests = rate.requests.saturating_add(1);
            rate.requests <= limit
        })
        .unwrap_or(false)
}

pub async fn serve_listener(
    state: Arc<OriginState>,
    listener: tokio::net::TcpListener,
) -> Result<(), std::io::Error> {
    let router = Router::new()
        .route("/health", get(|| async { StatusCode::NO_CONTENT }))
        .route("/v1/connect/mint", post(connect_mint))
        .route("/v1/connect/token", post(connect_token))
        .route(
            "/v1/connect/websocket-ticket",
            post(connect_websocket_ticket),
        )
        .route("/v1/sync/pair", post(pair))
        .route("/v1/sync/exchange", post(exchange))
        .route("/v1/sync/events", get(events))
        .route("/v1/sync/chunks/missing", post(missing_chunks))
        .route(
            "/v1/sync/chunks/{hash}",
            head(chunk_head).put(chunk_put).get(chunk_get),
        )
        .layer(DefaultBodyLimit::max(MAX_HTTP_BODY_BYTES))
        .with_state(state);
    axum::serve(
        listener,
        router.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MintRequest {
    proof: String,
}

async fn connect_mint(
    State(state): State<Arc<OriginState>>,
    headers: HeaderMap,
    ConnectInfo(address): ConnectInfo<SocketAddr>,
    Json(request): Json<MintRequest>,
) -> Result<Json<MintResponse>, ApiError> {
    let client = client_key(&headers, address);
    if headers.contains_key(header::AUTHORIZATION) || headers.contains_key("dpop") {
        return Err(ApiError::InvalidRequest);
    }
    if !state.allow_request(format!("mint:{client}"), 120) {
        return Err(ApiError::TooManyRequests);
    }
    let direct = state.direct.as_ref().ok_or(ApiError::NotFound)?;
    let response = direct
        .sessions
        .mint_from_relay(
            &request.proof,
            &direct.relay_verifier,
            &direct.identity,
            &state.owner,
            &direct.endpoint,
        )
        .map_err(|_| ApiError::Unauthorized)?;
    Ok(Json(response))
}

async fn connect_token(
    State(state): State<Arc<OriginState>>,
    headers: HeaderMap,
    ConnectInfo(address): ConnectInfo<SocketAddr>,
) -> Result<Json<TokenResponse>, ApiError> {
    let direct = state.direct.as_ref().ok_or(ApiError::NotFound)?;
    let client = client_key(&headers, address);
    let (credential, proof) =
        dpop_credentials(&headers).map_err(|_| state.unauthorized(&client))?;
    let url = format!(
        "{}/v1/connect/token",
        direct.endpoint.http_base_url.trim_end_matches('/')
    );
    let (response, principal) = direct
        .sessions
        .exchange_bootstrap(credential, proof, "POST", &url)
        .map_err(|_| state.unauthorized(&client))?;
    authorize_direct_principal(&state, &headers, address, &principal)?;
    Ok(Json(response))
}

async fn connect_websocket_ticket(
    State(state): State<Arc<OriginState>>,
    headers: HeaderMap,
    ConnectInfo(address): ConnectInfo<SocketAddr>,
) -> Result<Json<WebsocketTicketResponse>, ApiError> {
    let direct = state.direct.as_ref().ok_or(ApiError::NotFound)?;
    let client = client_key(&headers, address);
    let (access_token, proof) =
        dpop_credentials(&headers).map_err(|_| state.unauthorized(&client))?;
    let url = format!(
        "{}/v1/connect/websocket-ticket",
        direct.endpoint.http_base_url.trim_end_matches('/')
    );
    let (response, principal) = direct
        .sessions
        .issue_websocket_ticket(access_token, proof, "POST", &url)
        .map_err(|_| state.unauthorized(&client))?;
    authorize_direct_principal(&state, &headers, address, &principal)?;
    Ok(Json(response))
}

async fn pair(
    State(state): State<Arc<OriginState>>,
    headers: HeaderMap,
    ConnectInfo(address): ConnectInfo<SocketAddr>,
    Json(response): Json<PairingResponse>,
) -> Result<Json<PairingGrant>, ApiError> {
    let principal = state
        .authorize_http(&headers, address, &Method::POST, "/v1/sync/pair")
        .await?;
    let pending = {
        let mut guard = state
            .pending_pairing
            .lock()
            .map_err(|_| ApiError::Internal)?;
        let pending = guard.as_ref().ok_or(ApiError::PairingUnavailable)?;
        pending
            .validate_response(&response, &principal)
            .map_err(|_| ApiError::InvalidRequest)?;
        guard.take().ok_or(ApiError::PairingUnavailable)?
    };
    let grant = pending
        .complete(&response, &principal)
        .map_err(|_| ApiError::InvalidRequest)?;
    set_status(&state, SyncState::WaitingForDevice);
    Ok(Json(grant))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ExchangeRequest {
    device_id: String,
    envelope: crypto::SealedEnvelope,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ExchangeResponse {
    envelope: crypto::SealedEnvelope,
}

async fn exchange(
    State(state): State<Arc<OriginState>>,
    headers: HeaderMap,
    ConnectInfo(address): ConnectInfo<SocketAddr>,
    Json(request): Json<ExchangeRequest>,
) -> Result<Json<ExchangeResponse>, ApiError> {
    state
        .authorize_http(&headers, address, &Method::POST, "/v1/sync/exchange")
        .await?;
    Uuid::parse_str(&request.device_id).map_err(|_| ApiError::InvalidRequest)?;
    set_status(&state, SyncState::Syncing);
    let result = (|| {
        let aad = payload_aad(&state.workspace_id);
        let plaintext = crypto::open(&state.key, &request.envelope, &aad)
            .map_err(|_| ApiError::InvalidRequest)?;
        if plaintext.len() > MAX_ENVELOPE_BYTES {
            return Err(ApiError::PayloadTooLarge);
        }
        let payload: SyncPayload =
            serde_json::from_slice(&plaintext).map_err(|_| ApiError::InvalidRequest)?;
        if payload
            .operations
            .iter()
            .any(|operation| operation.dot.actor_id != request.device_id)
        {
            return Err(ApiError::InvalidRequest);
        }
        let mut connection = Connection::open(&state.db_path).map_err(|_| ApiError::Internal)?;
        let response = store::exchange(
            &mut connection,
            &state.workspace_id,
            &request.device_id,
            payload,
        )
        .map_err(|_| ApiError::InvalidRequest)?;
        store::project_ready_attachments(
            &mut connection,
            &state.workspace_id,
            &state.attachments_dir,
        )
        .map_err(|_| ApiError::InvalidRequest)?;
        let encoded = serde_json::to_vec(&response).map_err(|_| ApiError::Internal)?;
        let envelope = crypto::seal(&state.key, &encoded, &aad).map_err(|_| ApiError::Internal)?;
        let pending = store::pending_count(&connection, &state.workspace_id).unwrap_or(0);
        Ok((envelope, pending))
    })();
    match result {
        Ok((envelope, pending)) => {
            let _ = state.events.send(());
            let _ = state.app.emit("refresh", ());
            set_status(
                &state,
                if pending == 0 {
                    SyncState::Synced
                } else {
                    SyncState::WaitingForDevice
                },
            );
            Ok(Json(ExchangeResponse { envelope }))
        }
        Err(error) => {
            set_status(&state, SyncState::WaitingForDevice);
            Err(error)
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WebsocketTicketQuery {
    #[serde(rename = "wsTicket", alias = "ws_ticket")]
    ws_ticket: String,
}

async fn events(
    State(state): State<Arc<OriginState>>,
    headers: HeaderMap,
    ConnectInfo(address): ConnectInfo<SocketAddr>,
    Query(query): Query<WebsocketTicketQuery>,
    upgrade: WebSocketUpgrade,
) -> Result<impl IntoResponse, ApiError> {
    if headers.contains_key(header::AUTHORIZATION) || headers.contains_key("dpop") {
        return Err(ApiError::Unauthorized);
    }
    state.authorize_ticket(&query.ws_ticket, &headers, address)?;
    let pending = Connection::open(&state.db_path)
        .ok()
        .and_then(|connection| store::pending_count(&connection, &state.workspace_id).ok())
        .unwrap_or(1);
    set_status(
        &state,
        if pending == 0 {
            SyncState::Synced
        } else {
            SyncState::WaitingForDevice
        },
    );
    Ok(upgrade.on_upgrade(move |socket| event_socket(socket, state)))
}

async fn event_socket(mut socket: WebSocket, state: Arc<OriginState>) {
    state.connected_peers.fetch_add(1, Ordering::AcqRel);
    let mut receiver = state.events.subscribe();
    loop {
        tokio::select! {
            event = receiver.recv() => {
                if event.is_err() || socket.send(Message::Text("{\"type\":\"changes\"}".into())).await.is_err() {
                    break;
                }
            }
            _ = tokio::time::sleep(Duration::from_secs(30)) => {
                if socket.send(Message::Ping(Vec::new().into())).await.is_err() {
                    break;
                }
            }
            incoming = socket.recv() => {
                match incoming {
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                    _ => {}
                }
            }
        }
    }
    if state.connected_peers.fetch_sub(1, Ordering::AcqRel) == 1 {
        set_status(&state, SyncState::WaitingForDevice);
    }
}

async fn chunk_head(
    State(state): State<Arc<OriginState>>,
    headers: HeaderMap,
    ConnectInfo(address): ConnectInfo<SocketAddr>,
    Path(hash): Path<String>,
) -> Result<StatusCode, ApiError> {
    state
        .authorize_http(
            &headers,
            address,
            &Method::HEAD,
            &format!("/v1/sync/chunks/{hash}"),
        )
        .await?;
    validate_hash(&hash)?;
    let connection = Connection::open(&state.db_path).map_err(|_| ApiError::Internal)?;
    Ok(
        if store::chunk_source(&connection, &hash)
            .map_err(|_| ApiError::Internal)?
            .is_some()
        {
            StatusCode::NO_CONTENT
        } else {
            StatusCode::NOT_FOUND
        },
    )
}

async fn chunk_get(
    State(state): State<Arc<OriginState>>,
    headers: HeaderMap,
    ConnectInfo(address): ConnectInfo<SocketAddr>,
    Path(hash): Path<String>,
) -> Result<Json<crypto::SealedEnvelope>, ApiError> {
    state
        .authorize_http(
            &headers,
            address,
            &Method::GET,
            &format!("/v1/sync/chunks/{hash}"),
        )
        .await?;
    validate_hash(&hash)?;
    let connection = Connection::open(&state.db_path).map_err(|_| ApiError::Internal)?;
    let (path, offset, size) = store::chunk_source(&connection, &hash)
        .map_err(|_| ApiError::Internal)?
        .ok_or(ApiError::NotFound)?;
    if size > super::files::DEFAULT_CHUNK_SIZE {
        return Err(ApiError::InvalidRequest);
    }
    let mut file = File::open(path).map_err(|_| ApiError::NotFound)?;
    file.seek(SeekFrom::Start(offset))
        .map_err(|_| ApiError::Internal)?;
    let mut bytes = vec![0_u8; size];
    file.read_exact(&mut bytes)
        .map_err(|_| ApiError::Internal)?;
    if !super::files::verify_chunk(&hash, &bytes) {
        return Err(ApiError::Internal);
    }
    let envelope = crypto::seal(&state.key, &bytes, &chunk_aad(&state.workspace_id, &hash))
        .map_err(|_| ApiError::Internal)?;
    Ok(Json(envelope))
}

async fn chunk_put(
    State(state): State<Arc<OriginState>>,
    headers: HeaderMap,
    ConnectInfo(address): ConnectInfo<SocketAddr>,
    Path(hash): Path<String>,
    Json(envelope): Json<crypto::SealedEnvelope>,
) -> Result<StatusCode, ApiError> {
    state
        .authorize_http(
            &headers,
            address,
            &Method::PUT,
            &format!("/v1/sync/chunks/{hash}"),
        )
        .await?;
    validate_hash(&hash)?;
    let bytes = crypto::open(
        &state.key,
        &envelope,
        &chunk_aad(&state.workspace_id, &hash),
    )
    .map_err(|_| ApiError::InvalidRequest)?;
    if bytes.len() > super::files::DEFAULT_CHUNK_SIZE {
        return Err(ApiError::PayloadTooLarge);
    }
    let mut connection = Connection::open(&state.db_path).map_err(|_| ApiError::Internal)?;
    store::store_received_chunk(&connection, &state.chunks_dir, &hash, &bytes)
        .map_err(|_| ApiError::InvalidRequest)?;
    store::project_ready_attachments(&mut connection, &state.workspace_id, &state.attachments_dir)
        .map_err(|_| ApiError::InvalidRequest)?;
    let _ = state.app.emit("refresh", ());
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct MissingChunksRequest {
    hashes: Vec<String>,
}

#[derive(Serialize)]
struct MissingChunksResponse {
    hashes: Vec<String>,
}

async fn missing_chunks(
    State(state): State<Arc<OriginState>>,
    headers: HeaderMap,
    ConnectInfo(address): ConnectInfo<SocketAddr>,
    Json(request): Json<MissingChunksRequest>,
) -> Result<Json<MissingChunksResponse>, ApiError> {
    state
        .authorize_http(&headers, address, &Method::POST, "/v1/sync/chunks/missing")
        .await?;
    if request.hashes.len() > MAX_MISSING_HASHES {
        return Err(ApiError::PayloadTooLarge);
    }
    let mut unique = HashSet::with_capacity(request.hashes.len());
    for hash in request.hashes {
        validate_hash(&hash)?;
        unique.insert(hash);
    }
    let connection = Connection::open(&state.db_path).map_err(|_| ApiError::Internal)?;
    let mut missing = Vec::new();
    for hash in unique {
        if store::chunk_source(&connection, &hash)
            .map_err(|_| ApiError::Internal)?
            .is_none()
        {
            missing.push(hash);
        }
    }
    missing.sort();
    Ok(Json(MissingChunksResponse { hashes: missing }))
}

fn set_status(state: &OriginState, value: SyncState) {
    status::set(&state.app, &state.app.state::<SyncStatus>(), value);
}

fn validate_hash(hash: &str) -> Result<(), ApiError> {
    if hash.len() == 64
        && hash
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(())
    } else {
        Err(ApiError::InvalidRequest)
    }
}

fn payload_aad(workspace_id: &str) -> Vec<u8> {
    length_prefixed(&["clippy-sync-payload", "1", workspace_id])
}

fn chunk_aad(workspace_id: &str, hash: &str) -> Vec<u8> {
    length_prefixed(&["clippy-sync-chunk", "1", workspace_id, hash])
}

fn length_prefixed(fields: &[&str]) -> Vec<u8> {
    let mut output = Vec::new();
    for field in fields {
        output.extend_from_slice(&(field.len() as u32).to_be_bytes());
        output.extend_from_slice(field.as_bytes());
    }
    output
}

fn client_key(headers: &HeaderMap, address: SocketAddr) -> String {
    headers
        .get("cf-connecting-ip")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<std::net::IpAddr>().ok())
        .map(|value| value.to_string())
        .unwrap_or_else(|| address.ip().to_string())
}

fn dpop_credentials(headers: &HeaderMap) -> Result<(&str, &str), ApiError> {
    let access_token = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("DPoP "))
        .filter(|value| !value.is_empty() && !value.contains('\n') && !value.contains('\r'))
        .ok_or(ApiError::Unauthorized)?;
    let proof = headers
        .get("dpop")
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty())
        .ok_or(ApiError::Unauthorized)?;
    Ok((access_token, proof))
}

fn authorize_direct_principal(
    state: &OriginState,
    headers: &HeaderMap,
    address: SocketAddr,
    principal: &AuthenticatedPrincipal,
) -> Result<(), ApiError> {
    if !same_principal(principal, &state.owner) {
        return Err(ApiError::Forbidden);
    }
    let client = client_key(headers, address);
    if !state.allow_request(format!("auth:{client}:{}", principal.subject), 4_096) {
        return Err(ApiError::TooManyRequests);
    }
    Ok(())
}

fn same_principal(left: &AuthenticatedPrincipal, right: &AuthenticatedPrincipal) -> bool {
    left.subject.len() == right.subject.len()
        && bool::from(left.subject.as_bytes().ct_eq(right.subject.as_bytes()))
        && left.organization_id == right.organization_id
}

#[derive(Debug)]
enum ApiError {
    Unauthorized,
    Forbidden,
    TooManyRequests,
    PairingUnavailable,
    InvalidRequest,
    PayloadTooLarge,
    NotFound,
    Internal,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response<Body> {
        let status = match self {
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::Forbidden => StatusCode::FORBIDDEN,
            Self::TooManyRequests => StatusCode::TOO_MANY_REQUESTS,
            Self::PairingUnavailable | Self::NotFound => StatusCode::NOT_FOUND,
            Self::InvalidRequest => StatusCode::BAD_REQUEST,
            Self::PayloadTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            Self::Internal => StatusCode::INTERNAL_SERVER_ERROR,
        };
        status.into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aad_matches_swift_length_prefixed_contract() {
        let workspace = "workspace-123";
        let hash = "00".repeat(32);
        assert_eq!(
            payload_aad(workspace),
            length_prefixed(&["clippy-sync-payload", "1", workspace])
        );
        assert_eq!(
            payload_aad(workspace)
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>(),
            "00000013636c697070792d73796e632d7061796c6f616400000001310000000d776f726b73706163652d313233"
        );
        assert_eq!(
            chunk_aad(workspace, &hash),
            length_prefixed(&["clippy-sync-chunk", "1", workspace, &hash])
        );
        assert_ne!(payload_aad(workspace), payload_aad("workspace-124"));
    }

    #[test]
    fn authenticated_chunk_burst_does_not_consume_unauthenticated_bucket() {
        let rates = Mutex::new(HashMap::new());
        for _ in 0..256 {
            assert!(allow_bucket(&rates, "auth:peer", 4_096));
        }
        let rates = rates.lock().unwrap();
        assert_eq!(
            rates.get("auth:peer").map(|window| window.requests),
            Some(256)
        );
        assert!(!rates.contains_key("unauth:peer"));
    }

    #[test]
    fn direct_session_credentials_never_fall_back_to_bearer() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            "DPoP environment-token".parse().unwrap(),
        );
        headers.insert("dpop", "proof.jwt.value".parse().unwrap());
        assert_eq!(
            dpop_credentials(&headers).unwrap(),
            ("environment-token", "proof.jwt.value")
        );

        headers.insert(
            header::AUTHORIZATION,
            "Bearer legacy-token".parse().unwrap(),
        );
        assert!(matches!(
            dpop_credentials(&headers),
            Err(ApiError::Unauthorized)
        ));
    }
}
