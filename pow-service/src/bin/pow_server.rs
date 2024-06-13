use anyhow::Result;
use clap::Parser;
use std::{net::SocketAddr, path::PathBuf};

#[derive(Parser, Debug)]
#[command(version, about)]
struct Cli {
    /// ip:port to listen for worker connection
    #[arg(short, long)]
    address: SocketAddr,
    /// ip:port to listen for rest service
    #[arg(short, long)]
    rest: SocketAddr,
    /// network to work on, miannet or testnet
    #[arg(short, long)]
    network: String,
    /// path to db datadir
    #[arg(short, long)]
    datadir: PathBuf,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Cli::parse();
    let env = env_logger::Env::default().filter_or("RUST_LOG", "debug");
    env_logger::init_from_env(env);
    log::info!("pow worker listening on {}", args.address);
    log::info!("rest service listening on {}", args.rest);
    pow_service::tasks::proxy::start_proxy(args.address, args.rest, args.datadir, args.network)
        .await?;
    Ok(())
}
