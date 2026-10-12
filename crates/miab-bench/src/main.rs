mod http_server;
mod proxy;
mod scenarios;

use clap::Parser;
use http_server::HttpServer;
use proxy::UserspaceVpnProxy;
use scenarios::{
    format_bytes, print_results_table, run_http_iteration, run_miab_iteration, BenchRecord,
};
use std::net::SocketAddr;
use std::time::Duration;

#[derive(Parser, Debug)]
#[command(
    name = "miab-bench",
    about = "Benchmark harness for MIAB v1 vs HTTP with optional Userspace VPN simulation"
)]
struct Args {
    /// Payload sizes to benchmark, comma-separated (e.g. 1M,10M,50M)
    #[arg(short = 's', long = "sizes", default_value = "1M,10M,50M")]
    sizes: String,

    /// MIAB chunk bottle sizes to sweep, comma-separated (e.g. 16K,64K,256K)
    #[arg(short = 'c', long = "chunks", default_value = "16K,64K,256K")]
    chunks: String,

    /// Number of measurement iterations per scenario
    #[arg(short = 'i', long = "iterations", default_value_t = 3)]
    iterations: usize,

    /// Protocols to benchmark: 'all', 'miab', or 'http'
    #[arg(short = 'p', long = "protocols", default_value = "all")]
    protocols: String,

    /// Storage sink type: 'discard' (in-memory null sink) or 'disk' (filesystem)
    #[arg(long = "sink", default_value = "discard")]
    sink: String,

    /// Enable userspace VPN network simulation (unprivileged RTT latency + MTU slicing)
    #[arg(long = "simulate-vpn", default_value_t = false)]
    simulate_vpn: bool,

    /// Simulated one-way network latency in milliseconds
    #[arg(long = "latency-ms", default_value_t = 20)]
    latency_ms: u64,

    /// Simulated MTU packet slicing size in bytes (Tailscale default: 1280)
    #[arg(long = "mtu", default_value_t = 1280)]
    mtu: usize,

    /// Optional target remote MIAB server address (e.g. 100.x.y.z:6422 across Tailscale)
    #[arg(long = "miab-server")]
    miab_server: Option<String>,

    /// Optional target remote HTTP server upload URL (e.g. http://100.x.y.z:8080/upload)
    #[arg(long = "http-server")]
    http_server: Option<String>,

    /// Output results as a Markdown table
    #[arg(long = "markdown", default_value_t = false)]
    markdown: bool,
}

fn parse_size(s: &str) -> Result<usize, String> {
    let s = s.trim();
    let (num_part, multiplier) = if s.ends_with('k') || s.ends_with('K') {
        (&s[..s.len() - 1], 1024)
    } else if s.ends_with('m') || s.ends_with('M') {
        (&s[..s.len() - 1], 1024 * 1024)
    } else if s.ends_with('g') || s.ends_with('G') {
        (&s[..s.len() - 1], 1024 * 1024 * 1024)
    } else {
        (s, 1)
    };

    num_part
        .parse::<usize>()
        .map(|n| n * multiplier)
        .map_err(|e| format!("Invalid size '{}': {}", s, e))
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let args = Args::parse();

    let payload_sizes: Vec<usize> = args
        .sizes
        .split(',')
        .map(|s| parse_size(s).expect("invalid payload size"))
        .collect();

    let chunk_sizes: Vec<usize> = args
        .chunks
        .split(',')
        .map(|s| parse_size(s).expect("invalid chunk size"))
        .collect();

    let discard_mode = args.sink.to_lowercase() == "discard";
    let test_miab = args.protocols == "all" || args.protocols == "miab";
    let test_http = args.protocols == "all" || args.protocols == "http";

    println!(
        "\n=================================================================================="
    );
    println!(" MIAB-TRANSFER BENCHMARK SUITE");
    println!("==================================================================================");
    println!(
        " Storage Sink    : {}",
        if discard_mode {
            "Discard (In-Memory Null Sink)"
        } else {
            "Disk Storage"
        }
    );
    println!(" Iterations      : {}", args.iterations);
    println!(
        " Payload Sizes   : {:?}",
        payload_sizes
            .iter()
            .map(|&s| format_bytes(s))
            .collect::<Vec<_>>()
    );
    if test_miab {
        println!(
            " MIAB Chunk Sizes: {:?}",
            chunk_sizes
                .iter()
                .map(|&s| format_bytes(s))
                .collect::<Vec<_>>()
        );
    }

    if args.simulate_vpn {
        println!(" Network Mode    : SIMULATED VPN (Userspace Proxy)");
        println!(
            "   RTT Latency   : ~{} ms ({} ms each way)",
            args.latency_ms * 2,
            args.latency_ms
        );
        println!(
            "   MTU Slicing   : {} bytes (Tailscale WireGuard standard)",
            args.mtu
        );
    } else if args.miab_server.is_some() || args.http_server.is_some() {
        println!(" Network Mode    : LIVE REMOTE TARGET");
    } else {
        println!(" Network Mode    : Localhost (Raw Loopback)");
    }
    println!(
        "==================================================================================\n"
    );

    let temp_dir = tempfile::tempdir()?;
    let storage_root = temp_dir.path().to_path_buf();

    // Setup MIAB Server endpoint
    let (miab_target_addr, _miab_shutdown) = if let Some(ref remote) = args.miab_server {
        println!("==> Using remote MIAB server: {}", remote);
        (remote.clone(), None)
    } else {
        println!("==> Starting embedded miabd server...");
        let server =
            miabd::Server::bind_with_options("127.0.0.1:0", storage_root.clone(), discard_mode)
                .await?;
        let addr = server.local_addr()?;
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        tokio::spawn(async move {
            let _ = server.run_until_shutdown(shutdown_rx).await;
        });
        (format!("127.0.0.1:{}", addr.port()), Some(shutdown_tx))
    };

    // If VPN simulation is enabled, wrap MIAB address in userspace proxy
    let (active_miab_addr, _miab_proxy_shutdown) = if args.simulate_vpn {
        let parsed_addr: SocketAddr = miab_target_addr.parse()?;
        let proxy = UserspaceVpnProxy::start(
            parsed_addr,
            Duration::from_millis(args.latency_ms),
            args.mtu,
        )
        .await?;
        let proxy_addr = proxy.local_addr()?;
        let (p_tx, p_rx) = tokio::sync::oneshot::channel::<()>();
        tokio::spawn(async move {
            let _ = proxy.run_until_shutdown(p_rx).await;
        });
        (format!("127.0.0.1:{}", proxy_addr.port()), Some(p_tx))
    } else {
        (miab_target_addr, None)
    };

    // Setup HTTP Server endpoint if HTTP testing is enabled
    let (_http_server, active_http_url, _http_proxy_shutdown) = if test_http {
        if let Some(ref remote_url) = args.http_server {
            println!("==> Using remote HTTP server: {}", remote_url);
            (None, remote_url.clone(), None)
        } else {
            println!("==> Starting embedded Axum HTTP streaming server...");
            let server = HttpServer::start(storage_root.clone(), discard_mode).await?;
            let http_addr = server.addr;

            if args.simulate_vpn {
                let proxy = UserspaceVpnProxy::start(
                    http_addr,
                    Duration::from_millis(args.latency_ms),
                    args.mtu,
                )
                .await?;
                let proxy_addr = proxy.local_addr()?;
                let (p_tx, p_rx) = tokio::sync::oneshot::channel::<()>();
                tokio::spawn(async move {
                    let _ = proxy.run_until_shutdown(p_rx).await;
                });
                let url = format!("http://127.0.0.1:{}/upload", proxy_addr.port());
                (Some(server), url, Some(p_tx))
            } else {
                let url = format!("http://127.0.0.1:{}/upload", http_addr.port());
                (Some(server), url, None)
            }
        }
    } else {
        (None, String::new(), None)
    };

    println!("\nBeginning benchmark execution...\n");
    let mut records = Vec::new();

    for &payload_size in &payload_sizes {
        println!(
            "--> Benchmarking payload size: {}",
            format_bytes(payload_size)
        );
        // Deterministic patterned buffer
        let payload = vec![0xABu8; payload_size];

        // 1. Run MIAB scenarios across chunk sizes
        if test_miab {
            for &chunk_size in &chunk_sizes {
                print!(
                    "    [MIAB] chunk: {:<8} ({} iterations)... ",
                    format_bytes(chunk_size),
                    args.iterations
                );
                let mut durations = Vec::with_capacity(args.iterations);

                for _ in 0..args.iterations {
                    let d = run_miab_iteration(&active_miab_addr, &payload, chunk_size).await?;
                    durations.push(d);
                }

                let record = BenchRecord {
                    protocol: "MIAB v1".to_string(),
                    payload_bytes: payload_size,
                    chunk_bytes: chunk_size,
                    durations,
                };
                println!("done ({:.2} MB/s)", record.throughput_mb_s());
                records.push(record);
            }
        }

        // 2. Run HTTP scenario
        if test_http {
            let default_http_chunk = 64 * 1024; // 64 KB standard HTTP chunk
            print!(
                "    [HTTP] chunk: {:<8} ({} iterations)... ",
                format_bytes(default_http_chunk),
                args.iterations
            );
            let mut durations = Vec::with_capacity(args.iterations);

            for _ in 0..args.iterations {
                let d = run_http_iteration(&active_http_url, &payload, default_http_chunk).await?;
                durations.push(d);
            }

            let record = BenchRecord {
                protocol: "HTTP/1.1 (Chunked)".to_string(),
                payload_bytes: payload_size,
                chunk_bytes: default_http_chunk,
                durations,
            };
            println!("done ({:.2} MB/s)", record.throughput_mb_s());
            records.push(record);
        }
    }

    println!("\n");
    print_results_table(&records, args.markdown);

    Ok(())
}
