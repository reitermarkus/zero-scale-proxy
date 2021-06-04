use std::env;
use std::io::Cursor;
use std::io::Read;
use std::net::Ipv4Addr;
use std::sync::{Arc, RwLock};

use kube::{Api, api::{ListParams, Patch, PatchParams, WatchEvent}, Client};
use k8s_openapi::api::autoscaling::v1::Scale;
use k8s_openapi::api::autoscaling::v1::ScaleSpec;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::core::v1::Service;
use tokio::io;
use tokio::join;
use tokio::net::{TcpListener, TcpStream};
use tokio::time::{sleep, sleep_until, Duration, Instant};
use tokio_stream::wrappers::TcpListenerStream;
use futures::prelude::*;

async fn proxy(mut downstream: TcpStream, mut upstream: TcpStream) -> io::Result<()> {
  let (bytes_sent, bytes_received) = io::copy_bidirectional(&mut downstream, &mut upstream).await?;
  eprintln!("Sent {}, received {} bytes.", bytes_sent, bytes_received);
  Ok(())
}

struct ZeroScaler {
  name: String,
  namespace: String,
}

impl ZeroScaler {
  async fn deployments(&self) -> Result<Api<Deployment>, kube::Error> {
    let client = Client::try_default().await?;
    Ok(Api::namespaced(client, &self.namespace))
  }

  async fn scale(&self) -> Result<Scale, kube::Error> {
    self.deployments().await?.get_scale(&self.name).await
  }

  async fn replicas(&self) -> Result<i32, kube::Error> {
    let scale = self.scale().await?;
    Ok(scale.spec.and_then(|spec| spec.replicas).unwrap_or(0))
  }

  async fn scale_to(&self, replicas: i32) -> Result<(), kube::Error> {
    let patch_params = PatchParams {
      // dry_run: true,
      force: true,
      field_manager: Some("zero-scale-proxy".into()),
      ..Default::default()
    };
    let patch = Scale {
      metadata: ObjectMeta { name: Some(self.name.to_owned()), ..Default::default() },
      spec: Some(ScaleSpec {
        replicas: Some(replicas),
        ..Default::default()
      }),
      ..Default::default()
    };
    self.deployments().await?.patch_scale(&self.name, &patch_params, &Patch::Apply(patch)).await?;

    let lp = ListParams::default()
      .fields(&format!("metadata.name={}", self.name));

    let mut stream = self.deployments().await?.watch(&lp, "0").await?.boxed();

    while let Some(status) = stream.try_next().await.unwrap() {
      match status {
        WatchEvent::Added(s) | WatchEvent::Modified(s) => {
          if s.status.and_then(|s| s.available_replicas).unwrap_or(0) == replicas {
            break
          }
        },
        _ => {},
      }
    }

    Ok(())
  }
}

#[tokio::main]
async fn main() -> io::Result<()> {
  let service: String = env::var("SERVICE").expect("SERVICE is not set");
  let deployment: String = env::var("DEPLOYMENT").expect("DEPLOYMENT is not set");
  let namespace: String = env::var("NAMESPACE").expect("NAMESPACE is not set");

  let client = Client::try_default().await.unwrap();
  let services: Api<Service> = Api::namespaced(client, &namespace);
  let service = services.get(&service).await.unwrap();
  dbg!(&service);
  let load_balancer_ip = service.status.as_ref()
    .and_then(|s| s.load_balancer.as_ref())
    .and_then(|lb| lb.ingress.as_ref())
    .and_then(|i| i.iter().find_map(|i| i.ip.as_ref()));
  dbg!(&load_balancer_ip);
  let cluster_ip = service.spec.as_ref().and_then(|s| s.cluster_ip.as_ref());
  let upstream_ip = load_balancer_ip.or(cluster_ip).unwrap();
  let ports = service.spec.as_ref().and_then(|s| s.ports.as_ref());
  dbg!(&cluster_ip);
  dbg!(&ports);

  let port = ports.unwrap().first().unwrap().port as u16;

  let scaler = ZeroScaler {
    name: deployment.into(),
    namespace: namespace.into(),
  };

  let listener = TcpListener::bind((Ipv4Addr::new(0, 0, 0, 0), port)).await?;
  eprintln!("Listener: {:?}", listener);
  let listener_stream = TcpListenerStream::new(listener);

  let timeout = Duration::from_secs(30);
  let active_connections = Arc::new(RwLock::new((0, Instant::now())));

  let scale_down_timeout = || async {
    let active_connections = Arc::clone(&active_connections);

    loop {
      let (connection_count, last_update) = *active_connections.read().unwrap();

      let deadline = last_update + timeout;
      let mut timer = sleep_until(deadline);
      if deadline < Instant::now() {
        if connection_count == 0 && scaler.replicas().await.map(|r| r > 0).unwrap_or(false) {
          eprintln!("Reached idle timeout. Scaling down.");
          let _ = scaler.scale_to(0).await;
        }

        timer = sleep(timeout);
      }

      timer.await;
    }
  };

  let timeout_checker = scale_down_timeout();

  let proxy_server = listener_stream.try_for_each_concurrent(None, |downstream| async {
    let active_connections = Arc::clone(&active_connections);

    {
      let (ref mut connection_count, ref mut last_update) = *active_connections.write().unwrap();
      *connection_count += 1;
      *last_update = Instant::now();
    }

    let minecraft = true;


    let available_replicas = scaler.deployments().await.unwrap().get(&scaler.name).await.unwrap().status
      .and_then(|s| s.available_replicas).unwrap_or(0);
    dbg!(available_replicas);


    let downstream = if minecraft && available_replicas == 0 {
      let mut downstream_std = downstream.into_std()?;
      downstream_std.set_nonblocking(false)?;

      let mut proxy_state = "idle";


      let mut state = 0;
      let mut protocol_version = 0;

      use minecraft_protocol::Decoder;
      use minecraft_protocol::Encoder;
      use minecraft_protocol::packet::Packet;
      use minecraft_protocol::chat::{Message, Payload};

      while let Ok(mut packet) = Packet::decode(&mut downstream_std) {
        dbg!(&state);

        match state {
          0 if packet.id == 0 => {
            match minecraft_protocol::handshake::ServerBoundHandshake::decode(&mut packet.data.as_slice()) {
              Ok(handshake) => {
                dbg!(&handshake);

                state = handshake.next_state;
                protocol_version = handshake.protocol_version;
              },
              _ => break,
            }
          },
          1 => {
            use minecraft_protocol::status::*;

            match StatusServerBoundPacket::decode(packet.id as u8, &mut packet.data.as_slice()) {
              Ok(StatusServerBoundPacket::StatusRequest) => {
                let version = ServerVersion {
                    name: String::from("unknown"),
                    protocol: protocol_version as u32,
                };

                let players = OnlinePlayers {
                    online: 0,
                    max: 0,
                    sample: vec![],
                };

                let server_status = ServerStatus {
                    version,
                    description: Message::new(Payload::text(&format!("Markuscraft ({})", proxy_state))),
                    players,
                };

                let status_response = StatusResponse { server_status };
                dbg!(&status_response);

                let mut data = Vec::new();
                status_response.encode(&mut data).unwrap();

                let response = Packet {
                  id: packet.id,
                  data,
                };

                response.encode(&mut downstream_std);
                break
              },
              Ok(StatusServerBoundPacket::PingRequest(..)) => {
                use std::time::{SystemTime, UNIX_EPOCH};

                let time = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis();
                let mut data = Vec::new();
                PingResponse { time: time as u64 }.encode(&mut data).unwrap();

                let response = Packet {
                  id: packet.id,
                  data,
                };

                response.encode(&mut downstream_std);
                break
              }
              _ => break,
            }
         },
          2 => {
            use minecraft_protocol::login::*;

            match LoginServerBoundPacket::decode(packet.id as u8, &mut packet.data.as_slice()) {
              Ok(LoginServerBoundPacket::LoginStart(login_request)) => {
                dbg!(login_request);

                let scaling = scaler.scale_to(1);

                packet.data.clear();
                LoginDisconnect {
                  reason: Message::new(Payload::text("Server is starting, hang on…")),
                }.encode(&mut packet.data);

                packet.encode(&mut downstream_std);


                scaling.await;

                break
              },
              _ => break,
            }
          },
          _ => break,
        }
      }

      return Ok(());

      downstream_std.set_nonblocking(true)?;
      TcpStream::from_std(downstream_std)?
    } else {
      if scaler.replicas().await.map(|r| r == 0).unwrap_or(false) {
        eprintln!("Received request, scaling up.");
        let _ = scaler.scale_to(1).await;
      }

      downstream
    };

    eprintln!("Connecting to upstream server {}:{} …", upstream_ip, port);
    loop {
      match TcpStream::connect((upstream_ip.as_str(), port)).await {
        Ok(upstream) => {
          if let Err(err) = proxy(downstream, upstream).await {
            eprintln!("Proxy error: {}", err);
          }
          break
        },
        Err(err) => {
          eprintln!("Error connecting to upstream server: {}", err);
          eprintln!("Retrying…");
          continue
        },
      }
    }

    {
      let (ref mut connection_count, ref mut last_update) = *active_connections.write().unwrap();
      *connection_count -= 1;
      *last_update = Instant::now();
    }

    Ok(())
  });

  let (_, proxy_result) = join!(timeout_checker, proxy_server);
  proxy_result?;

  Ok(())
}
