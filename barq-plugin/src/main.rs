use anyhow::Result;

mod logger;
mod methods;
mod plugin;

/// Main entry point for Barq
#[tokio::main]
async fn main() -> Result<()> {
    logger::init();
    let plugin = plugin::build_plugin()?;
    // cln4rust returns from `start_async` when lightningd closes stdin.
    plugin.start_async().await;
    // Exit without waiting for payments or blocking gossip tasks still
    // running: lightningd is gone.
    std::process::exit(0)
}
