use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;

use anyhow::Context;
use futures::TryFutureExt;
use tokio::net::UdpSocket;
use tokio::sync::mpsc::{self, UnboundedSender, UnboundedReceiver};
use tokio::time::{Duration, timeout};

use crate::{IdleChecker, ZeroScaler};
use super::{middleware, scale_up};

async fn listener(port: u16) -> anyhow::Result<Arc<UdpSocket>> {
  let downstream = UdpSocket::bind((Ipv4Addr::new(0, 0, 0, 0), port)).await?;
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
  timeout_duration: Duration
) {
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
  host: impl AsRef<str>,
  port: u16,
  idle_checker: Arc<IdleChecker>,
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
    let idle_checker = idle_checker.clone();

    let make_sender = || {
      let (sender, mut receiver) = mpsc::unbounded_channel::<Vec<u8>>();

      tokio::spawn(async move {
        let socket = UdpSocket::bind((Ipv4Addr::new(0, 0, 0, 0), 0)).and_then(|socket| async {
          socket.connect(&upstream_addr).await?;
          Ok(socket)
        }).await;

        let upstream = match socket {
          Ok(socket) => Arc::new(socket),
          Err(err) => {
            log::error!("{}", err);
            return
          },
        };
        let upstream_send = Arc::clone(&upstream);
        let upstream_recv = Arc::clone(&upstream);

        match proxy_type.as_deref() {
          Some("7d2d") => {
            let middleware_res = timeout(
              timeout_duration,
              middleware::sdtd::udp(
                &mut receiver,
                downstream_send.clone(),
                downstream_addr,
                upstream_recv.clone(),
                upstream_send.clone(),
                scaler.clone()
              )
            ).await;

            if matches!(middleware_res, Ok(true) | Err(_)) {
              return
            }
          },
          _ => scale_up(scaler.as_ref()).await,
        }

        let _defer_guard = idle_checker.register_connection(downstream_addr);
        proxy(receiver, downstream_send, downstream_addr, upstream_recv, upstream_send, timeout_duration).await
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
