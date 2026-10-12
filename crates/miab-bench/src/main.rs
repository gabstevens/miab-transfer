mod http_server;
mod metrics;
mod proxy;
mod scenarios;

use clap::Parser;
use http_server::HttpServer;
use proxy::UserspaceVpnProxy;
use scenarios::{
    format_bytes, print_comprehensive_table, print_concurrency_table, run_concurrency_test,
    run_http_suite, run_miab_suite, BenchRecord, ConcurrencyRecord,
};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

#[derive(Parser, Debug)]
#[command(
    name = "miab-bench",
    about = "Benchmark harness for MIAB v1 vs HTTP: Throughput, Latency Percentiles, CPU Efficiency, Wire Tax, and Concurrency"
)]
struct Args {
    /// Payload sizes to benchmark, comma-separated (e.g. 10K,100K,1M,10M)
    #[arg(short = 's', long = "sizes", default_value = "10K,100K,1M,10M")]
    sizes: String,

    /// MIAB chunk bottle sizes to sweep, comma-separated (e.g. 16K,64K,256K)
    #[arg(short = 'c', long = "chunks", default_value = "16K,64K,256K")]
    chunks: String,

    /// Number of measurement iterations per scenario
    #[arg(short = 'i', long = "iterations", default_value_t = 5)]
    iterations: usize,

    /// Concurrency levels to stress test (comma-separated, e.g. 10,25,50. Set to 0 to skip)
    #[arg(long = "concurrency", default_value = "10,25")]
    concurrency: String,

    /// Payload size for the concurrency stress test
    #[arg(long = "concurrency-payload", default_value = "1M")]
    concurrency_payload: String,

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
    #[arg(long = "latency-ms", default_value_t = 10)]
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

    /// Output results as Markdown tables
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
        .filter(|s| !s.trim().is_empty())
        .map(|s| parse_size(s).expect("invalid payload size"))
        .collect();

    let chunk_sizes: Vec<usize> = args
        .chunks
        .split(',')
        .filter(|s| !s.trim().is_empty())
        .map(|s| parse_size(s).expect("invalid chunk size"))
        .collect();

    let concurrency_levels: Vec<usize> = args
        .concurrency
        .split(',')
        .filter(|s| !s.trim().is_empty())
        .filter_map(|s| s.trim().parse::<usize>().ok())
        .filter(|&c| c > 0)
        .collect();

    let conc_payload_size = parse_size(&args.concurrency_payload).unwrap_or(1024 * 1024);

    let discard_mode = args.sink.to_lowercase() == "discard";
    let test_miab = args.protocols == "all" || args.protocols == "miab";
    let test_http = args.protocols == "all" || args.protocols == "http";

    println!(
        "\n=================================================================================="
    );
    println!(" MIAB-TRANSFER MULTI-DIMENSIONAL BENCHMARK SUITE");
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
    if !concurrency_levels.is_empty() {
        println!(
            " Concurrency Test: {:?} concurrent streams (Payload: {})",
            concurrency_levels,
            format_bytes(conc_payload_size)
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

    // 1. Setup MIAB Server endpoint
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

    // 2. Setup HTTP Server endpoint if HTTP testing is enabled
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
    let mut records: Vec<BenchRecord> = Vec::new();

    // SECTION A: Throughput, Latency Percentiles, CPU Efficiency, and Wire Tax
    for &payload_size in &payload_sizes {
        println!(
            "--> Benchmarking payload size: {}",
            format_bytes(payload_size)
        );
        let payload = vec![0xABu8; payload_size];

        // 1. MIAB runs across chunk sizes
        if test_miab {
            for &chunk_size in &chunk_sizes {
                print!(
                    "    [MIAB] chunk: {:<8} ({} iterations)... ",
                    format_bytes(chunk_size),
                    args.iterations
                );
                let record =
                    run_miab_suite(&active_miab_addr, &payload, chunk_size, args.iterations)
                        .await?;
                println!(
                    "done ({:.2} MB/s, p50: {:?}, CPU: {:.1} ms/GB)",
                    record.throughput_mb_s(),
                    record.p50_duration(),
                    record.cpu_ms_per_gb()
                );
                records.push(record);
            }
        }

        // 2. HTTP run
        if test_http {
            let default_http_chunk = 64 * 1024;
            print!(
                "    [HTTP] chunk: {:<8} ({} iterations)... ",
                format_bytes(default_http_chunk),
                args.iterations
            );
            let record = run_http_suite(
                &active_http_url,
                &payload,
                default_http_chunk,
                args.iterations,
            )
            .await?;
            println!(
                "done ({:.2} MB/s, p50: {:?}, CPU: {:.1} ms/GB)",
                record.throughput_mb_s(),
                record.p50_duration(),
                record.cpu_ms_per_gb()
            );
            records.push(record);
        }
    }

    println!("\n");
    print_comprehensive_table(&records, args.markdown);

    // SECTION B: Concurrency and Memory Footprint Stress Test
    if !concurrency_levels.is_empty() {
        println!(
            "\n--> Running Concurrency & Memory Stress Test (Payload: {})...",
            format_bytes(conc_payload_size)
        );
        let conc_payload = Arc::new(vec![0xCDu8; conc_payload_size]);
        let mut conc_records: Vec<ConcurrencyRecord> = Vec::new();

        for &c in &concurrency_levels {
            if test_miab {
                print!("    [MIAB] {} parallel connections... ", c);
                let rec = run_concurrency_test(
                    true,
                    &active_miab_addr,
                    conc_payload.clone(),
                    64 * 1024,
                    c,
                )
                .await;
                println!(
                    "done ({:.2} MB/s aggregate, peak RSS: {}, errors: {})",
                    rec.aggregate_mb_s,
                    format_bytes(rec.peak_rss_bytes as usize),
                    rec.errors
                );
                conc_records.push(rec);
            }

            if test_http {
                print!("    [HTTP] {} parallel connections... ", c);
                let rec = run_concurrency_test(
                    false,
                    &active_http_url,
                    conc_payload.clone(),
                    64 * 1024,
                    c,
                )
                .await;
                println!(
                    "done ({:.2} MB/s aggregate, peak RSS: {}, errors: {})",
                    rec.aggregate_mb_s,
                    format_bytes(rec.peak_rss_bytes as usize),
                    rec.errors
                );
                conc_records.push(rec);
            }
        }

        print_concurrency_table(&conc_records, args.markdown);
    }

    println!();
    Ok(())
}
