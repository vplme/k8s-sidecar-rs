//! The sync state machine, mirroring upstream resources.py: list-based sync
//! with pagination and stale-key cleanup, watch loops with reconnect, the
//! old-object diff that deletes files for vanished keys (including from the
//! previous folder when the target-directory annotation moves), and
//! files_changed gating SCRIPT and REQ_URL.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use futures::StreamExt;
use kube::Client;

use crate::files::{self, CONTENT_TYPE_BASE64_BINARY, CONTENT_TYPE_TEXT};
use crate::health;
use crate::http;
use crate::kubeapi::{self, Item, Kind};
use crate::logger;
use crate::script;

pub struct Ctx {
    pub label: String,
    pub label_value: Option<String>,
    pub target_folder: String,
    pub folder_annotation: String,
    pub req_method: Option<String>,
    pub req_payload: Option<serde_json::Value>,
    pub script: Option<String>,
    pub unique_filenames: bool,
    pub enable_5xx: bool,
    pub resource_name: String,
    pub mode: Option<String>,
    pub watch_server_timeout: u64,
    pub watch_client_timeout: u64,
}

impl Ctx {
    fn selector(&self) -> String {
        match &self.label_value {
            Some(v) => format!("{}={}", self.label, v),
            None => self.label.clone(),
        }
    }
}

pub enum WErr {
    Kube(kube::Error),
    Other(String),
}

/// A synced resource reduced to what later removal needs: key names, plus the
/// value only for `*.url` keys, whose fetch happens even on the removal path.
/// Payload bytes stay out of this cache so resident memory does not scale with
/// the size of the watched resources. `text`/`binary` keep their `Option`-ness
/// so the "No data field" warnings fire identically when a cached item is fed
/// back through removal.
#[derive(Clone)]
struct CachedItem {
    namespace: String,
    name: String,
    text: Option<BTreeMap<String, Option<String>>>,
    binary: Option<BTreeMap<String, Option<Vec<u8>>>>,
}

fn keep_url_value<V: Clone>(key: &str, value: &V) -> Option<V> {
    key.ends_with(".url").then(|| value.clone())
}

impl CachedItem {
    fn from_item(item: &Item) -> CachedItem {
        CachedItem {
            namespace: item.namespace.clone(),
            name: item.name.clone(),
            text: item.text.as_ref().map(|m| {
                m.iter()
                    .map(|(k, v)| (k.clone(), keep_url_value(k, v)))
                    .collect()
            }),
            binary: item.binary.as_ref().map(|m| {
                m.iter()
                    .map(|(k, v)| (k.clone(), keep_url_value(k, v)))
                    .collect()
            }),
        }
    }

    /// Rebuild an `Item` for the removal path. Elided values come back empty,
    /// which removal never reads; `*.url` values come back verbatim.
    fn to_removal_item(&self) -> Item {
        Item {
            namespace: self.namespace.clone(),
            name: self.name.clone(),
            resource_version: String::new(),
            annotations: BTreeMap::new(),
            text: self.text.as_ref().map(|m| {
                m.iter()
                    .map(|(k, v)| (k.clone(), v.clone().unwrap_or_default()))
                    .collect()
            }),
            binary: self.binary.as_ref().map(|m| {
                m.iter()
                    .map(|(k, v)| (k.clone(), v.clone().unwrap_or_default()))
                    .collect()
            }),
        }
    }
}

/// Shared across all watcher tasks, like upstream's module-level dicts.
#[derive(Default)]
struct Maps {
    version: HashMap<(Kind, String), String>,
    object: HashMap<(Kind, String), CachedItem>,
    dest: HashMap<(Kind, String), String>,
}

static MAPS: OnceLock<Mutex<Maps>> = OnceLock::new();

fn maps() -> &'static Mutex<Maps> {
    MAPS.get_or_init(|| Mutex::new(Maps::default()))
}

#[derive(Clone)]
enum Content {
    Text(String),
    Bin(Vec<u8>),
}

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

async fn throttle() {
    tokio::time::sleep(Duration::from_secs(env_u64("ERROR_THROTTLE_SLEEP", 5))).await;
}

fn get_destination_folder(item: &Item, ctx: &Ctx) -> String {
    if let Some(annotation_value) = item.annotations.get(&ctx.folder_annotation) {
        let dest = if annotation_value.starts_with('/') {
            annotation_value.clone()
        } else if ctx.target_folder.ends_with('/') {
            format!("{}{}", ctx.target_folder, annotation_value)
        } else {
            format!("{}/{}", ctx.target_folder, annotation_value)
        };
        logger::info(&format!(
            "Found a folder override annotation, placing the {} in: {}",
            item.name, dest
        ));
        return dest;
    }
    ctx.target_folder.clone()
}

// Mirrors the Python signature; splitting it would only obscure the mapping.
#[allow(clippy::too_many_arguments)]
async fn update_file(
    data_key: &str,
    content: Content,
    dest_folder: &str,
    item: &Item,
    kind: Kind,
    ctx: &Ctx,
    content_type: &str,
    remove: bool,
) -> bool {
    let result: Result<bool, String> = async {
        // _get_file_data_and_name: the .url fetch happens even on the removal
        // path, exactly like upstream.
        let (filename, data): (String, Vec<u8>) = if let Some(stripped) = data_key.strip_suffix(".url") {
            let filename = stripped.to_string();
            let url = match &content {
                Content::Bin(b) => String::from_utf8(b.clone()).map_err(|e| e.to_string())?,
                Content::Text(t) => t.clone(),
            };
            let resp = http::request(Some(&url), Some("GET"), ctx.enable_5xx, None).await;
            (filename, http::body_if_ok(resp))
        } else {
            let data = match &content {
                Content::Text(t) => t.clone().into_bytes(),
                Content::Bin(b) => b.clone(),
            };
            (data_key.to_string(), data)
        };

        let filename = if ctx.unique_filenames {
            files::unique_filename(&filename, &item.namespace, kind.as_str(), &item.name)
        } else {
            filename
        };

        if !remove {
            files::write_data_to_file(dest_folder, &filename, &data, content_type)
                .map_err(|e| e.to_string())
        } else {
            Ok(files::remove_file(dest_folder, &filename))
        }
    }
    .await;

    match result {
        Ok(changed) => changed,
        Err(_) => {
            logger::error(&format!(
                "Error when updating from '{}' into '{}'",
                data_key, dest_folder
            ));
            false
        }
    }
}

async fn iterate_data(
    data: Vec<(String, Content)>,
    dest_folder: &str,
    item: &Item,
    kind: Kind,
    ctx: &Ctx,
    content_type: &str,
    remove: bool,
) -> bool {
    let mut changed = false;
    for (key, content) in data {
        changed |= update_file(
            &key,
            content,
            dest_folder,
            item,
            kind,
            ctx,
            content_type,
            remove,
        )
        .await;
    }
    changed
}

fn text_entries(m: &BTreeMap<String, String>) -> Vec<(String, Content)> {
    m.iter()
        .map(|(k, v)| (k.clone(), Content::Text(v.clone())))
        .collect()
}
fn bin_entries(m: &BTreeMap<String, Vec<u8>>) -> Vec<(String, Content)> {
    m.iter()
        .map(|(k, v)| (k.clone(), Content::Bin(v.clone())))
        .collect()
}

/// Keys of `old` minus those also present in `new` — the stale-key diff.
fn minus_common<K: Ord + Clone, V: Clone>(
    old: &BTreeMap<K, V>,
    new: Option<&BTreeMap<K, impl Sized>>,
) -> BTreeMap<K, V> {
    old.iter()
        .filter(|(k, _)| !new.is_some_and(|n| n.contains_key(k)))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

/// Mirror of _process_config_map / _process_secret.
async fn process_resource(
    ctx: &Ctx,
    kind: Kind,
    item: &Item,
    dest_folder: Option<&str>,
    is_removed: bool,
) -> bool {
    let key = (kind, item.key());
    let (old_item, old_dest) = {
        let mut m = maps().lock().unwrap();
        let old_item = match m.object.get(&key) {
            Some(cached) => cached.to_removal_item(),
            None => CachedItem::from_item(item).to_removal_item(),
        };
        let old_dest = m
            .dest
            .get(&key)
            .cloned()
            .or_else(|| dest_folder.map(String::from))
            .unwrap_or_default();
        if is_removed {
            m.object.remove(&key);
        } else {
            m.object.insert(key.clone(), CachedItem::from_item(item));
            m.dest
                .insert(key.clone(), dest_folder.unwrap_or_default().to_string());
        }
        (old_item, old_dest)
    };
    let dest: String = if is_removed {
        old_dest.clone()
    } else {
        dest_folder.unwrap_or_default().to_string()
    };

    let mut changed = false;
    match kind {
        Kind::ConfigMap => {
            if item.text.is_none() && item.binary.is_none() {
                logger::warning(&format!("No data/binaryData field in {}", kind.as_str()));
            }
            if let Some(text) = &item.text {
                logger::debug(&format!("Found 'data' on {}", kind.as_str()));
                changed |= iterate_data(
                    text_entries(text),
                    &dest,
                    item,
                    kind,
                    ctx,
                    CONTENT_TYPE_TEXT,
                    is_removed,
                )
                .await;
            }
            if let Some(old_text) = &old_item.text
                && !is_removed
            {
                let leftovers = if old_dest == dest {
                    minus_common(old_text, item.text.as_ref())
                } else {
                    old_text.clone()
                };
                changed |= iterate_data(
                    text_entries(&leftovers),
                    &old_dest,
                    &old_item,
                    kind,
                    ctx,
                    CONTENT_TYPE_TEXT,
                    true,
                )
                .await;
            }
            if let Some(bin) = &item.binary {
                logger::debug(&format!("Found 'binary_data' on {}", kind.as_str()));
                changed |= iterate_data(
                    bin_entries(bin),
                    &dest,
                    item,
                    kind,
                    ctx,
                    CONTENT_TYPE_BASE64_BINARY,
                    is_removed,
                )
                .await;
            }
            if let Some(old_bin) = &old_item.binary
                && !is_removed
            {
                let leftovers = if old_dest == dest {
                    minus_common(old_bin, item.binary.as_ref())
                } else {
                    old_bin.clone()
                };
                changed |= iterate_data(
                    bin_entries(&leftovers),
                    &old_dest,
                    &old_item,
                    kind,
                    ctx,
                    CONTENT_TYPE_BASE64_BINARY,
                    true,
                )
                .await;
            }
        }
        Kind::Secret => {
            if item.binary.is_none() {
                logger::warning(&format!("No data field in {}", kind.as_str()));
            }
            if let Some(bin) = &item.binary {
                changed |= iterate_data(
                    bin_entries(bin),
                    &dest,
                    item,
                    kind,
                    ctx,
                    CONTENT_TYPE_BASE64_BINARY,
                    is_removed,
                )
                .await;
            }
            if let Some(old_bin) = &old_item.binary
                && !is_removed
            {
                let leftovers = if old_dest == dest {
                    minus_common(old_bin, item.binary.as_ref())
                } else {
                    old_bin.clone()
                };
                changed |= iterate_data(
                    bin_entries(&leftovers),
                    &old_dest,
                    &old_item,
                    kind,
                    ctx,
                    CONTENT_TYPE_BASE64_BINARY,
                    true,
                )
                .await;
            }
        }
    }
    changed
}

/// Mirror of list_resources(): one full list-based sync pass.
pub async fn list_resources(
    client: &Client,
    ctx: &Ctx,
    kind: Kind,
    namespace: &str,
    request_url: Option<&str>,
    ignore_already_processed: bool,
) -> Result<(), WErr> {
    let args_repr = if namespace != "ALL" {
        format!("{{'namespace': '{}'}}", namespace)
    } else {
        "{}".to_string()
    };
    logger::info(&format!(
        "Performing list-based sync on {} resources: {}",
        kind.as_str(),
        args_repr
    ));

    // RESOURCE_NAME entries are reversed-split; a 3-part entry checks only the
    // namespace, a 2-part entry only the resource type — upstream quirks kept.
    let mut resource_names: Vec<String> = Vec::new();
    if namespace != "ALL" && !ctx.resource_name.is_empty() {
        for rn in ctx.resource_name.split(',') {
            let rev: Vec<&str> = rn.split('/').rev().collect();
            if rev.len() == 3 && rev[2] != namespace {
                continue;
            }
            if rev.len() == 2 && rev[1] != kind.as_str() {
                continue;
            }
            resource_names.push(rev[0].to_string());
        }
    }

    let mut items: Vec<Item> = Vec::new();
    if namespace != "ALL" && !resource_names.is_empty() {
        for rn in &resource_names {
            match kubeapi::read_named(client, kind, namespace, rn).await {
                Ok(Some(item)) => items.push(item),
                Ok(None) => {} // 404 tolerated
                Err(e) => return Err(WErr::Kube(e)),
            }
        }
    } else {
        let sel = ctx.selector();
        let mut cont: Option<String> = None;
        loop {
            let (page, next) = kubeapi::list_page(client, kind, namespace, &sel, 5, cont)
                .await
                .map_err(WErr::Kube)?;
            items.extend(page);
            match next {
                Some(c) => cont = Some(c),
                None => break,
            }
        }
    }

    let mut files_changed = false;
    let mut exist_keys: HashSet<String> = HashSet::new();

    for item in &items {
        exist_keys.insert(item.key());

        if ignore_already_processed {
            let skip = {
                let mut m = maps().lock().unwrap();
                let k = (kind, item.key());
                if m.version.get(&k) == Some(&item.resource_version) {
                    true
                } else {
                    m.version.insert(k, item.resource_version.clone());
                    false
                }
            };
            if skip {
                logger::debug(&format!(
                    "Ignoring {} {}/{}",
                    kind.as_str(),
                    item.namespace,
                    item.name
                ));
                continue;
            }
        }

        logger::debug(&format!(
            "Working on {}: {}/{}",
            kind.as_str(),
            item.namespace,
            item.name
        ));

        let dest = get_destination_folder(item, ctx);
        files_changed |= process_resource(ctx, kind, item, Some(&dest), false).await;
    }

    // Stale cleanup, scoped to this namespace: the maps are shared across
    // per-namespace tasks and an unscoped diff would let one task delete
    // another's files.
    let stale: Vec<Item> = {
        let m = maps().lock().unwrap();
        m.object
            .iter()
            .filter(|((k, key), cached)| {
                *k == kind
                    && (namespace == "ALL" || cached.namespace == namespace)
                    && !exist_keys.contains(key)
            })
            .map(|(_, cached)| cached.to_removal_item())
            .collect()
    };
    for item in stale {
        logger::debug(&format!(
            "Removing {}: {}/{}",
            kind.as_str(),
            item.namespace,
            item.name
        ));
        files_changed |= process_resource(ctx, kind, &item, None, true).await;
    }

    if files_changed {
        if let Some(s) = &ctx.script {
            script::execute(s).await.map_err(WErr::Other)?;
        }
        if let Some(url) = request_url {
            http::request(
                Some(url),
                ctx.req_method.as_deref(),
                ctx.enable_5xx,
                ctx.req_payload.as_ref(),
            )
            .await;
        }
    }
    Ok(())
}

/// One watch connection's event loop. Ends (Ok) when the server closes the
/// stream after WATCH_SERVER_TIMEOUT; the caller reconnects.
async fn watch_resource_iterator(
    client: &Client,
    ctx: &Ctx,
    kind: Kind,
    namespace: &str,
    request_url: Option<&str>,
    ignore_already_processed: bool,
) -> Result<(), WErr> {
    let sel = ctx.selector();
    let ns_repr = if namespace != "ALL" {
        format!(", 'namespace': '{}'", namespace)
    } else {
        String::new()
    };
    logger::debug(&format!(
        "Performing watch-based sync on {} resources: {{'label_selector': '{}', 'timeout_seconds': {}, '_request_timeout': {}{}}}",
        kind.as_str(),
        sel,
        ctx.watch_server_timeout,
        ctx.watch_client_timeout,
        ns_repr
    ));

    let mut stream = kubeapi::watch_items(
        client,
        kind,
        namespace,
        &sel,
        ctx.watch_server_timeout as u32,
    )
    .await
    .map_err(WErr::Kube)?;

    let mut first_event = true;
    while let Some(ev) = stream.next().await {
        let (event_type, item) = ev.map_err(WErr::Kube)?;

        if first_event {
            health::mark_ready();
            first_event = false;
        }
        health::update_k8s_contact();

        if ignore_already_processed {
            let skip = {
                let mut m = maps().lock().unwrap();
                let k = (kind, item.key());
                let mut skip = false;
                if m.version.get(&k) == Some(&item.resource_version) {
                    if event_type == "ADDED" || event_type == "MODIFIED" {
                        skip = true;
                    } else if event_type == "DELETED" {
                        m.version.remove(&k);
                    }
                }
                if !skip && (event_type == "ADDED" || event_type == "MODIFIED") {
                    m.version.insert(k, item.resource_version.clone());
                }
                skip
            };
            if skip {
                logger::debug(&format!(
                    "Ignoring {} {} {}/{}",
                    event_type,
                    kind.as_str(),
                    item.namespace,
                    item.name
                ));
                continue;
            }
        }

        logger::debug(&format!(
            "Working on {} {} {}/{}",
            event_type,
            kind.as_str(),
            item.namespace,
            item.name
        ));

        let dest = get_destination_folder(&item, ctx);
        let item_removed = event_type == "DELETED";
        let files_changed = process_resource(ctx, kind, &item, Some(&dest), item_removed).await;

        if files_changed {
            if let Some(s) = &ctx.script {
                script::execute(s).await.map_err(WErr::Other)?;
            }
            if let Some(url) = request_url {
                http::request(
                    Some(url),
                    ctx.req_method.as_deref(),
                    ctx.enable_5xx,
                    ctx.req_payload.as_ref(),
                )
                .await;
            }
        }
    }
    Ok(())
}

/// Per-(resource, namespace) watcher, mirroring _watch_resource_loop. The
/// `alive` flag feeds both the health endpoint and the main monitor loop.
pub async fn watch_task(
    client: Client,
    ctx: Arc<Ctx>,
    kind: Kind,
    namespace: String,
    request_url: Option<String>,
    ignore_already_processed: bool,
    alive: Arc<AtomicBool>,
) {
    loop {
        let sleep_mode = ctx.mode.as_deref() == Some("SLEEP")
            || (namespace != "ALL" && !ctx.resource_name.is_empty());

        let result = if sleep_mode {
            match list_resources(
                &client,
                &ctx,
                kind,
                &namespace,
                request_url.as_deref(),
                ignore_already_processed,
            )
            .await
            {
                Ok(()) => {
                    tokio::time::sleep(Duration::from_secs(env_u64("SLEEP_TIME", 60))).await;
                    Ok(())
                }
                Err(e) => Err(e),
            }
        } else {
            watch_resource_iterator(
                &client,
                &ctx,
                kind,
                &namespace,
                request_url.as_deref(),
                ignore_already_processed,
            )
            .await
        };

        if let Err(e) = result {
            match &e {
                // Upstream re-raises 500s, killing the watcher thread; the
                // monitor then exits the process.
                WErr::Kube(kube::Error::Api(er)) if er.code == 500 => {
                    logger::error(&format!(
                        "ApiException when calling kubernetes: ({}) Reason: {}",
                        er.code, er.reason
                    ));
                    break;
                }
                WErr::Kube(kube::Error::Api(er)) => {
                    logger::error(&format!(
                        "ApiException when calling kubernetes: ({}) Reason: {}\n",
                        er.code, er.reason
                    ));
                    throttle().await;
                }
                WErr::Kube(other) => {
                    logger::error(&format!(
                        "ProtocolError when calling kubernetes: {}\n",
                        other
                    ));
                    throttle().await;
                }
                WErr::Other(msg) => {
                    logger::error(&format!("Received unknown exception: {}\n", msg));
                    throttle().await;
                }
            }
        }
    }
    alive.store(false, Ordering::Relaxed);
}
