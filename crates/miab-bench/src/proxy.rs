use std::net::SocketAddr;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio::time::Instant;

struct DelayedPacket {
    deliver_at: Instant,
    data: Vec<u8>,
}

/// Unprivileged Userspace VPN/WAN Network Simulator.
///
/// Accurately simulates network latency and MTU packet slicing using
/// a pipelined delay queue without requiring root/sudo or modifying host interfaces.
pub struct UserspaceVpnProxy {
    listener: TcpListener,
    target_addr: SocketAddr,
    latency: Duration,
    mtu: usize,
}

impl UserspaceVpnProxy {
    /// Start a new proxy listening on an ephemeral localhost port.
    pub async fn start(
        target_addr: SocketAddr,
        latency: Duration,
        mtu: usize,
    ) -> Result<Self, std::io::Error> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        Ok(Self {
            listener,
            target_addr,
            latency,
            mtu,
        })
    }

    /// Retrieve the local address clients should connect to.
    pub fn local_addr(&self) -> Result<SocketAddr, std::io::Error> {
        self.listener.local_addr()
    }

    /// Run the proxy loop until a shutdown signal is received.
    pub async fn run_until_shutdown(
        self,
        mut shutdown_rx: tokio::sync::oneshot::Receiver<()>,
    ) -> Result<(), std::io::Error> {
        loop {
            tokio::select! {
                _ = &mut shutdown_rx => {
                    break;
                }
                accept_res = self.listener.accept() => {
                    let (client_socket, _) = accept_res?;
                    let target_addr = self.target_addr;
                    let latency = self.latency;
                    let mtu = self.mtu;

                    tokio::spawn(async move {
                        if let Ok(server_socket) = TcpStream::connect(target_addr).await {
                            let (c_rx, c_tx) = client_socket.into_split();
                            let (s_rx, s_tx) = server_socket.into_split();

                            let upstream = tokio::spawn(forward_with_pipeline_delay(
                                c_rx, s_tx, latency, mtu,
                            ));
                            let downstream = tokio::spawn(forward_with_pipeline_delay(
                                s_rx, c_tx, latency, mtu,
                            ));

                            let _ = tokio::join!(upstream, downstream);
                        }
                    });
                }
            }
        }
        Ok(())
    }
}

/// Forwards data from an AsyncRead to an AsyncWrite using an MTU-sliced pipelined delay queue.
async fn forward_with_pipeline_delay<R, W>(
    mut reader: R,
    mut writer: W,
    latency: Duration,
    mtu: usize,
) where
    R: AsyncReadExt + Unpin + Send + 'static,
    W: AsyncWriteExt + Unpin + Send + 'static,
{
    let (tx, mut rx) = mpsc::channel::<DelayedPacket>(1024);

    // Sender task: reads from socket, slices into MTU chunks, schedules deliver_at
    let sender_handle = tokio::spawn(async move {
        let mut buf = vec![0u8; mtu];
        loop {
            match reader.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => {
                    let deliver_at = Instant::now() + latency;
                    let packet = DelayedPacket {
                        deliver_at,
                        data: buf[..n].to_vec(),
                    };
                    if tx.send(packet).await.is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    // Receiver task: pops from queue and delivers when deliver_at arrives
    let receiver_handle = tokio::spawn(async move {
        while let Some(packet) = rx.recv().await {
            let now = Instant::now();
            if packet.deliver_at > now {
                tokio::time::sleep_until(packet.deliver_at).await;
            }
            if writer.write_all(&packet.data).await.is_err() {
                break;
            }
        }
        let _ = writer.shutdown().await;
    });

    let _ = tokio::join!(sender_handle, receiver_handle);
}
