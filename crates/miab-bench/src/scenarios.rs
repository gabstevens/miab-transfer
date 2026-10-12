use crate::metrics::{
    cpu_ms_per_gb, current_rss_bytes, http2_wire_overhead, http_wire_overhead, miab_wire_overhead,
    ProcessMetrics,
};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BenchProtocol {
    Miab,
    Http1,
    Http2,
}

impl BenchProtocol {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Miab => "MIAB v1",
            Self::Http1 => "HTTP/1.1 (Chunked)",
            Self::Http2 => "HTTP/2 (h2c Multiplexed)",
        }
    }
}

#[derive(Debug, Clone)]
pub struct BenchRecord {
    pub protocol: String,
    pub payload_bytes: usize,
    pub chunk_bytes: usize,
    pub durations: Vec<Duration>,
    pub cpu_duration: Duration,
    pub wire_overhead_bytes: usize,
}

impl BenchRecord {
    pub fn mean_duration(&self) -> Duration {
        if self.durations.is_empty() {
            return Duration::ZERO;
        }
        let total: Duration = self.durations.iter().sum();
        total / self.durations.len() as u32
    }

    pub fn sorted_durations(&self) -> Vec<Duration> {
        let mut d = self.durations.clone();
        d.sort();
        d
    }

    pub fn p50_duration(&self) -> Duration {
        let sorted = self.sorted_durations();
        if sorted.is_empty() {
            return Duration::ZERO;
        }
        let idx = (sorted.len() as f64 * 0.50).floor() as usize;
        sorted[idx.min(sorted.len() - 1)]
    }

    #[allow(dead_code)]
    pub fn p95_duration(&self) -> Duration {
        let sorted = self.sorted_durations();
        if sorted.is_empty() {
            return Duration::ZERO;
        }
        let idx = (sorted.len() as f64 * 0.95).floor() as usize;
        sorted[idx.min(sorted.len() - 1)]
    }

    pub fn p99_duration(&self) -> Duration {
        let sorted = self.sorted_durations();
        if sorted.is_empty() {
            return Duration::ZERO;
        }
        let idx = (sorted.len() as f64 * 0.99).floor() as usize;
        sorted[idx.min(sorted.len() - 1)]
    }

    #[allow(dead_code)]
    pub fn min_duration(&self) -> Duration {
        self.durations
            .iter()
            .copied()
            .min()
            .unwrap_or(Duration::ZERO)
    }

    #[allow(dead_code)]
    pub fn max_duration(&self) -> Duration {
        self.durations
            .iter()
            .copied()
            .max()
            .unwrap_or(Duration::ZERO)
    }

    pub fn throughput_mb_s(&self) -> f64 {
        let secs = self.mean_duration().as_secs_f64();
        if secs <= 0.0 {
            return 0.0;
        }
        (self.payload_bytes as f64 / (1024.0 * 1024.0)) / secs
    }

    #[allow(dead_code)]
    pub fn throughput_mbps(&self) -> f64 {
        let secs = self.mean_duration().as_secs_f64();
        if secs <= 0.0 {
            return 0.0;
        }
        (self.payload_bytes as f64 * 8.0 / 1_000_000.0) / secs
    }

    pub fn cpu_ms_per_gb(&self) -> f64 {
        let total_transferred = self.payload_bytes * self.durations.len();
        cpu_ms_per_gb(self.cpu_duration, total_transferred)
    }

    pub fn wire_tax_pct(&self) -> f64 {
        if self.payload_bytes == 0 {
            return 0.0;
        }
        (self.wire_overhead_bytes as f64 / self.payload_bytes as f64) * 100.0
    }
}

#[derive(Debug, Clone)]
pub struct ConcurrencyRecord {
    pub protocol: String,
    pub concurrency: usize,
    pub payload_bytes: usize,
    pub wall_duration: Duration,
    pub aggregate_mb_s: f64,
    #[allow(dead_code)]
    pub start_rss_bytes: u64,
    pub peak_rss_bytes: u64,
    pub rss_delta_bytes: i64,
    pub errors: usize,
}

/// Execute a single MIAB benchmark iteration.
pub async fn run_miab_iteration(
    server_addr: &str,
    payload: &[u8],
    chunk_size: usize,
) -> Result<Duration, Box<dyn std::error::Error + Send + Sync>> {
    let start = Instant::now();
    let resp = miab::client::send_bytes_with_chunk_size(
        server_addr,
        "bench/test_blob.bin",
        payload,
        chunk_size,
    )
    .await?;

    if !resp.is_ok() {
        return Err(format!("MIAB server returned error: {}", resp.message).into());
    }

    Ok(start.elapsed())
}

/// Execute a single HTTP/1.1 chunked benchmark iteration.
pub async fn run_http1_iteration(
    http_url: &str,
    payload: &[u8],
    chunk_size: usize,
) -> Result<Duration, Box<dyn std::error::Error + Send + Sync>> {
    let chunks: Vec<Result<Vec<u8>, std::io::Error>> =
        payload.chunks(chunk_size).map(|c| Ok(c.to_vec())).collect();

    let stream = futures_util::stream::iter(chunks);
    let body = reqwest::Body::wrap_stream(stream);
    let client = reqwest::Client::builder().http1_only().build()?;

    let start = Instant::now();
    let resp = client.put(http_url).body(body).send().await?;

    if !resp.status().is_success() {
        return Err(format!("HTTP/1.1 server returned status: {}", resp.status()).into());
    }

    Ok(start.elapsed())
}

/// Execute a single HTTP/2 cleartext (h2c) benchmark iteration.
pub async fn run_http2_iteration(
    http_url: &str,
    payload: &[u8],
    chunk_size: usize,
) -> Result<Duration, Box<dyn std::error::Error + Send + Sync>> {
    let chunks: Vec<Result<Vec<u8>, std::io::Error>> =
        payload.chunks(chunk_size).map(|c| Ok(c.to_vec())).collect();

    let stream = futures_util::stream::iter(chunks);
    let body = reqwest::Body::wrap_stream(stream);
    let client = reqwest::Client::builder().http2_prior_knowledge().build()?;

    let start = Instant::now();
    let resp = client.put(http_url).body(body).send().await?;

    if !resp.status().is_success() {
        return Err(format!("HTTP/2 server returned status: {}", resp.status()).into());
    }

    Ok(start.elapsed())
}

/// Measure a batch of MIAB iterations while profiling CPU and memory usage.
pub async fn run_miab_suite(
    server_addr: &str,
    payload: &[u8],
    chunk_size: usize,
    iterations: usize,
) -> Result<BenchRecord, Box<dyn std::error::Error + Send + Sync>> {
    let m_start = ProcessMetrics::snapshot();
    let mut durations = Vec::with_capacity(iterations);

    for _ in 0..iterations {
        let d = run_miab_iteration(server_addr, payload, chunk_size).await?;
        durations.push(d);
    }

    let m_end = ProcessMetrics::snapshot();
    let cpu_duration = m_end.cpu_delta(&m_start);
    let wire_overhead_bytes = miab_wire_overhead(payload.len(), chunk_size);

    Ok(BenchRecord {
        protocol: "MIAB v1".to_string(),
        payload_bytes: payload.len(),
        chunk_bytes: chunk_size,
        durations,
        cpu_duration,
        wire_overhead_bytes,
    })
}

/// Measure a batch of HTTP/1.1 iterations while profiling CPU and memory usage.
pub async fn run_http1_suite(
    http_url: &str,
    payload: &[u8],
    chunk_size: usize,
    iterations: usize,
) -> Result<BenchRecord, Box<dyn std::error::Error + Send + Sync>> {
    let m_start = ProcessMetrics::snapshot();
    let mut durations = Vec::with_capacity(iterations);

    for _ in 0..iterations {
        let d = run_http1_iteration(http_url, payload, chunk_size).await?;
        durations.push(d);
    }

    let m_end = ProcessMetrics::snapshot();
    let cpu_duration = m_end.cpu_delta(&m_start);
    let wire_overhead_bytes = http_wire_overhead(payload.len(), chunk_size);

    Ok(BenchRecord {
        protocol: "HTTP/1.1 (Chunked)".to_string(),
        payload_bytes: payload.len(),
        chunk_bytes: chunk_size,
        durations,
        cpu_duration,
        wire_overhead_bytes,
    })
}

/// Measure a batch of HTTP/2 iterations while profiling CPU and memory usage.
pub async fn run_http2_suite(
    http_url: &str,
    payload: &[u8],
    chunk_size: usize,
    iterations: usize,
) -> Result<BenchRecord, Box<dyn std::error::Error + Send + Sync>> {
    let m_start = ProcessMetrics::snapshot();
    let mut durations = Vec::with_capacity(iterations);

    for _ in 0..iterations {
        let d = run_http2_iteration(http_url, payload, chunk_size).await?;
        durations.push(d);
    }

    let m_end = ProcessMetrics::snapshot();
    let cpu_duration = m_end.cpu_delta(&m_start);
    let wire_overhead_bytes = http2_wire_overhead(payload.len(), chunk_size);

    Ok(BenchRecord {
        protocol: "HTTP/2 (h2c)".to_string(),
        payload_bytes: payload.len(),
        chunk_bytes: chunk_size,
        durations,
        cpu_duration,
        wire_overhead_bytes,
    })
}

/// Run concurrent parallel streams stress test measuring aggregate throughput and peak RSS.
pub async fn run_concurrency_test(
    protocol: BenchProtocol,
    target: &str,
    payload: Arc<Vec<u8>>,
    chunk_size: usize,
    concurrency: usize,
) -> ConcurrencyRecord {
    let start_rss = current_rss_bytes().unwrap_or(0);
    let peak_rss = Arc::new(AtomicUsize::new(start_rss as usize));

    // Background sampler task to monitor peak memory during parallel flood
    let peak_rss_clone = peak_rss.clone();
    let (sampler_stop_tx, mut sampler_stop_rx) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = &mut sampler_stop_rx => break,
                _ = tokio::time::sleep(Duration::from_millis(2)) => {
                    if let Some(rss) = current_rss_bytes() {
                        peak_rss_clone.fetch_max(rss as usize, Ordering::Relaxed);
                    }
                }
            }
        }
    });

    let errors = Arc::new(AtomicUsize::new(0));
    let start_wall = Instant::now();

    let http1_client = if protocol == BenchProtocol::Http1 {
        Some(Arc::new(
            reqwest::Client::builder()
                .http1_only()
                .build()
                .expect("failed to build http1 client"),
        ))
    } else {
        None
    };

    let http2_client = if protocol == BenchProtocol::Http2 {
        Some(Arc::new(
            reqwest::Client::builder()
                .http2_prior_knowledge()
                .build()
                .expect("failed to build http2 client"),
        ))
    } else {
        None
    };

    let mut handles = Vec::with_capacity(concurrency);
    for i in 0..concurrency {
        let target = target.to_string();
        let payload = payload.clone();
        let errors = errors.clone();
        let h1 = http1_client.clone();
        let h2 = http2_client.clone();

        handles.push(tokio::spawn(async move {
            match protocol {
                BenchProtocol::Miab => {
                    let path = format!("bench/concurrent_{}.bin", i);
                    if miab::client::send_bytes_with_chunk_size(
                        &target, &path, &payload, chunk_size,
                    )
                    .await
                    .is_err()
                    {
                        errors.fetch_add(1, Ordering::Relaxed);
                    }
                }
                BenchProtocol::Http1 => {
                    let client = h1.unwrap();
                    let chunks: Vec<Result<Vec<u8>, std::io::Error>> =
                        payload.chunks(chunk_size).map(|c| Ok(c.to_vec())).collect();
                    let stream = futures_util::stream::iter(chunks);
                    let body = reqwest::Body::wrap_stream(stream);
                    if let Ok(resp) = client.put(&target).body(body).send().await {
                        if !resp.status().is_success() {
                            errors.fetch_add(1, Ordering::Relaxed);
                        }
                    } else {
                        errors.fetch_add(1, Ordering::Relaxed);
                    }
                }
                BenchProtocol::Http2 => {
                    let client = h2.unwrap();
                    let chunks: Vec<Result<Vec<u8>, std::io::Error>> =
                        payload.chunks(chunk_size).map(|c| Ok(c.to_vec())).collect();
                    let stream = futures_util::stream::iter(chunks);
                    let body = reqwest::Body::wrap_stream(stream);
                    if let Ok(resp) = client.put(&target).body(body).send().await {
                        if !resp.status().is_success() {
                            errors.fetch_add(1, Ordering::Relaxed);
                        }
                    } else {
                        errors.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
        }));
    }

    for h in handles {
        let _ = h.await;
    }

    let wall_duration = start_wall.elapsed();
    let _ = sampler_stop_tx.send(());

    let final_peak = peak_rss.load(Ordering::Relaxed) as u64;
    let rss_delta = (final_peak as i64) - (start_rss as i64);
    let total_bytes = payload.len() * concurrency;
    let secs = wall_duration.as_secs_f64();
    let aggregate_mb_s = if secs > 0.0 {
        (total_bytes as f64 / (1024.0 * 1024.0)) / secs
    } else {
        0.0
    };

    ConcurrencyRecord {
        protocol: protocol.name().to_string(),
        concurrency,
        payload_bytes: payload.len(),
        wall_duration,
        aggregate_mb_s,
        start_rss_bytes: start_rss,
        peak_rss_bytes: final_peak,
        rss_delta_bytes: rss_delta,
        errors: errors.load(Ordering::Relaxed),
    }
}

/// Format bytes into a human-readable string (e.g. 1.0 MB, 64 KB).
pub fn format_bytes(bytes: usize) -> String {
    if bytes >= 1024 * 1024 * 1024 {
        format!("{:.1} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    } else if bytes >= 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{} KB", bytes / 1024)
    } else {
        format!("{} B", bytes)
    }
}

/// Format duration compactly (e.g. 842.1µs, 12.34ms, 1.25s).
pub fn format_duration(d: Duration) -> String {
    let micros = d.as_micros();
    if micros < 1000 {
        format!("{}µs", micros)
    } else if micros < 1_000_000 {
        format!("{:.2}ms", d.as_secs_f64() * 1000.0)
    } else {
        format!("{:.2}s", d.as_secs_f64())
    }
}

/// Print comprehensive comparison results table to terminal.
pub fn print_comprehensive_table(records: &[BenchRecord], is_markdown: bool) {
    if is_markdown {
        println!("| Payload | Protocol | Chunk | Median (p50) | p99 Latency | Throughput (MB/s) | CPU Time (ms/GB) | Wire Tax |");
        println!("| :--- | :--- | :--- | :--- | :--- | :--- | :--- | :--- |");
        for r in records {
            println!(
                "| {} | {} | {} | {} | {} | {:.2} MB/s | {:.2} ms/GB | {:.3}% |",
                format_bytes(r.payload_bytes),
                r.protocol,
                format_bytes(r.chunk_bytes),
                format_duration(r.p50_duration()),
                format_duration(r.p99_duration()),
                r.throughput_mb_s(),
                r.cpu_ms_per_gb(),
                r.wire_tax_pct(),
            );
        }
    } else {
        println!("{:=<122}", "");
        println!(
            "{:<9} | {:<24} | {:<8} | {:<12} | {:<12} | {:>14} | {:>15} | {:>9}",
            "Payload",
            "Protocol",
            "Chunk",
            "Median(p50)",
            "p99 Latency",
            "Throughput",
            "CPU Time / GB",
            "Wire Tax"
        );
        println!("{:-<122}", "");
        for r in records {
            println!(
                "{:<9} | {:<24} | {:<8} | {:<12} | {:<12} | {:>11.2} MB/s | {:>10.2} ms/GB | {:>8.3}%",
                format_bytes(r.payload_bytes),
                r.protocol,
                format_bytes(r.chunk_bytes),
                format_duration(r.p50_duration()),
                format_duration(r.p99_duration()),
                r.throughput_mb_s(),
                r.cpu_ms_per_gb(),
                r.wire_tax_pct(),
            );
        }
        println!("{:=<122}", "");
    }
}

/// Print concurrency stress test results table.
pub fn print_concurrency_table(records: &[ConcurrencyRecord], is_markdown: bool) {
    if is_markdown {
        println!("\n### Concurrency & Memory Stress Test");
        println!("| Protocol | Concurrency | Total Transferred | Wall Time | Aggregate (MB/s) | Peak RSS | Memory Delta | Errors |");
        println!("| :--- | :--- | :--- | :--- | :--- | :--- | :--- | :--- |");
        for r in records {
            let total_bytes = r.payload_bytes * r.concurrency;
            let delta_str = if r.rss_delta_bytes >= 0 {
                format!("+{}", format_bytes(r.rss_delta_bytes as usize))
            } else {
                format!("-{}", format_bytes((-r.rss_delta_bytes) as usize))
            };
            println!(
                "| {} | {} streams | {} | {} | {:.2} MB/s | {} | {} | {} |",
                r.protocol,
                r.concurrency,
                format_bytes(total_bytes),
                format_duration(r.wall_duration),
                r.aggregate_mb_s,
                format_bytes(r.peak_rss_bytes as usize),
                delta_str,
                r.errors
            );
        }
    } else {
        println!("\n{:=<122}", "");
        println!(" CONCURRENCY & MEMORY STRESS RESULTS");
        println!("{:=<122}", "");
        println!(
            "{:<24} | {:<11} | {:<14} | {:<10} | {:>15} | {:>10} | {:>12} | {:>6}",
            "Protocol",
            "Concurrency",
            "Total Data",
            "Wall Time",
            "Aggregate MB/s",
            "Peak RSS",
            "Mem Delta",
            "Errors"
        );
        println!("{:-<122}", "");
        for r in records {
            let total_bytes = r.payload_bytes * r.concurrency;
            let delta_str = if r.rss_delta_bytes >= 0 {
                format!("+{}", format_bytes(r.rss_delta_bytes as usize))
            } else {
                format!("-{}", format_bytes((-r.rss_delta_bytes) as usize))
            };
            println!(
                "{:<24} | {:>2} streams  | {:<14} | {:<10} | {:>12.2} MB/s | {:>10} | {:>12} | {:>6}",
                r.protocol,
                r.concurrency,
                format_bytes(total_bytes),
                format_duration(r.wall_duration),
                r.aggregate_mb_s,
                format_bytes(r.peak_rss_bytes as usize),
                delta_str,
                r.errors
            );
        }
        println!("{:=<122}", "");
    }
}
