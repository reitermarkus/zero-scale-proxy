use std::env;
use std::sync::{Arc, RwLock};
use std::net::Ipv4Addr;

use anyhow::Context;
use futures::prelude::*;
use tokio::io;
use tokio::net::{TcpListener, TcpStream};
use tokio::time::Instant;
use tokio_stream::wrappers::TcpListenerStream;

use crate::ZeroScaler;
use crate::minecraft;
use super::{register_connection, scale_up};

async fn proxy(mut downstream: TcpStream, mut upstream: TcpStream) -> io::Result<()> {
  let (bytes_sent, bytes_received) = io::copy_bidirectional(&mut downstream, &mut upstream).await?;
  log::info!("Sent {}, received {} bytes.", bytes_sent, bytes_received);
  Ok(())
}

async fn listener_stream(port: u16) -> anyhow::Result<TcpListenerStream> {
  let listener = TcpListener::bind((Ipv4Addr::new(0, 0, 0, 0), port)).await?;
  log::info!("Listening on {}/tcp.", listener.local_addr()?);
  Ok(TcpListenerStream::new(listener))
}

pub async fn tcp_proxy(host: impl AsRef<str>, port: u16, active_connections: Arc<RwLock<(usize, Instant)>>, scaler: &ZeroScaler, proxy_type: Option<String>) -> anyhow::Result<()> {
  let listener_stream = listener_stream(port).await?;

  listener_stream.err_into::<anyhow::Error>().try_for_each_concurrent(None, |downstream| async {
    let connect_to_upstream_server = || async {
      log::info!("Connecting to upstream server {}:{}.", host.as_ref(), port);
      TcpStream::connect((host.as_ref(), port)).await.context("Error connecting to upstream server")
    };

    let proxy = || async {
      let active_connections = Arc::clone(&active_connections);

      let peer_addr = downstream.peer_addr()?;
      let _defer_guard = register_connection(active_connections, peer_addr);

      let replicas = scaler.replica_status().await;

      let upstream = if replicas.available > 0 {
        Some(connect_to_upstream_server().await?)
      } else {
        None
      };

      let (downstream, mut upstream) = match proxy_type.as_deref() {
        Some("minecraft") => {
          let minecraft_favicon = env::var("MINECRAFT_FAVICON").ok();

          match minecraft::middleware(downstream, upstream, replicas.wanted, scaler, minecraft_favicon.as_deref()).await.context("Error in Minecraft middleware")? {
            Some((downstream, upstream)) => (downstream, upstream),
            None => return Ok(()),
          }
        },
        _ => {
          scale_up(scaler).await;
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
