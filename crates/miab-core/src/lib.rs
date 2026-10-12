pub mod error;
pub mod frame;

pub use error::MiabError;
pub use frame::{
    copy_chunk_payload, read_chunk_length, sanitize_path, write_chunk, write_terminal_bottle,
    BottleChunk, MiabHeader, MiabResponse,
};

/// 4-byte ASCII magic identifier for MIAB v1: "MIAB"
pub const MAGIC: [u8; 4] = [0x4D, 0x49, 0x41, 0x42];

/// MIAB v1 Protocol Version
pub const PROTOCOL_VERSION: u8 = 0x01;

/// Command: PUT / STREAM
pub const CMD_PUT: u8 = 0x01;

/// Default TCP port
pub const DEFAULT_PORT: u16 = 6422;

/// Default chunk size (64KB)
pub const DEFAULT_CHUNK_SIZE: usize = 65536;

/// Maximum allowed chunk size (16MB safety bound)
pub const MAX_CHUNK_SIZE: u32 = 16 * 1024 * 1024;

/// Server response status OK
pub const STATUS_OK: u8 = 0x00;

/// Server response status FAIL
pub const STATUS_ERR: u8 = 0x01;

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::path::Path;

    #[tokio::test]
    async fn test_header_roundtrip() {
        let header = MiabHeader::new("test/path/file.txt");
        let mut buffer = Vec::new();
        header.write_to(&mut buffer).await.unwrap();

        let mut cursor = Cursor::new(buffer);
        let decoded = MiabHeader::read_from(&mut cursor).await.unwrap();
        assert_eq!(header, decoded);
    }

    #[tokio::test]
    async fn test_header_invalid_magic() {
        let mut buffer = vec![0x00, 0x00, 0x00, 0x00, 0x01, 0x01, 0x00, 0x04];
        buffer.extend_from_slice(b"test");
        let mut cursor = Cursor::new(buffer);
        let err = MiabHeader::read_from(&mut cursor).await.unwrap_err();
        assert!(matches!(err, MiabError::InvalidMagic(_)));
    }

    #[tokio::test]
    async fn test_header_invalid_version() {
        let mut buffer = MAGIC.to_vec();
        buffer.push(0x02); // unsupported version
        buffer.push(CMD_PUT);
        buffer.extend_from_slice(&2u16.to_be_bytes());
        buffer.extend_from_slice(b"ab");
        let mut cursor = Cursor::new(buffer);
        let err = MiabHeader::read_from(&mut cursor).await.unwrap_err();
        assert!(matches!(err, MiabError::UnsupportedVersion(2)));
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

        let err_resp = MiabResponse::err("Permission denied");
        let mut buffer = Vec::new();
        err_resp.write_to(&mut buffer).await.unwrap();
        let mut cursor = Cursor::new(buffer);
        let decoded_err = MiabResponse::read_from(&mut cursor).await.unwrap();
        assert_eq!(err_resp, decoded_err);
        assert!(!decoded_err.is_ok());
    }

    #[tokio::test]
    async fn test_chunk_streaming() {
        let mut buffer = Vec::new();
        write_chunk(&mut buffer, b"hello world").await.unwrap();
        write_terminal_bottle(&mut buffer).await.unwrap();

        let mut cursor = Cursor::new(buffer);
        let len1 = read_chunk_length(&mut cursor).await.unwrap();
        assert_eq!(len1, 11);
        let mut data = vec![0u8; 11];
        tokio::io::AsyncReadExt::read_exact(&mut cursor, &mut data).await.unwrap();
        assert_eq!(&data, b"hello world");

        let len2 = read_chunk_length(&mut cursor).await.unwrap();
        assert_eq!(len2, 0); // terminal bottle
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
