use std::net::SocketAddr;
use std::sync::Arc;

use a2s::info::{Info, ServerType, ServerOS, ExtendedServerInfo};
use a2s::rules::Rule;
use anyhow::Context;
use futures::TryFutureExt;
use tokio::net::UdpSocket;
use tokio::sync::mpsc::UnboundedReceiver;
use pretty_hex::PrettyHex;

use crate::{ZeroScaler, ActiveConnection};
use super::{IDLE_MSG, STARTING_MSG};

const INFO_REQUEST: [u8; 25] = [
  0xff, 0xff, 0xff, 0xff, 0x54, 0x53, 0x6f, 0x75, 0x72, 0x63, 0x65, 0x20, 0x45, 0x6e, 0x67, 0x69,
  0x6e, 0x65, 0x20, 0x51, 0x75, 0x65, 0x72, 0x79, 0x00,
];

const RULES_REQUEST: [u8; 5] = [0xFF, 0xFF, 0xFF, 0xFF, 0x56];

const LOGIN_REQUEST: [u8; 5] = [0x08, 0x07, 0x00, 0x00, 0x00];

pub fn status_response(state: &str) -> Info {
  Info {
    protocol: 0,
    name: "7 Days to Die".into(),
    map: state.to_owned(),
    folder: "7DTD".into(),
    game: "7 Days to Die".into(),
    app_id: 0,
    players: 0,
    max_players: 0,
    bots: 0,
    server_type: ServerType::Dedicated,
    server_os: ServerOS::Linux,
    visibility: true,
    vac: false,
    the_ship: None,
    version: "".into(),
    edf: 0x80 | 0x10 | 0x20 | 0x01 | (0x40 * 0),
    extended_server_info: ExtendedServerInfo {
      port: Some(26902),
      steam_id: Some(90151620823146498),
      keywords: Some("AjxBAQAIAAOB0AESQYgBpAEePAQpHh4ABAQyugMAAwMDpAGkAaQBpAEFAAgtALMB".into()),
      game_id: Some(251570),
    },
    source_tv: None,
  }
}

pub fn rules_response(description: &str) -> Vec<Rule> {
  vec![
    Rule { name: "GameHost".into(),                    value: "7 Days to Die".into() },
    Rule { name: "GameName".into(),                    value: "World".into() },
    Rule { name: "ServerDescription".into(),           value: description.to_owned() },
    Rule { name: "ServerLoginConfirmationText".into(), value: "".into() },
    Rule { name: "ServerVisibility".into(),            value: "2".into() },
    Rule { name: "SteamID".into(),                     value: "90151742714337280".into() },
  ]
}

pub async fn udp(
  receiver: &mut UnboundedReceiver<Vec<u8>>,
  downstream_send: Arc<UdpSocket>,
  downstream_addr: SocketAddr,
  upstream_recv: Arc<UdpSocket>,
  upstream_send: Arc<UdpSocket>,
  scaler: Arc<ZeroScaler>,
) -> (bool, Option<ActiveConnection>) {
  let send_buf = match receiver.recv().await {
    Some(send_buf) => send_buf,
    None => return (true, None),
  };

  let mut recv_buf = vec![0; 64 * 1024];

  let send_fut = async {
    upstream_send.send(&send_buf).await.context("Error sending to upstream")
  };

  let (control_flow, active_connection, buf) = if send_buf.get(0..INFO_REQUEST.len()) == Some(&INFO_REQUEST) {
    log::trace!("info");

    let replicas = scaler.replica_status().await;
    if replicas.wanted == 0 {
      (true, None, status_response("idle").to_bytes())
    } else {
      let recv_fut = async {
        upstream_recv.recv_from(&mut recv_buf).await.context("Error receiving from upstream")
          .map(|(size, _)| recv_buf[..size].to_vec())
      };

      match send_fut.and_then(|_| recv_fut).await {
        Ok(ok) => (true, None, ok),
        Err(_) => (true, None, status_response("starting").to_bytes()),
      }
    }
  } else if send_buf.get(0..RULES_REQUEST.len()) == Some(&RULES_REQUEST) {
    log::trace!("rules");

    let replicas = scaler.replica_status().await;
    if replicas.wanted == 0 {
      (
        true,
        None,
        Rule::vec_to_bytes(rules_response(IDLE_MSG))
      )
    } else {
      match upstream_recv.recv_from(&mut recv_buf).await {
        Ok((size, _)) => (false, None, recv_buf[..size].to_vec()),
        Err(_) => (
          true,
          None,
          Rule::vec_to_bytes(rules_response(STARTING_MSG))
        ),
      }
    }
  } else {
    if send_buf.get(0..LOGIN_REQUEST.len()) == Some(&LOGIN_REQUEST) {
      log::trace!("login");
    } else {
      log::trace!("other");

      log::debug!("unknown message type: send_buf = {:?}", send_buf.hex_dump());
    }

    let register_fut = scaler.register_connection(downstream_addr);

    let recv_fut = async move {
      upstream_recv.recv_from(&mut recv_buf).await.context("Error receiving from upstream")
        .map(|(size, _)| recv_buf[..size].to_vec())
    };

    match tokio::join!(send_fut.and_then(|_| recv_fut), register_fut) {
      (Ok(buf), active_connection) => (false, Some(active_connection), buf),
      (Err(_), _) => return (true, None),
    }
  };

  match downstream_send.send_to(&buf, downstream_addr).await.context("Error sending to downstream") {
    Ok(_) => (control_flow, active_connection),
    Err(_) => (true, None),
  }
}
