use clap::Parser;
use miabd::server::{Server, ServerArgs};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let args = ServerArgs::parse();

    let parsed_auth = args.auth_token.as_ref().and_then(|s| {
        let clean = s.trim().replace('-', "");
        if clean.len() == 32 {
            let mut arr = [0u8; 16];
            for i in 0..16 {
                if let Ok(b) = u8::from_str_radix(&clean[i * 2..i * 2 + 2], 16) {
                    arr[i] = b;
                } else {
                    return None;
                }
            }
            Some(arr)
        } else if clean.len() == 16 {
            let mut arr = [0u8; 16];
            arr.copy_from_slice(clean.as_bytes());
            Some(arr)
        } else {
            None
        }
    });

    let server =
        Server::bind_with_options(&args.bind, args.storage_root, args.discard, parsed_auth).await?;
    server.run().await?;
    Ok(())
}
