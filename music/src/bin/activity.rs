use clap::Parser;
use discordia_bot::BotIdentity;
use discordia_music::activity::runtime;
use std::{
    error::Error,
    io::ErrorKind,
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::{
    fs::OpenOptions,
    io::{AsyncReadExt, AsyncWriteExt},
};

type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

#[derive(Parser)]
#[command(
    about = "Run the optional Music Lab activity service. Shared queue prototype; audio is not connected."
)]
struct Args {
    #[arg(long, env = "SERVER_URL", default_value = "ws://127.0.0.1:9000")]
    server: String,
    #[arg(long, env = "MUSIC_KEY_FILE", default_value = "music.key")]
    key_file: PathBuf,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let identity = identity(&args.key_file).await?;
    eprintln!("Music Lab — experimental shared queue; audio playback is not connected.");
    eprintln!(
        "Install this app key in Integrations → Install an app: {}",
        identity.pubkey()
    );
    tokio::select! {
        signal = tokio::signal::ctrl_c() => { signal?; eprintln!("Music service stopped."); Ok(()) }
        result = reconnect(&args.server, &identity) => result,
    }
}

async fn reconnect(server: &str, identity: &BotIdentity) -> Result<()> {
    loop {
        match runtime::connect(server, identity).await {
            Ok(()) => eprintln!("Host connection closed; reconnecting in 5 seconds."),
            Err(_) => eprintln!("Music service connection failed; reconnecting in 5 seconds."),
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}

async fn identity(path: &Path) -> Result<BotIdentity> {
    match tokio::fs::File::open(path).await {
        Ok(file) => {
            if file.metadata().await?.len() > 256 {
                return Err("the existing music key file is too large".into());
            }
            let mut secret = String::new();
            file.take(256).read_to_string(&mut secret).await?;
            Ok(BotIdentity::from_base58_secret(secret.trim())?)
        }
        Err(error) if error.kind() == ErrorKind::NotFound => {
            let identity = BotIdentity::generate();
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            options.mode(0o600);
            let mut file = options.open(path).await?;
            file.write_all(identity.secret_base58().as_bytes()).await?;
            file.sync_all().await?;
            Ok(identity)
        }
        Err(error) => Err(error.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn key_is_stable_and_invalid_file_is_never_replaced() {
        let dir = std::env::temp_dir().join(format!(
            "discordia-music-key-{}",
            discordia_bot::protocol::Id::new_v4()
        ));
        tokio::fs::create_dir(&dir).await.unwrap();
        let path = dir.join("app.key");
        let first = identity(&path).await.unwrap();
        assert_eq!(identity(&path).await.unwrap().pubkey(), first.pubkey());
        tokio::fs::write(&path, "invalid-existing-key")
            .await
            .unwrap();
        assert!(identity(&path).await.is_err());
        assert_eq!(
            tokio::fs::read_to_string(&path).await.unwrap(),
            "invalid-existing-key"
        );
        tokio::fs::remove_file(path).await.unwrap();
        tokio::fs::remove_dir(dir).await.unwrap();
    }
}
