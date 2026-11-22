#[tokio::main]
async fn main() -> anyhow::Result<()> {
    atrium_node::run().await
}
