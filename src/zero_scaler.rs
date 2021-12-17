use std::net::SocketAddr;
use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};

use atomic_instant::AtomicInstant;
use futures::prelude::*;
use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::autoscaling::v1::Scale;
use k8s_openapi::api::autoscaling::v1::ScaleSpec;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use kube::{Api, api::{ListParams, Patch, PatchParams, WatchEvent}, Client};
use tokio::time::{Duration};

#[derive(Debug)]
pub struct ReplicaStatus {
  pub available: usize,
  pub ready: usize,
  pub wanted: usize,
}

#[derive(Debug)]
pub struct ActiveConnections {
  count: AtomicUsize,
  last_update: AtomicInstant,
}

impl ActiveConnections {
  pub fn new() -> Arc<Self> {
    Arc::new(Self {
      count: AtomicUsize::new(0),
      last_update: AtomicInstant::now(),
    })
  }

  pub fn count(&self) -> usize {
    self.count.load(Ordering::SeqCst)
  }

  pub fn duration_since_last_update(&self) -> Duration {
    self.last_update.elapsed()
  }

  pub fn register(self: &Arc<Self>, peer_addr: SocketAddr) -> ActiveConnection {
    ActiveConnection::new(self.clone(), peer_addr)
  }
}

#[derive(Debug)]
#[must_use]
pub struct ActiveConnection {
  peer_addr: SocketAddr,
  connections: Arc<ActiveConnections>,
}

impl ActiveConnection {
  pub fn new(connections: Arc<ActiveConnections>, peer_addr: SocketAddr) -> Self {
    let count = connections.count.fetch_add(1, Ordering::SeqCst) + 1;
    log::info!("Peer {} connected, {} connections active.", peer_addr, count);
    connections.last_update.set_now();

    Self {
      peer_addr,
      connections,
    }
  }
}

impl Drop for ActiveConnection {
  fn drop(&mut self) {
    let count = self.connections.count.fetch_sub(1, Ordering::SeqCst) - 1;
    log::info!("Peer {} disconnected, {} connections active.", self.peer_addr, count);
    self.connections.last_update.set_now();
  }
}

#[derive(Debug)]
pub struct ZeroScaler {
  pub deployment: String,
  pub namespace: String,
  pub(crate) active_connections: Arc<ActiveConnections>,
}

impl ZeroScaler {
  pub fn new(deployment: String, namespace: String) -> Self {
    Self {
      deployment,
      namespace,
      active_connections: ActiveConnections::new(),
    }
  }

  pub async fn deployments(&self) -> Result<Api<Deployment>, kube::Error> {
    let client = Client::try_default().await?;
    Ok(Api::namespaced(client, &self.namespace))
  }

  pub async fn scale(&self) -> Result<Scale, kube::Error> {
    self.deployments().await?.get_scale(&self.deployment).await
  }

  pub async fn replicas(&self) -> Result<i32, kube::Error> {
    let scale = self.scale().await?;
    Ok(scale.spec.and_then(|spec| spec.replicas).unwrap_or(0))
  }

  pub async fn replica_status(&self) -> ReplicaStatus {
    let deployments = self.deployments().await;
    let deployment = if let Ok(deployments) = deployments {
      deployments.get(&self.deployment).await.ok()
    } else {
      None
    };
    let deployment_status = deployment.and_then(|d| d.status);
    let replicas = deployment_status.as_ref().and_then(|s| s.replicas).unwrap_or(0);
    let ready_replicas = deployment_status.as_ref().and_then(|s| s.ready_replicas).unwrap_or(0);
    let available_replicas = deployment_status.as_ref().and_then(|s| s.available_replicas).unwrap_or(0);
    log::debug!("{}/{} replicas ready, {}/{} available.", ready_replicas, replicas, available_replicas, replicas);

    ReplicaStatus {
      ready: ready_replicas as usize,
      available: available_replicas as usize,
      wanted: replicas as usize,
    }
  }

  pub async fn scale_to(&self, replicas: i32) -> Result<(), kube::Error> {
    let patch_params = PatchParams {
      force: true,
      field_manager: Some("zero-scale-proxy".into()),
      ..Default::default()
    };
    let patch = Scale {
      metadata: ObjectMeta { name: Some(self.deployment.to_owned()), ..Default::default() },
      spec: Some(ScaleSpec {
        replicas: Some(replicas),
      }),
      ..Default::default()
    };
    self.deployments().await?.patch_scale(&self.deployment, &patch_params, &Patch::Apply(patch)).await?;

    let lp = ListParams::default()
      .fields(&format!("metadata.name={}", self.deployment));

    let mut stream = self.deployments().await?.watch(&lp, "0").await?.boxed();

    while let Some(event) = stream.try_next().await? {
      log::debug!("event = {:?}", event);

      match event {
        WatchEvent::Added(s) | WatchEvent::Modified(s) => {
          let status = s.status;
          log::debug!("status = {:?}", status);
          if status.and_then(|s| s.ready_replicas).unwrap_or(0) == replicas {
            break
          }
        },
        _ => {},
      }
    }

    Ok(())
  }

  async fn scale_up(&self) {
    log::trace!("scale_up");

    if self.replicas().await.map(|r| r == 0).unwrap_or(false) {
      log::info!("Received request, scaling up.");
      match self.scale_to(1).await {
        Ok(_) => log::info!("Scaled up successfully."),
        Err(err) => log::error!("Error scaling up: {}", err),
      }
    } else {
      log::debug!("Already scaled up.");
    }
  }

  pub async fn register_connection(&self, peer_addr: SocketAddr) -> ActiveConnection {
    log::trace!("register_connection");

    let active_connection = self.active_connections.register(peer_addr);

    self.scale_up().await;

    active_connection
  }
}
