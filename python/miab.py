"""miab - Zero-dependency Message In A Bottle (MIAB v1) client for Python.

Implements the MIAB RFC-001 wire protocol over raw TCP.
Compatible with Python 3.8+ using only standard library modules (socket, struct, os, sys).
"""

from __future__ import annotations

import argparse
import os
from pathlib import Path
import socket
import struct
import sys
import time
from typing import BinaryIO, Optional, Tuple, Union

# Protocol constants (RFC-001)
MAGIC: bytes = b"MIAB"
PROTOCOL_VERSION: int = 1
CMD_PUT: int = 1
DEFAULT_PORT: int = 6422
DEFAULT_CHUNK_SIZE: int = 65536  # 64 KB
STATUS_OK: int = 0
STATUS_ERR: int = 1


class MiabError(Exception):
    """Base exception for all MIAB client errors."""


class MiabServerError(MiabError):
    """Raised when the MIAB server returns an error status response."""

    def __init__(self, status: int, message: str) -> None:
        super().__init__(f"Server error (status {status}): {message}")
        self.status = status
        self.message = message


class MiabProtocolError(MiabError):
    """Raised on wire framing, connection, or protocol violation."""


def parse_server_addr(server: str, default_port: int = DEFAULT_PORT) -> Tuple[str, int]:
    """Parse a server address string into (host, port).

    Supports 'host:port', 'host', or '[ipv6]:port'.
    """
    server = server.strip()
    if not server:
        raise ValueError("Server address cannot be empty")

    if server.startswith("["):
        # IPv6 [addr]:port
        closing = server.find("]")
        if closing == -1:
            raise ValueError(f"Invalid IPv6 address format: {server}")
        host = server[1:closing]
        rest = server[closing + 1 :]
        if rest.startswith(":"):
            port = int(rest[1:])
        else:
            port = default_port
        return host, port

    if ":" in server:
        host, port_str = server.rsplit(":", 1)
        try:
            return host, int(port_str)
        except ValueError:
            raise ValueError(f"Invalid port in server address: {server}")
    else:
        return server, default_port


def _recv_exact(sock: socket.socket, length: int) -> bytes:
    """Read exactly `length` bytes from a socket, or raise EOFError."""
    chunks = []
    received = 0
    while received < length:
        chunk = sock.recv(length - received)
        if not chunk:
            raise EOFError(
                f"Connection closed prematurely (received {received}/{length} bytes)"
            )
        chunks.append(chunk)
        received += len(chunk)
    return b"".join(chunks)


class Client:
    """MIAB v1 TCP client for sending files, byte buffers, and streams."""

    def __init__(
        self,
        server_addr: str,
        timeout: Optional[float] = None,
        chunk_size: int = DEFAULT_CHUNK_SIZE,
    ) -> None:
        """Initialize the client with a target server address.

        :param server_addr: Server address as 'host' or 'host:port' (default port: 6422).
        :param timeout: Optional socket timeout in seconds.
        :param chunk_size: Size of stream chunk frames (default: 65,536 bytes / 64 KB).
        """
        self.host, self.port = parse_server_addr(server_addr)
        self.timeout = timeout
        self.chunk_size = chunk_size

    def _connect(self) -> socket.socket:
        sock = socket.create_connection((self.host, self.port), timeout=self.timeout)
        # Enable TCP_NODELAY for responsive frame streaming
        try:
            sock.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
        except (AttributeError, OSError):
            pass
        return sock

    def _send_header(self, sock: socket.socket, remote_path: str) -> None:
        path_bytes = remote_path.encode("utf-8")
        if len(path_bytes) > 65535:
            raise ValueError(
                f"Remote path length ({len(path_bytes)}) exceeds 65535 bytes"
            )

        # Wire Header: Magic(4B) + Version(1B) + Opcode(1B) + PathLength(2B) + PathString(NB)
        header = (
            MAGIC
            + struct.pack(">BBH", PROTOCOL_VERSION, CMD_PUT, len(path_bytes))
            + path_bytes
        )
        sock.sendall(header)

    def _read_response(self, sock: socket.socket) -> str:
        # Response: Status(1B) + MessageLength(2B) + Message(MB)
        header = _recv_exact(sock, 3)
        status, msg_len = struct.unpack(">BH", header)
        msg_bytes = _recv_exact(sock, msg_len)
        message = msg_bytes.decode("utf-8", errors="replace")

        if status != STATUS_OK:
            raise MiabServerError(status, message)
        return message

    def put_bytes(self, remote_path: str, data: Union[bytes, bytearray, memoryview]) -> str:
        """Transfer an in-memory byte buffer to the remote server.

        :param remote_path: Destination relative path on the server.
        :param data: Byte buffer to transfer.
        :return: Server acknowledgment message string on success.
        :raises MiabServerError: If server rejects transfer (e.g., path traversal).
        """
        view = memoryview(data)
        total_len = len(view)

        with self._connect() as sock:
            self._send_header(sock, remote_path)

            offset = 0
            while offset < total_len:
                chunk = view[offset : offset + self.chunk_size]
                sock.sendall(struct.pack(">I", len(chunk)))
                sock.sendall(chunk)
                offset += len(chunk)

            # Terminal bottle: length = 0
            sock.sendall(struct.pack(">I", 0))

            return self._read_response(sock)

    def put_stream(
        self,
        remote_path: str,
        stream: BinaryIO,
        progress: bool = False,
        total_bytes: Optional[int] = None,
    ) -> str:
        """Transfer an arbitrary binary stream (e.g. sys.stdin.buffer) to the remote server.

        :param remote_path: Destination relative path on the server.
        :param stream: Readable binary stream.
        :param progress: Whether to display a terminal progress indicator.
        :param total_bytes: Optional known size for calculating percentage.
        :return: Server acknowledgment message string on success.
        """
        with self._connect() as sock:
            self._send_header(sock, remote_path)

            transferred = 0
            start_time = time.time()
            spinner = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]
            spin_idx = 0

            while True:
                chunk = stream.read(self.chunk_size)
                if not chunk:
                    break

                sock.sendall(struct.pack(">I", len(chunk)))
                sock.sendall(chunk)
                transferred += len(chunk)

                if progress:
                    spin_idx = (spin_idx + 1) % len(spinner)
                    elapsed = max(time.time() - start_time, 0.001)
                    speed_mb = (transferred / (1024 * 1024)) / elapsed
                    if total_bytes and total_bytes > 0:
                        pct = (transferred / total_bytes) * 100.0
                        sys.stderr.write(
                            f"\r{spinner[spin_idx]} [{pct:5.1f}%] {transferred / (1024*1024):.2f}/{total_bytes / (1024*1024):.2f} MB ({speed_mb:.2f} MB/s)"
                        )
                    else:
                        sys.stderr.write(
                            f"\r{spinner[spin_idx]} Streamed {transferred / (1024*1024):.2f} MB ({speed_mb:.2f} MB/s)"
                        )
                    sys.stderr.flush()

            if progress:
                sys.stderr.write("\r\033[K")
                sys.stderr.flush()

            # Terminal bottle: length = 0
            sock.sendall(struct.pack(">I", 0))

            return self._read_response(sock)

    def put_file(
        self,
        remote_path: str,
        local_path: Union[str, Path],
        progress: bool = False,
    ) -> str:
        """Transfer a local file from disk to the remote server.

        :param remote_path: Destination relative path on the server.
        :param local_path: Local file path on disk.
        :param progress: Whether to display a terminal progress bar.
        :return: Server acknowledgment message string on success.
        """
        path = Path(local_path)
        if not path.is_file():
            raise FileNotFoundError(f"Local file not found: {path}")

        file_size = path.stat().st_size
        with open(path, "rb") as f:
            return self.put_stream(
                remote_path=remote_path,
                stream=f,
                progress=progress,
                total_bytes=file_size,
            )


# Functional convenience wrappers
def put_bytes(server: str, remote_path: str, data: Union[bytes, bytearray, memoryview]) -> str:
    """Send bytes to a MIAB server using a one-off connection."""
    return Client(server).put_bytes(remote_path, data)


def put_file(server: str, remote_path: str, local_path: Union[str, Path], progress: bool = False) -> str:
    """Send a local file to a MIAB server using a one-off connection."""
    return Client(server).put_file(remote_path, local_path, progress=progress)


def put_stream(server: str, remote_path: str, stream: BinaryIO, progress: bool = False) -> str:
    """Stream binary data to a MIAB server using a one-off connection."""
    return Client(server).put_stream(remote_path, stream, progress=progress)


def main() -> None:
    """CLI entrypoint compatible with the Rust miab command line."""
    parser = argparse.ArgumentParser(
        prog="miab",
        description="Message In A Bottle Client CLI (Pure Python)",
    )
    subparsers = parser.add_subparsers(dest="subcommand", required=True)

    # put subcommand
    put_parser = subparsers.add_parser("put", help="Send a local file to the remote MIAB server")
    put_parser.add_argument("server", help="Server address (host or host:port, default: 6422)")
    put_parser.add_argument("remote_path", help="Remote destination relative path")
    put_parser.add_argument("local_file", help="Local file to upload")

    # stream subcommand
    stream_parser = subparsers.add_parser("stream", help="Stream stdin pipe to the remote MIAB server")
    stream_parser.add_argument("server", help="Server address (host or host:port, default: 6422)")
    stream_parser.add_argument("remote_path", help="Remote destination relative path")

    args = parser.parse_args()
    client = Client(args.server)

    try:
        if args.subcommand == "put":
            resp = client.put_file(args.remote_path, args.local_file, progress=True)
            print(f"✓ Transfer successful: {resp}")
        elif args.subcommand == "stream":
            resp = client.put_stream(args.remote_path, sys.stdin.buffer, progress=True)
            print(f"✓ Transfer successful: {resp}")
    except MiabServerError as e:
        sys.stderr.write(f"✗ Server error (status {e.status}): {e.message}\n")
        sys.exit(1)
    except Exception as e:
        sys.stderr.write(f"✗ Transfer failed: {e}\n")
        sys.exit(1)


if __name__ == "__main__":
    main()
