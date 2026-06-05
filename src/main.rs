use clap::{Args, Parser, Subcommand};
use std::{net::SocketAddr, path::PathBuf};
use tracing::{debug, info};

mod agent;
mod server;

#[derive(Debug, Parser)]
#[command(name = "MaIa", author, version, about)]
pub struct Cli {
    #[arg(short, long)]
    config: Option<PathBuf>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    Server(ServerArgs),
}

#[derive(Debug, Args)]
struct ServerArgs {
    #[arg(short, long, default_value = "127.0.0.1:3000")]
    addr: SocketAddr,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_target(false)
        .compact()
        .init();

    let cli = Cli::parse();
    debug!(config = ?cli.config, "config used");

    match cli.command {
        Commands::Server(server_args) => {
            info!(addr = %server_args.addr, "starting axum server");
            server::run(server_args.addr).await?;
        }
    };

    Ok(())
}
