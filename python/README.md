# `miab` Python Client

A zero-dependency, pure-Python client for the **Message In A Bottle (MIAB v1)** binary blob transport protocol.

## Features
- **Zero Dependencies**: Uses only Python standard library (`socket`, `struct`, `os`, `sys`, `pathlib`).
- **Bounded Memory Footprint**: Streams arbitrary file sizes and data streams in 64 KB chunk bottles using `memoryview` without loading whole files into RAM.
- **Drop-in Anywhere**: Copy `miab.py` directly into any Python project or IoT script, or install via pip.

## Installation

### Option 1: Drop-in single file
Copy `miab.py` directly into your project:
```bash
cp python/miab.py /path/to/your/project/
```

### Option 2: Pip install
```bash
pip install ./python
```

## Quick Start (Python API)

```python
import miab

# 1. Initialize client
client = miab.Client("127.0.0.1:6422")

# 2. Transfer in-memory bytes
result = client.put_bytes("sensor/readings.bin", b"\x00\x01\x02\x03\x04")
print(result)  # "Saved 5 bytes"

# 3. Transfer a local file with progress bar
result = client.put_file("camera/snapshot.jpg", "/tmp/local_snapshot.jpg", progress=True)
print(result)

# 4. Stream from an arbitrary binary pipe
with open("/var/log/app.log", "rb") as stream:
    client.put_stream("logs/app.log", stream)
```

## CLI Usage

```bash
# Upload a file
python3 miab.py put 127.0.0.1:6422 backups/data.tar.gz ./data.tar.gz

# Stream stdin
tar -czf - ./my_dir | python3 miab.py stream 127.0.0.1:6422 backups/archive.tar.gz
```
