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
    name: env::var("SERVER_NAME").unwrap_or_else(|_| "Counter-Strike: Global Offensive".into()),
    map: state.to_owned(),
    folder: "csgo".into(),
    game: "Counter-Strike: Global Offensive".into(),
    app_id: 730,
    players: 0,
    max_players: 20,
    bots: 0,
    server_type: ServerType::Dedicated,
    server_os: ServerOS::Linux,
    visibility: false,
    vac: false,
    the_ship: None,
    version: "".into(),
    edf: 0x80 | (0x10 * 0) | 0x20 | 0x01 | (0x40 * 0),
    extended_server_info: ExtendedServerInfo {
      port: Some(27015),
      steam_id: None,
      keywords: Some("empty,secure".into()),
      game_id: Some(730),
    },
    source_tv: None,
  }
}

pub fn rules_response(_description: &str) -> Vec<Rule> {
  vec![]
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
