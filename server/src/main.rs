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

    // Auto-spawn bundled LiveKit unless an external instance is configured via
    // LIVEKIT_URL.
    let want_autospawn = std::env::var("DIOXUSFUN_LIVEKIT_AUTOSPAWN")
        .map(|v| !matches!(v.as_str(), "0" | "false" | "no"))
        .unwrap_or(true);
    let livekit_present = std::env::var("LIVEKIT_URL").is_ok();

    let _livekit_handle = if want_autospawn && !livekit_present {
        // Pass None for address: standalone LiveKit needs no NAT config. Set
        // LIVEKIT_URL if behind NAT.
        match dioxusfun_server::livekit_bundle::spawn_livekit(None).await {
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

    let livekit_cfg = dioxusfun_server::livekit::LiveKitConfig::from_env();
    tracing::info!(
        explicit_url = ?livekit_cfg.explicit_url,
        port = livekit_cfg.port,
        "livekit configured (URLs handed to clients are derived per-connection unless explicit_url is set)"
    );

    // Operators (comma-separated hex pubkeys) can moderate the seeded Lobby.
    // Empty leaves it unmanaged.
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

    let data_dir: std::path::PathBuf = std::env::var("DIOXUSFUN_DATA_DIR")
        .unwrap_or_else(|_| "./discordia-data".into())
        .into();
    tracing::info!(data_dir = %data_dir.display(), "durable data directory");

    let cfg = dioxusfun_server::ServerConfig {
        livekit: livekit_cfg,
        operators,
        data_dir,
    };
    let serve_fut = dioxusfun_server::serve(addr, cfg);
    tokio::select! {
        result = serve_fut => {
            if let Err(e) = result {
                tracing::error!(error = %e, "server exited with error");
                std::process::exit(1);
            }
        }
        _ = tokio::signal::ctrl_c() => {
            tracing::info!("ctrl+c — shutting down");
        }
    }
}

/// The durable data root the CLI operates on (same default as the server).
fn cli_data_dir() -> std::path::PathBuf {
    std::env::var("DIOXUSFUN_DATA_DIR")
        .unwrap_or_else(|_| "./discordia-data".into())
        .into()
}

async fn open_store() -> dioxusfun_server::store::Store {
    let dir = cli_data_dir();
    dioxusfun_server::store::Store::open(&dir.join("db.sqlite"))
        .await
        .unwrap_or_else(|e| {
            eprintln!("error: cannot open store at {}: {e}", dir.display());
            std::process::exit(1);
        })
}

/// `discordia export --guild <uuid> <out.json>` | `--all <out-dir>`
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
            // Fail loudly: a backup that reports success while writing nothing
            // is worse than one that fails.
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

/// `discordia import <archive.json>` — writes the guild in under fresh ids.
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
