use axum::{
    body::Body, extract::State, http::StatusCode, response::IntoResponse, routing::put, Router,
};
use futures_util::StreamExt;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;

#[derive(Clone)]
pub struct ServerState {
    pub storage_root: PathBuf,
    pub discard: bool,
}

pub struct HttpServer {
    pub addr: SocketAddr,
    shutdown_tx: Option<tokio::sync::oneshot::Sender<()>>,
}

impl HttpServer {
    /// Start an embedded HTTP baseline server on a random localhost port.
    pub async fn start(storage_root: PathBuf, discard: bool) -> Result<Self, std::io::Error> {
        let state = Arc::new(ServerState {
            storage_root,
            discard,
        });

        let app = Router::new()
            .route("/upload", put(handle_upload))
            .with_state(state);

        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();

        tokio::spawn(async move {
            let _ = axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = shutdown_rx.await;
                })
                .await;
        });

        Ok(Self {
            addr,
            shutdown_tx: Some(shutdown_tx),
        })
    }

    pub fn shutdown(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
    }
}

impl Drop for HttpServer {
    fn drop(&mut self) {
        self.shutdown();
    }
}

async fn handle_upload(
    State(state): State<Arc<ServerState>>,
    body: Body,
) -> Result<impl IntoResponse, StatusCode> {
    let mut stream = body.into_data_stream();
    let mut total_bytes: u64 = 0;

    if state.discard {
        while let Some(chunk_res) = stream.next().await {
            let chunk = chunk_res.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
            total_bytes += chunk.len() as u64;
        }
    } else {
        let dest = state.storage_root.join("http_upload.dat");
        let mut file = tokio::fs::File::create(&dest)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

        while let Some(chunk_res) = stream.next().await {
            let chunk = chunk_res.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
            file.write_all(&chunk)
                .await
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
            total_bytes += chunk.len() as u64;
        }
        file.sync_all()
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    }

    Ok((StatusCode::OK, format!("Saved {} bytes", total_bytes)))
}
