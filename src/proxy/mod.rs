use crate::ZeroScaler;

pub mod middleware;

mod tcp;
pub use tcp::tcp_proxy as tcp;

mod udp;
pub use udp::udp_proxy as udp;

pub async fn scale_up(scaler: &ZeroScaler) {
  log::trace!("scale_up");

  if scaler.replicas().await.map(|r| r == 0).unwrap_or(false) {
    log::info!("Received request, scaling up.");
    match scaler.scale_to(1).await {
      Ok(_) => log::info!("Scaled up successfully."),
      Err(err) => log::error!("Error scaling up: {}", err),
    }
  }
}
