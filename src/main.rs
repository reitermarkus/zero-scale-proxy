use std::env;
use std::io::Cursor;
use std::io::Read;
use std::net::Ipv4Addr;
use std::sync::{Arc, RwLock};

use defer::defer;
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
  let timeout: Duration = Duration::from_secs(
    env::var("TIMEOUT").map(|t| t.parse::<u64>().expect("TIMEOUT is not a number")).unwrap_or(60)
  );

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

  let active_connections = Arc::new(RwLock::new((0, Instant::now())));

  let timeout_checker = async {
    let active_connections = Arc::clone(&active_connections);

    loop {
      let (connection_count, last_update) = *active_connections.read().unwrap();

      let deadline = last_update + timeout;
      let mut timer = sleep_until(deadline);
      if deadline < Instant::now() {
        if connection_count == 0 && scaler.replicas().await.map(|r| r > 0).unwrap_or(false) {
          eprintln!("Reached idle timeout. Scaling down.");

          if let Err(err) = scaler.scale_to(0).await {
            eprintln!("Error scaling down: {}", err);
          }
        }

        timer = sleep(timeout);
      }

      timer.await;
    }
  };

  let proxy_server = listener_stream.try_for_each_concurrent(None, |downstream| async {
    let active_connections = Arc::clone(&active_connections);

    {
      let (ref mut connection_count, ref mut last_update) = *active_connections.write().unwrap();
      *connection_count += 1;
      *last_update = Instant::now();
    }
    let _decrease_connection_count = defer(|| {
      let (ref mut connection_count, ref mut last_update) = *active_connections.write().unwrap();
      *connection_count -= 1;
      *last_update = Instant::now();
    });

    let minecraft = true;

    let deployments = scaler.deployments().await;
    let deployment = if let Ok(deployments) = deployments {
      deployments.get(&scaler.name).await.ok()
    } else {
      None
    };
    let deployment_status = deployment.and_then(|d| d.status);
    let replicas = deployment_status.as_ref().and_then(|s| s.replicas).unwrap_or(0);
    let ready_replicas = deployment_status.as_ref().and_then(|s| s.ready_replicas).unwrap_or(0);
    let available_replicas = deployment_status.as_ref().and_then(|s| s.available_replicas).unwrap_or(0);
    eprintln!("Replica status: {}/{} ready, {}/{} available", ready_replicas, replicas, available_replicas, replicas);
    let (connection_count, _) = *active_connections.read().unwrap();
    eprintln!("Connection count: {}", connection_count);

    let downstream = if minecraft && available_replicas == 0 {
      let mut downstream_std = downstream.into_std()?;
      downstream_std.set_nonblocking(false)?;

      use minecraft_protocol::Decoder;
      use minecraft_protocol::Encoder;
      use minecraft_protocol::packet::Packet;
      use minecraft_protocol::chat::{Message, Payload};
      use minecraft_protocol::status::*;
      use minecraft_protocol::login::*;

      let mut state = 0;
      let mut protocol_version = 0;
      while let Ok(mut packet) = Packet::decode(&mut downstream_std) {
        match state {
          0 if packet.id == 0 => {
            match minecraft_protocol::handshake::ServerBoundHandshake::decode(&mut packet.data.as_slice()) {
              Ok(handshake) => {
                state = handshake.next_state;
                protocol_version = handshake.protocol_version;
              },
              _ => break,
            }
          },
          1 => {
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

                let message = if replicas == 1 {
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
            match LoginServerBoundPacket::decode(packet.id as u8, &mut packet.data.as_slice()) {
              Ok(LoginServerBoundPacket::LoginStart(_)) => {
                let scaling = scaler.scale_to(1);

                packet.data.clear();
                LoginDisconnect {
                  reason: Message::new(Payload::text("Server is starting, hang on…")),
                }.encode(&mut packet.data);

                packet.encode(&mut downstream_std);

                if let Err(err) = scaling.await {
                  eprintln!("Scaling failed: {}", err);
                }

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

    Ok(())
  });

  let (_, proxy_result) = join!(timeout_checker, proxy_server);
  proxy_result?;

  Ok(())
}
