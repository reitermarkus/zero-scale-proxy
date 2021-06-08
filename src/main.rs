use std::env;
use std::net::Ipv4Addr;
use std::sync::{Arc, RwLock};

use anyhow::Context;
use defer::defer;
use futures::prelude::*;
use kube::{Api, Client};
use k8s_openapi::api::core::v1::Service;
use tokio::io;
use tokio::join;
use tokio::net::{TcpListener, TcpStream};
use tokio::time::{sleep, sleep_until, Duration, Instant};
use tokio_stream::wrappers::TcpListenerStream;

mod minecraft;
mod zero_scaler;
pub(crate) use zero_scaler::ZeroScaler;

async fn proxy(mut downstream: TcpStream, mut upstream: TcpStream) -> io::Result<()> {
  let (bytes_sent, bytes_received) = io::copy_bidirectional(&mut downstream, &mut upstream).await?;
  log::info!("Sent {}, received {} bytes.", bytes_sent, bytes_received);
  Ok(())
}

async fn listener_stream(port: u16) -> anyhow::Result<TcpListenerStream> {
  let listener = TcpListener::bind((Ipv4Addr::new(0, 0, 0, 0), port)).await?;
  log::info!("Listening on {}.", listener.local_addr()?);
  Ok(TcpListenerStream::new(listener))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
  env_logger::init();

  let service: String = env::var("SERVICE").expect("SERVICE is not set");
  let deployment: String = env::var("DEPLOYMENT").expect("DEPLOYMENT is not set");
  let namespace: String = env::var("NAMESPACE").expect("NAMESPACE is not set");
  let timeout: Duration = Duration::from_secs(
    env::var("TIMEOUT").map(|t| t.parse::<u64>().expect("TIMEOUT is not a number")).unwrap_or(60)
  );
  let minecraft: bool = env::var("MINECRAFT").map(|s| s.parse::<bool>().expect("MINECRAFT is not a boolean")).unwrap_or(false);
  let minecraft_favicon = env::var("MINECRAFT_FAVICON").ok();

  let client = Client::try_default().await?;
  let services: Api<Service> = Api::namespaced(client, &namespace);
  let service = services.get(&service).await?;
  let load_balancer_ip = service.status.as_ref()
    .and_then(|s| s.load_balancer.as_ref())
    .and_then(|lb| lb.ingress.as_ref())
    .and_then(|i| i.iter().find_map(|i| i.ip.as_ref()));
  let cluster_ip = service.spec.as_ref().and_then(|s| s.cluster_ip.as_ref());
  let upstream_ip = load_balancer_ip.or(cluster_ip).expect("Failed to get service IP");
  let ports = service.spec.as_ref().and_then(|s| s.ports.as_ref());

  let port = ports.and_then(|p| p.first()).expect("Failed to get service port").port as u16;

  let scaler = ZeroScaler {
    name: deployment.into(),
    namespace: namespace.into(),
  };

  let listener_stream = listener_stream(port).await?;

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
        }

        log::debug!("Idle timeout not yet reached. Next check in {} seconds.", timeout.as_secs());
        timer = sleep(timeout);
      }

      timer.await;
    }
  };

  let proxy_server = listener_stream.err_into::<anyhow::Error>().try_for_each_concurrent(None, |downstream| async {
    let connect_to_upstream_server = || async {
      log::info!("Connecting to upstream server {}:{}.", upstream_ip, port);
      TcpStream::connect((upstream_ip.as_str(), port)).await.context("Error connecting to upstream server")
    };

    let proxy = || async {
      let active_connections = Arc::clone(&active_connections);

      {
        let (ref mut connection_count, ref mut last_update) = *active_connections.write().unwrap();
        *connection_count += 1;
        log::info!("Connection count: {}", connection_count);
        *last_update = Instant::now();
      }
      let _decrease_connection_count = defer(|| {
        let (ref mut connection_count, ref mut last_update) = *active_connections.write().unwrap();
        *connection_count -= 1;
        log::info!("Connection count: {}", connection_count);
        *last_update = Instant::now();
      });

      let deployments = scaler.deployments().await;
      let deployment = if let Ok(deployments) = deployments {
        deployments.get(&scaler.name).await.ok()
      } else {
        None
      };
      let deployment_status = deployment.and_then(|d| d.status);
      let replicas = deployment_status.as_ref().and_then(|s| s.replicas).unwrap_or(0);
      let ready_replicas = deployment_status.as_ref().and_then(|s| s.ready_replicas).unwrap_or(0);
      let available_replicas = deployment_status.as_ref().and_then(|s| s.available_replicas).unwrap_or(0);
      log::info!("Replica status: {}/{} ready, {}/{} available", ready_replicas, replicas, available_replicas, replicas);

      let upstream = if available_replicas > 0 {
        Some(connect_to_upstream_server().await?)
      } else {
        None
      };

      let (downstream, mut upstream) = if minecraft {
        match minecraft::middleware(downstream, upstream, replicas, &scaler, minecraft_favicon.as_deref()).await.context("Error in Minecraft middleware")? {
          Some((downstream, upstream)) => (downstream, upstream),
          None => return Ok(()),
        }
      } else {
        if scaler.replicas().await.map(|r| r == 0).unwrap_or(false) {
          log::info!("Received request, scaling up.");
          let _ = scaler.scale_to(1).await;
        }

        (downstream, upstream)
      };

      let upstream = if let Some(upstream) = upstream.take() {
        upstream
      } else {
        connect_to_upstream_server().await?
      };

      proxy(downstream, upstream).await.context("Proxy error")?;

      Ok::<(), anyhow::Error>(())
    };

    if let Err(err) = proxy().await {
      log::error!("{}", err);
    }

    Ok(())
  });

  let (_, proxy_result) = join!(idle_checker, proxy_server);
  proxy_result?;

  Ok(())
}
