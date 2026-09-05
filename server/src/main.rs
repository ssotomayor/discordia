use std::net::SocketAddr;

use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("export") => return run_export(&args[2..]).await,
        Some("import") => return run_import(&args[2..]).await,
        _ => {}
    }

    let addr: SocketAddr = std::env::var("DIOXUSFUN_ADDR")
        .unwrap_or_else(|_| "0.0.0.0:9000".into())
        .parse()
        .expect("DIOXUSFUN_ADDR must be host:port");

    let data_dir = cli_data_dir();
    tracing::info!(data_dir = %data_dir.display(), "durable data directory");

    let livekit_cfg = dioxusfun_server::livekit::LiveKitConfig::from_env(&data_dir);

    let want_autospawn = std::env::var("DIOXUSFUN_LIVEKIT_AUTOSPAWN")
        .map(|v| !matches!(v.as_str(), "0" | "false" | "no"))
        .unwrap_or(true);
    let livekit_present = std::env::var("LIVEKIT_URL").is_ok();

    let _livekit_handle = if want_autospawn && !livekit_present {
        let creds = dioxusfun_server::livekit_bundle::Credentials {
            key: livekit_cfg.api_key.clone(),
            secret: livekit_cfg.api_secret.clone(),
        };
        match dioxusfun_server::livekit_bundle::spawn_livekit(None, &creds, &data_dir).await {
            Ok(child) => {
                tracing::info!("bundled livekit-server started on port 7880");
                Some(child)
            }
            Err(e) => {
                tracing::warn!(error = %e, "livekit subprocess not started — voice will be unavailable unless you set LIVEKIT_URL");
                None
            }
        }
    } else {
        if !want_autospawn {
            tracing::info!("DIOXUSFUN_LIVEKIT_AUTOSPAWN=0, not spawning bundled livekit");
        } else {
            tracing::info!("LIVEKIT_URL set, assuming external livekit instance");
        }
        None
    };

    tracing::info!(
        explicit_url = ?livekit_cfg.explicit_url,
        port = livekit_cfg.port,
        "livekit configured (URLs handed to clients are derived per-connection unless explicit_url is set)"
    );

    let operators: std::collections::HashSet<String> = std::env::var("DIOXUSFUN_OPERATORS")
        .unwrap_or_default()
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if !operators.is_empty() {
        tracing::info!(
            count = operators.len(),
            "operators configured for system guilds"
        );
    }

    let identities = dioxusfun_server::declared_identities(
        &std::env::var("DIOXUSFUN_PUBLIC_HOSTS").unwrap_or_default(),
        addr.port(),
    );
    if !identities.is_empty() {
        tracing::info!(?identities, "public names clients may dial");
    }

    let media_max_bytes = std::env::var("DIOXUSFUN_MEDIA_MAX_BYTES")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(dioxusfun_server::media::DEFAULT_MAX_BYTES);

    let mut cfg = dioxusfun_server::ServerConfig {
        livekit: livekit_cfg,
        operators,
        data_dir: data_dir.clone(),
        identities,
        media_max_bytes,
    };

    // The plaintext socket is for loopback and a TLS proxy; anything reaching
    // this box directly from elsewhere comes in over QUIC with the key below.
    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            tracing::error!(%addr, error = %e, "cannot bind the gateway");
            std::process::exit(1);
        }
    };
    let bound = listener.local_addr().unwrap_or(addr);
    cfg.identities
        .extend(dioxusfun_server::local_identities(bound.port()));

    let coordination = match std::env::var("DIOXUSFUN_RELAY_URL") {
        Ok(url) if !url.trim().is_empty() => {
            dioxusfun_server::quic::Coordination::Relay(url.trim().to_string())
        }
        _ => dioxusfun_server::quic::Coordination::None,
    };
    let quic_secret = dioxusfun_server::quic::persistent_secret(&data_dir).unwrap_or_else(|e| {
        tracing::error!(error = %e, "cannot persist the QUIC key; friends will have to re-add this server after a restart");
        iroh::SecretKey::generate()
    });
    let quic_endpoint = match dioxusfun_server::quic::bind_quic(Some(quic_secret), &coordination)
        .await
    {
        Ok(ep) => Some(ep),
        Err(e) => {
            tracing::warn!(error = %e, "QUIC endpoint not bound — only loopback and a TLS proxy can reach this gateway");
            None
        }
    };
    if let Some(ep) = quic_endpoint.as_ref() {
        cfg.identities
            .insert(dioxusfun_server::protocol::quic_origin(
                &ep.id().to_string(),
            ));
    }

    let router = match dioxusfun_server::build_router(cfg).await {
        Ok(router) => router,
        Err(e) => {
            tracing::error!(error = %e, "server failed to start");
            std::process::exit(1);
        }
    };

    let _quic = quic_endpoint.and_then(|ep| {
        let key = ep.id().to_string();
        match dioxusfun_server::quic::serve_on_with(ep, router.clone(), coordination.clone()) {
            Ok(handle) => {
                let port = handle.sockets.first().map(|s| s.port()).unwrap_or(0);
                let mut addrs: Vec<String> = dioxusfun_server::local_identities(port)
                    .into_iter()
                    .filter(|a| !a.starts_with("localhost") && !a.starts_with("127.") && !a.starts_with("[::1]"))
                    .collect();
                addrs.sort();
                if let dioxusfun_server::quic::Coordination::Relay(url) = &coordination {
                    addrs.push(url.clone());
                }
                tracing::info!(
                    share = %dioxusfun_server::protocol::format_quic_share(&key, &addrs),
                    "friends connect with this address (replace a private IP with your public one if you forward the UDP port)"
                );
                Some(handle)
            }
            Err(e) => {
                tracing::warn!(error = %e, "QUIC gateway not serving");
                None
            }
        }
    });

    let gateway = dioxusfun_server::serve_router(listener, router);
    tracing::info!(%bound, "plaintext gateway listening (loopback and TLS proxies only)");

    tokio::signal::ctrl_c().await.ok();
    tracing::info!("ctrl+c — shutting down");
    gateway.abort();
}

fn cli_data_dir() -> std::path::PathBuf {
    std::env::var("DIOXUSFUN_DATA_DIR")
        .unwrap_or_else(|_| "./discordia-data".into())
        .into()
}

async fn open_store() -> dioxusfun_server::store::Store {
    let dir = cli_data_dir();
    dioxusfun_server::store::Store::open_in(&dir)
        .await
        .unwrap_or_else(|e| {
            eprintln!("error: cannot open store at {}: {e}", dir.display());
            std::process::exit(1);
        })
}

async fn run_export(args: &[String]) {
    let store = open_store().await;
    match args.first().map(String::as_str) {
        Some("--guild") => {
            let (Some(id_str), Some(out)) = (args.get(1), args.get(2)) else {
                eprintln!("usage: discordia export --guild <uuid> <out.json>");
                std::process::exit(2);
            };
            let guild_id = id_str.parse().unwrap_or_else(|_| {
                eprintln!("error: '{id_str}' is not a valid guild id");
                std::process::exit(2);
            });
            let archive = store.export_guild(guild_id).await.unwrap_or_else(|e| {
                eprintln!("export failed: {e}");
                std::process::exit(1);
            });
            let Some(archive) = archive else {
                eprintln!("error: no guild {guild_id}");
                std::process::exit(1);
            };
            let json = serde_json::to_string_pretty(&archive).unwrap();
            std::fs::write(out, json).unwrap_or_else(|e| {
                eprintln!("error: cannot write {out}: {e}");
                std::process::exit(1);
            });
            println!("exported guild '{}' → {out}", archive.guild.name);
        }
        Some("--all") => {
            let Some(dir) = args.get(1) else {
                eprintln!("usage: discordia export --all <out-dir>");
                std::process::exit(2);
            };
            if let Err(e) = std::fs::create_dir_all(dir) {
                eprintln!("could not create {dir}: {e}");
                std::process::exit(1);
            }
            let loaded = store.load_all().await.unwrap();
            let (mut n, mut failed) = (0, 0);
            for g in &loaded.guilds {
                let path = format!("{dir}/{}.json", g.id);
                match store.export_guild(g.id).await {
                    Ok(Some(archive)) => {
                        let json = serde_json::to_string_pretty(&archive).unwrap();
                        match std::fs::write(&path, json) {
                            Ok(()) => n += 1,
                            Err(e) => {
                                eprintln!("could not write {path}: {e}");
                                failed += 1;
                            }
                        }
                    }
                    Ok(None) => {}
                    Err(e) => {
                        eprintln!("could not export {}: {e}", g.id);
                        failed += 1;
                    }
                }
            }
            println!("exported {n} guild(s) → {dir}/");
            if failed > 0 {
                eprintln!("{failed} guild(s) failed");
                std::process::exit(1);
            }
        }
        _ => {
            eprintln!("usage: discordia export --guild <uuid> <out.json> | --all <out-dir>");
            std::process::exit(2);
        }
    }
}

async fn run_import(args: &[String]) {
    let store = open_store().await;
    let Some(path) = args.first() else {
        eprintln!("usage: discordia import <archive.json>");
        std::process::exit(2);
    };
    let json = std::fs::read_to_string(path).unwrap_or_else(|e| {
        eprintln!("error: cannot read {path}: {e}");
        std::process::exit(1);
    });
    let archive: dioxusfun_server::archive::GuildArchive = serde_json::from_str(&json)
        .unwrap_or_else(|e| {
            eprintln!("error: {path} is not a valid guild archive: {e}");
            std::process::exit(1);
        });
    let name = archive.guild.name.clone();
    let new_id = store.import_guild(&archive).await.unwrap_or_else(|e| {
        eprintln!("import failed: {e}");
        std::process::exit(1);
    });
    println!("imported guild '{name}' as {new_id}");
}
