use std::{env, net::SocketAddr, sync::Arc};

use a2s::{
  info::{ExtendedServerInfo, Info, ServerOS, ServerType},
  rules::Rule,
};
use tokio::{net::UdpSocket, sync::mpsc::UnboundedReceiver};

use super::a2s_based;
use crate::{ActiveConnection, ZeroScaler};

pub fn status_response(state: &str) -> Info {
  Info {
    protocol: 0,
    name: env::var("SERVER_NAME").unwrap_or_else(|_| "7 Days to Die".into()),
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
    Rule { name: "GameHost".into(), value: "7 Days to Die".into() },
    Rule { name: "GameName".into(), value: "World".into() },
    Rule { name: "ServerDescription".into(), value: description.to_owned() },
    Rule { name: "ServerLoginConfirmationText".into(), value: "".into() },
    Rule { name: "ServerVisibility".into(), value: "2".into() },
    Rule { name: "SteamID".into(), value: "90151742714337280".into() },
  ]
}

pub async fn udp(
  receiver: &mut UnboundedReceiver<Vec<u8>>,
  downstream_send: Arc<UdpSocket>,
  downstream_addr: SocketAddr,
  upstream_recv: Arc<UdpSocket>,
  upstream_send: Arc<UdpSocket>,
  scaler: Arc<ZeroScaler>,
  transparent: bool,
) -> (bool, Option<ActiveConnection>) {
  a2s_based::udp(
    receiver,
    downstream_send,
    downstream_addr,
    upstream_recv,
    upstream_send,
    scaler,
    status_response,
    rules_response,
    transparent,
  )
  .await
}
