use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, RwLock};

use anyhow::Context;
use tokio::net::UdpSocket;
use tokio::sync::mpsc::{self, UnboundedSender};
use tokio::time::Instant;

use crate::ZeroScaler;
use super::{register_connection, scale_up};

async fn listener(port: u16) -> anyhow::Result<Arc<UdpSocket>> {
  let downstream = UdpSocket::bind((Ipv4Addr::new(0, 0, 0, 0), port)).await?;
  log::info!("Listening on {}/udp.", downstream.local_addr()?);
  Ok(Arc::new(downstream))
}

pub async fn udp_proxy(host: impl AsRef<str>, port: u16, active_connections: Arc<RwLock<(usize, Instant)>>, scaler: &ZeroScaler, proxy_type: Option<String>) -> anyhow::Result<()> {
  let host = host.as_ref();
  let upstream = format!("{}:{}", host, port);

  let downstream_recv = listener(port).await?;

  let mut senders = HashMap::<SocketAddr, UnboundedSender<Vec<u8>>>::new();

  loop {
    // Clean up cached senders whose receiver is gone.
    senders.retain(|downstream_addr, sender| {
      if sender.is_closed() {
        log::debug!("Removing cached {} sender for {}.", upstream, downstream_addr);
        false
      } else {
        true
      }
    });

    log::debug!("Cached senders for {}: {}", upstream, senders.len());

    match proxy_type.as_deref() {
      Some("7d2d") => {
        todo!()
      },
      _ => scale_up(scaler).await,
    }

    let mut buf = vec![0; 64 * 1024];
    let (size, downstream_addr) = match downstream_recv.recv_from(&mut buf).await.context("Error receiving from downstream") {
      Ok(ok) => ok,
      Err(err) => {
        log::error!("UDP recv_from failed: {}", err);
        continue
      },
    };
    buf.truncate(size);
    let _defer_guard = register_connection(active_connections.clone(), downstream_addr);
    let _replicas = scaler.replica_status().await;

    let upstream_addr = (host.to_owned(), port);
    let downstream_send = Arc::clone(&downstream_recv);

    let make_sender = || {
      let (sender, mut receiver) = mpsc::unbounded_channel::<Vec<u8>>();

      tokio::spawn(async move {
        let upstream = Arc::new(UdpSocket::bind((Ipv4Addr::new(0, 0, 0, 0), 0)).await?);
        upstream.connect(&upstream_addr).await?;

        let upstream_send = Arc::clone(&upstream);
        let upstream_recv = Arc::clone(&upstream);

        // Forward from downstream to upstream.
        let forwarder = async move {
          while let Some(buf) = receiver.recv().await {
            upstream_send.send(&buf).await.context("Error sending to upstream")?;
          }

          Ok::<(), anyhow::Error>(())
        };

        // Backward from upstream to downstream.
        let backwarder = async move {
          let mut buf = vec![0; 64 * 1024];

          loop {
            let (size, _) = upstream_recv.recv_from(&mut buf).await.context("Error receiving from upstream")?;
            downstream_send.send_to(&buf[..size], downstream_addr).await.context("Error sending to downstream")?;
          }

          #[allow(unused)]
          Ok::<(), anyhow::Error>(())
        };

        tokio::select! {
          res = forwarder => if let Err(err) = res {
            log::error!("Forwarder failed: {}", err);
          },
          res = backwarder => if let Err(err) = res {
            log::error!("Backwarder failed: {:?}", err);
          },
        }

        Ok::<(), anyhow::Error>(())
      });

      sender
    };

    let sender = senders.entry(downstream_addr).or_insert_with(|| {
      log::debug!("Creating {} sender for {}.", upstream, downstream_addr);
      make_sender()
    });
    sender.send(buf)?;
  }
}
