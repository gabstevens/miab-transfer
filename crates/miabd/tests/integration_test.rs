use miab::client;
use miabd::server::Server;
use sha2::{Digest, Sha256};
use std::io::Write;
use tempfile::tempdir;
use tokio::fs;

#[tokio::test]
async fn test_full_file_transfer_and_verification() {
    // 1. Starts miabd on a random localhost port using a temporary directory for storage
    let storage_dir = tempdir().expect("failed to create storage tempdir");
    let storage_path = storage_dir.path().to_path_buf();

    let server = Server::bind("127.0.0.1:0", storage_path.clone())
        .await
        .expect("failed to bind server");
    let bound_addr = server.local_addr().expect("failed to get local addr");
    let server_addr = format!("127.0.0.1:{}", bound_addr.port());

    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
    let server_handle = tokio::spawn(async move { server.run_until_shutdown(shutdown_rx).await });

    // 2. Generates a synthetic 5MB test file with known contents
    let source_dir = tempdir().expect("failed to create source tempdir");
    let source_file_path = source_dir.path().join("source_5mb.dat");

    const FIVE_MB: usize = 5 * 1024 * 1024;
    let mut file_data = Vec::with_capacity(FIVE_MB);
    // Fill with a repeating byte pattern
    for i in 0..FIVE_MB {
        file_data.push((i % 251) as u8);
    }

    let mut hasher = Sha256::new();
    hasher.update(&file_data);
    let original_sha256 = hasher.finalize();

    {
        let mut f = std::fs::File::create(&source_file_path).expect("failed to create source file");
        f.write_all(&file_data)
            .expect("failed to write source file");
        f.flush().expect("failed to flush source file");
    }

    // 3. Invokes the client transfer logic to send the file
    let remote_relative_path = "nested/deep/directory/transferred_5mb.dat";
    let resp = client::send_file(&server_addr, remote_relative_path, &source_file_path, false)
        .await
        .expect("transfer failed");

    assert!(resp.is_ok(), "Server returned error: {}", resp.message);

    // 4. Verifies the received file exists on disk and its SHA256 checksum matches the original
    let received_file_path = storage_path.join(remote_relative_path);
    assert!(
        received_file_path.exists(),
        "Received file does not exist at {}",
        received_file_path.display()
    );

    let received_bytes = fs::read(&received_file_path)
        .await
        .expect("failed to read received file");
    assert_eq!(received_bytes.len(), FIVE_MB);

    let mut recv_hasher = Sha256::new();
    recv_hasher.update(&received_bytes);
    let received_sha256 = recv_hasher.finalize();

    assert_eq!(
        original_sha256, received_sha256,
        "SHA256 checksum mismatch for transferred file"
    );

    // 5. Tests path traversal rejection (e.g., attempting to send "../../etc/evil.txt" fails with an error code)
    let evil_path = "../../etc/evil.txt";
    let evil_result = client::send_file(&server_addr, evil_path, &source_file_path, false).await;

    match evil_result {
        Ok(evil_resp) => {
            assert!(
                !evil_resp.is_ok(),
                "Expected server to reject path traversal, but got success: {}",
                evil_resp.message
            );
            assert_eq!(evil_resp.status, miab_core::STATUS_ERR);
            assert!(
                evil_resp.message.to_lowercase().contains("traversal")
                    || evil_resp.message.to_lowercase().contains("not allowed")
                    || evil_resp.message.to_lowercase().contains(".."),
                "Unexpected error message: {}",
                evil_resp.message
            );
        }
        Err(_) => {
            // Early connection drop or write error on rejection is also valid
        }
    }

    // Verify evil.txt was never created outside the storage root
    let evil_outside_path = storage_path.join(evil_path);
    assert!(!evil_outside_path.exists());

    // Clean shutdown of server
    let _ = shutdown_tx.send(());
    let _ = server_handle.await;
}

#[tokio::test]
async fn test_streaming_transfer() {
    let storage_dir = tempdir().expect("failed to create storage tempdir");
    let storage_path = storage_dir.path().to_path_buf();

    let server = Server::bind("127.0.0.1:0", storage_path.clone())
        .await
        .expect("failed to bind server");
    let bound_addr = server.local_addr().expect("failed to get local addr");
    let server_addr = format!("127.0.0.1:{}", bound_addr.port());

    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
    let server_handle = tokio::spawn(async move { server.run_until_shutdown(shutdown_rx).await });

    let synthetic_stream_data = b"streaming direct payload without known size beforehand";
    let cursor = std::io::Cursor::new(synthetic_stream_data.to_vec());

    let resp = client::send_stream(&server_addr, "streamed/output.txt", cursor, false)
        .await
        .expect("send_stream failed");

    assert!(resp.is_ok());

    let output_path = storage_path.join("streamed/output.txt");
    assert!(output_path.exists());
    let saved = fs::read(&output_path).await.expect("failed to read output");
    assert_eq!(saved, synthetic_stream_data);

    let _ = shutdown_tx.send(());
    let _ = server_handle.await;
}

#[tokio::test]
async fn test_invalid_magic_dropped_immediately() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;

    let storage_dir = tempdir().expect("failed to create storage tempdir");
    let storage_path = storage_dir.path().to_path_buf();

    let server = Server::bind("127.0.0.1:0", storage_path.clone())
        .await
        .expect("failed to bind server");
    let bound_addr = server.local_addr().expect("failed to get local addr");
    let server_addr = format!("127.0.0.1:{}", bound_addr.port());

    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
    let server_handle = tokio::spawn(async move { server.run_until_shutdown(shutdown_rx).await });

    let mut stream = TcpStream::connect(&server_addr)
        .await
        .expect("failed to connect");
    // Send bad magic bytes "NOPE"
    stream.write_all(b"NOPE").await.expect("failed to write");
    stream.flush().await.expect("failed to flush");

    // The server should drop connection immediately without writing response
    let mut resp_buf = [0u8; 10];
    let res = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        stream.read(&mut resp_buf),
    )
    .await
    .expect("test timed out waiting for server drop");

    match res {
        Ok(0) => {}                                                     // EOF
        Err(e) if e.kind() == std::io::ErrorKind::ConnectionReset => {} // Connection reset by peer
        other => panic!(
            "Expected connection dropped (EOF or Reset), got {:?}",
            other
        ),
    }

    let _ = shutdown_tx.send(());
    let _ = server_handle.await;
}

#[tokio::test]
async fn test_crc32_corruption_rejection() {
    let storage_dir = tempdir().expect("failed to create storage tempdir");
    let storage_path = storage_dir.path().to_path_buf();

    let server = Server::bind("127.0.0.1:0", storage_path.clone())
        .await
        .expect("failed to bind server");
    let bound_addr = server.local_addr().expect("failed to get local addr");
    let server_addr = format!("127.0.0.1:{}", bound_addr.port());

    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
    let server_handle = tokio::spawn(async move { server.run_until_shutdown(shutdown_rx).await });

    let mut socket = tokio::net::TcpStream::connect(&server_addr).await.unwrap();
    let header = miab_core::MiabHeader::new("corrupt_test.bin");
    header.write_to(&mut socket).await.unwrap();

    // Send chunk
    miab_core::write_chunk(&mut socket, b"some real data")
        .await
        .unwrap();

    // Deliberately send wrong CRC32: 0xDEADBEEF
    miab_core::write_terminal_bottle(&mut socket, Some(0xDEADBEEF))
        .await
        .unwrap();

    let resp = miab_core::MiabResponse::read_from(&mut socket)
        .await
        .unwrap();
    assert_eq!(resp.status, miab_core::STATUS_CHECKSUM_MISMATCH);

    // Verify partial staging file and destination file are cleaned up
    assert!(!storage_path.join("corrupt_test.bin").exists());
    assert!(!storage_path.join(".corrupt_test.bin.miab.part").exists());

    let _ = shutdown_tx.send(());
    let _ = server_handle.await;
}

#[tokio::test]
async fn test_no_overwrite_rejection() {
    let storage_dir = tempdir().expect("failed to create storage tempdir");
    let storage_path = storage_dir.path().to_path_buf();

    let server = Server::bind("127.0.0.1:0", storage_path.clone())
        .await
        .expect("failed to bind server");
    let bound_addr = server.local_addr().expect("failed to get local addr");
    let server_addr = format!("127.0.0.1:{}", bound_addr.port());

    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
    let server_handle = tokio::spawn(async move { server.run_until_shutdown(shutdown_rx).await });

    // 1. First upload succeeds
    let resp1 = client::send_bytes_with_chunk_size(&server_addr, "important.dat", b"initial", 1024)
        .await
        .unwrap();
    assert!(resp1.is_ok());

    // 2. Second upload with NO_OVERWRITE flag set should fail
    let mut socket = tokio::net::TcpStream::connect(&server_addr).await.unwrap();
    let mut header = miab_core::MiabHeader::new("important.dat");
    header.flags |= miab_core::HeaderFlags::NO_OVERWRITE;
    header.write_to(&mut socket).await.unwrap();

    let resp2 = miab_core::MiabResponse::read_from(&mut socket)
        .await
        .unwrap();
    assert_eq!(resp2.status, miab_core::STATUS_FILE_EXISTS);

    // Verify original content remains intact
    let content = tokio::fs::read(storage_path.join("important.dat"))
        .await
        .unwrap();
    assert_eq!(&content, b"initial");

    let _ = shutdown_tx.send(());
    let _ = server_handle.await;
}

#[tokio::test]
async fn test_auth_token_enforcement() {
    let storage_dir = tempdir().expect("failed to create storage tempdir");
    let storage_path = storage_dir.path().to_path_buf();
    let required_token = [0x42u8; 16];

    let server = Server::bind_with_options(
        "127.0.0.1:0",
        storage_path.clone(),
        false,
        Some(required_token),
    )
    .await
    .expect("failed to bind server");
    let bound_addr = server.local_addr().expect("failed to get local addr");
    let server_addr = format!("127.0.0.1:{}", bound_addr.port());

    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
    let server_handle = tokio::spawn(async move { server.run_until_shutdown(shutdown_rx).await });

    // 1. Client with wrong token is rejected with STATUS_AUTH_FAILED
    let mut socket_bad = tokio::net::TcpStream::connect(&server_addr).await.unwrap();
    let mut header_bad = miab_core::MiabHeader::new("auth_test.bin");
    header_bad.auth_token = [0x99u8; 16];
    header_bad.write_to(&mut socket_bad).await.unwrap();

    let resp_bad = miab_core::MiabResponse::read_from(&mut socket_bad)
        .await
        .unwrap();
    assert_eq!(resp_bad.status, miab_core::STATUS_AUTH_FAILED);

    // 2. Client with correct token is accepted
    let mut socket_good = tokio::net::TcpStream::connect(&server_addr).await.unwrap();
    let mut header_good = miab_core::MiabHeader::new("auth_test.bin");
    header_good.auth_token = required_token;
    header_good.write_to(&mut socket_good).await.unwrap();
    let empty_crc = crc32fast::Hasher::new().finalize();
    miab_core::write_terminal_bottle(&mut socket_good, Some(empty_crc))
        .await
        .unwrap();

    let resp_good = miab_core::MiabResponse::read_from(&mut socket_good)
        .await
        .unwrap();
    assert_eq!(resp_good.status, miab_core::STATUS_OK);

    let _ = shutdown_tx.send(());
    let _ = server_handle.await;
}
