use std::time::Duration;

/// Process metrics snapshot tracking CPU time and memory footprint.
#[derive(Debug, Clone, Copy, Default)]
pub struct ProcessMetrics {
    pub user_cpu: Duration,
    pub system_cpu: Duration,
    #[allow(dead_code)]
    pub resident_memory_bytes: u64,
}

impl ProcessMetrics {
    /// Capture current process metrics (user + system CPU time and resident memory).
    pub fn snapshot() -> Self {
        #[cfg(unix)]
        unsafe {
            let mut usage = std::mem::zeroed::<libc::rusage>();
            if libc::getrusage(libc::RUSAGE_SELF, &mut usage) == 0 {
                let user = Duration::new(
                    usage.ru_utime.tv_sec as u64,
                    (usage.ru_utime.tv_usec as u32) * 1000,
                );
                let sys = Duration::new(
                    usage.ru_stime.tv_sec as u64,
                    (usage.ru_stime.tv_usec as u32) * 1000,
                );

                let rss = current_rss_bytes().unwrap_or({
                    #[cfg(target_os = "linux")]
                    {
                        (usage.ru_maxrss as u64) * 1024
                    }
                    #[cfg(not(target_os = "linux"))]
                    {
                        usage.ru_maxrss as u64
                    }
                });

                return ProcessMetrics {
                    user_cpu: user,
                    system_cpu: sys,
                    resident_memory_bytes: rss,
                };
            }
        }

        ProcessMetrics::default()
    }

    /// Sum of user and system CPU time.
    pub fn total_cpu(&self) -> Duration {
        self.user_cpu + self.system_cpu
    }

    /// Compute delta CPU time between two snapshots.
    pub fn cpu_delta(&self, earlier: &ProcessMetrics) -> Duration {
        self.total_cpu().saturating_sub(earlier.total_cpu())
    }
}

/// Read current resident set size (RSS) in bytes directly from Linux /proc/self/statm.
#[cfg(target_os = "linux")]
pub fn current_rss_bytes() -> Option<u64> {
    if let Ok(content) = std::fs::read_to_string("/proc/self/statm") {
        let mut parts = content.split_whitespace();
        let _size = parts.next();
        if let Some(resident_pages_str) = parts.next() {
            if let Ok(resident_pages) = resident_pages_str.parse::<u64>() {
                let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) as u64 };
                return Some(resident_pages * page_size);
            }
        }
    }
    None
}

#[cfg(not(target_os = "linux"))]
pub fn current_rss_bytes() -> Option<u64> {
    None
}

/// Calculate CPU milliseconds consumed per Gigabyte transferred.
pub fn cpu_ms_per_gb(cpu_duration: Duration, total_bytes: usize) -> f64 {
    if total_bytes == 0 {
        return 0.0;
    }
    let cpu_ms = cpu_duration.as_secs_f64() * 1000.0;
    let gb = total_bytes as f64 / (1024.0 * 1024.0 * 1024.0);
    cpu_ms / gb
}

/// Exact protocol wire framing overhead for MIAB v1.
///
/// Layout:
/// - Initial Header: 4B Magic + 1B Version + 2B PathLen + Path ("bench/test.bin" = 14B) = 21B
/// - Chunks: N * 4B length prefixes
/// - Terminal: 4B (0x00000000)
/// - Response: 1B Status + 2B MsgLen + "Saved X bytes" (~22B) = 25B
pub fn miab_wire_overhead(payload_len: usize, chunk_len: usize) -> usize {
    let num_chunks = payload_len.div_ceil(chunk_len);
    let header_bytes = 4 + 1 + 2 + 14;
    let chunk_prefix_bytes = num_chunks * 4;
    let terminal_bottle_bytes = 4;
    let response_bytes = 1 + 2 + 22;
    header_bytes + chunk_prefix_bytes + terminal_bottle_bytes + response_bytes
}

/// Approximate protocol wire framing overhead for HTTP/1.1 Chunked Transfer.
///
/// Layout:
/// - Request headers: PUT /upload HTTP/1.1\r\nhost: ...\r\ntransfer-encoding: chunked... (~160B)
/// - Chunk delimiters: N * (hex_length + \r\n + \r\n) ≈ N * (5 to 8 bytes)
/// - Terminal chunk: 0\r\n\r\n (5 bytes)
/// - Response headers: HTTP/1.1 200 OK\r\ncontent-length: ... (~130B)
pub fn http_wire_overhead(payload_len: usize, chunk_len: usize) -> usize {
    let num_chunks = payload_len.div_ceil(chunk_len);
    let req_headers = 160;
    // Hex chunk header + \r\n before and \r\n after
    let chunk_delimiters = num_chunks * 8;
    let terminal_chunk = 5;
    let resp_headers = 130;
    req_headers + chunk_delimiters + terminal_chunk + resp_headers
}
