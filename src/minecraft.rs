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
  let packet = Packet::decode(source).map_err(|e| anyhow!("{:?}", e))?;
  packet.encode(destination).map_err(|e| anyhow!("{:?}", e))?;
  return Ok(packet);
}

fn status_response(replicas: i32) -> Packet {
  let version = ServerVersion {
      name: String::from("idle"),
      protocol: 0,
  };

  let players = OnlinePlayers {
      online: 0,
      max: 0,
      sample: vec![],
  };

  let message = if replicas > 0 {
    "Server is starting."
  } else {
    "Server is currently idle."
  };

  let server_status = ServerStatus {
      version,
      description: Message::new(Payload::text(message)),
      players,
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

pub async fn middleware(downstream: TcpStream, upstream: Option<TcpStream>, replicas: i32, scaler: &ZeroScaler) -> anyhow::Result<Option<(TcpStream, Option<TcpStream>)>> {
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
    let packet = if let Some(ref mut upstream_std) = upstream_std {
      proxy_packet(&mut downstream_std, upstream_std)?
    } else {
      Packet::decode(&mut downstream_std).map_err(|e| anyhow!("{:?}", e))?
    };

    match state {
      0 => {
        let handshake = ServerBoundHandshake::decode(&mut packet.data.as_slice()).map_err(|e| anyhow!("{:?}", e))?;
        state = handshake.next_state;
      },
      1 => match StatusServerBoundPacket::decode(packet.id as u8, &mut packet.data.as_slice()).map_err(|e| anyhow!("{:?}", e))? {
        StatusServerBoundPacket::StatusRequest => {
          if upstream_std.is_some() {
            break
          } else {
            status_response(replicas)
              .encode(&mut downstream_std)
              .map_err(|e| anyhow!("{:?}", e))?;
            return Ok(None)
          }
        },
        StatusServerBoundPacket::PingRequest(..) => {
          ping_response()
            .encode(&mut downstream_std)
            .map_err(|e| anyhow!("{:?}", e))?;
          return Ok(None)
        }
      },
      2 => match LoginServerBoundPacket::decode(packet.id as u8, &mut packet.data.as_slice()).map_err(|e| anyhow!("{:?}", e))? {
        LoginServerBoundPacket::LoginStart(_) => {
          if upstream_std.is_none() {
            log::info!("Received login request, scaling up.");
            let scaling = scaler.scale_to(1);

            login_response()
              .encode(&mut downstream_std)
              .map_err(|e| anyhow!("{:?}", e))?;

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
