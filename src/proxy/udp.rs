use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, RwLock};

use anyhow::Context;
use lru_time_cache::LruCache;
use pretty_hex::PrettyHex;
use tokio::io::{self, Interest};
use tokio::net::UdpSocket;
use tokio::sync::mpsc::{self, UnboundedSender, error::TryRecvError};
use tokio::time::{Instant, Duration, timeout};

use crate::ZeroScaler;
use super::{register_connection, scale_up};
use crate::sd2d;

async fn listener(port: u16) -> anyhow::Result<Arc<UdpSocket>> {
  let downstream = UdpSocket::bind((Ipv4Addr::new(0, 0, 0, 0), port)).await?;
  log::info!("Listening on {}/udp.", downstream.local_addr()?);
  Ok(Arc::new(downstream))
}

const INFO_REQUEST: [u8; 25] = [
  0xff, 0xff, 0xff, 0xff, 0x54, 0x53, 0x6f, 0x75, 0x72, 0x63, 0x65, 0x20, 0x45, 0x6e, 0x67, 0x69,
  0x6e, 0x65, 0x20, 0x51, 0x75, 0x65, 0x72, 0x79, 0x00,
];

pub async fn udp_proxy(host: impl AsRef<str>, port: u16, active_connections: Arc<RwLock<(usize, Instant)>>, scaler: Arc<ZeroScaler>, proxy_type: Option<String>, timeout_duration: Duration) -> anyhow::Result<()> {
  let host = host.as_ref();
  let upstream = format!("{}:{}", host, port);

  let downstream_recv = listener(port).await?;

  let mut senders = LruCache::<SocketAddr, UnboundedSender<Vec<u8>>>::with_expiry_duration(timeout_duration);

  loop {
    // Clean up cached senders whose receiver is gone.
    let mut closed_senders = vec![];
    for (downstream_addr, sender) in senders.peek_iter() {
      if sender.is_closed() {
        closed_senders.push(downstream_addr.clone());
      }
    }
    for downstream_addr in closed_senders {
      log::debug!("Removing cached {} sender for {}.", upstream, downstream_addr);
      senders.remove(&downstream_addr);
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

    log::debug!("Cached senders for {}: {}", upstream, senders.len());

    let upstream_addr = (host.to_owned(), port);
    let downstream_send = Arc::clone(&downstream_recv);
    let scaler = scaler.clone();
    let proxy_type = proxy_type.clone();
    let active_connections = active_connections.clone();

    let make_sender = || {
      let (sender, mut receiver) = mpsc::unbounded_channel::<Vec<u8>>();

      tokio::spawn(async move {
        let _defer_guard = register_connection(active_connections.clone(), downstream_addr);

        let upstream = Arc::new(UdpSocket::bind((Ipv4Addr::new(0, 0, 0, 0), 0)).await?);
        upstream.connect(&upstream_addr).await?;

        let upstream_send = Arc::clone(&upstream);
        let upstream_recv = Arc::clone(&upstream);

        let replicas = scaler.replica_status().await;

        let mut recv_buf = vec![0; 64 * 1024];
        match proxy_type.as_deref() {
          Some("7d2d") => {
            if replicas.available == 0 || true {
              let base_port = 26900;

              if port == base_port || port == base_port + 2 {
                if let Some(send_buf) = receiver.recv().await {
                  upstream_send.send(&send_buf).await.context("Error sending to upstream")?;

                  let buf = if send_buf == INFO_REQUEST {
                    let info = if replicas.wanted > 0 {
                      sd2d::status_response("starting")
                    } else {
                      sd2d::status_response("idle")
                    };
                    info.to_bytes()
                  } else {
                    log::info!("send_buf {}: {:?}", port, send_buf.hex_dump());

                    let (size, _) = upstream_recv.recv_from(&mut recv_buf).await.context("Error receiving from upstream")?;
                    log::info!("recv_buf {}: {:?}", port, (&recv_buf[..size]).hex_dump());

                    recv_buf[..size].to_vec()
                  };

                  timeout(timeout_duration, downstream_send.send_to(&buf, downstream_addr))
                    .await.context("Error sending to downstream")??;
                }
              }

              scale_up(scaler.as_ref()).await;
            }
          },
          _ => scale_up(scaler.as_ref()).await,
        }

        // Forward from downstream to upstream.
        let forwarder = async move {
          loop {
            let forward = async {
              if let Some(send_buf) = receiver.recv().await {
                Some(upstream_send.send(&send_buf)
                  .await.context("Error sending to upstream"))
              } else {
                None
              }
            };

            match timeout(timeout_duration, forward).await {
              Ok(Some(Err(err))) => return Err(err),
              Ok(None) => return Ok(()),
              Ok(_) => (),
              Err(_) => return Ok(()),
            }
          }

          Ok::<(), anyhow::Error>(())
        };

        // Backward from upstream to downstream.
        let backwarder = async move {
          loop {
            let backward = async {
              let (size, _) = upstream_recv.recv_from(&mut recv_buf)
                .await.context("Error receiving from upstream")?;

              downstream_send.send_to(&recv_buf[..size], downstream_addr)
                .await.context("Error sending to downstream")?;

              Ok::<(), anyhow::Error>(())
            };

            match timeout(timeout_duration, backward).await {
              Ok(Err(err)) => return Err(err),
              Ok(_) => (),
              Err(_) => return Ok(()),
            }
          }

          #[allow(unused)]
          Ok::<(), anyhow::Error>(())
        };

        tokio::select! {
          res = forwarder => if let Err(err) = res {
            log::error!("Forwarder for port {} failed: {}", port, err);
          },
          res = backwarder => if let Err(err) = res {
            log::error!("Backwarder for port {} failed: {:?}", port, err);
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
