//! Kubernetes client setup and a thin uniform layer over ConfigMap/Secret,
//! mirroring upstream client.py (config loading, SKIP_TLS_VERIFY, the
//! "Config for cluster api at '<url>' loaded." debug line) and the parts of
//! urllib3's transport-retry behaviour the sidecar relies on.

use std::collections::BTreeMap;
use std::time::Duration;

use futures::stream::{BoxStream, StreamExt};
use k8s_openapi::api::core::v1::{ConfigMap, Secret};
use kube::api::{Api, ListParams, WatchEvent, WatchParams};
use kube::{Client, Config};

use crate::logger;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Kind {
    ConfigMap,
    Secret,
}

impl Kind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Kind::ConfigMap => "configmap",
            Kind::Secret => "secret",
        }
    }
}

/// A ConfigMap or Secret reduced to what the sync logic needs. Secret `data`
/// arrives base64-decoded from the API deserializer, which matches upstream's
/// explicit b64decode of the string form.
#[derive(Clone)]
pub struct Item {
    pub namespace: String,
    pub name: String,
    pub resource_version: String,
    pub annotations: BTreeMap<String, String>,
    pub text: Option<BTreeMap<String, String>>,
    pub binary: Option<BTreeMap<String, Vec<u8>>>,
}

impl Item {
    pub fn key(&self) -> String {
        // Upstream concatenates with no separator; kept for fidelity.
        format!("{}{}", self.namespace, self.name)
    }

    fn from_cm(cm: ConfigMap) -> Item {
        Item {
            namespace: cm.metadata.namespace.unwrap_or_default(),
            name: cm.metadata.name.unwrap_or_default(),
            resource_version: cm.metadata.resource_version.unwrap_or_default(),
            annotations: cm.metadata.annotations.unwrap_or_default(),
            text: cm.data,
            binary: cm
                .binary_data
                .map(|m| m.into_iter().map(|(k, v)| (k, v.0)).collect()),
        }
    }

    fn from_secret(sec: Secret) -> Item {
        Item {
            namespace: sec.metadata.namespace.unwrap_or_default(),
            name: sec.metadata.name.unwrap_or_default(),
            resource_version: sec.metadata.resource_version.unwrap_or_default(),
            annotations: sec.metadata.annotations.unwrap_or_default(),
            text: None,
            binary: sec
                .data
                .map(|m| m.into_iter().map(|(k, v)| (k, v.0)).collect()),
        }
    }
}

fn kubeconfig_path() -> Option<String> {
    let path = std::env::var("KUBECONFIG")
        .unwrap_or_else(|_| format!("{}/.kube/config", std::env::var("HOME").unwrap_or_default()));
    if std::path::Path::new(&path).exists() {
        Some(path)
    } else {
        None
    }
}

/// Mirror of _initialize_kubeclient_configuration().
pub async fn init_client() -> Result<Client, String> {
    let mut config = match kubeconfig_path() {
        Some(path) => {
            logger::info(&format!("Loading config from '{}'...", path));
            let kc = kube::config::Kubeconfig::read_from(&path).map_err(|e| {
                format!(
                    "Unexpected error during Kubernetes client initialization: {}",
                    e
                )
            })?;
            Config::from_custom_kubeconfig(kc, &kube::config::KubeConfigOptions::default())
                .await
                .map_err(|e| {
                    format!(
                        "Unexpected error during Kubernetes client initialization: {}",
                        e
                    )
                })?
        }
        None => {
            logger::info("Loading incluster config...");
            Config::incluster().map_err(|e| {
                format!(
                    "Unexpected error during Kubernetes client initialization: {}",
                    e
                )
            })?
        }
    };

    if std::env::var("SKIP_TLS_VERIFY").as_deref() == Ok("true") {
        config.accept_invalid_certs = true;
    }

    // Upstream relaxes OpenSSL's VERIFY_X509_STRICT for legacy CAs here.
    // rustls has no equivalent flag, so this is accepted as a no-op with the
    // same warning line (documented deviation; see NOTES.md).
    if std::env::var("DISABLE_X509_STRICT_VERIFICATION")
        .map(|v| v.to_lowercase() == "true")
        .unwrap_or(false)
    {
        logger::warning("Disabling strict X.509 certificate verification");
    }

    // WATCH_CLIENT_TIMEOUT is the read-idle timeout that lets a dead watch be
    // noticed; it must stay above WATCH_SERVER_TIMEOUT.
    let client_timeout: u64 = std::env::var("WATCH_CLIENT_TIMEOUT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(66);
    config.read_timeout = Some(Duration::from_secs(client_timeout));

    let host = config.cluster_url.to_string();
    let host = host.trim_end_matches('/');
    logger::debug(&format!("Config for cluster api at '{}' loaded.", host));

    Client::try_from(config).map_err(|e| {
        format!(
            "Unexpected error during Kubernetes client initialization: {}",
            e
        )
    })
}

fn is_transport_error(e: &kube::Error) -> bool {
    !matches!(e, kube::Error::Api(_))
}

/// Retry transport-level failures the way urllib3's Retry (pushed into the
/// Python client's configuration) would; API errors pass straight through.
async fn with_transport_retry<T, F, Fut>(mut op: F) -> kube::Result<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = kube::Result<T>>,
{
    let total: u32 = std::env::var("REQ_RETRY_TOTAL")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(5);
    let backoff: f64 = std::env::var("REQ_RETRY_BACKOFF_FACTOR")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1.1);
    let mut attempt = 0u32;
    loop {
        match op().await {
            Ok(v) => return Ok(v),
            Err(e) if is_transport_error(&e) && attempt < total => {
                attempt += 1;
                if attempt > 1 {
                    let delay = (backoff * 2f64.powi(attempt as i32 - 1)).min(120.0);
                    tokio::time::sleep(Duration::from_secs_f64(delay)).await;
                }
            }
            Err(e) => return Err(e),
        }
    }
}

fn cm_api(client: &Client, ns: &str) -> Api<ConfigMap> {
    if ns == "ALL" {
        Api::all(client.clone())
    } else {
        Api::namespaced(client.clone(), ns)
    }
}
fn secret_api(client: &Client, ns: &str) -> Api<Secret> {
    if ns == "ALL" {
        Api::all(client.clone())
    } else {
        Api::namespaced(client.clone(), ns)
    }
}

pub async fn list_page(
    client: &Client,
    kind: Kind,
    ns: &str,
    label_selector: &str,
    limit: u32,
    cont: Option<String>,
) -> kube::Result<(Vec<Item>, Option<String>)> {
    let mut lp = ListParams::default().labels(label_selector).limit(limit);
    if let Some(c) = cont {
        lp = lp.continue_token(&c);
    }
    match kind {
        Kind::ConfigMap => {
            let api = cm_api(client, ns);
            let list = with_transport_retry(|| {
                let api = api.clone();
                let lp = lp.clone();
                async move { api.list(&lp).await }
            })
            .await?;
            let cont = list.metadata.continue_.clone().filter(|c| !c.is_empty());
            Ok((list.items.into_iter().map(Item::from_cm).collect(), cont))
        }
        Kind::Secret => {
            let api = secret_api(client, ns);
            let list = with_transport_retry(|| {
                let api = api.clone();
                let lp = lp.clone();
                async move { api.list(&lp).await }
            })
            .await?;
            let cont = list.metadata.continue_.clone().filter(|c| !c.is_empty());
            Ok((
                list.items.into_iter().map(Item::from_secret).collect(),
                cont,
            ))
        }
    }
}

/// Read a single named resource; Ok(None) on 404 like upstream's tolerated
/// ApiException(404).
pub async fn read_named(
    client: &Client,
    kind: Kind,
    ns: &str,
    name: &str,
) -> kube::Result<Option<Item>> {
    match kind {
        Kind::ConfigMap => {
            let api = cm_api(client, ns);
            let got = with_transport_retry(|| {
                let api = api.clone();
                let name = name.to_string();
                async move { api.get_opt(&name).await }
            })
            .await?;
            Ok(got.map(Item::from_cm))
        }
        Kind::Secret => {
            let api = secret_api(client, ns);
            let got = with_transport_retry(|| {
                let api = api.clone();
                let name = name.to_string();
                async move { api.get_opt(&name).await }
            })
            .await?;
            Ok(got.map(Item::from_secret))
        }
    }
}

/// One watch connection. The stream ends when the server closes it (after
/// timeout_seconds); the caller reconnects, which re-delivers the current
/// state as synthetic ADDED events — identical to upstream's loop.
pub async fn watch_items(
    client: &Client,
    kind: Kind,
    ns: &str,
    label_selector: &str,
    timeout_secs: u32,
) -> kube::Result<BoxStream<'static, kube::Result<(&'static str, Item)>>> {
    let wp = WatchParams::default()
        .labels(label_selector)
        .timeout(timeout_secs);
    match kind {
        Kind::ConfigMap => {
            let stream = cm_api(client, ns).watch(&wp, "0").await?;
            Ok(stream
                .filter_map(|ev| async move {
                    match ev {
                        Ok(WatchEvent::Added(o)) => Some(Ok(("ADDED", Item::from_cm(o)))),
                        Ok(WatchEvent::Modified(o)) => Some(Ok(("MODIFIED", Item::from_cm(o)))),
                        Ok(WatchEvent::Deleted(o)) => Some(Ok(("DELETED", Item::from_cm(o)))),
                        Ok(WatchEvent::Bookmark(_)) => None,
                        Ok(WatchEvent::Error(er)) => Some(Err(kube::Error::Api(er))),
                        Err(e) => Some(Err(e)),
                    }
                })
                .boxed())
        }
        Kind::Secret => {
            let stream = secret_api(client, ns).watch(&wp, "0").await?;
            Ok(stream
                .filter_map(|ev| async move {
                    match ev {
                        Ok(WatchEvent::Added(o)) => Some(Ok(("ADDED", Item::from_secret(o)))),
                        Ok(WatchEvent::Modified(o)) => Some(Ok(("MODIFIED", Item::from_secret(o)))),
                        Ok(WatchEvent::Deleted(o)) => Some(Ok(("DELETED", Item::from_secret(o)))),
                        Ok(WatchEvent::Bookmark(_)) => None,
                        Ok(WatchEvent::Error(er)) => Some(Err(kube::Error::Api(er))),
                        Err(e) => Some(Err(e)),
                    }
                })
                .boxed())
        }
    }
}

/// Kubernetes server version, for the IGNORE_ALREADY_PROCESSED >= 1.19 gate.
pub async fn server_version(
    client: &Client,
) -> kube::Result<k8s_openapi::apimachinery::pkg::version::Info> {
    client.apiserver_version().await
}
