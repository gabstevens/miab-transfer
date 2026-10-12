use indicatif::{ProgressBar, ProgressStyle};
use miab_core::{
    write_chunk, write_terminal_bottle, MiabError, MiabHeader, MiabResponse, DEFAULT_CHUNK_SIZE,
    DEFAULT_PORT,
};
use std::path::Path;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::net::TcpStream;

/// Normalize a server address string. If no port is specified, default to 6422.
pub fn normalize_server_addr(server: &str) -> String {
    if server.contains(':') {
        server.to_string()
    } else {
        format!("{}:{}", server, DEFAULT_PORT)
    }
}

/// Send a local file to the remote MIAB server.
pub async fn send_file(
    server: &str,
    remote_path: &str,
    local_file: &Path,
    show_progress: bool,
) -> Result<MiabResponse, MiabError> {
    let mut file = tokio::fs::File::open(local_file).await?;
    let file_size = file.metadata().await?.len();

    let addr = normalize_server_addr(server);
    let mut socket = TcpStream::connect(&addr).await?;

    // 1. Send initial header
    let header = MiabHeader::new(remote_path);
    header.write_to(&mut socket).await?;

    // 2. Setup progress bar if requested
    let pb = if show_progress {
        let bar = ProgressBar::new(file_size);
        if let Ok(style) = ProgressStyle::default_bar()
            .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({eta})")
        {
            bar.set_style(style.progress_chars("#>-"));
        }
        Some(bar)
    } else {
        None
    };

    let mut buf = vec![0u8; DEFAULT_CHUNK_SIZE];
    let (mut socket_rx, mut socket_tx) = socket.into_split();

    // 3. Concurrently stream chunks while listening for early server response
    let send_future = async {
        loop {
            let n = file.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            write_chunk(&mut socket_tx, &buf[..n]).await?;
            if let Some(ref p) = pb {
                p.inc(n as u64);
            }
        }
        write_terminal_bottle(&mut socket_tx).await?;
        Ok::<(), MiabError>(())
    };

    let response_future = async { MiabResponse::read_from(&mut socket_rx).await };

    tokio::select! {
        early_resp = response_future => {
            if let Some(p) = pb {
                p.finish_and_clear();
            }
            early_resp
        }
        send_res = send_future => {
            if let Err(e) = send_res {
                // If writing errored (e.g. server closed connection), attempt to read server error response
                if let Ok(resp) = MiabResponse::read_from(&mut socket_rx).await {
                    if let Some(p) = pb {
                        p.finish_and_clear();
                    }
                    return Ok(resp);
                }
                if let Some(p) = pb {
                    p.finish_and_clear();
                }
                return Err(e);
            }
            let resp = MiabResponse::read_from(&mut socket_rx).await;
            if let Some(p) = pb {
                p.finish_and_clear();
            }
            resp
        }
    }
}

/// Stream an arbitrary async reader (such as stdin) to the remote MIAB server.
pub async fn send_stream<R: AsyncRead + Unpin>(
    server: &str,
    remote_path: &str,
    mut reader: R,
    show_progress: bool,
) -> Result<MiabResponse, MiabError> {
    let addr = normalize_server_addr(server);
    let mut socket = TcpStream::connect(&addr).await?;

    // 1. Send initial header
    let header = MiabHeader::new(remote_path);
    header.write_to(&mut socket).await?;

    // 2. Setup progress bar if requested
    let pb = if show_progress {
        let bar = ProgressBar::new_spinner();
        if let Ok(style) = ProgressStyle::default_spinner()
            .template("{spinner:.green} [{elapsed_precise}] {bytes} sent ({bytes_per_sec})")
        {
            bar.set_style(style);
        }
        bar.set_message("Streaming bytes...");
        Some(bar)
    } else {
        None
    };

    let mut buf = vec![0u8; DEFAULT_CHUNK_SIZE];
    let (mut socket_rx, mut socket_tx) = socket.into_split();

    let send_future = async {
        loop {
            let n = reader.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            write_chunk(&mut socket_tx, &buf[..n]).await?;
            if let Some(ref p) = pb {
                p.inc(n as u64);
            }
        }
        write_terminal_bottle(&mut socket_tx).await?;
        Ok::<(), MiabError>(())
    };

    let response_future = async { MiabResponse::read_from(&mut socket_rx).await };

    tokio::select! {
        early_resp = response_future => {
            if let Some(p) = pb {
                p.finish_and_clear();
            }
            early_resp
        }
        send_res = send_future => {
            if let Err(e) = send_res {
                if let Ok(resp) = MiabResponse::read_from(&mut socket_rx).await {
                    if let Some(p) = pb {
                        p.finish_and_clear();
                    }
                    return Ok(resp);
                }
                if let Some(p) = pb {
                    p.finish_and_clear();
                }
                return Err(e);
            }
            let resp = MiabResponse::read_from(&mut socket_rx).await;
            if let Some(p) = pb {
                p.finish_and_clear();
            }
            resp
        }
    }
}
