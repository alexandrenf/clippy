use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

const KEYCHAIN_SERVICE: &str = "app.clippy.desktop.sync";
const RETRY_SECONDS: [u64; 5] = [1, 2, 4, 8, 16];

pub fn retry_delay(failure_index: usize) -> Duration {
    Duration::from_secs(RETRY_SECONDS[failure_index.min(RETRY_SECONDS.len() - 1)])
}

/// Owns the linked environment's cloudflared child. It is intentionally not
/// installed as a system service, but remains alive for the lifetime of the
/// linked Clippy runtime so a foreground phone can reach an otherwise-idle Mac.
pub struct TunnelRunner {
    child: Option<Child>,
    token_file: Option<tempfile::NamedTempFile>,
    environment: &'static str,
}

impl TunnelRunner {
    pub fn new(environment: &'static str) -> Result<Self, TunnelError> {
        if !matches!(environment, "staging" | "production") {
            return Err(TunnelError::InvalidEnvironment);
        }
        Ok(Self {
            child: None,
            token_file: None,
            environment,
        })
    }

    pub fn is_running(&mut self) -> bool {
        match self.child.as_mut() {
            Some(child) => match child.try_wait() {
                Ok(None) => true,
                _ => {
                    self.child = None;
                    self.token_file.take();
                    false
                }
            },
            None => false,
        }
    }

    pub fn start_if_needed(&mut self) -> Result<(), TunnelError> {
        if self.is_running() {
            return Ok(());
        }
        let token = load_tunnel_token(self.environment)?;
        let mut token_file = tempfile::Builder::new()
            .prefix("clippy-tunnel-token-")
            .tempfile()
            .map_err(|_| TunnelError::TemporaryFile)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(token_file.path(), fs::Permissions::from_mode(0o600))
                .map_err(|_| TunnelError::TemporaryFile)?;
        }
        token_file
            .write_all(&token)
            .map_err(|_| TunnelError::TemporaryFile)?;
        token_file.flush().map_err(|_| TunnelError::TemporaryFile)?;

        let executable = cloudflared_path()?;
        let child = Command::new(executable)
            .args(["tunnel", "--no-autoupdate", "run", "--token-file"])
            .arg(token_file.path())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| TunnelError::LaunchFailed)?;
        self.child = Some(child);
        // Keep the 0600 file alive until the owned child exits. `spawn()` does
        // not guarantee cloudflared has opened the path yet.
        self.token_file = Some(token_file);
        Ok(())
    }

    pub fn stop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        // NamedTempFile unlinks the credential after cloudflared has stopped.
        self.token_file.take();
    }
}

impl Drop for TunnelRunner {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(target_os = "macos")]
fn load_tunnel_token(environment: &str) -> Result<Vec<u8>, TunnelError> {
    let token = security_framework::passwords::get_generic_password(
        KEYCHAIN_SERVICE,
        &format!("cloudflare-tunnel:{environment}"),
    )
    .map_err(|_| TunnelError::MissingToken)?;
    if token.is_empty() || token.contains(&b'\n') || token.contains(&b'\r') {
        return Err(TunnelError::InvalidToken);
    }
    Ok(token)
}

fn cloudflared_path() -> Result<PathBuf, TunnelError> {
    if let Some(path) = std::env::var_os("CLIPPY_CLOUDFLARED_PATH") {
        let path = PathBuf::from(path);
        if path.is_absolute() && path.is_file() {
            return Ok(path);
        }
        return Err(TunnelError::MissingExecutable);
    }
    [
        PathBuf::from("/opt/homebrew/bin/cloudflared"),
        PathBuf::from("/usr/local/bin/cloudflared"),
    ]
    .into_iter()
    .find(|path| path.is_file())
    .ok_or(TunnelError::MissingExecutable)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TunnelError {
    InvalidEnvironment,
    MissingToken,
    InvalidToken,
    MissingExecutable,
    TemporaryFile,
    LaunchFailed,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_unknown_environment_names() {
        assert_eq!(
            TunnelRunner::new("development").err(),
            Some(TunnelError::InvalidEnvironment)
        );
    }

    #[test]
    fn reconnect_delay_caps_at_sixteen_seconds() {
        assert_eq!(
            (0..8)
                .map(retry_delay)
                .map(|value| value.as_secs())
                .collect::<Vec<_>>(),
            vec![1, 2, 4, 8, 16, 16, 16, 16]
        );
    }
}
