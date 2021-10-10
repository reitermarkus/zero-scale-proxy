use tokio::time::{Duration, sleep};

use crate::ZeroScaler;

#[derive(Debug)]
pub struct IdleChecker {
  timeout: Duration,
}

impl IdleChecker {
  pub fn new(timeout: Duration) -> Self {
    Self {
      timeout,
    }
  }

  pub async fn start(&self, scaler: &ZeroScaler) {
    // Don't run first check immediately.
    sleep(self.timeout).await;

    loop {
      self.check(scaler).await;
    }
  }

  pub async fn check(&self, scaler: &ZeroScaler) {
    log::trace!("check");
    log::debug!("Checking if idle timeout is reached.");

    let elapsed = scaler.active_connections.duration_since_last_update();
    let mut timeout = self.timeout;
    if elapsed > timeout {
      let connection_count = scaler.active_connections.count();
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
        let connection_plural = if connection_count == 1 {
          "connection is"
        } else {
          "connections are"
        };

        log::info!(
          "{} {} active, next idle check in {} seconds.",
          connection_count, connection_plural,
          timeout.as_secs()
        );
      }
    } else {
      timeout -= elapsed;
      log::info!("Timeout not yet reached, next idle check in {} seconds.", timeout.as_secs());
    }

    sleep(timeout).await
  }
}
