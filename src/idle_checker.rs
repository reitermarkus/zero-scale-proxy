use std::sync::{Arc, RwLock};
use std::net::SocketAddr;

use defer::defer;
use tokio::time::{Duration, Instant, sleep, sleep_until};

use crate::ZeroScaler;

#[derive(Debug)]
pub struct IdleChecker {
  timeout: Duration,
  active_connections: Arc<RwLock<(usize, Instant)>>,
}

impl IdleChecker {
  pub fn new(timeout: Duration) -> Self {
    Self {
      timeout,
      active_connections: Arc::new(RwLock::new((0, Instant::now()))),
    }
  }

  pub async fn start(&self, scaler: &ZeroScaler) {
    loop {
      self.check(scaler).await;
    }
  }

  pub async fn check(&self, scaler: &ZeroScaler) {
    let (connection_count, last_update) = *self.active_connections.read().unwrap();

    log::debug!("Checking if idle timeout is reached.");
    let deadline = last_update + self.timeout;
    let mut timer = sleep_until(deadline);
    let now = Instant::now();
    if deadline < now {
      if connection_count == 0 {
        match scaler.replicas().await {
          Ok(replicas) => {
            log::debug!("{} replicas are available.", replicas);

            if replicas >= 1 {
              log::info!("Reached idle timeout, scaling down.");
              match scaler.scale_to(0).await {
                Ok(_) => log::info!("Scaled down successfully."),
                Err(err) => {
                  log::error!("Error scaling down: {}", err);
                }
              }
            }
          },
          Err(err) => {
            log::error!("Failed getting replica count: {}", err);
          }
        }
      } else {
        log::info!("{} connections are active, next idle check in {} seconds.", connection_count, self.timeout.as_secs());
      }

      timer = sleep(self.timeout);
    } else {
      log::info!("Timeout not yet reached, next idle check in {} seconds.", (deadline - now).as_secs());
    }

    timer.await
  }

  pub fn register_connection(&self, peer_addr: SocketAddr) -> impl Drop {
    log::trace!("register_connection");

    let active_connections = self.active_connections.clone();

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
}
