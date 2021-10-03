use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, RwLock};

use anyhow::Context;
use pretty_hex::PrettyHex;
use tokio::net::UdpSocket;
use tokio::sync::mpsc::{self, UnboundedSender, UnboundedReceiver};
use tokio::time::{Instant, Duration, timeout};

use crate::ZeroScaler;
use super::{register_connection, scale_up};
use crate::sdtd;

async fn listener(port: u16) -> anyhow::Result<Arc<UdpSocket>> {
  let downstream = UdpSocket::bind((Ipv4Addr::new(0, 0, 0, 0), port)).await?;
  log::info!("Listening on {}/udp.", downstream.local_addr()?);
  Ok(Arc::new(downstream))
}

const INFO_REQUEST: [u8; 25] = [
  0xff, 0xff, 0xff, 0xff, 0x54, 0x53, 0x6f, 0x75, 0x72, 0x63, 0x65, 0x20, 0x45, 0x6e, 0x67, 0x69,
  0x6e, 0x65, 0x20, 0x51, 0x75, 0x65, 0x72, 0x79, 0x00,
];

const RULES_REQUEST: [u8; 5] = [0xFF, 0xFF, 0xFF, 0xFF, 0x56];


// Forward from downstream to upstream.
async fn forwarder(
  mut downstream_recv: UnboundedReceiver<Vec<u8>>,
  upstream_send: Arc<UdpSocket>,
  timeout_duration: Duration
) -> anyhow::Result<()> {
  loop {
    let forward = async {
      if let Some(send_buf) = downstream_recv.recv().await {
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
}

// Backward from upstream to downstream.
async fn backwarder(
  upstream_recv: Arc<UdpSocket>,
  downstream_send: Arc<UdpSocket>,
  downstream_addr: SocketAddr,
  timeout_duration: Duration
) -> anyhow::Result<()> {
  let mut recv_buf = vec![0; 64 * 1024];
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
}

async fn proxy(
  downstream_recv: UnboundedReceiver<Vec<u8>>,
  downstream_send: Arc<UdpSocket>,
  downstream_addr: SocketAddr,
  upstream_recv: Arc<UdpSocket>,
  upstream_send: Arc<UdpSocket>,
  timeout_duration: Duration
) {
  tokio::select! {
    res = forwarder(downstream_recv, upstream_send, timeout_duration) => if let Err(err) = res {
      log::error!("Forwarder failed: {}", err);
    },
    res = backwarder(upstream_recv, downstream_send, downstream_addr, timeout_duration) => if let Err(err) = res {
      log::error!("Backwarder failed: {:?}", err);
    },
  }
}

pub async fn udp_proxy(
  host: impl AsRef<str>,
  port: u16,
  active_connections: Arc<RwLock<(usize, Instant)>>,
  scaler: Arc<ZeroScaler>,
  proxy_type: Option<String>,
  timeout_duration: Duration
) -> anyhow::Result<()> {
  let host = host.as_ref();
  let upstream = format!("{}:{}", host, port);

  let downstream_recv = listener(port).await?;

  let mut senders = HashMap::<SocketAddr, UnboundedSender<Vec<u8>>>::new();

  loop {
    let mut buf = vec![0; 64 * 1024];
    let (size, downstream_addr) = match downstream_recv.recv_from(&mut buf).await.context("Error receiving from downstream") {
      Ok(ok) => ok,
      Err(err) => {
        log::error!("UDP recv_from failed: {}", err);
        continue
      },
    };
    buf.truncate(size);

    // log::debug!("Cached senders for {}: {}", upstream, senders.len());

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

        let mut recv_buf = vec![0; 64 * 1024];
        match proxy_type.as_deref() {
          Some("7d2d") => {
            loop {
              let should_continue = timeout(timeout_duration, async {
                if let Some(send_buf) = receiver.recv().await {
                  let send_res = upstream_send.send(&send_buf).await.context("Error sending to upstream");

                  log::info!("send_buf {}: {:?}", port, send_buf.hex_dump());

                  let (should_return, buf) = if send_buf.get(0..INFO_REQUEST.len()) == Some(&INFO_REQUEST) {
                    log::trace!("INFO_REQUEST");
                    let replicas = scaler.replica_status().await;
                    if replicas.wanted == 0 {
                      (true, sdtd::status_response("idle").to_bytes())
                    } else {
                      send_res?;
                      match upstream_recv.recv_from(&mut recv_buf).await {
                        Ok((size, _)) => (false, recv_buf[..size].to_vec()),
                        Err(_) => (true, sdtd::status_response("starting").to_bytes()),
                      }
                    }
                  } else {
                    use a2s::rules::Rule;

                    if send_buf.get(0..RULES_REQUEST.len()) == Some(&RULES_REQUEST) {
                      log::trace!("RULES_REQUEST");
                      let replicas = scaler.replica_status().await;
                      if replicas.wanted == 0 {
                        (true, Rule::vec_to_bytes(sdtd::rules_response("Server is currently idle, connect to scale up.")))
                      } else {
                        match upstream_recv.recv_from(&mut recv_buf).await {
                          Ok((size, _)) => {
                            use std::io::Cursor;
                            let rules = Rule::from_cursor(Cursor::new(recv_buf[4..size].to_vec()));
                            dbg!(rules);

                            (false, recv_buf[..size].to_vec())
                          },
                          Err(err) => (true, Rule::vec_to_bytes(sdtd::rules_response(&format!("Server is starting, hang on. ({})", err)))),
                        }
                      }
                    } else {
                      log::trace!("OTHER_REQUEST");

                      scale_up(scaler.as_ref()).await;

                      let (size, _) = upstream_recv.recv_from(&mut recv_buf).await.context("Error receiving from upstream")?;
                      log::info!("recv_buf {}: {:?}", port, (&recv_buf[..size]).hex_dump());
                      (false, recv_buf[..size].to_vec())
                    }
                  };

                  downstream_send.send_to(&buf, downstream_addr)
                    .await.context("Error sending to downstream")?;


                  Ok(should_return)
                } else {
                  Ok::<_, anyhow::Error>(true)
                }
              }).await?;

              log::trace!("should_continue = {:?}", should_continue);
              if !should_continue? {
                break
              }
            }
          },
          _ => scale_up(scaler.as_ref()).await,
        }

        Ok::<_, anyhow::Error>(proxy(receiver, downstream_send, downstream_addr, upstream_recv, upstream_send, timeout_duration).await)
      });

      sender
    };

    // Clean up cached senders whose receiver is gone.
    senders.retain(|downstream_addr, s| {
      if s.is_closed() {
        log::debug!("Removing cached {} sender for {}.", upstream, downstream_addr);
        false
      } else {
        true
      }
    });

    let sender = senders.entry(downstream_addr).or_insert_with(|| {
      log::debug!("Creating {} sender for {}.", upstream, downstream_addr);
      make_sender()
    });
    sender.send(buf)?;
  }
}
