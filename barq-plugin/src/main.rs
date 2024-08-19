#![allow(unused)]

use anyhow::Result;

mod methods;
mod plugin;

/// Main entry point for Barq
fn main() -> Result<()> {
    let plugin = plugin::build_plugin()?;
    plugin.start();
    Ok(())
}
