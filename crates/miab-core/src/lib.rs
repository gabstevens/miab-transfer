pub mod error;
pub mod frame;

pub use error::MiabError;
pub use frame::{
    compute_crc16, copy_chunk_payload_with_crc, read_chunk_length, read_terminal_checksum,
    sanitize_path, write_chunk, write_terminal_bottle, BottleChunk, ContentTag, HeaderFlags,
    MiabHeader, MiabResponse,
};

/// 4-byte ASCII magic identifier for MIAB v1: "MIAB"
pub const MAGIC: [u8; 4] = [0x4D, 0x49, 0x41, 0x42];

/// MIAB v1 Protocol Version
pub const PROTOCOL_VERSION: u8 = 0x01;

/// Fixed Header Preamble Size (64 bytes)
pub const HEADER_FIXED_SIZE: usize = 64;

// Command Opcodes
pub const CMD_PUT: u8 = 0x01;
pub const CMD_GET: u8 = 0x02;
pub const CMD_STAT: u8 = 0x03;
pub const CMD_DELETE: u8 = 0x04;

// Server Response Status Codes
pub const STATUS_OK: u8 = 0x00;
pub const STATUS_ERR: u8 = 0x01;
pub const STATUS_CHECKSUM_MISMATCH: u8 = 0x02;
pub const STATUS_FILE_EXISTS: u8 = 0x03;
pub const STATUS_AUTH_FAILED: u8 = 0x04;
pub const STATUS_INSUFFICIENT_SPACE: u8 = 0x05;
pub const STATUS_HEADER_CORRUPTED: u8 = 0x06;

// Network & Buffer Defaults
pub const DEFAULT_PORT: u16 = 6422;
pub const DEFAULT_CHUNK_SIZE: usize = 65536;
pub const MAX_CHUNK_SIZE: u32 = 16 * 1024 * 1024;
pub const UNKNOWN_SIZE: u64 = 0xFFFFFFFFFFFFFFFF;

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::path::Path;

    #[tokio::test]
    async fn test_header_roundtrip() {
        let mut header = MiabHeader::new("test/path/file.txt");
        header.expected_size = 1048576;
        header.timestamp_ms = 1760000000000;
        header.auth_token = [0xAA; 16];
        header.flags |= HeaderFlags::HAS_METADATA;
        header.metadata = b"{\"camera\":\"porch\"}".to_vec();

        let mut buffer = Vec::new();
        header.write_to(&mut buffer).await.unwrap();

        // Check that initial preamble is exactly 64 bytes
        assert_eq!(&buffer[0..4], &MAGIC);
        assert_eq!(buffer[4], PROTOCOL_VERSION);

        let mut cursor = Cursor::new(buffer);
        let decoded = MiabHeader::read_from(&mut cursor).await.unwrap();
        assert_eq!(header, decoded);
    }

    #[tokio::test]
    async fn test_header_invalid_magic() {
        let mut buffer = vec![0x00; 64];
        buffer.extend_from_slice(b"test");
        let mut cursor = Cursor::new(buffer);
        let err = MiabHeader::read_from(&mut cursor).await.unwrap_err();
        assert!(matches!(err, MiabError::InvalidMagic(_)));
    }

    #[tokio::test]
    async fn test_header_invalid_version() {
        let mut buffer = vec![0u8; 64];
        buffer[0..4].copy_from_slice(&MAGIC);
        buffer[4] = 0x99; // invalid version
        let mut cursor = Cursor::new(buffer);
        let err = MiabHeader::read_from(&mut cursor).await.unwrap_err();
        assert!(matches!(err, MiabError::UnsupportedVersion(0x99)));
    }

    #[tokio::test]
    async fn test_response_roundtrip() {
        let resp = MiabResponse::ok("Saved 1024 bytes");
        let mut buffer = Vec::new();
        resp.write_to(&mut buffer).await.unwrap();

        let mut cursor = Cursor::new(buffer);
        let decoded = MiabResponse::read_from(&mut cursor).await.unwrap();
        assert_eq!(resp, decoded);
        assert!(decoded.is_ok());

        let err_resp = MiabResponse::checksum_mismatch("CRC32 mismatch");
        let mut buffer = Vec::new();
        err_resp.write_to(&mut buffer).await.unwrap();
        let mut cursor = Cursor::new(buffer);
        let decoded_err = MiabResponse::read_from(&mut cursor).await.unwrap();
        assert_eq!(err_resp, decoded_err);
        assert!(!decoded_err.is_ok());
        assert_eq!(decoded_err.status, STATUS_CHECKSUM_MISMATCH);
    }

    #[tokio::test]
    async fn test_chunk_streaming_with_crc() {
        let mut buffer = Vec::new();
        let payload = b"hello world";
        let mut client_hasher = crc32fast::Hasher::new();
        client_hasher.update(payload);
        let expected_crc = client_hasher.finalize();

        write_chunk(&mut buffer, payload).await.unwrap();
        write_terminal_bottle(&mut buffer, Some(expected_crc))
            .await
            .unwrap();

        let mut cursor = Cursor::new(buffer);
        let len1 = read_chunk_length(&mut cursor).await.unwrap();
        assert_eq!(len1, 11);

        let mut server_hasher = crc32fast::Hasher::new();
        let mut sink = Vec::new();
        let mut temp_buf = vec![0u8; 64];
        copy_chunk_payload_with_crc(
            &mut cursor,
            &mut sink,
            len1,
            &mut temp_buf,
            &mut server_hasher,
        )
        .await
        .unwrap();
        assert_eq!(&sink, payload);

        let len2 = read_chunk_length(&mut cursor).await.unwrap();
        assert_eq!(len2, 0); // terminal bottle

        let received_crc = read_terminal_checksum(&mut cursor).await.unwrap();
        assert_eq!(received_crc, expected_crc);
        assert_eq!(server_hasher.finalize(), expected_crc);
    }

    #[test]
    fn test_path_sanitization() {
        let root = Path::new("/var/miab_storage");

        // Valid relative paths
        let p1 = sanitize_path(root, "photos/img.png").unwrap();
        assert_eq!(p1, Path::new("/var/miab_storage/photos/img.png"));

        let p2 = sanitize_path(root, "file.txt").unwrap();
        assert_eq!(p2, Path::new("/var/miab_storage/file.txt"));

        // Path traversal attempts
        assert!(sanitize_path(root, "../../etc/passwd").is_err());
        assert!(sanitize_path(root, "photos/../../etc/passwd").is_err());
        assert!(sanitize_path(root, "/etc/shadow").is_err());
        assert!(sanitize_path(root, "\\Windows\\System32").is_err());
        assert!(sanitize_path(root, "C:\\Windows\\System32").is_err());
        assert!(sanitize_path(root, "C:file.txt").is_err());
        assert!(sanitize_path(root, "").is_err());
    }
}
