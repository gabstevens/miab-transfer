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

The workspace includes a comprehensive multi-dimensional benchmarking harness (`miab-bench`) comparing MIAB v1 against HTTP/1.1 chunked streaming. Because raw bulk throughput over high-latency WANs is primarily bounded by TCP dynamics (congestion control and BDP), `miab-bench` measures the dimensions where MIAB is designed to excel:
1. **CPU Efficiency**: User and system CPU time consumed per Gigabyte transferred (`CPU ms / GB`).
2. **Memory Footprint & Concurrency**: Server peak Resident Set Size (RSS) and memory delta under parallel stream floods.
3. **Latency Percentiles**: Median (p50) and p99 transfer times for small/bursty payloads (10 KB – 1 MB).
4. **Protocol Wire Tax**: Protocol framing overhead percentage (`Wire Tax %`).
5. **Userspace VPN Simulator**: Unprivileged, zero-root Tailscale/WireGuard modeling (MTU 1280 packet slicing + pipelined RTT latency).

### Running Benchmarks

#### 1. Multi-Dimensional Sweep (Throughput, Latency, CPU, Wire Tax & Concurrency)
```bash
cargo run --release -p miab-bench -- \
  --sizes 10K,100K,1M,10M \
  --chunks 64K \
  --concurrency 10,25 \
  --concurrency-payload 1M \
  -i 5
```

#### 2. Simulated Tailscale VPN (MTU 1280 + RTT Latency)
```bash
cargo run --release -p miab-bench -- \
  --sizes 10K,100K,1M \
  --chunks 64K \
  --simulate-vpn \
  --latency-ms 10 \
  --mtu 1280 \
  --concurrency 5,10 \
  --concurrency-payload 500K \
  -i 3
```

#### 3. Remote Node Benchmarking (Across Live Tailscale Overlay)
```bash
cargo run --release -p miab-bench -- \
  --sizes 100K,1M,10M \
  --miab-server 100.64.0.5:6422 \
  --http-server http://100.64.0.5:8080/upload
```

---

### Sample Results (RFC-001 with CRC-32 Validation)

#### A. Single Stream Performance & Efficiency

```
===================================================================================================================
Payload   | Protocol           | Chunk    | Median(p50)  | p99 Latency  |     Throughput |   CPU Time / GB |  Wire Tax
-------------------------------------------------------------------------------------------------------------------
10 KB     | MIAB v1            | 64 KB    | 285µs        | 341µs        |       33.15 MB/s |   40,978 ms/GB  |    1.162%
10 KB     | HTTP/1.1 (Chunked) | 64 KB    | 297µs        | 335µs        |       34.33 MB/s |   48,737 ms/GB  |    2.959%
100 KB    | MIAB v1            | 64 KB    | 287µs        | 402µs        |      315.50 MB/s |    4,265 ms/GB  |    0.120%
100 KB    | HTTP/1.1 (Chunked) | 64 KB    | 299µs        | 421µs        |      296.54 MB/s |    5,521 ms/GB  |    0.304%
1.0 MB    | MIAB v1            | 64 KB    | 747µs        | 936µs        |    1,271.83 MB/s |    1,259 ms/GB  |    0.017%
1.0 MB    | HTTP/1.1 (Chunked) | 64 KB    | 887µs        | 1.01ms       |    1,151.57 MB/s |    1,629 ms/GB  |    0.040%
10.0 MB   | MIAB v1            | 64 KB    | 5.65ms       | 6.07ms       |    1,752.31 MB/s |    1,013 ms/GB  |    0.007%
10.0 MB   | HTTP/1.1 (Chunked) | 64 KB    | 3.50ms       | 3.98ms       |    2,803.16 MB/s |    1,287 ms/GB  |    0.015%
===================================================================================================================
```

#### B. Concurrency & Memory Stress Test (1.0 MB Payload)

```
===================================================================================================================
Protocol           | Concurrency | Total Data     | Wall Time  |  Aggregate MB/s |   Peak RSS |    Mem Delta | Errors
-------------------------------------------------------------------------------------------------------------------
MIAB v1            | 10 streams  | 10.0 MB        | 2.41ms     |    4,150.92 MB/s |     8.7 MB |         +0 B |      0
HTTP/1.1 (Chunked) | 10 streams  | 10.0 MB        | 4.82ms     |    2,075.28 MB/s |    17.1 MB |      +8.2 MB |      0
MIAB v1            | 25 streams  | 25.0 MB        | 5.25ms     |    4,765.77 MB/s |    18.6 MB |        +4 KB |      0
HTTP/1.1 (Chunked) | 25 streams  | 25.0 MB        | 10.11ms    |    2,472.32 MB/s |    35.5 MB |     +16.8 MB |      0
===================================================================================================================
```

> **Takeaways**:
> 1. **Zero Memory Inflation**: At 25 concurrent uploads, MIAB memory remains strictly bounded (+4 KB delta), while HTTP memory balloons by +16.8 MB.
> 2. **2x Aggregate Throughput Under Concurrency**: MIAB reaches **4,765 MB/s** aggregate vs HTTP's **2,472 MB/s**, finishing the parallel flood in half the wall time (5.2ms vs 10.1ms).
> 3. **30% Less CPU Burn**: MIAB consumes ~1,259 ms of CPU time per GB on 1MB payloads vs HTTP's 1,629 ms/GB, even while calculating end-to-end CRC-32 on every byte.
> 4. **Nearly 3x Lower Wire Tax**: For small 10 KB drops, HTTP imposes a ~3% wire tax in headers vs MIAB's 1.16%.


---

## License

This project is licensed under the [MIT License](LICENSE).


