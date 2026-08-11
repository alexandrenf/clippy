pub mod auth;
pub mod auth_login;
pub mod cloud;
pub mod config;
pub mod crypto;
pub mod files;
pub mod model;
mod serde_u64;
pub mod status;
pub mod store;

use cloud::{CloudBatch, CloudClient};
use config::{Environment, SyncConfig};
use crypto::{
    AccountEnrollment, AuthenticatedPrincipal, PairingResponse, PendingPairing, SealedEnvelope,
    WorkspaceKey,
};
use model::SyncPayload;
use rusqlite::Connection;
use serde::Serialize;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::Notify;
use uuid::Uuid;

const MAX_BATCH_PLAINTEXT_BYTES: usize = 550_000;
const MAX_CHUNKS_PER_PASS: usize = 64;
const ENROLLMENT_TTL_MS: u64 = 10 * 60 * 1_000;

pub struct SyncRuntime {
    db_path: PathBuf,
    data_dir: PathBuf,
    active: Mutex<Option<ActiveSync>>,
    scan_wake: Arc<Notify>,
}

struct ActiveSync {
    config: SyncConfig,
    cancelled: Arc<AtomicBool>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncSignIn {
    environment: &'static str,
    endpoint: String,
}

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
        let retry_delays = [5_u64, 15, 30, 60, 5 * 60];
        let mut retry = 0;
        loop {
            if !auth_login::is_signed_in(environment, &runtime.db_path).await {
                eprintln!("clippy sync activation skipped: no valid local session");
                break;
            }
            match activate(&app, &runtime, environment, false).await {
                Ok(_) => break,
                Err(error) => eprintln!("clippy sync activation failed: {error}"),
            }
            tokio::time::sleep(Duration::from_secs(retry_delays[retry])).await;
            retry = (retry + 1).min(retry_delays.len() - 1);
        }
    });
}

pub fn shutdown(_app: &AppHandle) {}

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
    {
        let active = runtime.active.lock().map_err(|_| "Sync runtime is busy")?;
        if active
            .as_ref()
            .is_some_and(|active| active.config.environment != environment)
        {
            return Err("Restart Clippy before changing the sync environment".into());
        }
    }
    if !auth_login::is_signed_in(environment, &runtime.db_path).await {
        auth_login::sign_in(environment, &runtime.db_path).await?;
        let mut active = runtime.active.lock().map_err(|_| "Sync runtime is busy")?;
        if let Some(session) = active.take() {
            session.cancelled.store(true, Ordering::Release);
            runtime.scan_wake.notify_one();
        }
    }
    let config = activate(&app, &runtime, environment, true).await?;
    Ok(SyncSignIn {
        environment: environment.as_str(),
        endpoint: config
            .convex_url
            .to_string()
            .trim_end_matches('/')
            .to_string(),
    })
}

#[tauri::command]
pub async fn sync_auth_status(
    runtime: State<'_, SyncRuntime>,
    environment: Option<String>,
) -> Result<bool, String> {
    let environment = environment
        .as_deref()
        .map(Environment::parse)
        .transpose()
        .map_err(|_| "Choose staging or production".to_string())?
        .unwrap_or(Environment::Production);
    Ok(auth_login::is_signed_in(environment, &runtime.db_path).await)
}

#[tauri::command]
pub fn sign_out_sync(
    app: AppHandle,
    runtime: State<'_, SyncRuntime>,
    environment: Option<String>,
) -> Result<(), String> {
    let environment = environment
        .as_deref()
        .map(Environment::parse)
        .transpose()
        .map_err(|_| "Choose staging or production".to_string())?
        .unwrap_or(Environment::Production);
    let mut active = runtime.active.lock().map_err(|_| "Sync runtime is busy")?;
    if active
        .as_ref()
        .is_some_and(|session| session.config.environment == environment)
    {
        if let Some(session) = active.take() {
            session.cancelled.store(true, Ordering::Release);
            runtime.scan_wake.notify_one();
        }
    }
    auth_login::sign_out(environment, &runtime.db_path);
    status::set(
        &app,
        &app.state::<status::SyncStatus>(),
        status::SyncState::Idle,
    );
    Ok(())
}

#[tauri::command]
pub fn sync_status(status: State<'_, status::SyncStatus>) -> status::SyncState {
    status::get(&status)
}

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
) -> Result<SyncConfig, String> {
    if let Some(active) = runtime
        .active
        .lock()
        .map_err(|_| "Sync runtime is busy")?
        .as_ref()
    {
        return Ok(active.config.clone());
    }
    let local_workspace_id = workspace_id(&runtime.db_path, environment, false)?;
    let provisional_workspace_id = local_workspace_id
        .clone()
        .unwrap_or_else(|| Uuid::nil().to_string());
    let public_config = SyncConfig::for_environment(environment, provisional_workspace_id)
        .map_err(|_| "Sync public configuration is invalid".to_string())?;
    let mut token = auth_login::access_token(environment, &runtime.db_path)?;
    if auth_login::access_token_expires_soon(&token) {
        token = auth_login::refresh_access_token(environment, &runtime.db_path).await?;
    }
    let verifier = auth::WorkOsVerifier::new(
        public_config.workos_issuer.to_string(),
        public_config.workos_audience.clone(),
    )
    .map_err(|_| "WorkOS configuration is invalid")?;
    let owner = verifier
        .verify(&token)
        .await
        .map_err(|_| "Desktop sign-in expired; sign in again from Clippy Settings")?;
    let cloud = CloudClient::connect(
        public_config.convex_url.as_str(),
        environment,
        token,
        &runtime.db_path,
    )
    .await
    .map_err(|_| "Could not connect to Convex")?;
    let mut connection =
        Connection::open(&runtime.db_path).map_err(|_| "Could not open the local sync database")?;
    let actor =
        store::ensure_identity(&connection).map_err(|_| "Could not create a device identity")?;
    let account_workspace = cloud
        .account_workspace()
        .await
        .map_err(|_| "Could not read the account workspace from Convex")?;
    let (config, key) = if let Some(account_workspace) = account_workspace {
        join_account_workspace(
            &runtime.db_path,
            environment,
            &account_workspace.workspace_id,
            &actor,
            &owner,
            &cloud,
        )
        .await?
    } else {
        if !create_workspace {
            return Err("No sync workspace exists yet".into());
        }
        let workspace_id = match local_workspace_id {
            Some(workspace_id) => workspace_id,
            None => {
                let workspace_id = Uuid::new_v4().to_string();
                save_workspace_id(&runtime.db_path, environment, &workspace_id)?;
                workspace_id
            }
        };
        let config = SyncConfig::for_environment(environment, workspace_id.clone())
            .map_err(|_| "Sync public configuration is invalid".to_string())?;
        let key = match crypto::load_workspace_key(&workspace_id)
            .map_err(|_| "Could not read the workspace key from Keychain")?
        {
            Some(key) => key,
            None => {
                let key = WorkspaceKey::random();
                crypto::store_workspace_key(&workspace_id, &key)
                    .map_err(|_| "Could not store the workspace key in Keychain")?;
                key
            }
        };
        match cloud
            .bootstrap(&workspace_id, &actor, "Clippy on this Mac")
            .await
        {
            Ok(()) => (config, key),
            Err(_) => {
                let raced_workspace = cloud
                    .account_workspace()
                    .await
                    .map_err(|_| "Convex rejected this sync workspace")?
                    .ok_or_else(|| "Convex rejected this sync workspace".to_string())?;
                join_account_workspace(
                    &runtime.db_path,
                    environment,
                    &raced_workspace.workspace_id,
                    &actor,
                    &owner,
                    &cloud,
                )
                .await?
            }
        }
    };
    store::scan_local(&mut connection, &config.workspace_id, &actor)
        .map_err(|_| "Could not scan local notes for sync")?;
    save_environment(&runtime.db_path, environment)?;
    let cancelled = Arc::new(AtomicBool::new(false));
    {
        let mut active = runtime.active.lock().map_err(|_| "Sync runtime is busy")?;
        if active.is_none() {
            *active = Some(ActiveSync {
                config: config.clone(),
                cancelled: cancelled.clone(),
            });
        }
    }
    spawn_coordinator(
        app.clone(),
        runtime.db_path.clone(),
        runtime.data_dir.clone(),
        config.clone(),
        actor,
        key,
        owner,
        cloud,
        runtime.scan_wake.clone(),
        cancelled,
    );
    Ok(config)
}

async fn join_account_workspace(
    db_path: &PathBuf,
    environment: Environment,
    workspace_id: &str,
    actor: &str,
    owner: &AuthenticatedPrincipal,
    cloud: &CloudClient,
) -> Result<(SyncConfig, WorkspaceKey), String> {
    if let Ok(configured) = std::env::var("CLIPPY_SYNC_WORKSPACE_ID") {
        if configured != workspace_id {
            return Err("CLIPPY_SYNC_WORKSPACE_ID does not match this account".into());
        }
    }
    save_workspace_id(db_path, environment, workspace_id)?;
    let config = SyncConfig::for_environment(environment, workspace_id.to_string())
        .map_err(|_| "Sync public configuration is invalid".to_string())?;
    if let Some(key) = crypto::load_workspace_key(workspace_id)
        .map_err(|_| "Could not read the workspace key from Keychain")?
    {
        let enrolled = cloud
            .is_device_enrolled(workspace_id, actor)
            .await
            .map_err(|_| "Could not verify this Mac with Convex")?;
        if !enrolled {
            cloud
                .bootstrap(workspace_id, actor, "Clippy on this Mac")
                .await
                .map_err(|_| "Convex rejected this Mac")?;
        }
        return Ok((config, key));
    }

    let key = enroll_desktop_device(&config, actor, owner, cloud).await?;
    crypto::store_workspace_key(workspace_id, &key)
        .map_err(|_| "Could not store the workspace key in Keychain")?;
    Ok((config, key))
}

async fn enroll_desktop_device(
    config: &SyncConfig,
    actor: &str,
    owner: &AuthenticatedPrincipal,
    cloud: &CloudClient,
) -> Result<WorkspaceKey, String> {
    let enrollment = AccountEnrollment::new();
    let enrollment_id = Uuid::new_v4().to_string();
    let requested = cloud
        .request_enrollment(
            &enrollment_id,
            actor,
            "Clippy on this Mac",
            &enrollment.public_key,
            true,
        )
        .await
        .map_err(|_| "Could not request secure account enrollment")?;
    if requested.state == "noWorkspace" {
        return Err("No account workspace exists yet".into());
    }
    if requested.workspace_id.as_deref() != Some(config.workspace_id.as_str()) {
        return Err("Convex returned the wrong account workspace".into());
    }

    let deadline = tokio::time::Instant::now() + Duration::from_secs(10 * 60 + 15);
    let mut delay = 1_u64;
    loop {
        if tokio::time::Instant::now() >= deadline {
            return Err("No signed-in Clippy device approved this Mac in time".into());
        }
        let status = cloud
            .enrollment_status(&enrollment_id, actor)
            .await
            .map_err(|_| "Could not read secure account enrollment")?;
        if let Some(status) = status {
            if status.state == "expired" {
                return Err("Secure account enrollment expired".into());
            }
            if status.state == "granted" {
                let offer = status
                    .offer
                    .ok_or_else(|| "Enrollment grant is incomplete".to_string())?;
                let grant = status
                    .grant
                    .ok_or_else(|| "Enrollment grant is incomplete".to_string())?;
                if status.workspace_id.as_deref() != Some(config.workspace_id.as_str())
                    || offer.workspace_id != config.workspace_id
                    || offer.sync_url.trim_end_matches('/')
                        != config.convex_url.as_str().trim_end_matches('/')
                    || offer.workos_issuer.trim_end_matches('/')
                        != config.workos_issuer.as_str().trim_end_matches('/')
                    || offer.workos_audience != config.workos_audience
                    || offer.expires_at_ms < now_ms()
                {
                    return Err("Enrollment grant does not match this account".into());
                }
                let key = enrollment
                    .unwrap(&offer, &grant, owner)
                    .map_err(|_| "Could not decrypt the workspace key")?;
                let accepted = cloud
                    .accept_enrollment(&enrollment_id, actor)
                    .await
                    .map_err(|_| "Could not accept secure account enrollment")?;
                if accepted.workspace_id != config.workspace_id {
                    return Err("Convex accepted the wrong account workspace".into());
                }
                return Ok(key);
            }
        }
        tokio::time::sleep(Duration::from_secs(delay)).await;
        delay = (delay * 2).min(15);
    }
}

fn now_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[allow(clippy::too_many_arguments)]
fn spawn_coordinator(
    app: AppHandle,
    db_path: PathBuf,
    data_dir: PathBuf,
    config: SyncConfig,
    actor: String,
    key: WorkspaceKey,
    owner: AuthenticatedPrincipal,
    cloud: CloudClient,
    wake: Arc<Notify>,
    cancelled: Arc<AtomicBool>,
) {
    spawn_convex_change_watcher(
        cloud.clone(),
        config.workspace_id.clone(),
        actor.clone(),
        wake.clone(),
        cancelled.clone(),
    );
    tauri::async_runtime::spawn(async move {
        let mut confirmed_remote_chunks = HashSet::new();
        loop {
            if cancelled.load(Ordering::Acquire) {
                break;
            }
            status::set(
                &app,
                &app.state::<status::SyncStatus>(),
                status::SyncState::Syncing,
            );
            let mut completed = false;
            for _ in 0..8 {
                if cancelled.load(Ordering::Acquire) {
                    break;
                }
                match sync_once(
                    &app,
                    &db_path,
                    &data_dir,
                    &config,
                    &actor,
                    &key,
                    &owner,
                    &cloud,
                    &mut confirmed_remote_chunks,
                )
                .await
                {
                    Ok(has_more) => {
                        completed = true;
                        if !has_more {
                            break;
                        }
                    }
                    Err(error) => {
                        eprintln!("clippy sync exchange failed: {error}");
                        completed = false;
                        break;
                    }
                }
            }
            if cancelled.load(Ordering::Acquire) {
                break;
            }
            status::set(
                &app,
                &app.state::<status::SyncStatus>(),
                if completed {
                    status::SyncState::Synced
                } else {
                    status::SyncState::WaitingForDevice
                },
            );

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
        }
    });
}

fn spawn_convex_change_watcher(
    cloud: CloudClient,
    workspace_id: String,
    actor: String,
    wake: Arc<Notify>,
    cancelled: Arc<AtomicBool>,
) {
    tauri::async_runtime::spawn(async move {
        let retry_delays = [1_u64, 2, 4, 8, 16, 30];
        let mut retry = 0;
        while !cancelled.load(Ordering::Acquire) {
            let result = cloud
                .watch_changes(&workspace_id, &actor, wake.clone(), cancelled.clone())
                .await;
            if cancelled.load(Ordering::Acquire) {
                break;
            }
            if result.is_ok() {
                retry = 0;
                continue;
            }
            let delay = retry_delays[retry];
            retry = (retry + 1).min(retry_delays.len() - 1);
            tokio::time::sleep(Duration::from_secs(delay)).await;
        }
    });
}

#[allow(clippy::too_many_arguments)]
async fn sync_once(
    app: &AppHandle,
    db_path: &PathBuf,
    data_dir: &PathBuf,
    config: &SyncConfig,
    actor: &str,
    key: &WorkspaceKey,
    owner: &AuthenticatedPrincipal,
    cloud: &CloudClient,
    confirmed_remote_chunks: &mut HashSet<String>,
) -> Result<bool, String> {
    let mut connection = Connection::open(db_path).map_err(|_| "open local sync database")?;
    let emitted = store::scan_local(&mut connection, &config.workspace_id, actor)
        .map_err(|_| "scan local changes")?;

    grant_pending_enrollments(config, actor, key, owner, cloud).await?;

    let changes = cloud
        .changes(&config.workspace_id, actor)
        .await
        .map_err(|_| "read Convex frontier")?;
    let accepted_through = changes
        .iter()
        .find(|entry| entry.actor_id == actor)
        .map(|entry| entry.latest_counter)
        .unwrap_or(0);
    let candidates = store::pending_upload_chunk_hashes(
        &connection,
        &config.workspace_id,
        actor,
        accepted_through,
    )
    .map_err(|_| "list local attachment chunks")?
    .into_iter()
    .filter(|hash| !confirmed_remote_chunks.contains(hash))
    .collect::<Vec<_>>();
    if !candidates.is_empty() {
        let batch = candidates
            .iter()
            .take(MAX_CHUNKS_PER_PASS)
            .cloned()
            .collect::<Vec<_>>();
        let uploads = cloud
            .prepare_uploads(&config.workspace_id, &batch)
            .await
            .map_err(|_| "prepare R2 uploads")?;
        for upload in uploads {
            if !upload.exists {
                let (path, offset, size) = store::chunk_source(&connection, &upload.hash)
                    .map_err(|_| "locate attachment chunk")?
                    .ok_or_else(|| "attachment chunk disappeared".to_string())?;
                let bytes = read_chunk(&path, offset, size)?;
                let sealed = crypto::seal(
                    key,
                    &bytes,
                    &crypto::chunk_aad(&config.workspace_id, &upload.hash),
                )
                .map_err(|_| "encrypt attachment chunk")?;
                let body = serde_json::to_vec(&sealed).map_err(|_| "encode attachment chunk")?;
                cloud
                    .upload(
                        upload
                            .url
                            .as_deref()
                            .ok_or_else(|| "missing R2 upload URL".to_string())?,
                        body,
                    )
                    .await
                    .map_err(|_| "upload attachment chunk")?;
            }
            confirmed_remote_chunks.insert(upload.hash);
        }
        if candidates.len() > batch.len() {
            return Ok(true);
        }
    }

    let mut limit = 256;
    let mut outbound = store::cloud_outbound_payload(
        &connection,
        &config.workspace_id,
        actor,
        accepted_through,
        limit,
    )
    .map_err(|_| "build outbound batch")?;
    let mut outbound_may_have_more = false;
    while !outbound.operations.is_empty() {
        let encoded = serde_json::to_vec(&outbound).map_err(|_| "encode outbound batch")?;
        if encoded.len() <= MAX_BATCH_PLAINTEXT_BYTES {
            let first = outbound.operations.first().unwrap().dot.counter;
            let last = outbound.operations.last().unwrap().dot.counter;
            let envelope = crypto::seal(
                key,
                &encoded,
                &crypto::batch_aad(&config.workspace_id, actor, first, last),
            )
            .map_err(|_| "encrypt outbound batch")?;
            outbound_may_have_more = outbound.operations.len() == limit;
            let response = cloud
                .push(
                    &config.workspace_id,
                    &CloudBatch {
                        actor_id: actor.to_string(),
                        first_counter: first,
                        last_counter: last,
                        envelope,
                    },
                )
                .await
                .map_err(|_| "push Convex batch")?;
            if response.accepted_through < last {
                return Err("Convex did not acknowledge the complete batch".into());
            }
            break;
        }
        if limit == 1 {
            return Err("One local operation is too large for Convex".into());
        }
        limit = (limit / 2).max(1);
        outbound = store::cloud_outbound_payload(
            &connection,
            &config.workspace_id,
            actor,
            accepted_through,
            limit,
        )
        .map_err(|_| "resize outbound batch")?;
    }

    let frontier = store::cloud_frontier(&connection, &config.workspace_id)
        .map_err(|_| "read local frontier")?;
    let remote_is_ahead = changes
        .iter()
        .any(|entry| entry.latest_counter > frontier.0.get(&entry.actor_id).copied().unwrap_or(0));
    let batches = if remote_is_ahead {
        cloud
            .pull(&config.workspace_id, actor, &frontier)
            .await
            .map_err(|_| "pull Convex batches")?
    } else {
        Vec::new()
    };
    let pulled_full_page = batches.len() >= 12;
    let mut applied = 0;
    for batch in batches {
        let plaintext = crypto::open(
            key,
            &batch.envelope,
            &crypto::batch_aad(
                &config.workspace_id,
                &batch.actor_id,
                batch.first_counter,
                batch.last_counter,
            ),
        )
        .map_err(|_| "decrypt Convex batch")?;
        if plaintext.len() > MAX_BATCH_PLAINTEXT_BYTES {
            return Err("Convex batch exceeded the client limit".into());
        }
        let payload: SyncPayload =
            serde_json::from_slice(&plaintext).map_err(|_| "decode Convex batch")?;
        applied += store::apply_cloud_batch(
            &mut connection,
            &config.workspace_id,
            &batch.actor_id,
            batch.first_counter,
            batch.last_counter,
            payload,
        )
        .map_err(|_| "apply Convex batch")?;
    }

    let missing = store::missing_chunk_hashes(&connection, &config.workspace_id)
        .map_err(|_| "find missing attachment chunks")?;
    if !missing.is_empty() {
        let batch = missing
            .iter()
            .take(MAX_CHUNKS_PER_PASS)
            .cloned()
            .collect::<Vec<_>>();
        let urls = cloud
            .download_urls(&config.workspace_id, &batch)
            .await
            .map_err(|_| "prepare R2 downloads")?;
        let chunks_dir = data_dir
            .join("sync")
            .join(config.environment.as_str())
            .join("chunks");
        for download in urls {
            let encoded = cloud
                .download(&download.url)
                .await
                .map_err(|_| "download attachment chunk")?;
            let envelope: SealedEnvelope =
                serde_json::from_slice(&encoded).map_err(|_| "decode attachment chunk")?;
            let bytes = crypto::open(
                key,
                &envelope,
                &crypto::chunk_aad(&config.workspace_id, &download.hash),
            )
            .map_err(|_| "decrypt attachment chunk")?;
            store::store_received_chunk(&connection, &chunks_dir, &download.hash, &bytes)
                .map_err(|_| "store attachment chunk")?;
            confirmed_remote_chunks.insert(download.hash);
        }
        if missing.len() > batch.len() {
            return Ok(true);
        }
    }
    store::project_ready_attachments(
        &mut connection,
        &config.workspace_id,
        &data_dir.join("attachments"),
    )
    .map_err(|_| "project attachment")?;
    if emitted > 0 || applied > 0 {
        let _ = app.emit("refresh", ());
    }
    Ok(pulled_full_page || outbound_may_have_more)
}

async fn grant_pending_enrollments(
    config: &SyncConfig,
    actor: &str,
    key: &WorkspaceKey,
    owner: &AuthenticatedPrincipal,
    cloud: &CloudClient,
) -> Result<(), String> {
    let requests = cloud
        .pending_enrollments(&config.workspace_id, actor)
        .await
        .map_err(|_| "read pending enrollments")?;
    for request in requests {
        let pending = PendingPairing::new(
            config.workspace_id.clone(),
            config
                .convex_url
                .to_string()
                .trim_end_matches('/')
                .to_string(),
            config
                .workos_issuer
                .to_string()
                .trim_end_matches('/')
                .to_string(),
            config.workos_audience.clone(),
            key.clone(),
            owner.clone(),
            ENROLLMENT_TTL_MS,
        );
        let offer = pending.offer.clone();
        let grant = pending
            .complete(
                &PairingResponse {
                    phone_public_key: request.phone_public_key,
                    one_time_token: offer.one_time_token.clone(),
                },
                owner,
            )
            .map_err(|_| "create enrollment grant")?;
        cloud
            .grant_enrollment(
                &config.workspace_id,
                actor,
                &request.enrollment_id,
                &offer,
                &grant,
            )
            .await
            .map_err(|_| "publish enrollment grant")?;
    }
    Ok(())
}

fn read_chunk(path: &str, offset: u64, size: usize) -> Result<Vec<u8>, String> {
    use std::io::{Read, Seek, SeekFrom};
    if size > files::DEFAULT_CHUNK_SIZE {
        return Err("attachment chunk is too large".into());
    }
    let mut file = std::fs::File::open(path).map_err(|_| "open attachment chunk")?;
    file.seek(SeekFrom::Start(offset))
        .map_err(|_| "seek attachment chunk")?;
    let mut bytes = vec![0; size];
    file.read_exact(&mut bytes)
        .map_err(|_| "read attachment chunk")?;
    Ok(bytes)
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

fn save_workspace_id(
    db_path: &PathBuf,
    environment: Environment,
    workspace_id: &str,
) -> Result<(), String> {
    if Uuid::parse_str(workspace_id).is_err() {
        return Err("Convex returned an invalid workspace identity".into());
    }
    let connection = Connection::open(db_path).map_err(|_| "Could not open settings")?;
    let setting = format!("sync_workspace_id:{}", environment.as_str());
    crate::db::set_setting(&connection, &setting, workspace_id)
        .map_err(|_| "Could not save the workspace identity".to_string())
}

fn save_environment(db_path: &PathBuf, environment: Environment) -> Result<(), String> {
    let connection = Connection::open(db_path).map_err(|_| "Could not open settings")?;
    crate::db::set_setting(&connection, "sync_environment", environment.as_str())
        .map_err(|_| "Could not save the sync environment".to_string())
}

/// Sync persistence is additive: local rows keep their numeric IDs while
/// immutable operations, causal frontiers, and verified chunk locations live
/// in separate tables.
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
