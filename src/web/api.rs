use crate::store::SharedStore;
pub struct ApiServer {
    pub address: std::net::SocketAddr,
}
impl ApiServer {
    pub fn start(_store: SharedStore, _port: u16) -> anyhow::Result<Self> {
        anyhow::bail!("The local HTTP API server is available in the desktop app")
    }
    pub async fn shutdown(self) {}
}
