use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;

use anyhow::Context;
use tokio::net::UdpSocket;
use tokio::sync::mpsc::{self, UnboundedSender, UnboundedReceiver};
use tokio::time::{Duration, timeout};

use crate::{ZeroScaler, ProxyType};
use super::middleware;

async fn listener(host: Ipv4Addr, port: u16) -> anyhow::Result<Arc<UdpSocket>> {
  let downstream = UdpSocket::bind((host, port)).await?;
  log::info!("Listening on {}/udp.", downstream.local_addr()?);
  Ok(Arc::new(downstream))
}

// Forward from downstream to upstream.
async fn forwarder(
  mut downstream_recv: UnboundedReceiver<Vec<u8>>,
  upstream_send: Arc<UdpSocket>,
  timeout_duration: Duration
) -> anyhow::Result<()> {
  loop {
    let forward = async {
      let send_buf = timeout(timeout_duration, downstream_recv.recv()).await
        .context("Timed out receiving from downstream")?;

      if let Some(send_buf) = send_buf {
        timeout(timeout_duration, upstream_send.send(&send_buf)).await
          .context("Timed out sending to upstream")?
          .context("Error sending to upstream")?;

        Ok(Some(()))
      } else {
        Ok(None)
      }
    };

    match forward.await {
      Ok(Some(())) => continue,
      Ok(None) => return Ok(()),
      Err(err) => return Err(err),
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
      let (size, _) = timeout(timeout_duration, upstream_recv.recv_from(&mut recv_buf)).await
        .context("Timed out receiving from upstream")?
        .context("Error receiving from upstream")?;

      timeout(timeout_duration, downstream_send.send_to(&recv_buf[..size], downstream_addr)).await
        .context("Timed out sending to downstream")?
        .context("Error sending to downstream")?;

      Ok(())
    };

    match backward.await {
      Ok(()) => continue,
      Err(err) => return Err(err),
    }
  }
}

async fn proxy(
  downstream_recv: UnboundedReceiver<Vec<u8>>,
  downstream_send: Arc<UdpSocket>,
  downstream_addr: SocketAddr,
  upstream_recv: Arc<UdpSocket>,
  upstream_send: Arc<UdpSocket>,
  timeout_duration: Duration,
  transparent: bool
) {
  if transparent {
    if let Err(err) = forwarder(downstream_recv, upstream_send, timeout_duration).await {
      log::error!("Forwarder failed: {}", err);
    }

    return
  }

  tokio::select! {
    res = forwarder(downstream_recv, upstream_send, timeout_duration) => if let Err(err) = res {
      log::error!("Forwarder failed: {}", err);
    },
    res = backwarder(upstream_recv, downstream_send, downstream_addr, timeout_duration) => if let Err(err) = res {
      log::error!("Backwarder failed: {}", err);
    },
  }
}

pub async fn udp_proxy(
  host: Ipv4Addr,
  port: u16,
  scaler: Arc<ZeroScaler>,
  proxy_type: ProxyType,
  timeout_duration: Duration
) -> anyhow::Result<()> {
  let upstream = format!("{}:{}", host, port);

  let downstream_recv = listener(Ipv4Addr::new(0, 0, 0, 0), port).await?;

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

    let make_sender = || {
      let (sender, mut receiver) = mpsc::unbounded_channel::<Vec<u8>>();

      tokio::spawn(async move {
        #[allow(unused_assignments, unused_mut)]
        let mut transparent = false;

        #[cfg(not(target_os = "linux"))]
        let socket = UdpSocket::bind((Ipv4Addr::new(0, 0, 0, 0), 0)).await;

        #[cfg(target_os = "linux")]
        let socket = {
          // https://www.kernel.org/doc/html/v5.12/networking/tproxy.html
          transparent = std::env::var("TRANSPARENT_IP").ok().map(|s| s == "true").unwrap_or(false);

          if transparent {
            use socket2::{Domain, Socket, Type};
            let socket = Socket::new(Domain::IPV4, Type::DGRAM.nonblocking().cloexec(), None).and_then(|s| {
              s.set_reuse_address(true)?;
              s.set_reuse_port(true)?;
              s.set_ip_transparent(true)?;
              s.bind(&downstream_addr.into())?;
              Ok(s)
            });

            match socket {
              Ok(socket) => UdpSocket::from_std(socket.into()),
              Err(err) => Err(err.into()),
            }
          } else {
            UdpSocket::bind((Ipv4Addr::new(0, 0, 0, 0), 0)).await
          }
        };

        let socket = match socket {
          Ok(socket) => socket.connect(&upstream_addr).await.map(|_| socket),
          Err(err) => Err(err),
        };

        let upstream = match socket {
          Ok(socket) => {
            if let Ok(local_addr) = socket.local_addr() {
              log::info!("Bound UDP socket on {}.", local_addr);
            }

            Arc::new(socket)
          },
          Err(err) => {
            log::error!("{}", err);
            return
          },
        };
        let upstream_send = Arc::clone(&upstream);
        let upstream_recv = Arc::clone(&upstream);

        let active_connection = match proxy_type {
          ProxyType::Csgo => {
            let middleware_res = timeout(
              timeout_duration,
              middleware::csgo::udp(
                &mut receiver,
                downstream_send.clone(),
                downstream_addr,
                upstream_recv.clone(),
                upstream_send.clone(),
                scaler.clone(),
                transparent
              )
            ).await;

            match middleware_res {
              Ok((true, _)) | Err(_) => return,
              Ok((false, active_connection)) => active_connection,
            }
          },
          ProxyType::Sdtd => {
            let middleware_res = timeout(
              timeout_duration,
              middleware::sdtd::udp(
                &mut receiver,
                downstream_send.clone(),
                downstream_addr,
                upstream_recv.clone(),
                upstream_send.clone(),
                scaler.clone(),
                transparent
              )
            ).await;

            match middleware_res {
              Ok((true, _)) | Err(_) => return,
              Ok((false, active_connection)) => active_connection,
            }
          },
          ProxyType::TeamSpeak => {
            Some(scaler.register_connection(downstream_addr).await)
          },
          _ => {
            Some(scaler.register_connection(downstream_addr).await)
          },
        };

        proxy(receiver, downstream_send, downstream_addr, upstream_recv, upstream_send, timeout_duration, transparent).await;
        drop(active_connection)
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
