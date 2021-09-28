use futures::prelude::*;
use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::autoscaling::v1::Scale;
use k8s_openapi::api::autoscaling::v1::ScaleSpec;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use kube::{Api, api::{ListParams, Patch, PatchParams, WatchEvent}, Client};

pub struct ReplicaStatus {
  pub available: usize,
  pub ready: usize,
  pub wanted: usize,
}

pub struct ZeroScaler {
  pub name: String,
  pub namespace: String,
}

impl ZeroScaler {
  pub async fn deployments(&self) -> Result<Api<Deployment>, kube::Error> {
    let client = Client::try_default().await?;
    Ok(Api::namespaced(client, &self.namespace))
  }

  pub async fn scale(&self) -> Result<Scale, kube::Error> {
    self.deployments().await?.get_scale(&self.name).await
  }

  pub async fn replicas(&self) -> Result<i32, kube::Error> {
    let scale = self.scale().await?;
    Ok(scale.spec.and_then(|spec| spec.replicas).unwrap_or(0))
  }

  pub async fn replica_status(&self) -> ReplicaStatus {
    let deployments = self.deployments().await;
    let deployment = if let Ok(deployments) = deployments {
      deployments.get(&self.name).await.ok()
    } else {
      None
    };
    let deployment_status = deployment.and_then(|d| d.status);
    let replicas = deployment_status.as_ref().and_then(|s| s.replicas).unwrap_or(0);
    let ready_replicas = deployment_status.as_ref().and_then(|s| s.ready_replicas).unwrap_or(0);
    let available_replicas = deployment_status.as_ref().and_then(|s| s.available_replicas).unwrap_or(0);
    log::info!("{}/{} replicas ready, {}/{} available.", ready_replicas, replicas, available_replicas, replicas);

    ReplicaStatus {
      ready: ready_replicas as usize,
      available: available_replicas as usize,
      wanted: replicas as usize,
    }
  }

  pub async fn scale_to(&self, replicas: i32) -> Result<(), kube::Error> {
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
      }),
      ..Default::default()
    };
    self.deployments().await?.patch_scale(&self.name, &patch_params, &Patch::Apply(patch)).await?;

    let lp = ListParams::default()
      .fields(&format!("metadata.name={}", self.name));

    let mut stream = self.deployments().await?.watch(&lp, "0").await?.boxed();

    while let Ok(Some(status)) = stream.try_next().await {
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
