use clap::Parser;
use miab_core::{
    copy_chunk_payload, read_chunk_length, sanitize_path, MiabError, MiabHeader, MiabResponse,
    DEFAULT_CHUNK_SIZE,
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
}

/// The MIAB server daemon instance.
pub struct Server {
    listener: TcpListener,
    storage_root: PathBuf,
    discard: bool,
}

impl Server {
    /// Bind the server to the given address and configure the storage root.
    pub async fn bind(addr: &str, storage_root: PathBuf) -> Result<Self, MiabError> {
        Self::bind_with_options(addr, storage_root, false).await
    }

    /// Bind the server with custom discard mode option.
    pub async fn bind_with_options(
        addr: &str,
        storage_root: PathBuf,
        discard: bool,
    ) -> Result<Self, MiabError> {
        let listener = TcpListener::bind(addr).await?;
        if !discard {
            tokio::fs::create_dir_all(&storage_root).await?;
        }
        Ok(Self {
            listener,
            storage_root,
            discard,
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
            "MIAB daemon listening for incoming connections"
        );

        loop {
            let (socket, peer_addr) = self.listener.accept().await?;
            let storage_root = self.storage_root.clone();
            let discard = self.discard;

            tokio::spawn(async move {
                handle_connection(socket, peer_addr, storage_root, discard).await;
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

                    tokio::spawn(async move {
                        handle_connection(socket, peer_addr, storage_root, discard).await;
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
) {
    tracing::info!(peer = %peer_addr, "Connection accepted");

    if let Err(err) = process_connection(&mut socket, &storage_root, discard).await {
        tracing::warn!(peer = %peer_addr, error = %err, "Transfer failed");

        // Per protocol spec: if magic mismatch, drop connection immediately without response
        if !matches!(err, MiabError::InvalidMagic(_)) {
            let resp = MiabResponse::err(err.to_string());
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

/// Process protocol execution for a connection:
/// 1. Decode header
/// 2. Sanitize and validate relative path
/// 3. Create parent directories and destination file (if not discard)
/// 4. Stream bottle chunks directly to disk (or sink)
/// 5. Flush file and send OK response
async fn process_connection(
    socket: &mut TcpStream,
    storage_root: &Path,
    discard: bool,
) -> Result<(), MiabError> {
    // 1. Read initial header
    let header = MiabHeader::read_from(socket).await?;
    tracing::info!(path = %header.path, "Processing incoming transfer request");

    // 2. Sanitize destination path
    let target_path = sanitize_path(storage_root, &header.path)?;
    tracing::debug!(target_path = %target_path.display(), "Resolved target destination");

    let mut total_bytes: u64 = 0;
    let mut buf = vec![0u8; DEFAULT_CHUNK_SIZE];

    if discard {
        // Discard mode: stream directly into sink for network/framing benchmarking
        let mut sink = tokio::io::sink();
        loop {
            let chunk_len = read_chunk_length(socket).await?;
            if chunk_len == 0 {
                break;
            }
            copy_chunk_payload(socket, &mut sink, chunk_len, &mut buf).await?;
            total_bytes += chunk_len as u64;
        }
    } else {
        // 3. Create parent directories if needed
        if let Some(parent) = target_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        // 4. Create target file
        let mut file = tokio::fs::File::create(&target_path).await?;

        // 5. Read incoming chunk bottles sequentially and write directly to disk
        loop {
            let chunk_len = read_chunk_length(socket).await?;
            if chunk_len == 0 {
                // Terminal bottle: EOF reached
                break;
            }

            copy_chunk_payload(socket, &mut file, chunk_len, &mut buf).await?;
            total_bytes += chunk_len as u64;
        }

        // 6. Flush file descriptor to disk
        file.sync_all().await?;
        tracing::info!(
            path = %target_path.display(),
            total_bytes = total_bytes,
            "File written and synced to disk"
        );
    }

    // 7. Send success response before closing connection
    let resp = MiabResponse::ok(format!("Saved {} bytes", total_bytes));
    resp.write_to(socket).await?;
    socket.flush().await?;

    Ok(())
}
