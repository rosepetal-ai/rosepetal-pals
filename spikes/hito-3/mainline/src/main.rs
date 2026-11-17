//! `mainline`: Iroh plus a private Mainline DHT with a capability index of
//! signed peers; the node is [`spike_mainline::Mainline`].

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    spike_common::run_node::<spike_mainline::Mainline>().await
}
