# miab-transfer (Message In A Bottle)

[![CI](https://github.com/gabstevens/miab-transfer/actions/workflows/ci.yml/badge.svg)](https://github.com/gabstevens/miab-transfer/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.83%2B-orange.svg)](https://www.rust-lang.org/)
[![Python](https://img.shields.io/badge/python-3.8%2B-green.svg)](https://www.python.org/)

**miab-transfer** is a high-performance, bounded-memory binary blob transport engine designed for edge nodes, IoT devices, embedded systems, and homelabs operating over private networks (e.g., Tailscale, WireGuard, LAN).

It eliminates the protocol bloat, memory buffering, and complexity of HTTP/REST, multipart MIME, and gRPC by streaming binary packets ("Bottles") over a raw TCP connection with strict big-endian framing.

---

## Key Features

- **Bounded Memory Footprint**: Never buffers whole payloads in RAM. Files and streams are processed in reusable 64 KB chunk slices directly between the socket and disk.
- **Zero Overhead Framing**: Minimalist binary wire protocol (RFC 001). No JSON, HTTP headers, or TLS negotiation in-band.
- **Hardened Security**: The server strictly confines files to `--storage-root`, rejecting directory traversal (`..`), leading slashes, and Windows drive/colon identifiers.
- **Multi-Crate Rust Architecture**:
  - `miab-core`: Protocol framing, codecs, path sanitization, and errors.
  - `miabd`: Async multithreaded server daemon built on Tokio.
  - `miab`: Fast client CLI with transfer progress bars.
- **Zero-Dependency Python Client**: Standalone Python module (`miab.py`) using only the standard library (`socket`, `struct`) for instant drop-in use on low-power nodes.
- **Formal Wire Specification**: See [SPEC.md](SPEC.md) for the complete RFC-001 specification.

---

## Wire Protocol Overview (MIAB v1)

Transfers execute over a single TCP connection (default port `6422`):

```
Client                                                   Server
  |                                                         |
  |--- [1] Initial Header (Magic: "MIAB", Version, Path) -->|
  |                                                         |
  |--- [2] Bottle Chunk (4B Length + Payload Slice) ------->|
  |--- [...] Additional Chunks ---------------------------->|
  |                                                         |
  |--- [3] Terminal Bottle (0x00000000 / EOF) ------------->|
  |                                                         |
  |<-- [4] Server Response (Status Code + Message) ---------|
  |                                                         |
 [TCP Close]                                           [TCP Close]
```

Full packet layouts, state machines, and test vectors are documented in [SPEC.md](SPEC.md).

---

## Installation & Build

### Prerequisites
- [Rust](https://www.rust-lang.org/) 1.83+
- Python 3.8+ (optional, for Python client)

### Build Rust Workspace
```bash
git clone https://github.com/gabstevens/miab-transfer.git
cd miab-transfer
cargo build --release
```

Compiled binaries will be available in `target/release/`:
- `miabd` (Server daemon)
- `miab` (Client CLI)

---

## Usage

### 1. Starting the Server Daemon (`miabd`)

```bash
# Start daemon listening on all interfaces on port 6422, saving to ./storage
./target/release/miabd -b 0.0.0.0:6422 -s /var/miab-storage
```

Options:
- `-b, --bind <ADDR>`: Bind socket address (default: `0.0.0.0:6422`).
- `-s, --storage-root <PATH>`: Base storage directory (default: `./storage`).

### 2. Client CLI (`miab`)

#### Send a file:
```bash
miab put 192.168.1.100:6422 photos/cam01/capture.jpg ./capture.jpg
```

#### Stream from standard input:
```bash
# Stream an archive directly without creating temporary files
tar -czf - /var/log | miab stream 192.168.1.100:6422 backups/logs.tar.gz

# Stream camera pipe
ffmpeg -i /dev/video0 -f mpegts - | miab stream 192.168.1.100:6422 live/cam.ts
```

---

## Deployment & Homelab Tooling

### 1. One-Line Installer
Install binaries directly on Linux (x86_64, aarch64, armv7) or macOS:
```bash
curl -sSfL https://raw.githubusercontent.com/gabstevens/miab-transfer/main/deploy/install.sh | bash

# Or install and enable the systemd daemon automatically:
curl -sSfL https://raw.githubusercontent.com/gabstevens/miab-transfer/main/deploy/install.sh | bash -s -- --service
```

### 2. Running via Systemd
A hardened, sandboxed systemd service file is provided in `deploy/miabd.service`:
```bash
# Copy binary, service unit, and default config
sudo cp target/release/miabd /usr/local/bin/
sudo cp deploy/miabd.service /etc/systemd/system/
sudo cp deploy/miabd.default /etc/default/miabd

# Enable and start service
sudo systemctl daemon-reload
sudo systemctl enable --now miabd
sudo systemctl status miabd
```

### 3. Running with Docker / Podman
Build and run the lightweight container (<15 MB):
```bash
# Build container image
docker build -t miabd .

# Run daemon with persistent storage volume
docker run -d \
  --name miabd \
  -p 6422:6422 \
  -v /mnt/storage:/storage \
  miabd
```

---

## Python Client (Zero Dependencies)

The Python client requires **no third-party dependencies** and can be dropped directly into any project or IoT node:

### Drop-in usage:
```bash
cp miab.py /path/to/your/project/
```

### Python API Example:
```python
import miab

client = miab.Client("192.168.1.100:6422")

# Send an in-memory byte buffer
client.put_bytes("telemetry/metrics.bin", b"\x01\x02\x03\x04")

# Send a local file with progress
client.put_file("firmware/update.bin", "/tmp/update.bin", progress=True)

# Stream an open file / pipe
with open("/var/log/syslog", "rb") as f:
    client.put_stream("logs/syslog", f)
```

### Python CLI:
```bash
python3 miab.py put 192.168.1.100:6422 backups/data.tar.gz ./data.tar.gz
cat /proc/cpuinfo | python3 miab.py stream 192.168.1.100:6422 sys/cpuinfo.txt
```

---

## Testing & Verification

Run the automated integration and unit test suite:

```bash
# Run all Rust unit and integration tests (including 5MB transfer and traversal rejection)
cargo test --workspace

# Run Python client integration test against live miabd instance
python3 test_binding.py
```

---

## Benchmarking (`miab-bench`)

The workspace includes a dedicated benchmarking harness (`miab-bench`) that compares MIAB v1 against HTTP/1.1 chunked streaming uploads. It supports memory-bounded throughput sweeps, in-memory null sink mode (`--sink discard`) to eliminate disk bottlenecks, and a **userspace VPN network simulator** that accurately models Tailscale/WireGuard conditions without requiring root/sudo privileges or touching host network interfaces.

### 1. Loopback Benchmark (Zero Overhead)
Compare MIAB vs HTTP streaming over localhost:
```bash
cargo run --release -p miab-bench -- --sizes 1M,5M,20M --chunks 64K,256K -i 3
```

### 2. Simulated Tailscale VPN (MTU 1280 + RTT Latency)
Test realistic VPN / WAN performance using the built-in unprivileged pipelined proxy:
```bash
cargo run --release -p miab-bench -- \
  --sizes 1M,5M \
  --chunks 64K,256K \
  --simulate-vpn \
  --latency-ms 10 \
  --mtu 1280 \
  -i 3
```

### 3. Remote Node Benchmarking (Across Real Tailscale Overlay)
Benchmark directly against a live remote node across a VPN or LAN:
```bash
cargo run --release -p miab-bench -- \
  --sizes 10M,50M \
  --miab-server 100.64.0.5:6422 \
  --http-server http://100.64.0.5:8080/upload
```

### Sample Benchmark Results (Localhost vs Simulated Tailscale VPN)

```
=========================================================================================================
Payload    | Protocol           | Chunk      | Mean Time    | Min / Max         | Throughput   | Bandwidth     
---------------------------------------------------------------------------------------------------------
[Localhost Loopback]
5.0 MB     | MIAB v1            | 64 KB      | 2.26ms       | 2.23ms / 2.30ms   | 2208.98 MB/s | 18530 Mbps
5.0 MB     | MIAB v1            | 256 KB     | 1.99ms       | 1.79ms / 2.18ms   | 2516.89 MB/s | 21113 Mbps
5.0 MB     | HTTP/1.1 (Chunked) | 64 KB      | 2.06ms       | 1.84ms / 2.29ms   | 2422.13 MB/s | 20318 Mbps
---------------------------------------------------------------------------------------------------------
[Simulated Tailscale VPN: 20ms RTT, MTU 1280]
5.0 MB     | MIAB v1            | 64 KB      | 88.91ms      | 69.72ms / 108.1ms |   56.24 MB/s |   471 Mbps
5.0 MB     | MIAB v1            | 256 KB     | 68.77ms      | 67.85ms / 69.70ms |   72.70 MB/s |   609 Mbps
5.0 MB     | HTTP/1.1 (Chunked) | 64 KB      | 71.69ms      | 68.94ms / 74.44ms |   69.75 MB/s |   585 Mbps
=========================================================================================================
```

---

## License

This project is licensed under the [MIT License](LICENSE).

