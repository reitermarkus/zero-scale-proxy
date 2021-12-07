use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::anyhow;
use futures::future::{self, Either};
use minecraft_protocol::decoder::Decoder;
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

use crate::{ZeroScaler, ActiveConnection};

fn proxy_packet(source: &mut std::net::TcpStream, destination: &mut std::net::TcpStream) -> anyhow::Result<Packet> {
  log::trace!("proxy_packet");
  let packet = Packet::decode(source).map_err(|e| anyhow!("Error decoding packet: {:?}", e))?;
  packet.encode(destination).map_err(|e| anyhow!("Error forwarding packet: {:?}", e))?;
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
    reason: Message::new(Payload::text("Server is starting, hang on…")),
  };

  let mut data = Vec::new();
  response.encode(&mut data).unwrap();

  Packet {
    id: 0x00,
    data,
  }
}

pub async fn tcp(downstream: TcpStream, upstream: Option<TcpStream>, replicas: usize, scaler: &ZeroScaler, favicon: Option<&str>) -> anyhow::Result<Option<(TcpStream, Option<TcpStream>, Option<ActiveConnection>)>> {
  let downstream_addr = downstream.peer_addr()?;

  let mut downstream_std = downstream.into_std()?;
  downstream_std.set_nonblocking(false)?;

  let mut upstream_std = if let Some(upstream) = upstream {
    let upstream_std = upstream.into_std()?;
    upstream_std.set_nonblocking(false)?;
    Some(upstream_std)
  } else {
    None
  };

  let mut active_connection = None;
  let mut state = 0;
  loop {
    log::trace!("middleware loop");
    log::trace!("state = {}", state);

    let packet = if let Some(ref mut upstream_std) = upstream_std {
      proxy_packet(&mut downstream_std, upstream_std)?
    } else {
      log::trace!("Packet::decode");
      Packet::decode(&mut downstream_std).map_err(|e| anyhow!("Error decoding packet: {:?}", e))?
    };

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

            if upstream_std.is_some() {
              break
            } else {
              let response = if replicas > 0 {
                status_response("starting", "Server is starting.", favicon)
              } else {
                status_response("idle", "Server is currently idle.", favicon)
              };

              response
                .encode(&mut downstream_std)
                .map_err(|e| anyhow!("Error sending status response: {:?}", e))?;
              return Ok(None)
            }
          },
          StatusServerBoundPacket::PingRequest(..) => {
            log::trace!("ping");

            ping_response()
              .encode(&mut downstream_std)
              .map_err(|e| anyhow!("Error sending ping response: {:?}", e))?;
            return Ok(None)
          }
        }
      },
      2 => match LoginServerBoundPacket::decode(packet.id as u8, &mut packet.data.as_slice()).map_err(|e| anyhow!("{:?}", e))? {
        LoginServerBoundPacket::LoginStart(_) => {
          log::trace!("login");

          let register_fut = scaler.register_connection(downstream_addr);

          let response_fut = if upstream_std.is_none() {
            Either::Left(async {
              login_response()
                .encode(&mut downstream_std)
                .map_err(|e| anyhow!("Error sending login response: {:?}", e))
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

  downstream_std.set_nonblocking(true)?;
  let downstream = TcpStream::from_std(downstream_std)?;

  let upstream = if let Some(upstream_std) = upstream_std {
    upstream_std.set_nonblocking(true)?;
    Some(TcpStream::from_std(upstream_std)?)
  } else {
    None
  };

  Ok(Some((downstream, upstream, active_connection)))
}
