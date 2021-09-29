use std::env;
use std::sync::{Arc, RwLock};

use futures::future::Either;
use futures::future::try_join_all;
use kube::{Api, Client};
use k8s_openapi::api::core::v1::Service;
use tokio::join;
use tokio::time::{sleep, sleep_until, Duration, Instant};

mod minecraft;
pub mod sd2d;
mod zero_scaler;
pub(crate) use zero_scaler::ZeroScaler;

mod proxy;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
  env_logger::init();

  let deployment: String = env::var("DEPLOYMENT").expect("DEPLOYMENT is not set");
  let namespace: String = env::var("NAMESPACE").expect("NAMESPACE is not set");
  let timeout: Duration = Duration::from_secs(
    env::var("TIMEOUT").map(|t| t.parse::<u64>().expect("TIMEOUT is not a number")).unwrap_or(600)
  );
  let proxy_type = env::var("PROXY_TYPE").ok();

  let upstreams: Vec<(String, (u16, String))> = if let Some((ip, ports)) = env::var("UPSTREAM_IP").ok().zip(env::var("PORTS").ok()) {
    let ports = ports.split(',').flat_map(|port| {
      if let Some((port, protocol)) = port.split_once("/") {
        vec![
          (port.parse::<u16>().unwrap(), protocol.to_owned()),
        ].into_iter()
      } else {
        vec![
          (port.parse::<u16>().unwrap(), "tcp".into()),
          (port.parse::<u16>().unwrap(), "udp".into()),
        ].into_iter()
      }
    });

    ports.map(|port| (ip.clone(), port)).collect()
  } else {
    let service: String = env::var("SERVICE").expect("SERVICE is not set");

    let services: Vec<Service> = try_join_all(service.split(',').map(|service| {
      let namespace = namespace.clone();
      async move {
        let client = Client::try_default().await?;
        let services: Api<Service> = Api::namespaced(client, &namespace);
        services.get(service).await
      }
    }).collect::<Vec<_>>()).await?;

    services.into_iter().flat_map(|service| {
      let load_balancer_ip = service.status.as_ref()
        .and_then(|s| s.load_balancer.as_ref())
        .and_then(|lb| lb.ingress.as_ref())
        .and_then(|i| i.iter().find_map(|i| i.ip.as_ref()));
      let cluster_ip = service.spec.as_ref().and_then(|s| s.cluster_ip.as_ref());

      let ip = load_balancer_ip.or(cluster_ip).expect("Failed to get service IP").to_owned();

      let ports: Vec<(u16, String)> = service.spec.as_ref().and_then(|s| {
        s.ports.as_ref().map(|ports| ports.iter().map(|port| {
          (port.port as u16, port.protocol.as_ref().map(|p| p.to_lowercase()).unwrap_or_else(|| "tcp".into()))
        }).collect::<Vec<_>>())
      }).unwrap_or_default();

      ports.into_iter().map(move |port| (ip.clone(), port))
    }).collect()
  };

  let scaler = Arc::new(ZeroScaler {
    name: deployment,
    namespace,
  });

  log::info!("Proxying the following ports:");
  for (ip, (port, protocol)) in &upstreams {
    log::info!("  {}:{}/{}", ip, port, protocol);
  }

  let active_connections = Arc::new(RwLock::new((0, Instant::now())));

  let idle_checker = async {
    let active_connections = Arc::clone(&active_connections);

    loop {
      let (connection_count, last_update) = *active_connections.read().unwrap();

      log::debug!("Checking if idle timeout is reached.");
      let deadline = last_update + timeout;
      let mut timer = sleep_until(deadline);
      if deadline < Instant::now() {
        log::debug!("Connection count: {}", connection_count);

        if connection_count == 0 {
          log::debug!("Checking replica count.");
          let replicas = match scaler.replicas().await {
            Ok(replicas) => replicas,
            Err(err) => {
              log::error!("Failed getting replica count: {}", err);
              continue
            }
          };
          log::debug!("Replicas: {}", replicas);

          if replicas >= 1 {
            log::info!("Reached idle timeout. Scaling down.");

            if let Err(err) = scaler.scale_to(0).await {
              log::error!("Error scaling down: {}", err);
              continue
            }
          }
        } else {
          log::debug!("Idle timeout not yet reached. Next check in {} seconds.", timeout.as_secs());
        }

        timer = sleep(timeout);
      }

      timer.await;
    }
  };

  let proxies = try_join_all(upstreams.into_iter().map(|(ip, (port, protocol))| match protocol.as_ref() {
    "tcp" => {
      Either::Left(proxy::tcp(ip, port, active_connections.clone(), scaler.as_ref(), proxy_type.clone()))
    },
    "udp" => {
      Either::Right(proxy::udp(ip, port, active_connections.clone(), scaler.clone(), proxy_type.clone(), timeout))
    },
    _ => unreachable!(),
  }));

  let (_, proxies_result) = join!(idle_checker, proxies);
  proxies_result?;

  Ok(())
}
