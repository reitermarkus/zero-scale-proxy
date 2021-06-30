use std::env;
use std::net::Ipv4Addr;
use std::net::SocketAddr;
use std::sync::{Arc, RwLock};
use std::collections::HashMap;

use anyhow::Context;
use defer::defer;
use futures::prelude::*;
use futures::future::Either;
use futures::future::try_join_all;
use kube::{Api, Client};
use k8s_openapi::api::core::v1::Service;
use tokio::io;
use tokio::join;
use tokio::sync::mpsc::{self, UnboundedSender};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::time::{sleep, sleep_until, Duration, Instant};
use tokio_stream::wrappers::TcpListenerStream;

mod minecraft;
mod sd2d;
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

async fn tcp_proxy(host: &str, port: u16, active_connections: Arc<RwLock<(usize, Instant)>>, scaler: &ZeroScaler, proxy_type: Option<&str>) -> anyhow::Result<()> {
  let listener_stream = listener_stream(port).await?;

  listener_stream.err_into::<anyhow::Error>().try_for_each_concurrent(None, |downstream| async {
    let connect_to_upstream_server = || async {
      log::info!("Connecting to upstream server {}:{}.", host, port);
      TcpStream::connect((host, port)).await.context("Error connecting to upstream server")
    };

    let proxy = || async {
      let active_connections = Arc::clone(&active_connections);

      let peer_addr = downstream.peer_addr()?;

      {
        let (ref mut connection_count, ref mut last_update) = *active_connections.write().unwrap();
        *connection_count += 1;
        log::info!("New connection: {}", peer_addr);
        log::info!("Connection count: {}", connection_count);
        *last_update = Instant::now();
      }
      let _decrease_connection_count = defer(|| {
        let (ref mut connection_count, ref mut last_update) = *active_connections.write().unwrap();
        *connection_count -= 1;
        log::info!("Connection ended: {}", peer_addr);
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

      let (downstream, mut upstream) = match proxy_type {
        Some("minecraft") => {
          let minecraft_favicon = env::var("MINECRAFT_FAVICON").ok();

          match minecraft::middleware(downstream, upstream, replicas, &scaler, minecraft_favicon.as_deref()).await.context("Error in Minecraft middleware")? {
            Some((downstream, upstream)) => (downstream, upstream),
            None => return Ok(()),
          }
        },
        _ => {
          if scaler.replicas().await.map(|r| r == 0).unwrap_or(false) {
            log::info!("Received request, scaling up.");
            let _ = scaler.scale_to(1).await;
          }

          (downstream, upstream)
        }
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
  }).await?;

  Ok(())
}

async fn udp_proxy(host: &str, port: u16) -> anyhow::Result<()> {
  let upstream_addr = (host, port);

  let downstream = UdpSocket::bind((Ipv4Addr::new(0, 0, 0, 0), port)).await?;
  println!("Listening on: {}/udp", downstream.local_addr()?);
  let downstream_recv = Arc::new(downstream);

  let mut senders = HashMap::<SocketAddr, UnboundedSender<Vec<u8>>>::new();

  loop {
    let mut buf = vec![0; 64 * 1024];
    let (size, downstream_addr) = match downstream_recv.recv_from(&mut buf).await {
      Ok(ok) => ok,
      Err(err) => {
        log::error!("UDP recv_from failed: {}", err);
        continue
      },
    };
    buf.truncate(size);

    let (host, port) = upstream_addr;
    let upstream_addr = (host.to_owned(), port);
    let downstream_send = Arc::clone(&downstream_recv);

    let make_sender = || {
      let (sender, mut receiver) = mpsc::unbounded_channel::<Vec<u8>>();

      tokio::spawn(async move {
        let upstream = Arc::new(UdpSocket::bind((Ipv4Addr::new(0, 0, 0, 0), 0)).await?);
        upstream.connect(&upstream_addr).await?;

        let upstream_send = Arc::clone(&upstream);
        let upstream_recv = Arc::clone(&upstream);

        let forwarder = async move {
          loop {
            match receiver.recv().await {
              Some(buf) => {
                upstream_send.send(&buf).await?;
              }
              None => break,
            }
          }

          #[allow(unreachable_code)]
          Ok::<(), anyhow::Error>(())
        };

        let backwarder = async move {
          let mut buf = vec![0; 64 * 1024];

          loop {
            let (size, _) = upstream_recv.recv_from(&mut buf).await?;
            downstream_send.send_to(&buf[..size], downstream_addr).await?;
          }

          #[allow(unreachable_code)]
          Ok::<(), anyhow::Error>(())
        };

        tokio::select! {
          res = forwarder => res?,
          res = backwarder => res?,
        };

        #[allow(unreachable_code)]
        Ok::<(), anyhow::Error>(())
      });

      sender
    };

    // Recreate sender if the receiver is gone.
    if senders.get(&downstream_addr).map(|s| s.is_closed()).unwrap_or(false) {
      senders.remove(&downstream_addr);
    }

    let sender = senders.entry(downstream_addr).or_insert_with(make_sender);
    sender.send(buf)?;
  }
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
  let proxy_type = env::var("PROXY_TYPE").ok();

  let (upstream_ip, ports) = if let Some((ip, ports)) = env::var("UPSTREAM_IP").ok().zip(env::var("PORTS").ok()) {
    let ports = ports.split(",").flat_map(|port| {
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
    }).collect::<Vec<_>>();

    (ip, ports)
  } else {
    let client = Client::try_default().await?;
    let services: Api<Service> = Api::namespaced(client, &namespace);
    let service = services.get(&service).await?;

    let load_balancer_ip = service.status.as_ref()
      .and_then(|s| s.load_balancer.as_ref())
      .and_then(|lb| lb.ingress.as_ref())
      .and_then(|i| i.iter().find_map(|i| i.ip.as_ref()));
    let cluster_ip = service.spec.as_ref().and_then(|s| s.cluster_ip.as_ref());

    let ports = service.spec.as_ref().and_then(|s| {
      s.ports.as_ref().map(|ports| ports.iter().map(|port| {
        (port.port as u16, port.protocol.as_ref().map(|p| p.to_lowercase()).unwrap_or("tcp".into()))
      }).collect())
    }).unwrap_or_default();

    (
      load_balancer_ip.or(cluster_ip).expect("Failed to get service IP").to_owned(),
      ports
    )
  };

  let scaler = ZeroScaler {
    name: deployment.into(),
    namespace: namespace.into(),
  };

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

  let proxies = try_join_all(ports.into_iter().map(|(port, protocol)| match protocol.as_ref() {
    "tcp" => {
      Either::Left(tcp_proxy(&upstream_ip, port, active_connections.clone(), &scaler, proxy_type.as_deref()))
    },
    "udp" => {
      let upstream_ip = &upstream_ip;
      Either::Right(async move {
        if let Err(err) = udp_proxy(upstream_ip, port).await {
          log::error!("UDP proxy error: {:?}", err)
        }

        Ok(())
      })
    },
    _ => unreachable!(),
  }));

  let (_, proxies_result) = join!(idle_checker, proxies);
  proxies_result?;

  Ok(())
}
