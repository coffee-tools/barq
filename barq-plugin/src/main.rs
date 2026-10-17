use anyhow::Result;

mod logger;
mod methods;
mod plugin;

/// Main entry point for Barq
fn main() -> Result<()> {
    logger::init();
    let plugin = plugin::build_plugin()?;
    // Keep the runtime alive past `start`: dropping it would wait for any
    // blocking gossip task still running.
    let _runtime = plugin.state.runtime.clone();
    // cln4rust returns from `start` when lightningd closes stdin.
    plugin.start();
    std::process::exit(0)
}
