//! Drop-in Rust reimplementation of kiwigrid/k8s-sidecar. Entry point mirrors
//! upstream sidecar.py: same env vars, same CLI flags, same log lines, same
//! exit behaviour — including the quirk that a missing LABEL/FOLDER logs a
//! CRITICAL line and exits 0 (upstream's `return -1` from main() is
//! discarded).

mod files;
mod health;
mod http;
mod kubeapi;
mod logger;
mod resources;
mod script;

use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use resources::{Ctx, WErr};

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

fn env_is_true(name: &str) -> bool {
    env(name).is_some_and(|v| v.to_lowercase() == "true")
}

/// argparse equivalent for the two supported flags; unknown flags exit 2.
fn parse_flags() -> (Option<String>, Option<String>) {
    let mut username_file = None;
    let mut password_file = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((f, v)) => (f.to_string(), Some(v.to_string())),
            None => (arg.clone(), None),
        };
        match flag.as_str() {
            "--req-username-file" => {
                username_file = inline.or_else(|| args.next());
            }
            "--req-password-file" => {
                password_file = inline.or_else(|| args.next());
            }
            other => {
                eprintln!("error: unrecognized arguments: {}", other);
                std::process::exit(2);
            }
        }
    }
    (username_file, password_file)
}

fn werr_to_string(e: &WErr) -> String {
    match e {
        WErr::Kube(kube::Error::Api(er)) => format!("({}) Reason: {}", er.code, er.reason),
        WErr::Kube(other) => other.to_string(),
        WErr::Other(msg) => msg.clone(),
    }
}

fn main() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("failed to build tokio runtime");
    rt.block_on(run());
}

async fn run() {
    // reqwest's rustls path uses the process-default crypto provider; kube
    // installs its own per-client, so without this every reqwest
    // Client::build() fails with "builder error".
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    let (username_file, password_file) = parse_flags();
    logger::init();
    http::init(username_file, password_file);

    logger::info("Starting collector");

    health::start_health_server();

    // SIGTERM: upstream logs and exits 0 for graceful pod shutdown.
    tokio::spawn(async {
        if let Ok(mut sig) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            sig.recv().await;
            logger::info("Subprocess exiting gracefully");
            std::process::exit(0);
        }
    });

    let folder_annotation = match env("FOLDER_ANNOTATION") {
        Some(v) => v,
        None => {
            logger::info(
                "No folder annotation was provided, defaulting to k8s-sidecar-target-directory",
            );
            "k8s-sidecar-target-directory".to_string()
        }
    };

    let Some(label) = env("LABEL") else {
        logger::fatal("Should have added LABEL as environment variable! Exit");
        return; // exit 0, like upstream's discarded `return -1`
    };

    let label_value = env("LABEL_VALUE");
    if let Some(v) = &label_value {
        if !v.is_empty() {
            logger::debug(&format!("Filter labels with value: {}", v));
        }
    }

    let Some(target_folder) = env("FOLDER") else {
        logger::fatal("Should have added FOLDER as environment variable! Exit");
        return;
    };

    let resource = env("RESOURCE").unwrap_or_else(|| "configmap".to_string());
    let (kinds, resources_repr): (Vec<kubeapi::Kind>, String) = if resource == "both" {
        (
            vec![kubeapi::Kind::Secret, kubeapi::Kind::ConfigMap],
            "('secret', 'configmap')".to_string(),
        )
    } else {
        let kind = match resource.as_str() {
            "secret" => kubeapi::Kind::Secret,
            _ => kubeapi::Kind::ConfigMap,
        };
        (vec![kind], format!("('{}',)", resource))
    };
    logger::debug(&format!("Selected resource type: {}", resources_repr));

    let resource_name = env("RESOURCE_NAME").unwrap_or_default();
    logger::debug(&format!("Selected resource name: {}", resource_name));

    let request_method = env("REQ_METHOD");
    let request_url = env("REQ_URL");
    let request_skip_init = env("REQ_SKIP_INIT").unwrap_or_else(|| "false".into()).to_lowercase() == "true";

    let request_payload = env("REQ_PAYLOAD").filter(|p| !p.is_empty()).map(|p| {
        serde_json::from_str::<serde_json::Value>(&p).unwrap_or_else(|_| {
            logger::warning("Payload will be posted as quoted json");
            serde_json::Value::String(p)
        })
    });

    let script_path = env("SCRIPT");

    let client = match kubeapi::init_client().await {
        Ok(c) => c,
        Err(e) => {
            logger::error(&e);
            std::process::exit(1);
        }
    };

    let unique_filenames = env_is_true("UNIQUE_FILENAMES");
    if unique_filenames {
        logger::info("Unique filenames will be enforced.");
    } else {
        logger::info("Unique filenames will not be enforced.");
    }

    let enable_5xx = env_is_true("ENABLE_5XX");
    if enable_5xx {
        logger::info("5xx response content will be enabled.");
    } else {
        logger::info("5xx response content will not be enabled.");
    }

    let mut ignore_already_processed = false;
    if env_is_true("IGNORE_ALREADY_PROCESSED") {
        match kubeapi::server_version(&client).await {
            Ok(version) => {
                let v_major: String = version.major.chars().filter(|c| c.is_ascii_digit()).collect();
                let v_minor: String = version.minor.chars().filter(|c| c.is_ascii_digit()).collect();
                let maj: u64 = v_major.parse().unwrap_or(0);
                let min: u64 = v_minor.parse().unwrap_or(0);
                if !v_major.is_empty() && !v_minor.is_empty() && (maj > 1 || (maj == 1 && min >= 19)) {
                    logger::info("Ignore already processed resource version will be enabled.");
                    ignore_already_processed = true;
                } else {
                    logger::info(&format!(
                        "Can't enable 'ignore already processed resource version', kubernetes api version ({}) is lower than v1.19 or unrecognized format.",
                        version.git_version
                    ));
                }
            }
            Err(_) => {
                logger::error("Exception when calling VersionApi");
            }
        }
    }
    if !ignore_already_processed {
        logger::debug("Ignore already processed resource version will not be enabled.");
    }

    // Upstream reads the ServiceAccount namespace file unconditionally, even
    // when NAMESPACE is set, and dies if it is missing. Kept.
    let sa_namespace = match std::fs::read_to_string("/var/run/secrets/kubernetes.io/serviceaccount/namespace") {
        Ok(ns) => ns,
        Err(e) => {
            logger::error(&format!(
                "FileNotFoundError: {}: '/var/run/secrets/kubernetes.io/serviceaccount/namespace'",
                e
            ));
            std::process::exit(1);
        }
    };
    let namespace = env("NAMESPACE").unwrap_or(sa_namespace);

    let method = env("METHOD");

    let ctx = Arc::new(Ctx {
        label,
        label_value: label_value.filter(|v| !v.is_empty()),
        target_folder,
        folder_annotation,
        req_method: request_method,
        req_payload: request_payload,
        script: script_path,
        unique_filenames,
        enable_5xx,
        resource_name,
        mode: method.clone(),
        watch_server_timeout: env("WATCH_SERVER_TIMEOUT").and_then(|v| v.parse().ok()).unwrap_or(60),
        watch_client_timeout: env("WATCH_CLIENT_TIMEOUT").and_then(|v| v.parse().ok()).unwrap_or(66),
    });

    if method.as_deref() == Some("LIST") {
        for kind in &kinds {
            for ns in namespace.split(',') {
                if let Err(e) =
                    resources::list_resources(&client, &ctx, *kind, ns, request_url.as_deref(), ignore_already_processed)
                        .await
                {
                    logger::error(&werr_to_string(&e));
                    std::process::exit(1);
                }
            }
        }
        health::mark_ready();
        return;
    }

    // Watch/sleep methods: initial list-based sync so files exist at startup.
    logger::info("Performing initial list-based sync before starting watch.");
    let init_request_url = if request_skip_init {
        logger::info("Skipping initial request to external endpoint.");
        None
    } else {
        request_url.clone()
    };
    for kind in &kinds {
        for ns in namespace.split(',') {
            // ignore_already_processed=true for the initial list, like upstream.
            if let Err(e) =
                resources::list_resources(&client, &ctx, *kind, ns, init_request_url.as_deref(), true).await
            {
                logger::error(&werr_to_string(&e));
                std::process::exit(1);
            }
        }
    }

    health::mark_ready();
    logger::info("Initial sync complete, sidecar is ready.");

    // One watcher task per (resource, namespace), like upstream's threads.
    let mut watchers: Vec<(tokio::task::JoinHandle<()>, String, kubeapi::Kind, Arc<AtomicBool>)> = Vec::new();
    for kind in &kinds {
        for ns in namespace.split(',') {
            let alive = Arc::new(AtomicBool::new(true));
            let handle = tokio::spawn(resources::watch_task(
                client.clone(),
                ctx.clone(),
                *kind,
                ns.to_string(),
                request_url.clone(),
                ignore_already_processed,
                alive.clone(),
            ));
            watchers.push((handle, ns.to_string(), *kind, alive));
        }
    }
    health::register_watchers(watchers.iter().map(|(_, _, _, a)| a.clone()).collect());

    loop {
        health::update_k8s_contact();
        let mut died = false;
        for (handle, ns, kind, _) in &watchers {
            if handle.is_finished() {
                logger::error(&format!("Process for {}/{} died", ns, kind.as_str()));
                died = true;
            }
        }
        if died {
            logger::fatal("At least one process died. Stopping and exiting");
            std::process::exit(1);
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}
