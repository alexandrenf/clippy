use std::{
    fs,
    os::unix::{fs::PermissionsExt, net::UnixDatagram},
    path::PathBuf,
    time::Duration,
};
use tauri::{AppHandle, Emitter};

pub const SOCKET_NAME: &str = "clippy-mcp.sock";

/// Wake the already-running desktop UI after the local stdio MCP process
/// commits a database change. The socket is owner-only and carries no data.
pub fn start(app: AppHandle, data_dir: PathBuf) {
    let socket_path = data_dir.join(SOCKET_NAME);
    std::thread::spawn(move || {
        if let Err(error) = fs::remove_file(&socket_path) {
            if error.kind() != std::io::ErrorKind::NotFound {
                eprintln!("clippy: could not replace stale MCP wake socket: {error}");
                return;
            }
        }
        let socket = match UnixDatagram::bind(&socket_path) {
            Ok(socket) => socket,
            Err(error) => {
                eprintln!("clippy: could not start MCP wake socket: {error}");
                return;
            }
        };
        if let Err(error) = fs::set_permissions(&socket_path, fs::Permissions::from_mode(0o600)) {
            eprintln!("clippy: could not protect MCP wake socket: {error}");
            return;
        }
        let _ = socket.set_read_timeout(Some(Duration::from_secs(1)));
        let mut message = [0_u8; 16];
        loop {
            match socket.recv(&mut message) {
                Ok(7) if &message[..7] == b"refresh" => {
                    crate::sync::wake(&app);
                    let _ = app.emit("refresh", ());
                }
                Ok(_) => {}
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) => {}
                Err(error) => {
                    eprintln!("clippy: MCP wake socket stopped: {error}");
                    break;
                }
            }
        }
    });
}
