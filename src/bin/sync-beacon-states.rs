use anyhow::Result;

#[tokio::main]
pub async fn main() -> Result<()> {
    eth_analysis::sync_beacon_states().await
}
