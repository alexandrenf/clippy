use super::config::Environment;
use super::crypto::{PairingGrant, PairingOffer, SealedEnvelope};
use convex::{AuthTokenFetcher, AuthenticationToken, ConvexClient, FunctionResult, Value};
use futures::StreamExt;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::{Mutex, Notify};
use url::Url;

const MAX_STORAGE_HASHES: usize = 64;

#[derive(Clone)]
pub struct CloudClient {
    deployment_url: Url,
    environment: Environment,
    db_path: PathBuf,
    token: Arc<Mutex<String>>,
    http: reqwest::Client,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActorCounter {
    pub actor_id: String,
    #[serde(deserialize_with = "super::serde_u64::deserialize")]
    pub latest_counter: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudBatch {
    pub actor_id: String,
    #[serde(deserialize_with = "super::serde_u64::deserialize")]
    pub first_counter: u64,
    #[serde(deserialize_with = "super::serde_u64::deserialize")]
    pub last_counter: u64,
    pub envelope: SealedEnvelope,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PushResponse {
    #[serde(deserialize_with = "super::serde_u64::deserialize")]
    pub accepted_through: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingEnrollment {
    pub enrollment_id: String,
    pub phone_public_key: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountWorkspace {
    pub workspace_id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DeviceRegistration {
    pub enrolled: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnrollmentRequestResult {
    pub state: String,
    pub workspace_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnrollmentStatus {
    pub state: String,
    pub workspace_id: Option<String>,
    pub offer: Option<PairingOffer>,
    pub grant: Option<PairingGrant>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcceptedEnrollment {
    pub workspace_id: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageUpload {
    pub hash: String,
    pub exists: bool,
    pub url: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StorageDownload {
    pub hash: String,
    pub url: String,
}

impl CloudClient {
    pub async fn connect(
        deployment_url: &str,
        environment: Environment,
        token: String,
        db_path: &Path,
    ) -> Result<Self, CloudError> {
        let deployment_url = Url::parse(deployment_url).map_err(|_| CloudError::Connection)?;
        Ok(Self {
            deployment_url,
            environment,
            db_path: db_path.to_path_buf(),
            token: Arc::new(Mutex::new(token)),
            http: reqwest::Client::builder()
                .https_only(true)
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .map_err(|_| CloudError::Connection)?,
        })
    }

    pub async fn bootstrap(
        &self,
        workspace_id: &str,
        actor_id: &str,
        device_name: &str,
    ) -> Result<(), CloudError> {
        let _: serde_json::Value = self
            .mutation(
                "sync:bootstrap",
                serde_json::json!({
                    "workspaceId": workspace_id,
                    "actorId": actor_id,
                    "deviceName": device_name,
                    "platform": "macos",
                }),
            )
            .await?;
        Ok(())
    }

    pub async fn account_workspace(&self) -> Result<Option<AccountWorkspace>, CloudError> {
        self.query("sync:accountWorkspace", serde_json::json!({}))
            .await
    }

    pub async fn is_device_enrolled(
        &self,
        workspace_id: &str,
        actor_id: &str,
    ) -> Result<bool, CloudError> {
        let response: DeviceRegistration = self
            .query(
                "sync:deviceRegistration",
                serde_json::json!({ "workspaceId": workspace_id, "actorId": actor_id }),
            )
            .await?;
        Ok(response.enrolled)
    }

    pub async fn changes(
        &self,
        workspace_id: &str,
        actor_id: &str,
    ) -> Result<Vec<ActorCounter>, CloudError> {
        self.query(
            "sync:changes",
            serde_json::json!({ "workspaceId": workspace_id, "actorId": actor_id }),
        )
        .await
    }

    /// Keep the desktop coordinator attached to Convex's reactive frontier.
    /// The HTTP query remains the source of data for each exchange; this small
    /// subscription only wakes that exchange as soon as any device advances.
    pub async fn watch_changes(
        &self,
        workspace_id: &str,
        actor_id: &str,
        wake: Arc<Notify>,
        cancelled: Arc<AtomicBool>,
    ) -> Result<(), CloudError> {
        let mut client = ConvexClient::new(self.deployment_url.as_str())
            .await
            .map_err(|_| CloudError::Connection)?;
        let environment = self.environment;
        let db_path = self.db_path.clone();
        let fetcher: AuthTokenFetcher = Box::new(move |force_refresh| {
            let db_path = db_path.clone();
            Box::pin(async move {
                let current = super::auth_login::access_token(environment, &db_path)
                    .map_err(anyhow::Error::msg)?;
                let token =
                    if force_refresh || super::auth_login::access_token_expires_soon(&current) {
                        super::auth_login::refresh_access_token(environment, &db_path)
                            .await
                            .map_err(anyhow::Error::msg)?
                    } else {
                        current
                    };
                Ok(AuthenticationToken::User(token))
            })
        });
        client.set_auth_callback(Some(fetcher)).await;
        let args = BTreeMap::from([
            (
                "workspaceId".to_string(),
                Value::String(workspace_id.to_string()),
            ),
            ("actorId".to_string(), Value::String(actor_id.to_string())),
        ]);
        let mut subscription = client
            .subscribe("sync:coordinationSignals", args)
            .await
            .map_err(|_| CloudError::Connection)?;
        let mut last_signal: Option<Value> = None;

        loop {
            tokio::select! {
                result = subscription.next() => match result {
                    Some(FunctionResult::Value(value)) => {
                        if last_signal.as_ref() != Some(&value) {
                            last_signal = Some(value);
                            wake.notify_one();
                        }
                    },
                    Some(FunctionResult::ErrorMessage(_)
                        | FunctionResult::ConvexError(_)) => return Err(CloudError::Rejected),
                    None => return Err(CloudError::Connection),
                },
                _ = tokio::time::sleep(std::time::Duration::from_secs(1)) => {
                    if cancelled.load(Ordering::Acquire) {
                        return Ok(());
                    }
                }
            }
        }
    }

    pub async fn push(
        &self,
        workspace_id: &str,
        batch: &CloudBatch,
    ) -> Result<PushResponse, CloudError> {
        self.mutation(
            "sync:push",
            serde_json::json!({
                "workspaceId": workspace_id,
                "actorId": batch.actor_id,
                "firstCounter": batch.first_counter,
                "lastCounter": batch.last_counter,
                "envelope": batch.envelope,
            }),
        )
        .await
    }

    pub async fn pull(
        &self,
        workspace_id: &str,
        actor_id: &str,
        frontier: &super::model::VersionVector,
    ) -> Result<Vec<CloudBatch>, CloudError> {
        let frontier = frontier
            .0
            .iter()
            .map(|(actor_id, counter)| {
                serde_json::json!({ "actorId": actor_id, "counter": counter })
            })
            .collect::<Vec<_>>();
        self.query(
            "sync:pull",
            serde_json::json!({
                "workspaceId": workspace_id,
                "actorId": actor_id,
                "frontier": frontier,
            }),
        )
        .await
    }

    pub async fn pending_enrollments(
        &self,
        workspace_id: &str,
        actor_id: &str,
    ) -> Result<Vec<PendingEnrollment>, CloudError> {
        self.query(
            "sync:pendingEnrollments",
            serde_json::json!({ "workspaceId": workspace_id, "actorId": actor_id }),
        )
        .await
    }

    pub async fn request_enrollment(
        &self,
        enrollment_id: &str,
        actor_id: &str,
        device_name: &str,
        public_key: &str,
        recover_key: bool,
    ) -> Result<EnrollmentRequestResult, CloudError> {
        self.mutation(
            "sync:requestEnrollment",
            serde_json::json!({
                "enrollmentId": enrollment_id,
                "actorId": actor_id,
                "deviceName": device_name,
                "phonePublicKey": public_key,
                "platform": "macos",
                "recoverKey": recover_key,
            }),
        )
        .await
    }

    pub async fn enrollment_status(
        &self,
        enrollment_id: &str,
        actor_id: &str,
    ) -> Result<Option<EnrollmentStatus>, CloudError> {
        self.query(
            "sync:enrollmentStatus",
            serde_json::json!({ "enrollmentId": enrollment_id, "actorId": actor_id }),
        )
        .await
    }

    pub async fn accept_enrollment(
        &self,
        enrollment_id: &str,
        actor_id: &str,
    ) -> Result<AcceptedEnrollment, CloudError> {
        self.mutation(
            "sync:acceptEnrollment",
            serde_json::json!({ "enrollmentId": enrollment_id, "actorId": actor_id }),
        )
        .await
    }

    pub async fn grant_enrollment(
        &self,
        workspace_id: &str,
        actor_id: &str,
        enrollment_id: &str,
        offer: &PairingOffer,
        grant: &PairingGrant,
    ) -> Result<(), CloudError> {
        let _: serde_json::Value = self
            .mutation(
                "sync:grantEnrollment",
                serde_json::json!({
                    "workspaceId": workspace_id,
                    "actorId": actor_id,
                    "enrollmentId": enrollment_id,
                    "offer": offer,
                    "grant": grant,
                }),
            )
            .await?;
        Ok(())
    }

    pub async fn prepare_uploads(
        &self,
        workspace_id: &str,
        hashes: &[String],
    ) -> Result<Vec<StorageUpload>, CloudError> {
        validate_hashes(hashes)?;
        self.action(
            "storage:prepareUploads",
            serde_json::json!({ "workspaceId": workspace_id, "hashes": hashes }),
        )
        .await
    }

    pub async fn download_urls(
        &self,
        workspace_id: &str,
        hashes: &[String],
    ) -> Result<Vec<StorageDownload>, CloudError> {
        validate_hashes(hashes)?;
        self.action(
            "storage:downloadUrls",
            serde_json::json!({ "workspaceId": workspace_id, "hashes": hashes }),
        )
        .await
    }

    pub async fn upload(&self, url: &str, body: Vec<u8>) -> Result<(), CloudError> {
        self.http
            .put(url)
            .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
            .body(body)
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(|_| CloudError::Storage)?;
        Ok(())
    }

    pub async fn download(&self, url: &str) -> Result<Vec<u8>, CloudError> {
        let response = self
            .http
            .get(url)
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(|_| CloudError::Storage)?;
        let bytes = response.bytes().await.map_err(|_| CloudError::Storage)?;
        if bytes.len() > super::files::DEFAULT_CHUNK_SIZE * 2 {
            return Err(CloudError::InvalidResponse);
        }
        Ok(bytes.to_vec())
    }

    async fn query<T: DeserializeOwned>(
        &self,
        name: &str,
        value: serde_json::Value,
    ) -> Result<T, CloudError> {
        self.call("query", name, value).await
    }

    async fn mutation<T: DeserializeOwned>(
        &self,
        name: &str,
        value: serde_json::Value,
    ) -> Result<T, CloudError> {
        self.call("mutation", name, value).await
    }

    async fn action<T: DeserializeOwned>(
        &self,
        name: &str,
        value: serde_json::Value,
    ) -> Result<T, CloudError> {
        self.call("action", name, value).await
    }

    async fn call<T: DeserializeOwned>(
        &self,
        kind: &str,
        name: &str,
        args: serde_json::Value,
    ) -> Result<T, CloudError> {
        let endpoint = self
            .deployment_url
            .join(&format!("api/{kind}"))
            .map_err(|_| CloudError::Connection)?;
        let token = self.bearer_token().await?;
        let response = self
            .http
            .post(endpoint)
            .bearer_auth(token)
            .json(&serde_json::json!({
                "path": name,
                "args": [args],
                "format": "convex_encoded_json",
            }))
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(|_| CloudError::Connection)?;
        let response: FunctionResponse<T> = response
            .json()
            .await
            .map_err(|_| CloudError::InvalidResponse)?;
        match response {
            FunctionResponse::Success { value } => Ok(value),
            FunctionResponse::Error { .. } => Err(CloudError::Rejected),
        }
    }

    async fn bearer_token(&self) -> Result<String, CloudError> {
        let mut token = self.token.lock().await;
        if super::auth_login::access_token_expires_soon(&token) {
            *token = super::auth_login::refresh_access_token(self.environment, &self.db_path)
                .await
                .map_err(|_| CloudError::Authentication)?;
        }
        Ok(token.clone())
    }
}

#[derive(Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
enum FunctionResponse<T> {
    Success {
        value: T,
    },
    Error {
        #[serde(rename = "errorMessage")]
        _message: String,
    },
}

fn validate_hashes(hashes: &[String]) -> Result<(), CloudError> {
    if hashes.is_empty()
        || hashes.len() > MAX_STORAGE_HASHES
        || hashes.iter().any(|hash| {
            hash.len() != 64
                || !hash
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
    {
        return Err(CloudError::InvalidResponse);
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloudError {
    Connection,
    Authentication,
    Rejected,
    InvalidResponse,
    Storage,
}
