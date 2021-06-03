use std::net::Ipv4Addr;
use std::sync::{Arc, RwLock};

use kube::{Client, Api};
use kube::api::PatchParams;
use kube::api::Patch;
use k8s_openapi::api::autoscaling::v1::Scale;
use k8s_openapi::api::autoscaling::v1::ScaleSpec;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use k8s_openapi::api::apps::v1::Deployment;
use tokio::io;
use tokio::net::{TcpListener, TcpStream, UdpSocket};
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
  async fn deployments(&self) -> Api<Deployment> {
    let client = Client::try_default().await.unwrap();
    Api::namespaced(client, &self.namespace)
  }

  async fn scale(&self) -> Scale {
    self.deployments().await.get_scale(&self.name).await.unwrap()
  }

  async fn replicas(&self) -> i32 {
    let scale = self.scale().await;
    scale.spec.and_then(|spec| spec.replicas).unwrap_or(0)
  }

  async fn scale_to(&self, replicas: i32) {
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
    self.deployments().await.patch_scale(&self.name, &patch_params, &Patch::Apply(patch)).await;

    use kube::{api::{Api, ListParams, ResourceExt, WatchEvent}, Client};

    let lp = ListParams::default()
      .fields(&format!("metadata.name={}", self.name));

    let mut stream = self.deployments().await.watch(&lp, "0").await.unwrap().boxed();

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
  }
}

#[tokio::main]
async fn main() -> io::Result<()> {
  let port = 25565;
  let target = "10.0.1.23";
  let target_port = 25565;
  let target_name = "minecraft-minecraft";
  let target_namespace = "default";

  let scaler = ZeroScaler {
    name: target_name.into(),
    namespace: target_namespace.into(),
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
        if connection_count == 0 {
          if scaler.replicas().await > 0 {
            eprintln!("Reached idle timeout. Scaling down.");
            scaler.scale_to(0).await;
          }
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

    if scaler.replicas().await == 0 {
      eprintln!("Received request, scaling up.");
      scaler.scale_to(1).await;
    }

    eprintln!("Connecting to upstream server …");
    match TcpStream::connect((target, target_port)).await {
      Ok(upstream) => {
        let _ = proxy(downstream, upstream).await;
      },
      Err(err) => (),
    }

    {
      let (ref mut connection_count, ref mut last_update) = *active_connections.write().unwrap();
      *connection_count -= 1;
      *last_update = Instant::now();
    }

    Ok(())
  });

  tokio::join!(timeout_checker, proxy_server);

  Ok(())
}
