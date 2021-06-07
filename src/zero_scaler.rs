use futures::prelude::*;
use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::autoscaling::v1::Scale;
use k8s_openapi::api::autoscaling::v1::ScaleSpec;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use kube::{Api, api::{ListParams, Patch, PatchParams, WatchEvent}, Client};

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
        ..Default::default()
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
