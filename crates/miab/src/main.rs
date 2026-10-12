use clap::{Parser, Subcommand};
use miab::client::{send_file, send_stream};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "miab", about = "Message In A Bottle Client CLI")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Send a local file to the remote MIAB server
    Put {
        /// Server address (host or host:port, default port: 6422)
        server: String,
        /// Remote destination relative path
        remote_path: String,
        /// Local file to upload
        local_file: PathBuf,
    },
    /// Stream stdin pipe to the remote MIAB server
    Stream {
        /// Server address (host or host:port, default port: 6422)
        server: String,
        /// Remote destination relative path
        remote_path: String,
    },
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    let result = match cli.command {
        Commands::Put {
            server,
            remote_path,
            local_file,
        } => send_file(&server, &remote_path, &local_file, true).await,
        Commands::Stream {
            server,
            remote_path,
        } => send_stream(&server, &remote_path, tokio::io::stdin(), true).await,
    };

    match result {
        Ok(resp) => {
            if resp.is_ok() {
                println!("✓ Transfer successful: {}", resp.message);
                std::process::exit(0);
            } else {
                eprintln!("✗ Server error (status {}): {}", resp.status, resp.message);
                std::process::exit(1);
            }
        }
        Err(err) => {
            eprintln!("✗ Transfer failed: {}", err);
            std::process::exit(1);
        }
    }
}
