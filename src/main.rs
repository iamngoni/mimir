mod models;
mod server;
mod sessions;

use anyhow::Result;
use rmcp::ServiceExt;
use tracing_subscriber::EnvFilter;

use crate::server::MimirServer;

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging (writes to stderr so it doesn't interfere with stdio transport)
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();

    tracing::info!("Starting Mimir MCP server");

    let server = MimirServer::new();
    let transport = rmcp::transport::stdio();

    // Serve the MCP server over stdio and wait for it to finish
    server.serve(transport).await?.waiting().await?;

    Ok(())
}
