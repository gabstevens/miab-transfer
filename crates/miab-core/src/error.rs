use thiserror::Error;

#[derive(Error, Debug)]
pub enum MiabError {
    #[error("Invalid magic bytes: expected [0x4D, 0x49, 0x41, 0x42] ('MIAB'), got {0:02X?}")]
    InvalidMagic([u8; 4]),

    #[error("Unsupported protocol version: {0} (supported: 1)")]
    UnsupportedVersion(u8),

    #[error("Unsupported command opcode: {0} (supported: 1 for PUT/STREAM)")]
    UnsupportedCommand(u8),

    #[error("Invalid UTF-8 sequence in path: {0}")]
    InvalidUtf8Path(std::string::FromUtf8Error),

    #[error("Invalid UTF-8 sequence in response: {0}")]
    InvalidUtf8Response(std::string::FromUtf8Error),

    #[error("Path too long: length {0} exceeds max u16 length (65535)")]
    PathTooLong(usize),

    #[error("Message too long: length {0} exceeds max u16 length (65535)")]
    MessageTooLong(usize),

    #[error("Chunk too large: {0} bytes (max allowed is {1})")]
    ChunkTooLarge(u32, u32),

    #[error("Path traversal detected: {0}")]
    PathTraversalDetected(String),

    #[error("Protocol error: {0}")]
    Protocol(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}
