use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::anyhow;
use futures::future::{self, Either};
use minecraft_protocol::error::DecodeError;
use minecraft_protocol::encoder::Encoder;
use minecraft_protocol::data::chat::Payload;
use minecraft_protocol::data::chat::Message;
use minecraft_protocol::data::server_status::OnlinePlayers;
use minecraft_protocol::data::server_status::ServerVersion;
use minecraft_protocol::data::server_status::ServerStatus;
use minecraft_protocol::packet::Packet;
use minecraft_protocol::version::v1_14_4::handshake::HandshakeServerBoundPacket;
use minecraft_protocol::version::v1_14_4::login::LoginServerBoundPacket;
use minecraft_protocol::version::v1_14_4::login::LoginDisconnect;
use minecraft_protocol::version::v1_14_4::status::StatusServerBoundPacket;
use minecraft_protocol::version::v1_14_4::status::StatusResponse;
use minecraft_protocol::version::v1_14_4::status::PingResponse;
use tokio::net::TcpStream;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::{ZeroScaler, ActiveConnection};
use super::{IDLE_MSG, STARTING_MSG};

async fn read_packet(buf: &mut Vec<u8>, source: &mut TcpStream, compression: bool) -> anyhow::Result<Packet> {
  let mut total_bytes_needed = 1;

  loop {
    while buf.len() < total_bytes_needed {
      source.take((total_bytes_needed - buf.len()) as u64).read_to_end(buf).await?;
    }

    match Packet::decode(&mut buf.as_slice(), compression) {
      Ok(packet) => return Ok(packet),
      Err(DecodeError::Incomplete { bytes_needed }) => {
        total_bytes_needed += bytes_needed;
      },
      err => return err.map_err(|e| anyhow!("Error forwarding packet: {:?}", e))
    }
  }
}

async fn proxy_packet(buf: &mut Vec<u8>, source: &mut TcpStream, destination: &mut TcpStream, compression: bool) -> anyhow::Result<Packet> {
  log::trace!("proxy_packet");
  let packet = read_packet(buf, source, compression).await?;
  destination.write_all(buf).await.map_err(|e| anyhow!("Error forwarding packet: {:?}", e))?;
  Ok(packet)
}

fn status_response(state: &str, message: &str, favicon: Option<&str>) -> Packet {
  let version = ServerVersion {
    name: String::from(state),
    protocol: 0,
  };

  let players = OnlinePlayers {
    online: 0,
    max: 0,
    sample: vec![],
  };

  let favicon = favicon.map(|data| format!("data:image/png;base64,{}", data));

  let server_status = ServerStatus {
    version,
    description: Message::new(Payload::text(message)),
    players,
    favicon,
  };

  let status_response = StatusResponse { server_status };

  let mut data = Vec::new();
  status_response.encode(&mut data).unwrap();

  Packet {
    id: 0x00,
    data,
  }
}

fn ping_response() -> Packet {
  let response = PingResponse {
    time: SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as u64
  };

  let mut data = Vec::new();
  response.encode(&mut data).unwrap();

  Packet {
    id: 0x01,
    data,
  }
}

fn login_response() -> Packet {
  let response = LoginDisconnect {
    reason: Message::new(Payload::text(STARTING_MSG)),
  };

  let mut data = Vec::new();
  response.encode(&mut data).unwrap();

  Packet {
    id: 0x00,
    data,
  }
}

pub async fn tcp(mut downstream: TcpStream, mut upstream: Option<TcpStream>, replicas: usize, scaler: &ZeroScaler, favicon: Option<&str>) -> anyhow::Result<Option<(TcpStream, Option<TcpStream>, Option<ActiveConnection>)>> {
  let downstream_addr = downstream.peer_addr()?;

  let mut active_connection = None;
  let mut state = 0;
  let compression = false;

  let mut buf = Vec::new();
  loop {
    log::trace!("middleware loop");
    log::trace!("state = {}", state);

    buf.clear();
    let packet = if let Some(ref mut upstream) = upstream {
      proxy_packet(&mut buf, &mut downstream, upstream, compression).await?
    } else {
      log::trace!("Packet::decode");
      read_packet(&mut buf, &mut downstream, compression).await?
    };
    buf.clear();

    match state {
      0 => {
        log::trace!("handshake");

        let handshake = HandshakeServerBoundPacket::decode(packet.id as u8, &mut packet.data.as_slice())
          .map_err(|e| anyhow!("Error decoding handshake packet: {:?}", e))?;

        match handshake {
          HandshakeServerBoundPacket::Handshake(handshake) => {
            state = handshake.next_state;
          }
        }
      },
      1 => {
        let status_packet = StatusServerBoundPacket::decode(packet.id as u8, &mut packet.data.as_slice())
          .map_err(|e| anyhow!("Error decoding status packet: {:?}", e))?;

        match status_packet {
          StatusServerBoundPacket::StatusRequest => {
            log::trace!("status");

            if upstream.is_some() {
              break
            } else {
              let response = if replicas > 0 {
                status_response("starting", STARTING_MSG, favicon)
              } else {
                status_response("idle", IDLE_MSG, favicon)
              };

              response
                .encode(&mut buf, compression.then(|| 0))
                .map_err(|e| anyhow!("Error sending status response: {:?}", e))?;
              downstream.write_all(&buf).await?;
              return Ok(None)
            }
          },
          StatusServerBoundPacket::PingRequest(..) => {
            log::trace!("ping");

            if upstream.is_none() {
              ping_response()
                .encode(&mut buf, compression.then(|| 0))
                .map_err(|e| anyhow!("Error sending ping response: {:?}", e))?;
              downstream.write_all(&buf).await?;
            }
            return Ok(None)
          }
        }
      },
      2 => match LoginServerBoundPacket::decode(packet.id as u8, &mut packet.data.as_slice()).map_err(|e| anyhow!("{:?}", e))? {
        LoginServerBoundPacket::LoginStart(_) => {
          log::trace!("login");

          let register_fut = scaler.register_connection(downstream_addr);

          let response_fut = if upstream.is_none() {
            Either::Left(async {
              login_response()
                .encode(&mut buf, compression.then(|| 0))
                .map_err(|e| anyhow!("Error sending login response: {:?}", e))?;
              downstream.write_all(&buf).await.map_err(|e| anyhow!("{:?}", e))
            })
          } else {
            Either::Right(future::ok(()))
          };

          let (connection, res) = tokio::join!(register_fut, response_fut);
          res?;

          active_connection = Some(connection);
          break
        },
        _ => break,
      },
      _ => break,
    }
  }

  Ok(Some((downstream, upstream, active_connection)))
}
