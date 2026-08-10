use serde::{Deserialize, Serialize};
use std::env;
use url::Url;

const KEYCHAIN_SERVICE: &str = "app.clippy.desktop.sync";

pub const STAGING_WORKOS_AUDIENCE: &str = "client_01KZMNQXBXWT2A807NZCE6V2HV";
pub const STAGING_WORKOS_ISSUER: &str = "https://fashionable-machine-85-staging.authkit.app";
pub const PRODUCTION_WORKOS_AUDIENCE: &str = "client_01KZMNK73NWS9NDAPC3T54S2PE";
pub const PRODUCTION_WORKOS_ISSUER: &str = "https://brave-mermaid-84.authkit.app";
pub const STAGING_TUNNEL_URL: &str = "https://clippy-staging.saudecomalex.com";
pub const PRODUCTION_TUNNEL_URL: &str = "https://clippy.saudecomalex.com";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Environment {
    Staging,
    Production,
}

impl Environment {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Staging => "staging",
            Self::Production => "production",
        }
    }

    pub fn parse(value: &str) -> Result<Self, ConfigError> {
        match value {
            "staging" => Ok(Self::Staging),
            "production" => Ok(Self::Production),
            _ => Err(ConfigError::InvalidEnvironment),
        }
    }

    fn endpoint_suffix(self) -> &'static str {
        match self {
            Self::Staging => "clippy-staging.saudecomalex.com",
            Self::Production => "clippy.saudecomalex.com",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeEndpoint {
    pub http_base_url: String,
    pub ws_base_url: String,
}

impl RuntimeEndpoint {
    pub fn http_url(&self) -> Result<Url, ConfigError> {
        parse_endpoint_url(&self.http_base_url, "https")
    }

    pub fn ws_url(&self) -> Result<Url, ConfigError> {
        parse_endpoint_url(&self.ws_base_url, "wss")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkedRuntime {
    pub relay_issuer: String,
    pub relay_signing_public_jwk: String,
    pub owner_subject: String,
    pub owner_organization: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncConfig {
    pub environment: Environment,
    pub endpoint: RuntimeEndpoint,
    pub workos_issuer: Url,
    pub workos_audience: String,
    pub workspace_id: String,
    pub origin_port: u16,
    pub linked: Option<LinkedRuntime>,
}

impl SyncConfig {
    pub fn for_environment(
        environment: Environment,
        workspace_id: String,
    ) -> Result<Self, ConfigError> {
        if uuid::Uuid::parse_str(&workspace_id).is_err() {
            return Err(ConfigError::MissingWorkspace);
        }
        let linked = load_linked_runtime(environment, &workspace_id)?;
        let endpoint = match linked.as_ref() {
            Some(_) => load_linked_endpoint(environment)?,
            None => legacy_endpoint(environment)?,
        };
        validate_endpoint(environment, &endpoint)?;

        let issuer = value("CLIPPY_WORKOS_ISSUER").unwrap_or_else(|| match environment {
            Environment::Staging => STAGING_WORKOS_ISSUER.to_string(),
            Environment::Production => PRODUCTION_WORKOS_ISSUER.to_string(),
        });
        let audience = value("CLIPPY_WORKOS_AUDIENCE").unwrap_or_else(|| match environment {
            Environment::Staging => STAGING_WORKOS_AUDIENCE.to_string(),
            Environment::Production => PRODUCTION_WORKOS_AUDIENCE.to_string(),
        });
        let workos_issuer = parse_https(&issuer)?;
        if audience.is_empty() {
            return Err(ConfigError::MissingAudience);
        }
        Ok(Self {
            environment,
            endpoint,
            workos_issuer,
            workos_audience: audience,
            workspace_id,
            origin_port: match environment {
                Environment::Staging => 49_832,
                Environment::Production => 49_833,
            },
            linked,
        })
    }
}

fn legacy_endpoint(environment: Environment) -> Result<RuntimeEndpoint, ConfigError> {
    let http_base_url = value("CLIPPY_SYNC_TUNNEL_URL").unwrap_or_else(|| match environment {
        Environment::Staging => STAGING_TUNNEL_URL.to_string(),
        Environment::Production => PRODUCTION_TUNNEL_URL.to_string(),
    });
    let http = parse_endpoint_url(&http_base_url, "https")?;
    let mut ws = http.clone();
    ws.set_scheme("wss").map_err(|_| ConfigError::InvalidUrl)?;
    Ok(RuntimeEndpoint {
        http_base_url: canonical_base(&http),
        ws_base_url: canonical_base(&ws),
    })
}

fn load_linked_runtime(
    environment: Environment,
    workspace_id: &str,
) -> Result<Option<LinkedRuntime>, ConfigError> {
    let prefix = format!("connect:{}", environment.as_str());
    let Some(environment_id) = keychain_string(&format!("{prefix}:environment-id"))? else {
        return Ok(None);
    };
    if environment_id != workspace_id || uuid::Uuid::parse_str(&environment_id).is_err() {
        return Err(ConfigError::IdentityMismatch);
    }
    let relay_issuer = required_keychain_string(&format!("{prefix}:relay-issuer"))?;
    let relay_issuer = parse_origin(&relay_issuer)?.to_string();
    let relay_signing_public_jwk =
        required_keychain_string(&format!("{prefix}:relay-signing-public-jwk"))?;
    let owner_subject = required_keychain_string(&format!("{prefix}:owner-sub"))?;
    if owner_subject.len() > 512 {
        return Err(ConfigError::InvalidIdentity);
    }
    let owner_organization = keychain_string(&format!("{prefix}:owner-org"))?;
    if owner_organization
        .as_ref()
        .is_some_and(|value| value.len() > 512)
    {
        return Err(ConfigError::InvalidIdentity);
    }
    Ok(Some(LinkedRuntime {
        relay_issuer: relay_issuer.trim_end_matches('/').to_string(),
        relay_signing_public_jwk,
        owner_subject,
        owner_organization,
    }))
}

fn load_linked_endpoint(environment: Environment) -> Result<RuntimeEndpoint, ConfigError> {
    let account = format!("connect:{}:endpoint", environment.as_str());
    let encoded = required_keychain_string(&account)?;
    serde_json::from_str(&encoded).map_err(|_| ConfigError::InvalidEndpoint)
}

fn validate_endpoint(
    environment: Environment,
    endpoint: &RuntimeEndpoint,
) -> Result<(), ConfigError> {
    let http = endpoint.http_url()?;
    let ws = endpoint.ws_url()?;
    if http.host_str() != ws.host_str() {
        return Err(ConfigError::InvalidEndpoint);
    }
    let host = http.host_str().ok_or(ConfigError::InvalidEndpoint)?;
    let suffix = environment.endpoint_suffix();
    if host != suffix && !host.ends_with(&format!(".{suffix}")) {
        return Err(ConfigError::UntrustedEndpoint);
    }
    if endpoint.http_base_url.trim_end_matches('/') != canonical_base(&http)
        || endpoint.ws_base_url.trim_end_matches('/') != canonical_base(&ws)
    {
        return Err(ConfigError::InvalidEndpoint);
    }
    Ok(())
}

fn value(key: &str) -> Option<String> {
    env::var(key).ok().filter(|value| !value.trim().is_empty())
}

fn parse_https(value: &str) -> Result<Url, ConfigError> {
    let url = Url::parse(value).map_err(|_| ConfigError::InvalidUrl)?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || url.username() != ""
        || url.password().is_some()
    {
        return Err(ConfigError::InsecureUrl);
    }
    Ok(url)
}

fn parse_origin(value: &str) -> Result<Url, ConfigError> {
    let url = parse_https(value)?;
    if url.path() != "/" || url.query().is_some() || url.fragment().is_some() {
        return Err(ConfigError::InvalidUrl);
    }
    Ok(url)
}

fn parse_endpoint_url(value: &str, scheme: &str) -> Result<Url, ConfigError> {
    let url = Url::parse(value).map_err(|_| ConfigError::InvalidUrl)?;
    if url.scheme() != scheme
        || url.host_str().is_none()
        || url.username() != ""
        || url.password().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(ConfigError::InsecureUrl);
    }
    Ok(url)
}

fn canonical_base(url: &Url) -> String {
    url.as_str().trim_end_matches('/').to_string()
}

fn required_keychain_string(account: &str) -> Result<String, ConfigError> {
    keychain_string(account)?.ok_or(ConfigError::MissingLinkedSetting)
}

#[cfg(target_os = "macos")]
fn keychain_string(account: &str) -> Result<Option<String>, ConfigError> {
    match security_framework::passwords::get_generic_password(KEYCHAIN_SERVICE, account) {
        Ok(value) => {
            let value = String::from_utf8(value).map_err(|_| ConfigError::InvalidEncoding)?;
            if value.trim().is_empty() || value.contains('\n') || value.contains('\r') {
                return Err(ConfigError::InvalidEncoding);
            }
            Ok(Some(value))
        }
        Err(error) if error.code() == -25300 => Ok(None),
        Err(_) => Err(ConfigError::KeychainFailed),
    }
}

#[cfg(not(target_os = "macos"))]
fn keychain_string(_account: &str) -> Result<Option<String>, ConfigError> {
    Err(ConfigError::KeychainFailed)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigError {
    MissingWorkspace,
    MissingAudience,
    MissingLinkedSetting,
    InvalidEnvironment,
    InvalidUrl,
    InsecureUrl,
    InvalidEndpoint,
    UntrustedEndpoint,
    IdentityMismatch,
    InvalidIdentity,
    InvalidEncoding,
    KeychainFailed,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requires_https_transport() {
        assert_eq!(
            parse_https("http://sync.example.com"),
            Err(ConfigError::InsecureUrl)
        );
        assert!(parse_https("https://sync.example.com").is_ok());
    }

    #[test]
    fn endpoint_requires_matching_allowlisted_https_and_wss_hosts() {
        let valid = RuntimeEndpoint {
            http_base_url: "https://device.clippy.saudecomalex.com".into(),
            ws_base_url: "wss://device.clippy.saudecomalex.com".into(),
        };
        assert!(validate_endpoint(Environment::Production, &valid).is_ok());
        let mismatched = RuntimeEndpoint {
            ws_base_url: "wss://attacker.example.com".into(),
            ..valid.clone()
        };
        assert_eq!(
            validate_endpoint(Environment::Production, &mismatched),
            Err(ConfigError::InvalidEndpoint)
        );
        let staging_on_production = RuntimeEndpoint {
            http_base_url: "https://x.clippy-staging.saudecomalex.com".into(),
            ws_base_url: "wss://x.clippy-staging.saudecomalex.com".into(),
        };
        assert_eq!(
            validate_endpoint(Environment::Production, &staging_on_production),
            Err(ConfigError::UntrustedEndpoint)
        );
    }
}
