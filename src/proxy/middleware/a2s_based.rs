use std::{io::Cursor, net::SocketAddr, sync::Arc};

use a2s::{info::Info, players::Player, rules::Rule};
use anyhow::Context;
use futures::TryFutureExt;
use log::{log_enabled, Level::Debug};
use pretty_hex::PrettyHex;
use tokio::{net::UdpSocket, sync::mpsc::UnboundedReceiver};

use super::{IDLE_MSG, STARTING_MSG};
use crate::{ActiveConnection, ZeroScaler};

const INFO_REQUEST: [u8; 25] = [
  0xff, 0xff, 0xff, 0xff, b'T', b'S', b'o', b'u', b'r', b'c', b'e', b' ', b'E', b'n', b'g', b'i', b'n', b'e', b' ',
  b'Q', b'u', b'e', b'r', b'y', 0x00,
];
const PLAYER_REQUEST: [u8; 5] = [0xff, 0xff, 0xff, 0xff, b'U'];
const RULES_REQUEST: [u8; 5] = [0xff, 0xff, 0xff, 0xff, b'V'];

const SDTD_LOGIN_REQUEST: [u8; 5] = [0x08, 0x07, 0x00, 0x00, 0x00];
const CSGO_CONNECT_REQUEST: [u8; 12] = [0xff, 0xff, 0xff, 0xff, 0x71, 0x63, 0x6f, 0x6e, 0x6e, 0x65, 0x63, 0x74];
const CSGO_LAN_SEARCH_REQUEST: [u8; 33] = [
  0xff, 0xff, 0xff, 0xff, 0x00, 0x00, 0x00, 0x00, 0xf5, 0x35, 0x00, 0x00, 0x0d, 0x00, 0x00, 0x00, 0x00, b'L', b'a',
  b'n', b'S', b'e', b'a', b'r', b'c', b'h', 0x00, 0x0b, 0x0b, 0x00, 0x00, 0x00, 0x00,
];

pub async fn udp(
  receiver: &mut UnboundedReceiver<Vec<u8>>,
  downstream_send: Arc<UdpSocket>,
  downstream_addr: SocketAddr,
  upstream_recv: Arc<UdpSocket>,
  upstream_send: Arc<UdpSocket>,
  scaler: Arc<ZeroScaler>,
  status_response: impl FnOnce(&str) -> Info,
  rules_response: impl FnOnce(&str) -> Vec<Rule>,
  transparent: bool,
) -> (bool, Option<ActiveConnection>) {
  let send_buf = match receiver.recv().await {
    Some(send_buf) => send_buf,
    None => return (true, None),
  };

  let send_fut = async { upstream_send.send(&send_buf).await.context("Error sending to upstream") };
  let recv_fut = async move {
    let mut recv_buf = vec![0; 64 * 1024];

    if transparent {
      Ok(recv_buf[..4].to_vec())
    } else {
      upstream_recv
        .recv_from(&mut recv_buf)
        .await
        .context("Error receiving from upstream")
        .map(|(size, _)| recv_buf[..size].to_vec())
    }
  };

  let (control_flow, active_connection, buf) = if send_buf.get(0..INFO_REQUEST.len()) == Some(&INFO_REQUEST) {
    log::trace!("info");

    let replicas = scaler.replica_status().await;
    if replicas.wanted == 0 {
      (true, None, status_response("idle").to_bytes())
    } else {
      match send_fut.and_then(|_| recv_fut).await {
        Ok(ok) => {
          if log_enabled!(Debug) && !transparent {
            let info = Info::from_cursor(Cursor::new(ok[4..].to_vec()));
            log::debug!("INFO response: {:#?}", info);
          }

          (true, None, ok)
        },
        Err(_) => (true, None, status_response("starting").to_bytes()),
      }
    }
  } else if send_buf.get(0..RULES_REQUEST.len()) == Some(&RULES_REQUEST) {
    log::trace!("rules");

    let replicas = scaler.replica_status().await;
    if replicas.wanted == 0 {
      (true, None, Rule::vec_to_bytes(rules_response(IDLE_MSG)))
    } else if replicas.ready == 0 {
      (true, None, Rule::vec_to_bytes(rules_response(STARTING_MSG)))
    } else {
      match send_fut.and_then(|_| recv_fut).await {
        Ok(ok) => {
          if log_enabled!(Debug) && !transparent {
            let info = Rule::from_cursor(Cursor::new(ok[4..].to_vec()));
            log::debug!("RULES response: {:#?}", info);
          }

          (false, None, ok)
        },
        Err(err) => (true, None, Rule::vec_to_bytes(rules_response(&format!("Server Error: {}", err)))),
      }
    }
  } else if send_buf.get(0..PLAYER_REQUEST.len()) == Some(&PLAYER_REQUEST) {
    log::trace!("player");

    match send_fut.and_then(|_| recv_fut).await {
      Ok(ok) => {
        if log_enabled!(Debug) && !transparent {
          let app_id = 0; // TODO
          let info = Player::from_cursor(Cursor::new(ok[4..].to_vec()), app_id);
          log::debug!("PLAYER response: {:#?}", info);
        }

        (true, None, ok)
      },
      Err(_) => return (true, None),
    }
  } else if send_buf.get(0..CSGO_LAN_SEARCH_REQUEST.len()) == Some(&CSGO_LAN_SEARCH_REQUEST) {
    log::trace!("lan_search");

    match send_fut.and_then(|_| recv_fut).await {
      Ok(ok) => {
        if log_enabled!(Debug) && !transparent {
          log::debug!("LAN_SEARCH response: {:#?}", ok.hex_dump());
        }

        (true, None, ok)
      },
      Err(_) => return (true, None),
    }
  } else {
    if send_buf.get(0..SDTD_LOGIN_REQUEST.len()) == Some(&SDTD_LOGIN_REQUEST) {
      log::trace!("login");
    } else if send_buf.get(0..CSGO_CONNECT_REQUEST.len()) == Some(&CSGO_CONNECT_REQUEST) {
      log::trace!("connect");
    } else {
      log::trace!("other");

      log::debug!("unknown message type: send_buf = {:?}", send_buf.hex_dump());
    }

    let register_fut = scaler.register_connection(downstream_addr);

    match tokio::join!(send_fut.and_then(|_| recv_fut), register_fut) {
      (Ok(buf), active_connection) => (false, Some(active_connection), buf),
      (Err(_), _) => return (true, None),
    }
  };

  if transparent {
    return (control_flow, active_connection)
  }

  match downstream_send.send_to(&buf, downstream_addr).await.context("Error sending to downstream") {
    Ok(_) => (control_flow, active_connection),
    Err(_) => (true, None),
  }
}
