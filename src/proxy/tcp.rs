use std::env;
use std::net::{Ipv4Addr, SocketAddr};

use anyhow::Context;
use futures::prelude::*;
use tokio::io;
use tokio::net::{TcpListener, TcpStream};
use tokio_stream::wrappers::TcpListenerStream;

use crate::{ZeroScaler, ProxyType};
use super::middleware;

async fn proxy(mut downstream: TcpStream, mut upstream: TcpStream) -> io::Result<()> {
  let (bytes_sent, bytes_received) = io::copy_bidirectional(&mut downstream, &mut upstream).await?;
  log::info!("Sent {}, received {} bytes.", bytes_sent, bytes_received);
  Ok(())
}

async fn listener_stream(host: Ipv4Addr, port: u16) -> anyhow::Result<TcpListenerStream> {
  let listener = TcpListener::bind((host, port)).await?;
  log::info!("Listening on {}/tcp.", listener.local_addr()?);
  Ok(TcpListenerStream::new(listener))
}

pub async fn tcp_proxy(host: Ipv4Addr, port: u16, scaler: &ZeroScaler, proxy_type: ProxyType) -> anyhow::Result<()> {
  let listener_stream = listener_stream(Ipv4Addr::new(0, 0, 0, 0), port).await?;

  listener_stream.err_into::<anyhow::Error>().try_for_each_concurrent(None, |downstream| async {
    let connect_to_upstream_server = || async {
      log::info!("Connecting to upstream server {}:{}.", host, port);
      TcpStream::connect(SocketAddr::new(host.into(), port)).await.context("Error connecting to upstream server")
    };

    let proxy = || async {
      let peer_addr = downstream.peer_addr()?;
      let replicas = scaler.replica_status().await;

      let upstream = if replicas.available > 0 {
        Some(connect_to_upstream_server().await?)
      } else {
        None
      };

      let (downstream, mut upstream, active_connection) = match proxy_type {
        ProxyType::Minecraft => {
          let minecraft_favicon = env::var("MINECRAFT_FAVICON").ok();

          match middleware::minecraft::tcp(downstream, upstream, replicas.wanted, scaler, minecraft_favicon.as_deref()).await.context("Error in Minecraft middleware")? {
            Some((downstream, upstream, active_connection)) => {
              (downstream, upstream, active_connection)
            },
            None => return Ok(()),
          }
        },
        _ => {
          let active_connection = scaler.register_connection(peer_addr).await;
          (downstream, upstream, Some(active_connection))
        }
      };

      let upstream = if let Some(upstream) = upstream.take() {
        upstream
      } else {
        connect_to_upstream_server().await?
      };

      match proxy(downstream, upstream).await {
        Ok(()) => (),
        Err(err) if err.kind() == io::ErrorKind::ConnectionReset => (),
        Err(err) => return Err(err.into()),
      }

      drop(active_connection);

      Ok::<(), anyhow::Error>(())
    };

    if let Err(err) = proxy().await {
      log::error!("{}", err);
    }

    Ok(())
  }).await?;

  Ok(())
}
