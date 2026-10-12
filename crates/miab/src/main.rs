use clap::{Parser, Subcommand};
use miab::client::{
    parse_token_str, send_file_with_options, send_stream_with_options, SendOptions,
};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "miab",
    about = "Message In A Bottle Client CLI (MIAB v1 / RFC-001)"
)]
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
        /// Optional 16-byte hex authentication token
        #[arg(short = 't', long = "token")]
        token: Option<String>,
        /// Do not overwrite remote file if it already exists
        #[arg(long = "no-overwrite", default_value_t = false)]
        no_overwrite: bool,
        /// Mark remote file as executable (chmod +x)
        #[arg(short = 'x', long = "executable", default_value_t = false)]
        executable: bool,
    },
    /// Stream stdin pipe to the remote MIAB server
    Stream {
        /// Server address (host or host:port, default port: 6422)
        server: String,
        /// Remote destination relative path
        remote_path: String,
        /// Optional 16-byte hex authentication token
        #[arg(short = 't', long = "token")]
        token: Option<String>,
        /// Do not overwrite remote file if it already exists
        #[arg(long = "no-overwrite", default_value_t = false)]
        no_overwrite: bool,
        /// Mark remote file as executable (chmod +x)
        #[arg(short = 'x', long = "executable", default_value_t = false)]
        executable: bool,
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
            token,
            no_overwrite,
            executable,
        } => {
            let auth_token = token.as_deref().and_then(parse_token_str);
            send_file_with_options(
                &server,
                &remote_path,
                &local_file,
                SendOptions {
                    show_progress: true,
                    auth_token,
                    no_overwrite,
                    set_executable: executable,
                    ..Default::default()
                },
            )
            .await
        }
        Commands::Stream {
            server,
            remote_path,
            token,
            no_overwrite,
            executable,
        } => {
            let auth_token = token.as_deref().and_then(parse_token_str);
            send_stream_with_options(
                &server,
                &remote_path,
                tokio::io::stdin(),
                SendOptions {
                    show_progress: true,
                    auth_token,
                    no_overwrite,
                    set_executable: executable,
                    ..Default::default()
                },
            )
            .await
        }
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
