use clap::Parser;
use miab_core::{
    copy_chunk_payload_with_crc, read_chunk_length, read_terminal_checksum, sanitize_path,
    MiabError, MiabHeader, MiabResponse, DEFAULT_CHUNK_SIZE, UNKNOWN_SIZE,
};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// Server daemon configuration arguments.
#[derive(Parser, Debug, Clone)]
#[command(name = "miabd", about = "Message In A Bottle Daemon (miabd)")]
pub struct ServerArgs {
    /// Bind socket address
    #[arg(short = 'b', long = "bind", default_value = "0.0.0.0:6422")]
    pub bind: String,

    /// Base directory to store incoming files
    #[arg(short = 's', long = "storage-root", default_value = "./storage")]
    pub storage_root: PathBuf,

    /// Discard incoming payload without writing to disk (for network benchmarking)
    #[arg(long = "discard", default_value_t = false)]
    pub discard: bool,

    /// Optional 16-byte hex auth token required from clients (e.g. 010203...0f)
    #[arg(long = "auth-token")]
    pub auth_token: Option<String>,
}

/// The MIAB server daemon instance.
pub struct Server {
    listener: TcpListener,
    storage_root: PathBuf,
    discard: bool,
    auth_token: Option<[u8; 16]>,
}

impl Server {
    /// Bind the server to the given address and configure the storage root.
    pub async fn bind(addr: &str, storage_root: PathBuf) -> Result<Self, MiabError> {
        Self::bind_with_options(addr, storage_root, false, None).await
    }

    /// Bind the server with custom discard mode and auth token options.
    pub async fn bind_with_options(
        addr: &str,
        storage_root: PathBuf,
        discard: bool,
        auth_token: Option<[u8; 16]>,
    ) -> Result<Self, MiabError> {
        let listener = TcpListener::bind(addr).await?;
        if !discard {
            tokio::fs::create_dir_all(&storage_root).await?;
        }
        Ok(Self {
            listener,
            storage_root,
            discard,
            auth_token,
        })
    }

    /// Retrieve the local bound socket address.
    pub fn local_addr(&self) -> Result<SocketAddr, MiabError> {
        Ok(self.listener.local_addr()?)
    }

    /// Run the server loop accepting connections indefinitely.
    pub async fn run(self) -> Result<(), MiabError> {
        let addr = self.listener.local_addr()?;
        tracing::info!(
            listen_addr = %addr,
            storage_root = %self.storage_root.display(),
            discard = self.discard,
            has_auth = self.auth_token.is_some(),
            "MIAB daemon listening for incoming connections"
        );

        loop {
            let (socket, peer_addr) = self.listener.accept().await?;
            let storage_root = self.storage_root.clone();
            let discard = self.discard;
            let auth_token = self.auth_token;

            tokio::spawn(async move {
                handle_connection(socket, peer_addr, storage_root, discard, auth_token).await;
            });
        }
    }

    /// Run the server until a shutdown signal is received.
    pub async fn run_until_shutdown(
        self,
        mut shutdown_rx: tokio::sync::oneshot::Receiver<()>,
    ) -> Result<(), MiabError> {
        let addr = self.listener.local_addr()?;
        tracing::info!(
            listen_addr = %addr,
            storage_root = %self.storage_root.display(),
            discard = self.discard,
            has_auth = self.auth_token.is_some(),
            "MIAB daemon started with shutdown listener"
        );

        loop {
            tokio::select! {
                _ = &mut shutdown_rx => {
                    tracing::info!("Shutdown signal received, stopping listener");
                    break;
                }
                accept_res = self.listener.accept() => {
                    let (socket, peer_addr) = accept_res?;
                    let storage_root = self.storage_root.clone();
                    let discard = self.discard;
                    let auth_token = self.auth_token;

                    tokio::spawn(async move {
                        handle_connection(socket, peer_addr, storage_root, discard, auth_token).await;
                    });
                }
            }
        }
        Ok(())
    }
}

/// Handle a single incoming client connection.
async fn handle_connection(
    mut socket: TcpStream,
    peer_addr: SocketAddr,
    storage_root: PathBuf,
    discard: bool,
    expected_auth: Option<[u8; 16]>,
) {
    tracing::info!(peer = %peer_addr, "Connection accepted");

    if let Err(err) = process_connection(&mut socket, &storage_root, discard, expected_auth).await {
        tracing::warn!(peer = %peer_addr, error = %err, "Transfer failed");

        // Per protocol spec: if magic mismatch, drop connection immediately without response
        if !matches!(err, MiabError::InvalidMagic(_)) {
            let resp = match err {
                MiabError::ChecksumMismatch { .. } => {
                    MiabResponse::checksum_mismatch(err.to_string())
                }
                MiabError::FileExists(_) => MiabResponse::file_exists(err.to_string()),
                MiabError::AuthFailed => MiabResponse::auth_failed(err.to_string()),
                MiabError::InsufficientSpace { .. } => {
                    MiabResponse::insufficient_space(err.to_string())
                }
                MiabError::HeaderCorrupted(_) => MiabResponse::header_corrupted(err.to_string()),
                _ => MiabResponse::err(err.to_string()),
            };

            let _ = resp.write_to(&mut socket).await;
            let _ = socket.flush().await;
            let _ = socket.shutdown().await;

            // Short drain to avoid TCP RST in case client was actively streaming
            let mut drain = [0u8; 4096];
            let _ = tokio::time::timeout(Duration::from_millis(50), async {
                while let Ok(n) = socket.read(&mut drain).await {
                    if n == 0 {
                        break;
                    }
                }
            })
            .await;
        }
    } else {
        tracing::info!(peer = %peer_addr, "Transfer finished cleanly");
    }
}

/// Check available filesystem disk space on Unix platforms.
#[cfg(unix)]
fn check_free_space(path: &Path, needed_bytes: u64) -> Result<(), MiabError> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    // Use parent directory if file does not exist yet
    let check_path = if path.exists() {
        path.to_path_buf()
    } else {
        path.parent().unwrap_or(path).to_path_buf()
    };

    let c_path = CString::new(check_path.as_os_str().as_bytes())
        .map_err(|e| MiabError::Protocol(format!("Invalid path for statvfs: {}", e)))?;

    unsafe {
        let mut stat: libc::statvfs = std::mem::zeroed();
        if libc::statvfs(c_path.as_ptr(), &mut stat) == 0 {
            let free_bytes = (stat.f_bavail as u64) * (stat.f_bsize as u64);
            if free_bytes < needed_bytes {
                return Err(MiabError::InsufficientSpace {
                    needed: needed_bytes,
                    available: free_bytes,
                });
            }
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn check_free_space(_path: &Path, _needed_bytes: u64) -> Result<(), MiabError> {
    Ok(())
}

/// Process protocol execution for a connection:
/// 1. Decode 64-byte header and path
/// 2. Validate auth token if configured
/// 3. Sanitize target path
/// 4. Check NO_OVERWRITE and disk space
/// 5. Stream chunks into target (or atomic .part staging) while computing running CRC-32
/// 6. Verify Terminal Bottle CRC-32
/// 7. Commit atomic rename and permissions
/// 8. Transmit OK response
async fn process_connection(
    socket: &mut TcpStream,
    storage_root: &Path,
    discard: bool,
    expected_auth: Option<[u8; 16]>,
) -> Result<(), MiabError> {
    // 1. Read 64-byte header preamble and path
    let header = MiabHeader::read_from(socket).await?;
    tracing::info!(path = %header.path, "Processing incoming transfer request");

    // 2. Validate auth token if required
    if let Some(token) = expected_auth {
        if header.auth_token != token {
            return Err(MiabError::AuthFailed);
        }
    }

    // 3. Sanitize target path
    let target_path = sanitize_path(storage_root, &header.path)?;

    // 4. Check NO_OVERWRITE
    if header.is_no_overwrite() && target_path.exists() {
        return Err(MiabError::FileExists(header.path));
    }

    let mut total_bytes: u64 = 0;
    let mut buf = vec![0u8; DEFAULT_CHUNK_SIZE];
    let mut hasher = crc32fast::Hasher::new();

    if discard {
        // Discard mode for network benchmarking
        let mut sink = tokio::io::sink();
        loop {
            let chunk_len = read_chunk_length(socket).await?;
            if chunk_len == 0 {
                break;
            }
            copy_chunk_payload_with_crc(socket, &mut sink, chunk_len, &mut buf, &mut hasher)
                .await?;
            total_bytes += chunk_len as u64;
        }

        // Verify CRC-32 if present in terminal bottle
        if header.is_checksum_present() {
            let expected_crc = read_terminal_checksum(socket).await?;
            let calculated_crc = hasher.finalize();
            if expected_crc != calculated_crc {
                return Err(MiabError::ChecksumMismatch {
                    expected: expected_crc,
                    actual: calculated_crc,
                });
            }
        }
    } else {
        // Create parent directories if needed
        if let Some(parent) = target_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        // Check available disk space
        if header.expected_size != UNKNOWN_SIZE {
            check_free_space(&target_path, header.expected_size)?;
        }

        // Atomic staging file if requested
        let is_atomic = header.is_atomic() && !header.is_append();
        let write_path = if is_atomic {
            let file_name = target_path
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "file.bin".to_string());
            target_path.with_file_name(format!(".{}.miab.part", file_name))
        } else {
            target_path.clone()
        };

        // Open file: append or truncate
        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .append(header.is_append())
            .truncate(!header.is_append())
            .open(&write_path)
            .await?;

        // Pre-allocate disk space if expected size is known and not appending
        if header.expected_size != UNKNOWN_SIZE && !header.is_append() {
            let _ = file.set_len(header.expected_size).await;
        }

        // Stream incoming chunks directly into file descriptor
        let write_result: Result<(), MiabError> = async {
            loop {
                let chunk_len = read_chunk_length(socket).await?;
                if chunk_len == 0 {
                    break;
                }
                copy_chunk_payload_with_crc(socket, &mut file, chunk_len, &mut buf, &mut hasher)
                    .await?;
                total_bytes += chunk_len as u64;
            }

            // Verify CRC-32 if present
            if header.is_checksum_present() {
                let expected_crc = read_terminal_checksum(socket).await?;
                let calculated_crc = hasher.finalize();
                if expected_crc != calculated_crc {
                    return Err(MiabError::ChecksumMismatch {
                        expected: expected_crc,
                        actual: calculated_crc,
                    });
                }
            }

            file.sync_all().await?;
            Ok(())
        }
        .await;

        if let Err(err) = write_result {
            // Clean up partial staging file on abort
            if is_atomic {
                let _ = tokio::fs::remove_file(&write_path).await;
            }
            return Err(err);
        }

        // Commit atomic rename if staging was used
        if is_atomic {
            tokio::fs::rename(&write_path, &target_path).await?;
        }

        // Apply executable permissions if requested
        #[cfg(unix)]
        if header.is_executable() {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(0o755);
            let _ = tokio::fs::set_permissions(&target_path, perms).await;
        }

        tracing::info!(
            path = %target_path.display(),
            total_bytes = total_bytes,
            "File successfully written, verified, and committed to disk"
        );
    }

    // Send success response
    let resp = MiabResponse::ok(format!("Saved {} bytes", total_bytes));
    resp.write_to(socket).await?;
    socket.flush().await?;

    Ok(())
}
