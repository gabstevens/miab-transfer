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

The workspace includes a comprehensive multi-dimensional benchmarking harness (`miab-bench`) comparing **MIAB v1** against both **HTTP/1.1 chunked streaming** and **HTTP/2 (`h2c` binary framing & multiplexing)**. Because raw bulk throughput over high-latency WANs is primarily bounded by TCP dynamics (congestion control and BDP), `miab-bench` measures the dimensions where MIAB is designed to excel:
1. **CPU Efficiency**: User and system CPU time consumed per Gigabyte transferred (`CPU ms / GB`).
2. **Memory Footprint & Concurrency**: Server peak Resident Set Size (RSS) and memory delta under parallel stream floods.
3. **Latency Percentiles**: Median (p50) and p99 transfer times for small/bursty payloads (10 KB – 10 MB).
4. **Protocol Wire Tax**: Protocol framing overhead percentage (`Wire Tax %`).
5. **Userspace VPN Simulator**: Unprivileged, zero-root Tailscale/WireGuard modeling (MTU 1280 packet slicing + pipelined RTT latency).

### Running Benchmarks

#### 1. Multi-Dimensional Sweep (Throughput, Latency, CPU, Wire Tax & Concurrency)
```bash
# Run 3-way comparison across MIAB v1, HTTP/1.1, and HTTP/2
cargo run --release -p miab-bench -- \
  --sizes 10K,100K,1M,10M \
  --chunks 64K,256K \
  --concurrency 10,25 \
  --concurrency-payload 1M \
  -i 5

# Or benchmark specific protocols: --protocols miab,http1,http2
```

#### 2. Simulated Tailscale VPN (MTU 1280 + RTT Latency)
```bash
cargo run --release -p miab-bench -- \
  --sizes 100K,1M,5M \
  --chunks 64K \
  --simulate-vpn \
  --latency-ms 10 \
  --mtu 1280 \
  --concurrency 5,10 \
  --concurrency-payload 1M \
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

### Benchmark Results (3-Way Comparison: MIAB v1 vs HTTP/1.1 vs HTTP/2)

#### A. Single Stream Performance & Efficiency (Local Loopback)

| Payload | Protocol | Chunk | Median (p50) | p99 Latency | Throughput (MB/s) | CPU Time (ms/GB) | Wire Tax |
| :--- | :--- | :--- | :--- | :--- | :--- | :--- | :--- |
| **10 KB** | **MIAB v1** | 64 KB | **231µs** | **259µs** | **41.23 MB/s** | **29,954 ms/GB** | **1.162%** |
| 10 KB | HTTP/1.1 (Chunked) | 64 KB | 292µs | 404µs | 31.73 MB/s | 53,023 ms/GB | 2.959% |
| 10 KB | HTTP/2 (h2c) | 64 KB | 1.62ms | 41.97ms | 0.67 MB/s | 161,586 ms/GB | 1.768% |
| **100 KB** | **MIAB v1** | 64 KB | **1.15ms** | **1.19ms** | **84.22 MB/s** | **15,327 ms/GB** | **0.120%** |
| 100 KB | HTTP/1.1 (Chunked) | 64 KB | 1.33ms | 1.53ms | 72.98 MB/s | 22,384 ms/GB | 0.304% |
| 100 KB | HTTP/2 (h2c) | 64 KB | 1.64ms | 1.82ms | 57.92 MB/s | 35,344 ms/GB | 0.242% |
| **1.0 MB** | **MIAB v1** | 256 KB | **2.47ms** | **2.78ms** | **422.76 MB/s** | **3,469 ms/GB** | **0.012%** |
| 1.0 MB | HTTP/1.1 (Chunked) | 64 KB | 1.66ms | 2.40ms | 525.92 MB/s | 3,843 ms/GB | 0.040% |
| 1.0 MB | HTTP/2 (h2c) | 64 KB | 42.88ms | 44.91ms | 33.59 MB/s | 5,274 ms/GB | 0.091% |
| **10.0 MB** | **MIAB v1** | 256 KB | **3.62ms** | **3.87ms** | **2,718.58 MB/s** | **666 ms/GB** | **0.003%** |
| 10.0 MB | HTTP/1.1 (Chunked) | 64 KB | 3.78ms | 4.19ms | 2,575.66 MB/s | 1,272 ms/GB | 0.015% |
| 10.0 MB | HTTP/2 (h2c) | 64 KB | 8.50ms | 12.09ms | 1,034.82 MB/s | 2,109 ms/GB | 0.076% |

#### B. Concurrency & Memory Stress Test (1.0 MB Payload per Stream)

| Protocol | Concurrency | Total Transferred | Wall Time | Aggregate (MB/s) | Peak RSS | Memory Delta | Errors |
| :--- | :--- | :--- | :--- | :--- | :--- | :--- | :--- |
| **MIAB v1** | **10 streams** | 10.0 MB | **2.44ms** | **4,096.99 MB/s** | **20.3 MB** | **+0 B** | **0** |
| HTTP/1.1 (Chunked) | 10 streams | 10.0 MB | 3.90ms | 2,563.97 MB/s | 26.7 MB | +6.4 MB | 0 |
| HTTP/2 (Multiplexed) | 10 streams | 10.0 MB | 51.06ms | 195.85 MB/s | 31.3 MB | +1.2 MB | 0 |
| **MIAB v1** | **25 streams** | 25.0 MB | **5.15ms** | **4,851.41 MB/s** | **33.8 MB** | **+4 KB** | **0** |
| HTTP/1.1 (Chunked) | 25 streams | 25.0 MB | 7.89ms | 3,170.05 MB/s | 44.5 MB | +10.7 MB | 0 |
| HTTP/2 (Multiplexed) | 25 streams | 25.0 MB | 66.66ms | 375.04 MB/s | 56.2 MB | +9.9 MB | 0 |

#### C. Tailscale VPN Simulation (MTU 1280 + 20ms RTT Latency)

| Protocol | Concurrency | Total Transferred | Wall Time | Aggregate (MB/s) | Peak RSS | Memory Delta |
| :--- | :--- | :--- | :--- | :--- | :--- | :--- |
| **MIAB v1** | **5 streams** | 5.0 MB | **36.34ms** | **137.58 MB/s** | **18.0 MB** | **+1.0 MB** |
| HTTP/1.1 (Chunked) | 5 streams | 5.0 MB | 74.32ms | 67.28 MB/s | 18.7 MB | +720 KB |
| HTTP/2 (Multiplexed) | 5 streams | 5.0 MB | 326.34ms | 15.32 MB/s | 20.0 MB | +1.3 MB |
| **MIAB v1** | **10 streams** | 10.0 MB | **83.40ms** | **119.91 MB/s** | **21.9 MB** | **+1.9 MB** |
| HTTP/1.1 (Chunked) | 10 streams | 10.0 MB | 44.50ms | 224.70 MB/s | 25.9 MB | +4.0 MB |
| HTTP/2 (Multiplexed) | 10 streams | 10.0 MB | 442.87ms | 22.58 MB/s | 27.0 MB | +1.1 MB |

> **Key Architectural Takeaways**:
> 1. **Zero Memory Inflation**: Under 25 concurrent connections, MIAB v1 allocates strictly bounded streaming windows resulting in virtually zero resident memory growth (**+4 KB**), whereas HTTP/1.1 inflates memory by **+10.7 MB** and HTTP/2 by **+9.9 MB**.
> 2. **Superior Concurrency Throughput**: MIAB achieves **4,851 MB/s aggregate throughput** at 25 parallel streams — **1.5x faster than HTTP/1.1** (3,170 MB/s) and **12.9x faster than HTTP/2** (375 MB/s).
> 3. **The HTTP/2 Binary Framing & Multiplexing Penalty for Bulk Data**:
>    - While HTTP/2 excels at multiplexing many small web assets (CSS/JS/images) over a single TCP connection, bulk binary transfer triggers significant overhead: 16 KB maximum frame slicing (9-byte headers per frame), flow-control updates (`WINDOW_UPDATE`), and single-connection TCP head-of-line blocking.
>    - Under simulated Tailscale network latency (20ms RTT), HTTP/2 multiplexed transfers dropped to ~15–22 MB/s, while MIAB maintained ~120–137 MB/s.
> 4. **CPU & Wire Efficiency**: At 10MB payloads, MIAB consumes only **666 ms CPU time per GB** (compared to 1,272 ms for HTTP/1.1 and 2,109 ms for HTTP/2) while validating end-to-end CRC-32 integrity, with an infinitesimal **0.003% protocol wire tax**.


---

## License

This project is licensed under the [MIT License](LICENSE).


