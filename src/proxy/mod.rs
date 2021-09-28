use std::net::SocketAddr;
use std::sync::{Arc, RwLock};

use defer::defer;
use tokio::time::Instant;

use crate::ZeroScaler;

mod tcp;
pub use tcp::tcp_proxy as tcp;

mod udp;
pub use udp::udp_proxy as udp;

pub fn register_connection(active_connections: Arc<RwLock<(usize, Instant)>>, peer_addr: SocketAddr) -> impl Drop {
  {
    let (ref mut connection_count, ref mut last_update) = *active_connections.write().unwrap();
    *connection_count += 1;
    log::info!("Peer {} connected, {} connections active.", peer_addr, connection_count);
    *last_update = Instant::now();
  }
  defer(move || {
    let (ref mut connection_count, ref mut last_update) = *active_connections.write().unwrap();
    *connection_count -= 1;
    log::info!("Peer {} disconnected, {} connections active.", peer_addr, connection_count);
    *last_update = Instant::now();
  })
}

pub async fn scale_up(scaler: &ZeroScaler) {
  if scaler.replicas().await.map(|r| r == 0).unwrap_or(false) {
    log::info!("Received request, scaling up.");
    let _ = scaler.scale_to(1).await;
  }
}
