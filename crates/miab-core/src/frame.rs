use std::path::{Component, Path, PathBuf};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::error::MiabError;
use crate::{CMD_PUT, MAGIC, MAX_CHUNK_SIZE, PROTOCOL_VERSION, STATUS_ERR, STATUS_OK};

/// MIAB v1 Initial Header sent by the client upon establishing a connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MiabHeader {
    pub version: u8,
    pub command: u8,
    pub path: String,
}

impl MiabHeader {
    /// Create a new default PUT header for the given relative path.
    pub fn new(path: impl Into<String>) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            command: CMD_PUT,
            path: path.into(),
        }
    }

    /// Decode the initial header from an async stream.
    pub async fn read_from<R: AsyncRead + Unpin>(reader: &mut R) -> Result<Self, MiabError> {
        let mut magic = [0u8; 4];
        reader.read_exact(&mut magic).await?;
        if magic != MAGIC {
            return Err(MiabError::InvalidMagic(magic));
        }

        let version = reader.read_u8().await?;
        if version != PROTOCOL_VERSION {
            return Err(MiabError::UnsupportedVersion(version));
        }

        let command = reader.read_u8().await?;
        if command != CMD_PUT {
            return Err(MiabError::UnsupportedCommand(command));
        }

        let path_len = reader.read_u16().await? as usize;
        let mut path_bytes = vec![0u8; path_len];
        reader.read_exact(&mut path_bytes).await?;

        let path = String::from_utf8(path_bytes).map_err(MiabError::InvalidUtf8Path)?;

        Ok(Self {
            version,
            command,
            path,
        })
    }

    /// Encode and write the initial header to an async stream.
    pub async fn write_to<W: AsyncWrite + Unpin>(&self, writer: &mut W) -> Result<(), MiabError> {
        let path_bytes = self.path.as_bytes();
        let path_len = u16::try_from(path_bytes.len())
            .map_err(|_| MiabError::PathTooLong(path_bytes.len()))?;

        writer.write_all(&MAGIC).await?;
        writer.write_u8(self.version).await?;
        writer.write_u8(self.command).await?;
        writer.write_u16(path_len).await?;
        writer.write_all(path_bytes).await?;
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

/// Write the terminal bottle chunk (chunk length 0) indicating EOF.
pub async fn write_terminal_bottle<W: AsyncWrite + Unpin>(writer: &mut W) -> Result<(), MiabError> {
    writer.write_u32(0).await?;
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

/// Stream the payload of a chunk into a writer using a bounded buffer slice.
pub async fn copy_chunk_payload<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    reader: &mut R,
    writer: &mut W,
    chunk_len: u32,
    buffer: &mut [u8],
) -> Result<(), MiabError> {
    let mut remaining = chunk_len as usize;
    while remaining > 0 {
        let to_read = remaining.min(buffer.len());
        reader.read_exact(&mut buffer[..to_read]).await?;
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
///
/// Rejects any path containing `..`, leading slashes `/` or `\`, Windows drive prefixes,
/// or invalid path components.
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
