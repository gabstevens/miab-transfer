#!/usr/bin/env python3
"""Integration test for native Python MIAB client against the miabd Rust server."""

import os
import shutil
import socket
import subprocess
import sys
import tempfile
import time


def find_free_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def main() -> None:
    workspace_root = os.path.dirname(os.path.abspath(__file__))
    os.chdir(workspace_root)
    sys.path.insert(0, os.path.join(workspace_root, "python"))
    sys.path.insert(0, workspace_root)

    env = os.environ.copy()
    env["PATH"] = f"{os.path.expanduser('~/.cargo/bin')}:{env.get('PATH', '')}"

    # Ensure miabd binary is built
    miabd_bin = os.path.join(workspace_root, "target", "debug", "miabd")
    if not os.path.exists(miabd_bin):
        print("Building miabd server binary...")
        subprocess.run(["cargo", "build", "-p", "miabd"], cwd=workspace_root, check=True, env=env)

    # 1. Start miabd in the background on a test port
    test_port = find_free_port()
    storage_dir = tempfile.mkdtemp(prefix="miab_test_storage_")
    print(f"==> Step 1: Starting miabd on 127.0.0.1:{test_port} with storage {storage_dir}...")

    server_proc = subprocess.Popen(
        [miabd_bin, "-b", f"127.0.0.1:{test_port}", "-s", storage_dir],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )

    try:
        # Wait until server accepts connections
        server_ready = False
        for _ in range(50):
            try:
                with socket.create_connection(("127.0.0.1", test_port), timeout=0.1):
                    server_ready = True
                    break
            except (ConnectionRefusedError, OSError):
                time.sleep(0.05)

        if not server_ready:
            raise RuntimeError(f"miabd server failed to start on port {test_port}")
        print("miabd daemon is running and accepting connections.")

        # 2. Import native Python miab module
        print("==> Step 2: Importing native Python miab module...")
        import miab

        client = miab.Client(f"127.0.0.1:{test_port}")

        # 3. Test put_bytes with 1MB synthetic data
        print("==> Step 3: Testing client.put_bytes with 1MB buffer...")
        one_mb = 1024 * 1024  # 1MB
        test_data = bytes((i % 251) for i in range(one_mb))
        remote_bytes_file = "python_native/test_1mb.bin"

        res_msg = client.put_bytes(remote_bytes_file, test_data)
        print(f"Server response: {res_msg}")

        # Verify file exists on disk and content matches exactly
        saved_bytes_path = os.path.join(storage_dir, remote_bytes_file)
        assert os.path.exists(saved_bytes_path), f"File missing at {saved_bytes_path}"
        with open(saved_bytes_path, "rb") as f:
            on_disk_bytes = f.read()

        assert len(on_disk_bytes) == one_mb, f"Size mismatch: {len(on_disk_bytes)} != {one_mb}"
        assert on_disk_bytes == test_data, "Content mismatch for 1MB payload!"
        print("✓ Verified: 1MB byte buffer transferred and verified byte-for-byte!")

        # 4. Test put_file with a local file
        print("==> Step 4: Testing client.put_file...")
        local_sample_file = os.path.join(storage_dir, "sample_local.txt")
        file_payload = b"Pure Python MIAB client test payload for put_file!"
        with open(local_sample_file, "wb") as f:
            f.write(file_payload)

        remote_file_target = "python_native/sample_uploaded.txt"
        file_resp = client.put_file(remote_file_target, local_sample_file)
        print(f"Server response: {file_resp}")

        uploaded_file_path = os.path.join(storage_dir, remote_file_target)
        assert os.path.exists(uploaded_file_path), f"File missing at {uploaded_file_path}"
        with open(uploaded_file_path, "rb") as f:
            assert f.read() == file_payload, "put_file content mismatch!"
        print("✓ Verified: put_file works properly and file matches on disk!")

        # 5. Test put_stream with arbitrary stream
        print("==> Step 5: Testing client.put_stream...")
        import io

        stream_payload = b"Streamed data directly via put_stream"
        stream_resp = client.put_stream("python_native/streamed.txt", io.BytesIO(stream_payload))
        print(f"Server response: {stream_resp}")
        streamed_file_path = os.path.join(storage_dir, "python_native/streamed.txt")
        assert os.path.exists(streamed_file_path)
        with open(streamed_file_path, "rb") as f:
            assert f.read() == stream_payload
        print("✓ Verified: put_stream works properly!")

        # 6. Test path traversal rejection
        print("==> Step 6: Testing path traversal security rejection...")
        traversal_rejected = False
        try:
            client.put_bytes("../../evil.txt", b"malicious content")
        except miab.MiabServerError as e:
            traversal_rejected = True
            print(f"Correctly caught expected server error: {e}")
            assert e.status == miab.STATUS_ERR
        assert traversal_rejected, "Path traversal attempt should have raised MiabServerError!"
        assert not os.path.exists(os.path.join(storage_dir, "../../evil.txt"))
        print("✓ Verified: Path traversal rejected safely by server!")

        # 7. Test Python CLI put and stream
        print("==> Step 7: Testing Python CLI interface...")
        cli_test_file = os.path.join(storage_dir, "cli_src.txt")
        with open(cli_test_file, "w") as f:
            f.write("CLI test source content")

        cli_put_res = subprocess.run(
            [sys.executable, "miab.py", "put", f"127.0.0.1:{test_port}", "cli/test_cli.txt", cli_test_file],
            cwd=workspace_root,
            capture_output=True,
            text=True,
        )
        assert cli_put_res.returncode == 0, f"CLI put failed: {cli_put_res.stderr}"
        assert os.path.exists(os.path.join(storage_dir, "cli/test_cli.txt"))
        print("✓ Verified: Python CLI 'put' succeeded!")

        cli_stream_res = subprocess.run(
            [sys.executable, "miab.py", "stream", f"127.0.0.1:{test_port}", "cli/streamed_cli.txt"],
            input="Streamed from stdin pipe",
            cwd=workspace_root,
            capture_output=True,
            text=True,
        )
        assert cli_stream_res.returncode == 0, f"CLI stream failed: {cli_stream_res.stderr}"
        with open(os.path.join(storage_dir, "cli/streamed_cli.txt"), "r") as f:
            assert f.read() == "Streamed from stdin pipe"
        print("✓ Verified: Python CLI 'stream' succeeded!")

        print("\nAll native Python client tests PASSED successfully!")

    finally:
        print("==> Terminating miabd server process...")
        server_proc.terminate()
        try:
            server_proc.wait(timeout=2)
        except subprocess.TimeoutExpired:
            server_proc.kill()
        shutil.rmtree(storage_dir, ignore_errors=True)


if __name__ == "__main__":
    main()
