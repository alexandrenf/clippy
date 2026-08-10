pub mod auth;
pub mod auth_login;
pub mod config;
pub mod connect;
pub mod crypto;
pub mod files;
pub mod link;
pub mod model;
pub mod origin;
pub mod schedule;
pub mod status;
pub mod store;
pub mod tunnel;

use config::{Environment, SyncConfig};
use crypto::WorkspaceKey;
use rusqlite::Connection;
use serde::Serialize;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{AppHandle, Manager, State};
use tokio::sync::Notify;
use uuid::Uuid;

const KEYCHAIN_SERVICE: &str = "app.clippy.desktop.sync";
const AUTH_KEYCHAIN_SERVICE: &str = "app.clippy.desktop.auth.v2";

pub struct SyncRuntime {
    db_path: PathBuf,
    data_dir: PathBuf,
    active: Mutex<Option<ActiveSync>>,
    scan_wake: Arc<Notify>,
}

struct ActiveSync {
    config: SyncConfig,
    origin: Arc<origin::OriginState>,
    _tunnel: Arc<Mutex<tunnel::TunnelRunner>>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncSignIn {
    environment: &'static str,
    endpoint: String,
}

/// Installs the coordinator without creating credentials or network resources.
/// Production is the normal default; environment variables remain explicit
/// development overrides. If a prior workspace is configured, startup restores
/// its origin and outbound-only tunnel so an active phone can wake the Mac via
/// WebSocket without hot polling.
pub fn initialize(app: &AppHandle, db_path: PathBuf, data_dir: PathBuf) {
    app.manage(SyncRuntime {
        db_path,
        data_dir,
        active: Mutex::new(None),
        scan_wake: Arc::new(Notify::new()),
    });
    status::set(
        app,
        &app.state::<status::SyncStatus>(),
        status::SyncState::Idle,
    );

    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let runtime = app.state::<SyncRuntime>();
        let environment = selected_environment(&runtime.db_path).unwrap_or(Environment::Production);
        if workspace_id(&runtime.db_path, environment, false)
            .ok()
            .flatten()
            .is_none()
        {
            return;
        }
        let mut retry_index = 0_usize;
        loop {
            match activate(&app, &runtime, environment, false).await {
                Ok(_) => return,
                Err(error)
                    if error.contains("sign-in expired") || error.contains("Sign in first") =>
                {
                    status::set(
                        &app,
                        &app.state::<status::SyncStatus>(),
                        status::SyncState::Idle,
                    );
                    return;
                }
                Err(_) => {
                    status::set(
                        &app,
                        &app.state::<status::SyncStatus>(),
                        status::SyncState::WaitingForDevice,
                    );
                    tokio::time::sleep(tunnel::retry_delay(retry_index)).await;
                    retry_index = retry_index.saturating_add(1);
                }
            }
        }
    });
}

#[tauri::command]
pub async fn sign_in_sync(
    app: AppHandle,
    runtime: State<'_, SyncRuntime>,
    environment: Option<String>,
) -> Result<SyncSignIn, String> {
    let environment = environment
        .as_deref()
        .map(Environment::parse)
        .transpose()
        .map_err(|_| "Choose staging or production".to_string())?
        .unwrap_or(Environment::Production);
    if runtime
        .active
        .lock()
        .map_err(|_| "Sync runtime is busy")?
        .is_some()
    {
        return Err("Restart Clippy before changing the signed-in sync environment".into());
    }
    auth_login::sign_in(environment).await?;
    let endpoint = link::link(environment, &runtime.db_path, "Clippy on this Mac").await?;
    activate(&app, &runtime, environment, true).await?;
    Ok(SyncSignIn {
        environment: environment.as_str(),
        endpoint,
    })
}

#[tauri::command]
pub async fn sync_auth_status(environment: Option<String>) -> Result<bool, String> {
    let environment = environment
        .as_deref()
        .map(Environment::parse)
        .transpose()
        .map_err(|_| "Choose staging or production".to_string())?
        .unwrap_or(Environment::Production);
    let linked =
        load_keychain(&format!("connect:{}:environment-id", environment.as_str())).is_some();
    Ok(linked && auth_login::is_signed_in(environment).await)
}

#[tauri::command]
pub fn sync_status(status: State<'_, status::SyncStatus>) -> status::SyncState {
    status::get(&status)
}

/// Called by the existing mutation refresh hook. Local writes schedule one
/// immediate scanner pass; the long timers are only a safety net for out-of-
/// process database changes.
pub fn wake(app: &AppHandle) {
    if let Some(runtime) = app.try_state::<SyncRuntime>() {
        runtime.scan_wake.notify_one();
    }
}

async fn activate(
    app: &AppHandle,
    runtime: &SyncRuntime,
    environment: Environment,
    create_workspace: bool,
) -> Result<(SyncConfig, Arc<origin::OriginState>), String> {
    if let Some(active) = runtime
        .active
        .lock()
        .map_err(|_| "Sync runtime is busy")?
        .as_ref()
    {
        if active.config.environment != environment {
            return Err("Restart Clippy before switching sync environments".into());
        }
        return Ok((active.config.clone(), active.origin.clone()));
    }

    let workspace_id = workspace_id(&runtime.db_path, environment, create_workspace)?
        .ok_or_else(|| "No sync workspace has been paired yet".to_string())?;
    let config = SyncConfig::for_environment(environment, workspace_id)
        .map_err(|_| "Sync public configuration is invalid".to_string())?;
    let (owner, verifier, direct, legacy_bearer_migration) = if let Some(linked) = &config.linked {
        let owner = crypto::AuthenticatedPrincipal {
            subject: linked.owner_subject.clone(),
            organization_id: linked.owner_organization.clone(),
        };
        let relay_verifier = connect::RelayProofVerifier::new(
            linked.relay_issuer.clone(),
            config.workspace_id.clone(),
            &linked.relay_signing_public_jwk,
        )
        .map_err(|_| "Pinned relay trust is invalid")?;
        let identity = connect::EnvironmentIdentity::load_or_create(environment.as_str())
            .map_err(|_| "Could not load the environment signing identity")?;
        let endpoint = connect::RelayEndpoint {
            http_base_url: config.endpoint.http_base_url.clone(),
            ws_base_url: config.endpoint.ws_base_url.clone(),
        };
        (
            owner,
            None,
            Some(origin::DirectSessionOrigin {
                sessions: connect::ConnectSessionStore::new(),
                relay_verifier,
                identity,
                endpoint,
            }),
            false,
        )
    } else {
        // Pre-link migration only. Once environment identity exists in
        // Keychain, startup never consults or prefers a WorkOS bearer token.
        let verifier = auth::WorkOsVerifier::new(
            config.workos_issuer.to_string(),
            config.workos_audience.clone(),
        )
        .map_err(|_| "WorkOS configuration is invalid".to_string())?;
        let access_token = load_keychain_from(
            AUTH_KEYCHAIN_SERVICE,
            &format!("workos:{}:access-token", environment.as_str()),
        )
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .ok_or_else(|| {
            format!(
                "Sign in from Clippy Settings before pairing {}",
                environment.as_str()
            )
        })?;
        let owner = verifier
            .verify(&access_token)
            .await
            .map_err(|error| match error {
                auth::AuthError::InvalidToken => {
                    "Desktop sign-in expired; sign in again from Clippy Settings".to_string()
                }
                auth::AuthError::JwksUnavailable => {
                    "WorkOS signing keys are temporarily unavailable".to_string()
                }
                auth::AuthError::Configuration => "WorkOS configuration is invalid".to_string(),
            })?;
        (owner, Some(verifier), None, true)
    };

    let key = match crypto::load_workspace_key(&config.workspace_id)
        .map_err(|_| "Could not read the workspace key from Keychain")?
    {
        Some(key) => key,
        None if create_workspace => {
            let key = WorkspaceKey::random();
            crypto::store_workspace_key(&config.workspace_id, &key)
                .map_err(|_| "Could not store the workspace key in Keychain")?;
            key
        }
        None => return Err("The workspace key is missing; pair this Mac again".into()),
    };

    let mut connection =
        Connection::open(&runtime.db_path).map_err(|_| "Could not open the local sync database")?;
    let actor =
        store::ensure_identity(&connection).map_err(|_| "Could not create a device identity")?;
    let emitted = store::scan_local(&mut connection, &config.workspace_id, &actor)
        .map_err(|_| "Could not scan local notes for sync")?;

    let chunks_dir = runtime
        .data_dir
        .join("sync")
        .join(environment.as_str())
        .join("chunks");
    let origin = origin::OriginState::new(
        config.workspace_id.clone(),
        runtime.db_path.clone(),
        chunks_dir,
        runtime.data_dir.join("attachments"),
        key,
        config.endpoint.http_base_url.clone(),
        config
            .workos_issuer
            .as_str()
            .trim_end_matches('/')
            .to_string(),
        config.workos_audience.clone(),
        owner,
        verifier,
        direct,
        legacy_bearer_migration,
        app.clone(),
    );
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", config.origin_port))
        .await
        .map_err(|_| format!("Sync origin port {} is unavailable", config.origin_port))?;

    let mut runner = tunnel::TunnelRunner::new(environment.as_str())
        .map_err(|_| "Cloudflare tunnel configuration is invalid")?;
    runner.start_if_needed().map_err(|_| {
        format!(
            "Cloudflare tunnel is unavailable; check the {} Keychain token and cloudflared",
            environment.as_str()
        )
    })?;
    let runner = Arc::new(Mutex::new(runner));

    {
        let mut active = runtime.active.lock().map_err(|_| "Sync runtime is busy")?;
        if active.is_some() {
            return Err("Sync was activated concurrently; try again".into());
        }
        *active = Some(ActiveSync {
            config: config.clone(),
            origin: origin.clone(),
            _tunnel: runner.clone(),
        });
    }
    save_environment(&runtime.db_path, environment)?;

    let origin_for_server = origin.clone();
    tauri::async_runtime::spawn(async move {
        let _ = origin::serve_listener(origin_for_server, listener).await;
    });
    spawn_tunnel_monitor(app.clone(), runner);
    spawn_scanner(
        app.clone(),
        runtime.db_path.clone(),
        config.workspace_id.clone(),
        actor,
        runtime.scan_wake.clone(),
        origin.clone(),
    );

    status::set(
        app,
        &app.state::<status::SyncStatus>(),
        if emitted == 0 {
            status::SyncState::Synced
        } else {
            status::SyncState::WaitingForDevice
        },
    );
    Ok((config, origin))
}

fn spawn_scanner(
    app: AppHandle,
    db_path: PathBuf,
    workspace_id: String,
    actor: String,
    wake: Arc<Notify>,
    origin: Arc<origin::OriginState>,
) {
    tauri::async_runtime::spawn(async move {
        loop {
            let visible = app
                .get_webview_window("main")
                .and_then(|window| window.is_visible().ok())
                .unwrap_or(false);
            let safety_delay = if visible {
                Duration::from_secs(30)
            } else {
                Duration::from_secs(5 * 60)
            };
            tokio::select! {
                _ = wake.notified() => {}
                _ = tokio::time::sleep(safety_delay) => {}
            }
            let emitted = (|| {
                let mut connection = Connection::open(&db_path).ok()?;
                store::scan_local(&mut connection, &workspace_id, &actor).ok()
            })()
            .unwrap_or(0);
            if emitted > 0 {
                let _ = origin.events.send(());
                status::set(
                    &app,
                    &app.state::<status::SyncStatus>(),
                    status::SyncState::WaitingForDevice,
                );
            }
        }
    });
}

fn spawn_tunnel_monitor(app: AppHandle, runner: Arc<Mutex<tunnel::TunnelRunner>>) {
    tauri::async_runtime::spawn(async move {
        let mut retry_index = 0_usize;
        let mut stable_since = std::time::Instant::now();
        loop {
            let running = runner
                .lock()
                .map(|mut runner| runner.is_running())
                .unwrap_or(false);
            if running {
                if stable_since.elapsed() >= Duration::from_secs(30) {
                    retry_index = 0;
                }
                tokio::time::sleep(Duration::from_secs(5)).await;
            } else {
                status::set(
                    &app,
                    &app.state::<status::SyncStatus>(),
                    status::SyncState::WaitingForDevice,
                );
                tokio::time::sleep(tunnel::retry_delay(retry_index)).await;
                let restarted = runner
                    .lock()
                    .map(|mut runner| runner.start_if_needed().is_ok())
                    .unwrap_or(false);
                if restarted {
                    stable_since = std::time::Instant::now();
                } else {
                    retry_index = retry_index.saturating_add(1);
                }
            }
        }
    });
}

fn selected_environment(db_path: &PathBuf) -> Option<Environment> {
    if let Ok(value) = std::env::var("CLIPPY_SYNC_ENVIRONMENT") {
        return Environment::parse(&value).ok();
    }
    let connection = Connection::open(db_path).ok()?;
    crate::db::get_setting(&connection, "sync_environment")
        .as_deref()
        .and_then(|value| Environment::parse(value).ok())
        .or(Some(Environment::Production))
}

fn workspace_id(
    db_path: &PathBuf,
    environment: Environment,
    create: bool,
) -> Result<Option<String>, String> {
    if let Ok(value) = std::env::var("CLIPPY_SYNC_WORKSPACE_ID") {
        if Uuid::parse_str(&value).is_err() {
            return Err("CLIPPY_SYNC_WORKSPACE_ID is not a UUID".into());
        }
        return Ok(Some(value));
    }
    let connection = Connection::open(db_path).map_err(|_| "Could not open settings")?;
    let setting = format!("sync_workspace_id:{}", environment.as_str());
    if let Some(value) = crate::db::get_setting(&connection, &setting) {
        if Uuid::parse_str(&value).is_ok() {
            return Ok(Some(value));
        }
    }
    if !create {
        return Ok(None);
    }
    let value = Uuid::new_v4().to_string();
    crate::db::set_setting(&connection, &setting, &value)
        .map_err(|_| "Could not save the workspace identity")?;
    Ok(Some(value))
}

fn save_environment(db_path: &PathBuf, environment: Environment) -> Result<(), String> {
    let connection = Connection::open(db_path).map_err(|_| "Could not open settings")?;
    crate::db::set_setting(&connection, "sync_environment", environment.as_str())
        .map_err(|_| "Could not save the sync environment".to_string())
}

#[cfg(target_os = "macos")]
fn load_keychain(account: &str) -> Option<Vec<u8>> {
    load_keychain_from(KEYCHAIN_SERVICE, account)
}

#[cfg(target_os = "macos")]
fn load_keychain_from(service: &str, account: &str) -> Option<Vec<u8>> {
    security_framework::passwords::get_generic_password(service, account).ok()
}

/// Sync persistence is additive: local tables and numeric IDs remain stable,
/// while immutable operations, causal frontiers, peer acknowledgements, and
/// encrypted chunk indexes live in separate tables.
pub fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS sync_operations(
           actor_id TEXT NOT NULL,
           counter INTEGER NOT NULL CHECK(counter > 0),
           workspace_id TEXT NOT NULL,
           entity_kind TEXT NOT NULL,
           entity_id TEXT NOT NULL,
           operation_json TEXT NOT NULL,
           received_at INTEGER NOT NULL,
           PRIMARY KEY(workspace_id, actor_id, counter)
         );
         CREATE INDEX IF NOT EXISTS idx_sync_operations_workspace
           ON sync_operations(workspace_id, actor_id, counter);
         CREATE TABLE IF NOT EXISTS sync_frontier(
           workspace_id TEXT NOT NULL,
           actor_id TEXT NOT NULL,
           counter INTEGER NOT NULL,
           PRIMARY KEY(workspace_id, actor_id)
         );
         CREATE TABLE IF NOT EXISTS sync_content_conflicts(
           workspace_id TEXT NOT NULL,
           entity_id TEXT NOT NULL,
           versions_json TEXT NOT NULL,
           updated_at INTEGER NOT NULL,
           PRIMARY KEY(workspace_id, entity_id)
         );
         CREATE TABLE IF NOT EXISTS sync_file_chunks(
           sha256 TEXT PRIMARY KEY,
           size INTEGER NOT NULL,
           stored_path TEXT NOT NULL,
           verified_at INTEGER NOT NULL
         );",
    )?;
    store::migrate(conn)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migration_is_idempotent() {
        let conn = crate::db::init(std::path::Path::new(":memory:")).unwrap();
        migrate(&conn).unwrap();
        migrate(&conn).unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name LIKE 'sync_%'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(count >= 8);
    }
}
