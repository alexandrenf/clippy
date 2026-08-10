use std::env;
use url::Url;

pub const STAGING_WORKOS_AUDIENCE: &str = "client_01KZMNQXBXWT2A807NZCE6V2HV";
pub const STAGING_WORKOS_ISSUER: &str = "https://fashionable-machine-85-staging.authkit.app";
pub const STAGING_CONVEX_URL: &str = "https://courteous-okapi-555.convex.cloud";
pub const PRODUCTION_WORKOS_AUDIENCE: &str = "client_01KZMNK73NWS9NDAPC3T54S2PE";
pub const PRODUCTION_WORKOS_ISSUER: &str = "https://brave-mermaid-84.authkit.app";
pub const PRODUCTION_CONVEX_URL: &str = "https://descriptive-gecko-343.convex.cloud";

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
}

#[derive(Debug, Clone)]
pub struct SyncConfig {
    pub environment: Environment,
    pub convex_url: Url,
    pub workos_issuer: Url,
    pub workos_audience: String,
    pub workspace_id: String,
}

impl SyncConfig {
    pub fn for_environment(
        environment: Environment,
        workspace_id: String,
    ) -> Result<Self, ConfigError> {
        if uuid::Uuid::parse_str(&workspace_id).is_err() {
            return Err(ConfigError::MissingWorkspace);
        }
        let convex_url = value("CLIPPY_CONVEX_URL")
            .or_else(|| {
                value(match environment {
                    Environment::Staging => "CLIPPY_STAGING_CONVEX_URL",
                    Environment::Production => "CLIPPY_PRODUCTION_CONVEX_URL",
                })
            })
            .unwrap_or_else(|| match environment {
                Environment::Staging => STAGING_CONVEX_URL.to_string(),
                Environment::Production => PRODUCTION_CONVEX_URL.to_string(),
            });
        let convex_url = parse_convex_url(&convex_url)?;
        let issuer = value("CLIPPY_WORKOS_ISSUER").unwrap_or_else(|| match environment {
            Environment::Staging => STAGING_WORKOS_ISSUER.to_string(),
            Environment::Production => PRODUCTION_WORKOS_ISSUER.to_string(),
        });
        let workos_audience =
            value("CLIPPY_WORKOS_AUDIENCE").unwrap_or_else(|| match environment {
                Environment::Staging => STAGING_WORKOS_AUDIENCE.to_string(),
                Environment::Production => PRODUCTION_WORKOS_AUDIENCE.to_string(),
            });
        if workos_audience.is_empty() {
            return Err(ConfigError::MissingAudience);
        }
        Ok(Self {
            environment,
            convex_url,
            workos_issuer: parse_https_origin(&issuer)?,
            workos_audience,
            workspace_id,
        })
    }
}

fn value(key: &str) -> Option<String> {
    env::var(key).ok().filter(|value| !value.trim().is_empty())
}

fn parse_convex_url(value: &str) -> Result<Url, ConfigError> {
    let url = parse_https_origin(value)?;
    let host = url.host_str().ok_or(ConfigError::InvalidUrl)?;
    if host != "localhost" && !host.ends_with(".convex.cloud") {
        return Err(ConfigError::UntrustedConvexUrl);
    }
    Ok(url)
}

fn parse_https_origin(value: &str) -> Result<Url, ConfigError> {
    let url = Url::parse(value).map_err(|_| ConfigError::InvalidUrl)?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || url.username() != ""
        || url.password().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(ConfigError::InvalidUrl);
    }
    Ok(url)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigError {
    InvalidEnvironment,
    MissingWorkspace,
    MissingAudience,
    InvalidUrl,
    UntrustedConvexUrl,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_convex_cloud_origins_are_accepted() {
        assert!(parse_convex_url("https://happy-otter-123.convex.cloud").is_ok());
        assert_eq!(
            parse_convex_url("https://sync.example.com"),
            Err(ConfigError::UntrustedConvexUrl)
        );
        assert_eq!(
            parse_convex_url("http://happy-otter-123.convex.cloud"),
            Err(ConfigError::InvalidUrl)
        );
    }
}
