use std::env;
use std::net::Ipv4Addr;
use std::sync::{Arc, RwLock};

use kube::{Api, api::{ListParams, Patch, PatchParams, WatchEvent}, Client};
use k8s_openapi::api::autoscaling::v1::Scale;
use k8s_openapi::api::autoscaling::v1::ScaleSpec;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use k8s_openapi::api::apps::v1::Deployment;
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
  let port: u16 = env::var("PORT").expect("PORT is not set").parse().unwrap();
  let target: String = env::var("TARGET").expect("TARGET is not set");
  let target_port: u16 = env::var("TARGET_PORT").expect("PORT is not set").parse().unwrap();
  let target_name: String = env::var("TARGET_NAME").expect("TARGET_NAME is not set");
  let target_namespace: String = env::var("TARGET_NAMESPACE").expect("TARGET_NAMESPACE is not set");

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

    if scaler.replicas().await.map(|r| r == 0).unwrap_or(false) {
      eprintln!("Received request, scaling up.");
      let _ = scaler.scale_to(1).await;
    }

    eprintln!("Connecting to upstream server …");
    loop {
      match TcpStream::connect((target.as_str(), target_port)).await {
        Ok(upstream) => {
          let _ = proxy(downstream, upstream).await;
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
