use std::path::Path;

use uuid::Uuid;

async fn open_store(dir: &Path) -> discordia_server::store::Store {
    discordia_server::store::Store::open_in(dir)
        .await
        .unwrap_or_else(|e| {
            eprintln!("error: cannot open store at {}: {e}", dir.display());
            std::process::exit(1);
        })
}

pub async fn export(data_dir: &Path, guild: Option<Uuid>, out: &Path) {
    let store = open_store(data_dir).await;
    match guild {
        Some(guild_id) => {
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
                eprintln!("error: cannot write {}: {e}", out.display());
                std::process::exit(1);
            });
            println!(
                "exported guild '{}' → {}",
                archive.guild.name,
                out.display()
            );
        }
        None => {
            if let Err(e) = std::fs::create_dir_all(out) {
                eprintln!("could not create {}: {e}", out.display());
                std::process::exit(1);
            }
            let loaded = store.load_all().await.unwrap();
            let (mut n, mut failed) = (0, 0);
            for g in &loaded.guilds {
                let path = out.join(format!("{}.json", g.id));
                match store.export_guild(g.id).await {
                    Ok(Some(archive)) => {
                        let json = serde_json::to_string_pretty(&archive).unwrap();
                        match std::fs::write(&path, json) {
                            Ok(()) => n += 1,
                            Err(e) => {
                                eprintln!("could not write {}: {e}", path.display());
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
            println!("exported {n} guild(s) → {}/", out.display());
            if failed > 0 {
                eprintln!("{failed} guild(s) failed");
                std::process::exit(1);
            }
        }
    }
}

pub async fn import(data_dir: &Path, path: &Path) {
    let store = open_store(data_dir).await;
    let json = std::fs::read_to_string(path).unwrap_or_else(|e| {
        eprintln!("error: cannot read {}: {e}", path.display());
        std::process::exit(1);
    });
    let archive: discordia_server::archive::GuildArchive = serde_json::from_str(&json)
        .unwrap_or_else(|e| {
            eprintln!(
                "error: {} is not a valid guild archive: {e}",
                path.display()
            );
            std::process::exit(1);
        });
    let name = archive.guild.name.clone();
    let new_id = store.import_guild(&archive).await.unwrap_or_else(|e| {
        eprintln!("import failed: {e}");
        std::process::exit(1);
    });
    println!("imported guild '{name}' as {new_id}");
}
