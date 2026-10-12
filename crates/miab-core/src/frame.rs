use std::path::{Component, Path, PathBuf};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::error::MiabError;
use crate::{
    CMD_PUT, HEADER_FIXED_SIZE, MAGIC, MAX_CHUNK_SIZE, PROTOCOL_VERSION, STATUS_AUTH_FAILED,
    STATUS_CHECKSUM_MISMATCH, STATUS_ERR, STATUS_FILE_EXISTS, STATUS_HEADER_CORRUPTED,
    STATUS_INSUFFICIENT_SPACE, STATUS_OK, UNKNOWN_SIZE,
};

/// Header Flags bitmask constants per RFC 001.
pub struct HeaderFlags;

impl HeaderFlags {
    pub const NO_OVERWRITE: u16 = 0x0001;
    pub const APPEND: u16 = 0x0002;
    pub const ATOMIC_TMP: u16 = 0x0004;
    pub const SET_EXECUTABLE: u16 = 0x0008;
    pub const CHECKSUM_PRESENT: u16 = 0x0010;
    pub const HAS_METADATA: u16 = 0x0020;
}

/// Content format tag per RFC 001.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u16)]
pub enum ContentTag {
    #[default]
    GenericBinary = 0x0000,
    JpegImage = 0x0001,
    VideoStream = 0x0002,
    CsvTelemetry = 0x0003,
    TarArchive = 0x0004,
    SqliteDb = 0x0005,
    ZstdCompressed = 0x0006,
}

impl ContentTag {
    pub fn from_u16(val: u16) -> Self {
        match val {
            0x0001 => Self::JpegImage,
            0x0002 => Self::VideoStream,
            0x0003 => Self::CsvTelemetry,
            0x0004 => Self::TarArchive,
            0x0005 => Self::SqliteDb,
            0x0006 => Self::ZstdCompressed,
            _ => Self::GenericBinary,
        }
    }
}

/// Calculate CRC-16/CCITT checksum over a byte slice.
pub fn compute_crc16(data: &[u8]) -> u16 {
    let mut crc: u16 = 0xFFFF;
    for &byte in data {
        crc ^= (byte as u16) << 8;
        for _ in 0..8 {
            if crc & 0x8000 != 0 {
                crc = (crc << 1) ^ 0x1021;
            } else {
                crc <<= 1;
            }
        }
    }
    crc
}

/// MIAB v1 Initial Header (64-byte fixed preamble + variable path & metadata).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MiabHeader {
    pub version: u8,
    pub opcode: u8,
    pub flags: u16,
    pub content_tag: u16,
    pub expected_size: u64,
    pub byte_offset: u64,
    pub timestamp_ms: u64,
    pub auth_token: [u8; 16],
    pub trace_id: u64,
    pub path: String,
    pub metadata: Vec<u8>,
}

impl MiabHeader {
    /// Create a new default PUT header for the given relative path with atomic staging and CRC32 enabled.
    pub fn new(path: impl Into<String>) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            opcode: CMD_PUT,
            flags: HeaderFlags::ATOMIC_TMP | HeaderFlags::CHECKSUM_PRESENT,
            content_tag: ContentTag::GenericBinary as u16,
            expected_size: UNKNOWN_SIZE,
            byte_offset: 0,
            timestamp_ms: 0,
            auth_token: [0u8; 16],
            trace_id: 0,
            path: path.into(),
            metadata: Vec::new(),
        }
    }

    /// Check if NO_OVERWRITE flag is set.
    pub fn is_no_overwrite(&self) -> bool {
        self.flags & HeaderFlags::NO_OVERWRITE != 0
    }

    /// Check if APPEND flag is set.
    pub fn is_append(&self) -> bool {
        self.flags & HeaderFlags::APPEND != 0
    }

    /// Check if ATOMIC_TMP flag is set.
    pub fn is_atomic(&self) -> bool {
        self.flags & HeaderFlags::ATOMIC_TMP != 0
    }

    /// Check if SET_EXECUTABLE flag is set.
    pub fn is_executable(&self) -> bool {
        self.flags & HeaderFlags::SET_EXECUTABLE != 0
    }

    /// Check if CHECKSUM_PRESENT flag is set.
    pub fn is_checksum_present(&self) -> bool {
        self.flags & HeaderFlags::CHECKSUM_PRESENT != 0
    }

    /// Check if HAS_METADATA flag is set.
    pub fn has_metadata(&self) -> bool {
        self.flags & HeaderFlags::HAS_METADATA != 0
    }

    /// Decode the 64-byte header preamble and trailing path/metadata from an async stream.
    pub async fn read_from<R: AsyncRead + Unpin>(reader: &mut R) -> Result<Self, MiabError> {
        let mut magic = [0u8; 4];
        reader.read_exact(&mut magic).await?;
        if magic != MAGIC {
            return Err(MiabError::InvalidMagic(magic));
        }

        let mut remaining = [0u8; HEADER_FIXED_SIZE - 4];
        reader.read_exact(&mut remaining).await?;

        let mut preamble = [0u8; HEADER_FIXED_SIZE];
        preamble[0..4].copy_from_slice(&magic);
        preamble[4..HEADER_FIXED_SIZE].copy_from_slice(&remaining);

        // 2. Version
        let version = preamble[4];
        if version != PROTOCOL_VERSION {
            return Err(MiabError::UnsupportedVersion(version));
        }

        // 3. Opcode
        let opcode = preamble[5];

        // 4. Flags
        let flags = u16::from_be_bytes([preamble[6], preamble[7]]);

        // 5. Content Tag
        let content_tag = u16::from_be_bytes([preamble[8], preamble[9]]);

        // 6. Path Length
        let path_len = u16::from_be_bytes([preamble[10], preamble[11]]) as usize;

        // 7. Expected Total Size
        let expected_size = u64::from_be_bytes([
            preamble[12],
            preamble[13],
            preamble[14],
            preamble[15],
            preamble[16],
            preamble[17],
            preamble[18],
            preamble[19],
        ]);

        // 8. Byte Offset
        let byte_offset = u64::from_be_bytes([
            preamble[20],
            preamble[21],
            preamble[22],
            preamble[23],
            preamble[24],
            preamble[25],
            preamble[26],
            preamble[27],
        ]);

        // 9. Timestamp (ms)
        let timestamp_ms = u64::from_be_bytes([
            preamble[28],
            preamble[29],
            preamble[30],
            preamble[31],
            preamble[32],
            preamble[33],
            preamble[34],
            preamble[35],
        ]);

        // 10. Auth Token (16 bytes)
        let mut auth_token = [0u8; 16];
        auth_token.copy_from_slice(&preamble[36..52]);

        // 11. Trace ID
        let trace_id = u64::from_be_bytes([
            preamble[52],
            preamble[53],
            preamble[54],
            preamble[55],
            preamble[56],
            preamble[57],
            preamble[58],
            preamble[59],
        ]);

        // 12. Metadata Length
        let metadata_len = u16::from_be_bytes([preamble[60], preamble[61]]) as usize;

        // 13. Header CRC-16
        let header_crc16 = u16::from_be_bytes([preamble[62], preamble[63]]);
        if header_crc16 != 0 {
            let computed_crc = compute_crc16(&preamble[..62]);
            if computed_crc != header_crc16 {
                return Err(MiabError::HeaderCorrupted(format!(
                    "CRC16 mismatch: expected 0x{:04X}, computed 0x{:04X}",
                    header_crc16, computed_crc
                )));
            }
        }

        // Read relative path string
        let mut path_bytes = vec![0u8; path_len];
        reader.read_exact(&mut path_bytes).await?;
        let path = String::from_utf8(path_bytes).map_err(MiabError::InvalidUtf8Path)?;

        // Read optional metadata block if flag set and length > 0
        let mut metadata = Vec::new();
        if flags & HeaderFlags::HAS_METADATA != 0 && metadata_len > 0 {
            metadata = vec![0u8; metadata_len];
            reader.read_exact(&mut metadata).await?;
        }

        Ok(Self {
            version,
            opcode,
            flags,
            content_tag,
            expected_size,
            byte_offset,
            timestamp_ms,
            auth_token,
            trace_id,
            path,
            metadata,
        })
    }

    /// Encode and write the 64-byte header and extensions to an async stream.
    pub async fn write_to<W: AsyncWrite + Unpin>(&self, writer: &mut W) -> Result<(), MiabError> {
        let path_bytes = self.path.as_bytes();
        let path_len = u16::try_from(path_bytes.len())
            .map_err(|_| MiabError::PathTooLong(path_bytes.len()))?;

        let metadata_len = u16::try_from(self.metadata.len())
            .map_err(|_| MiabError::MetadataTooLong(self.metadata.len()))?;

        let mut flags = self.flags;
        if !self.metadata.is_empty() {
            flags |= HeaderFlags::HAS_METADATA;
        }

        let mut preamble = [0u8; HEADER_FIXED_SIZE];
        preamble[0..4].copy_from_slice(&MAGIC);
        preamble[4] = self.version;
        preamble[5] = self.opcode;
        preamble[6..8].copy_from_slice(&flags.to_be_bytes());
        preamble[8..10].copy_from_slice(&self.content_tag.to_be_bytes());
        preamble[10..12].copy_from_slice(&path_len.to_be_bytes());
        preamble[12..20].copy_from_slice(&self.expected_size.to_be_bytes());
        preamble[20..28].copy_from_slice(&self.byte_offset.to_be_bytes());
        preamble[28..36].copy_from_slice(&self.timestamp_ms.to_be_bytes());
        preamble[36..52].copy_from_slice(&self.auth_token);
        preamble[52..60].copy_from_slice(&self.trace_id.to_be_bytes());
        preamble[60..62].copy_from_slice(&metadata_len.to_be_bytes());

        // Compute CRC-16 over first 62 bytes
        let header_crc = compute_crc16(&preamble[..62]);
        preamble[62..64].copy_from_slice(&header_crc.to_be_bytes());

        writer.write_all(&preamble).await?;
        writer.write_all(path_bytes).await?;
        if !self.metadata.is_empty() {
            writer.write_all(&self.metadata).await?;
        }
        writer.flush().await?;
        Ok(())
    }
}

/// In-memory representation of a single bottle chunk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BottleChunk {
    pub payload: Vec<u8>,
}

impl BottleChunk {
    pub fn new(payload: Vec<u8>) -> Self {
        Self { payload }
    }

    pub fn is_terminal(&self) -> bool {
        self.payload.is_empty()
    }
}

/// Write a single data chunk to the async writer.
pub async fn write_chunk<W: AsyncWrite + Unpin>(
    writer: &mut W,
    chunk: &[u8],
) -> Result<(), MiabError> {
    if chunk.is_empty() {
        return Ok(());
    }
    let len = u32::try_from(chunk.len())
        .map_err(|_| MiabError::ChunkTooLarge(chunk.len() as u32, MAX_CHUNK_SIZE))?;
    writer.write_u32(len).await?;
    writer.write_all(chunk).await?;
    Ok(())
}

/// Write the terminal bottle chunk (length 0), optionally appending a 4-byte CRC-32 checksum.
pub async fn write_terminal_bottle<W: AsyncWrite + Unpin>(
    writer: &mut W,
    checksum: Option<u32>,
) -> Result<(), MiabError> {
    writer.write_u32(0).await?;
    if let Some(crc) = checksum {
        writer.write_u32(crc).await?;
    }
    writer.flush().await?;
    Ok(())
}

/// Read the length of the next chunk. Returns 0 for terminal bottle (EOF).
pub async fn read_chunk_length<R: AsyncRead + Unpin>(reader: &mut R) -> Result<u32, MiabError> {
    let len = reader.read_u32().await?;
    if len > MAX_CHUNK_SIZE {
        return Err(MiabError::ChunkTooLarge(len, MAX_CHUNK_SIZE));
    }
    Ok(len)
}

/// Read the terminal CRC-32 checksum if present.
pub async fn read_terminal_checksum<R: AsyncRead + Unpin>(
    reader: &mut R,
) -> Result<u32, MiabError> {
    Ok(reader.read_u32().await?)
}

/// Stream the payload of a chunk into a writer using a bounded buffer slice,
/// while simultaneously updating a running CRC-32 hasher.
pub async fn copy_chunk_payload_with_crc<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    reader: &mut R,
    writer: &mut W,
    chunk_len: u32,
    buffer: &mut [u8],
    hasher: &mut crc32fast::Hasher,
) -> Result<(), MiabError> {
    let mut remaining = chunk_len as usize;
    while remaining > 0 {
        let to_read = remaining.min(buffer.len());
        reader.read_exact(&mut buffer[..to_read]).await?;
        hasher.update(&buffer[..to_read]);
        writer.write_all(&buffer[..to_read]).await?;
        remaining -= to_read;
    }
    Ok(())
}

/// Server response frame sent before closing the connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MiabResponse {
    pub status: u8,
    pub message: String,
}

impl MiabResponse {
    /// Create a success response (status 0x00).
    pub fn ok(message: impl Into<String>) -> Self {
        Self {
            status: STATUS_OK,
            message: message.into(),
        }
    }

    /// Create an error response (status 0x01).
    pub fn err(message: impl Into<String>) -> Self {
        Self {
            status: STATUS_ERR,
            message: message.into(),
        }
    }

    /// Create a checksum mismatch error response (status 0x02).
    pub fn checksum_mismatch(message: impl Into<String>) -> Self {
        Self {
            status: STATUS_CHECKSUM_MISMATCH,
            message: message.into(),
        }
    }

    /// Create a file exists error response (status 0x03).
    pub fn file_exists(message: impl Into<String>) -> Self {
        Self {
            status: STATUS_FILE_EXISTS,
            message: message.into(),
        }
    }

    /// Create an auth failed error response (status 0x04).
    pub fn auth_failed(message: impl Into<String>) -> Self {
        Self {
            status: STATUS_AUTH_FAILED,
            message: message.into(),
        }
    }

    /// Create an insufficient space error response (status 0x05).
    pub fn insufficient_space(message: impl Into<String>) -> Self {
        Self {
            status: STATUS_INSUFFICIENT_SPACE,
            message: message.into(),
        }
    }

    /// Create a header corrupted error response (status 0x06).
    pub fn header_corrupted(message: impl Into<String>) -> Self {
        Self {
            status: STATUS_HEADER_CORRUPTED,
            message: message.into(),
        }
    }

    /// Returns true if status is Success (0x00).
    pub fn is_ok(&self) -> bool {
        self.status == STATUS_OK
    }

    /// Read and decode a response frame from the stream.
    pub async fn read_from<R: AsyncRead + Unpin>(reader: &mut R) -> Result<Self, MiabError> {
        let status = reader.read_u8().await?;
        let msg_len = reader.read_u16().await? as usize;
        let mut msg_bytes = vec![0u8; msg_len];
        reader.read_exact(&mut msg_bytes).await?;
        let message = String::from_utf8(msg_bytes).map_err(MiabError::InvalidUtf8Response)?;
        Ok(Self { status, message })
    }

    /// Encode and write a response frame to the stream.
    pub async fn write_to<W: AsyncWrite + Unpin>(&self, writer: &mut W) -> Result<(), MiabError> {
        let msg_bytes = self.message.as_bytes();
        let msg_len = u16::try_from(msg_bytes.len())
            .map_err(|_| MiabError::MessageTooLong(msg_bytes.len()))?;

        writer.write_u8(self.status).await?;
        writer.write_u16(msg_len).await?;
        writer.write_all(msg_bytes).await?;
        writer.flush().await?;
        Ok(())
    }
}

/// Sanitize the requested relative path and safely resolve it inside `storage_root`.
pub fn sanitize_path(storage_root: &Path, rel_path: &str) -> Result<PathBuf, MiabError> {
    if rel_path.is_empty() {
        return Err(MiabError::PathTraversalDetected(
            "Path is empty".to_string(),
        ));
    }

    // Reject leading slashes
    if rel_path.starts_with('/') || rel_path.starts_with('\\') {
        return Err(MiabError::PathTraversalDetected(
            "Leading slashes are not allowed".to_string(),
        ));
    }

    // Reject paths containing ".."
    if rel_path.contains("..") {
        return Err(MiabError::PathTraversalDetected(
            "Path cannot contain '..' components".to_string(),
        ));
    }

    // Reject Windows drive prefixes or colons (e.g. C:, D:\, foo:bar)
    if rel_path.contains(':') {
        return Err(MiabError::PathTraversalDetected(
            "Windows drive prefixes and colons are not allowed".to_string(),
        ));
    }

    let path = Path::new(rel_path);
    let mut safe_rel = PathBuf::new();

    for comp in path.components() {
        match comp {
            Component::Normal(segment) => {
                let seg_str = segment.to_string_lossy();
                if seg_str == ".." || seg_str.is_empty() {
                    return Err(MiabError::PathTraversalDetected(
                        "Invalid path segment".to_string(),
                    ));
                }
                safe_rel.push(segment);
            }
            Component::CurDir => {
                // Safely skip redundant current-dir reference '.'
            }
            Component::ParentDir => {
                return Err(MiabError::PathTraversalDetected(
                    "Parent directory traversal '..' is not allowed".to_string(),
                ));
            }
            Component::RootDir => {
                return Err(MiabError::PathTraversalDetected(
                    "Root directory prefix is not allowed".to_string(),
                ));
            }
            Component::Prefix(_) => {
                return Err(MiabError::PathTraversalDetected(
                    "Prefix component is not allowed".to_string(),
                ));
            }
        }
    }

    if safe_rel.as_os_str().is_empty() {
        return Err(MiabError::PathTraversalDetected(
            "Relative path resolved to empty".to_string(),
        ));
    }

    Ok(storage_root.join(safe_rel))
}
