use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct BenchRecord {
    pub protocol: String,
    pub payload_bytes: usize,
    pub chunk_bytes: usize,
    pub durations: Vec<Duration>,
}

impl BenchRecord {
    pub fn mean_duration(&self) -> Duration {
        if self.durations.is_empty() {
            return Duration::ZERO;
        }
        let total: Duration = self.durations.iter().sum();
        total / self.durations.len() as u32
    }

    pub fn min_duration(&self) -> Duration {
        self.durations
            .iter()
            .copied()
            .min()
            .unwrap_or(Duration::ZERO)
    }

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

    pub fn throughput_mbps(&self) -> f64 {
        let secs = self.mean_duration().as_secs_f64();
        if secs <= 0.0 {
            return 0.0;
        }
        (self.payload_bytes as f64 * 8.0 / 1_000_000.0) / secs
    }
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
pub async fn run_http_iteration(
    http_url: &str,
    payload: &[u8],
    chunk_size: usize,
) -> Result<Duration, Box<dyn std::error::Error + Send + Sync>> {
    let chunks: Vec<Result<Vec<u8>, std::io::Error>> =
        payload.chunks(chunk_size).map(|c| Ok(c.to_vec())).collect();

    let stream = futures_util::stream::iter(chunks);
    let body = reqwest::Body::wrap_stream(stream);
    let client = reqwest::Client::new();

    let start = Instant::now();
    let resp = client.put(http_url).body(body).send().await?;

    if !resp.status().is_success() {
        return Err(format!("HTTP server returned status: {}", resp.status()).into());
    }

    Ok(start.elapsed())
}

/// Format bytes into a human-readable string (e.g. 1.00 MB, 64 KB).
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

/// Print formatted comparison results table to terminal.
pub fn print_results_table(records: &[BenchRecord], is_markdown: bool) {
    if is_markdown {
        println!("| Payload | Protocol | Chunk Size | Mean Time | Min / Max | Throughput (MB/s) | Bandwidth (Mbps) |");
        println!("| :--- | :--- | :--- | :--- | :--- | :--- | :--- |");
        for r in records {
            println!(
                "| {} | {} | {} | {:.2?} | {:.2?} / {:.2?} | {:.2} MB/s | {:.2} Mbps |",
                format_bytes(r.payload_bytes),
                r.protocol,
                format_bytes(r.chunk_bytes),
                r.mean_duration(),
                r.min_duration(),
                r.max_duration(),
                r.throughput_mb_s(),
                r.throughput_mbps()
            );
        }
    } else {
        println!("{:=<105}", "");
        println!(
            "{:<10} | {:<18} | {:<10} | {:<12} | {:<15} | {:<16} | {:<14}",
            "Payload", "Protocol", "Chunk", "Mean Time", "Min / Max", "Throughput", "Bandwidth"
        );
        println!("{:-<105}", "");
        for r in records {
            println!(
                "{:<10} | {:<18} | {:<10} | {:<12.2?} | {:<15} | {:>10.2} MB/s | {:>9.2} Mbps",
                format_bytes(r.payload_bytes),
                r.protocol,
                format_bytes(r.chunk_bytes),
                r.mean_duration(),
                format!("{:.2?} / {:.2?}", r.min_duration(), r.max_duration()),
                r.throughput_mb_s(),
                r.throughput_mbps()
            );
        }
        println!("{:=<105}", "");
    }
}
