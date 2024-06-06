use anyhow::Result;
use clap::Parser;
use std::net::SocketAddr;

#[derive(Parser, Debug)]
#[command(version, about)]
struct Cli {
    /// proxy to connect to
    #[arg(short, long)]
    address: SocketAddr,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Cli::parse();
    let env = env_logger::Env::default().filter_or("RUST_LOG", "info");
    env_logger::init_from_env(env);
    log::info!("proxy to connect to {}", args.address);
    pow_service::tasks::worker::start_worker(args.address).await?;
    Ok(())
}
