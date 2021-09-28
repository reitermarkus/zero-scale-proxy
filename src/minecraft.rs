use std::time::{SystemTime, UNIX_EPOCH};

use crate::ZeroScaler;

use anyhow::anyhow;
use minecraft_protocol::Decoder;
use minecraft_protocol::Encoder;
use minecraft_protocol::chat::Payload;
use minecraft_protocol::chat::Message;
use minecraft_protocol::handshake::ServerBoundHandshake;
use minecraft_protocol::login::LoginServerBoundPacket;
use minecraft_protocol::login::LoginDisconnect;
use minecraft_protocol::packet::Packet;
use minecraft_protocol::status::StatusServerBoundPacket;
use minecraft_protocol::status::StatusResponse;
use minecraft_protocol::status::ServerVersion;
use minecraft_protocol::status::PingResponse;
use minecraft_protocol::status::OnlinePlayers;
use minecraft_protocol::status::ServerStatus;

use tokio::net::TcpStream;

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

pub async fn middleware(downstream: TcpStream, upstream: Option<TcpStream>, replicas: i32, scaler: &ZeroScaler, favicon: Option<&str>) -> anyhow::Result<Option<(TcpStream, Option<TcpStream>)>> {
  let mut downstream_std = downstream.into_std()?;
  downstream_std.set_nonblocking(false)?;

  let mut upstream_std = if let Some(upstream) = upstream {
    let upstream_std = upstream.into_std()?;
    upstream_std.set_nonblocking(false)?;
    Some(upstream_std)
  } else {
    None
  };

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

        let handshake = ServerBoundHandshake::decode(&mut packet.data.as_slice())
          .map_err(|e| anyhow!("Error decoding handshake packet: {:?}", e))?;
        state = handshake.next_state;
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

          if upstream_std.is_none() {
            log::info!("Received login request, scaling up.");
            let scaling = scaler.scale_to(1);

            login_response()
              .encode(&mut downstream_std)
              .map_err(|e| anyhow!("Error sending login response: {:?}", e))?;

            if let Err(err) = scaling.await {
              log::error!("Scaling failed: {}", err);
            }
          }

          break
        },
        _ => break,
      },
      _ => break,
    }
  }

  downstream_std.set_nonblocking(true)?;
  let downstream = TcpStream::from_std(downstream_std)?;

  let upstream = if let Some(upstream_std) = upstream_std.take() {
    upstream_std.set_nonblocking(true)?;
    Some(TcpStream::from_std(upstream_std)?)
  } else {
    None
  };

  Ok(Some((downstream, upstream)))
}
