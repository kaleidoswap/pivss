use clap::Parser;
use pivss_server::api::build_router;
use pivss_server::config::Config;
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "pivss-server",
    about = "P2P Incentivized Versioned Storage Service"
)]
struct Args {
    /// Path to a TOML config file (defaults are used when omitted).
    #[arg(short, long)]
    config: Option<PathBuf>,
    /// Override the listen address, e.g. 127.0.0.1:8339.
    #[arg(short, long)]
    listen: Option<String>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let args = Args::parse();
    let mut config = Config::load(args.config.as_ref())?;
    if let Some(listen) = args.listen {
        config.listen = listen;
    }

    let listen = config.listen.clone();
    let state = pivss_server::build_state(config).await?;

    tracing::info!(
        npub = %state.keys.npub(),
        storage = state.store.backend_name(),
        seeder = state.seeder.name(),
        "pivss server starting"
    );
    tracing::info!("host panel:  http://{listen}/panel");
    tracing::info!("client app:  http://{listen}/app");

    let listener = tokio::net::TcpListener::bind(&listen).await?;
    axum::serve(listener, build_router(state)).await?;
    Ok(())
}
