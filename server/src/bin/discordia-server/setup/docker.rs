use std::path::Path;

use super::{Answers, Layout, Port, Step, rendezvous_ports, run_step, server_ports};

pub const LAYOUT: Layout = Layout {
    server_addr: "0.0.0.0:9000",
    server_data: "/data",
    rendezvous_data: "/data",
    external_sfu: true,
    keys_in_file: false,
};

const IMAGES: &str = "ghcr.io/ssotomayor/discordia";
/// The version the bundled SFU pins in `server/build.rs`.
const LIVEKIT_IMAGE: &str = "livekit/livekit-server:v1.13.9";
const SFU_CONFIG: &str = include_str!("../../../../../deploy/livekit.yaml");

pub fn plan(a: &Answers, dir: &Path, tag: &str) -> (Vec<Step>, Vec<Port>) {
    let compose = dir.join("docker-compose.yml").to_string_lossy().to_string();
    let mut steps = vec![
        Step::Write {
            path: a.config_path.clone(),
            contents: super::config_for(a, LAYOUT).render(),
            mode: 0o600,
        },
        Step::Write {
            path: dir.join("docker-compose.yml"),
            contents: compose_file(a, tag),
            mode: 0o644,
        },
    ];
    if a.role.server() || a.shared_voice {
        steps.push(Step::Write {
            path: dir.join(".env"),
            contents: format!(
                "LIVEKIT_API_KEY={}\nLIVEKIT_API_SECRET={}\n",
                a.livekit.key, a.livekit.secret
            ),
            mode: 0o600,
        });
    }
    if a.role.server() {
        steps.push(Step::Write {
            path: dir.join("livekit.yaml"),
            contents: SFU_CONFIG.to_string(),
            mode: 0o644,
        });
    }
    steps.push(run_step(&["docker", "compose", "-f", &compose, "pull"]));
    steps.push(run_step(&[
        "docker",
        "compose",
        "-f",
        &compose,
        "up",
        "-d",
        "--remove-orphans",
    ]));

    let mut ports = Vec::new();
    if a.role.server() {
        ports.extend(server_ports(true));
    }
    if a.role.rendezvous() {
        ports.extend(rendezvous_ports(a.turn));
    }
    (steps, ports)
}

/// `name:` fixes the project, so volumes never depend on which directory or `-p` ran it.
fn compose_file(a: &Answers, tag: &str) -> String {
    let mut out = String::from(
        "# Written by `discordia-server setup --docker`; rerun it rather than editing this.\n\
         name: discordia\n\
         services:\n",
    );
    if a.role.server() {
        out.push_str(&format!(
            "  discordia:\n\
             \x20   image: {IMAGES}-server:{tag}\n\
             \x20   restart: unless-stopped\n\
             \x20   ports:\n\
             \x20     - \"127.0.0.1:9000:9000\"\n\
             \x20     - \"9001:9001/udp\"\n\
             \x20   environment:\n\
             \x20     DISCORDIA_CONFIG: /etc/discordia/discordia.toml\n\
             \x20     LIVEKIT_API_KEY: ${{LIVEKIT_API_KEY:?}}\n\
             \x20     LIVEKIT_API_SECRET: ${{LIVEKIT_API_SECRET:?}}\n\
             \x20   volumes:\n\
             \x20     - ./discordia.toml:/etc/discordia/discordia.toml:ro\n\
             \x20     - discordia-data:/data\n\
             \x20 livekit:\n\
             \x20   image: {LIVEKIT_IMAGE}\n\
             \x20   restart: unless-stopped\n\
             \x20   network_mode: host\n\
             \x20   command: --config /etc/livekit.yaml\n\
             \x20   environment:\n\
             \x20     LIVEKIT_KEYS: \"${{LIVEKIT_API_KEY:?}}: ${{LIVEKIT_API_SECRET:?}}\"\n\
             \x20   volumes:\n\
             \x20     - ./livekit.yaml:/etc/livekit.yaml:ro\n"
        ));
    }
    if a.role.rendezvous() {
        out.push_str(&format!(
            "  rendezvous:\n\
             \x20   image: {IMAGES}-rendezvous:{tag}\n\
             \x20   restart: unless-stopped\n\
             \x20   ports:\n\
             \x20     - \"7700:7700\"\n\
             \x20     - \"7701:7701\"\n"
        ));
        if a.turn {
            out.push_str(
                "      - \"7702:7702/udp\"\n\
                 \x20     - \"7710-7809:7710-7809/udp\"\n",
            );
        }
        out.push_str(
            "    environment:\n\
             \x20     DISCORDIA_CONFIG: /etc/discordia/discordia.toml\n",
        );
        if a.shared_voice {
            out.push_str(
                "      LIVEKIT_API_KEY: ${LIVEKIT_API_KEY:?}\n\
                 \x20     LIVEKIT_API_SECRET: ${LIVEKIT_API_SECRET:?}\n",
            );
        }
        out.push_str(
            "    volumes:\n\
             \x20     - ./discordia.toml:/etc/discordia/discordia.toml:ro\n\
             \x20     - rendezvous-data:/data\n",
        );
    }
    out.push_str("volumes:\n");
    if a.role.server() {
        out.push_str("  discordia-data:\n");
    }
    if a.role.rendezvous() {
        out.push_str("  rendezvous-data:\n");
    }
    out
}

/// A rerun reads the pair back from `.env`, where this layout keeps it.
pub fn env_pair(dir: &Path) -> Option<(String, String)> {
    let text = std::fs::read_to_string(dir.join(".env")).ok()?;
    let get = |name: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix(name)?.strip_prefix('='))
            .map(|v| v.trim().trim_matches('"').to_string())
            .filter(|v| !v.is_empty())
    };
    get("LIVEKIT_API_KEY").zip(get("LIVEKIT_API_SECRET"))
}

pub fn report(a: &Answers, dir: &Path) {
    let compose = dir.join("docker-compose.yml").to_string_lossy().to_string();
    println!();
    if a.role.server() {
        let share = super::wait_for(30, || {
            std::process::Command::new("docker")
                .args(["compose", "-f", &compose, "exec", "-T", "discordia"])
                .args(["discordia-server", "share"])
                .output()
                .is_ok_and(|o| o.status.success())
        })
        .then(|| {
            std::process::Command::new("docker")
                .args(["compose", "-f", &compose, "exec", "-T", "discordia"])
                .args(["discordia-server", "share"])
                .output()
                .ok()
        })
        .flatten()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
        match share {
            Some(s) => {
                println!("Server      {s}\n            paste it in the client: + → Join a server")
            }
            None => println!(
                "Server      not up after 30 s: docker compose -f {compose} logs discordia"
            ),
        }
    }
    if a.role.rendezvous() {
        let host = crate::serve::bare_host(&a.public_address);
        println!(
            "Rendezvous  ws://{host}:7700\n            add it in the client: Settings → Rendezvous servers"
        );
    }
    println!(
        "\nConfig      {}  (edit, then: docker compose -f {compose} up -d --force-recreate)\n\
         Update      docker compose -f {compose} pull && docker compose -f {compose} up -d",
        a.config_path.display(),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::setup::{Role, tests::answers};

    #[test]
    fn a_rendezvous_alone_needs_no_sfu_and_no_secret() {
        let mut a = answers(Role::Rendezvous);
        a.config_path = "/opt/discordia/discordia.toml".into();
        let (steps, _) = plan(&a, Path::new("/opt/discordia"), "latest");
        let written: Vec<String> = steps
            .iter()
            .filter_map(|s| match s {
                Step::Write { path, .. } => Some(path.to_string_lossy().to_string()),
                _ => None,
            })
            .collect();
        assert_eq!(
            written,
            [
                "/opt/discordia/discordia.toml",
                "/opt/discordia/docker-compose.yml"
            ]
        );
        let compose = compose_file(&a, "latest");
        assert!(!compose.contains("livekit"));
        assert!(compose.contains("7710-7809:7710-7809/udp"));
    }

    #[test]
    fn the_server_reaches_its_sfu_container_by_the_public_address() {
        let s = crate::setup::config_for(&answers(Role::Both), LAYOUT)
            .server
            .unwrap();
        assert_eq!(s.livekit_url.as_deref(), Some("ws://203.0.113.7:7880"));
        assert_eq!(s.livekit_autospawn, Some(false));
        assert_eq!(s.livekit_api_secret, None);
        let compose = compose_file(&answers(Role::Both), "abc1234");
        assert!(compose.contains("discordia-server:abc1234"));
        assert!(compose.contains("\"127.0.0.1:9000:9000\""));
    }

    #[test]
    fn the_pair_reads_back_from_env() {
        let dir = std::env::temp_dir().join(format!("setup-env-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(".env"),
            "LIVEKIT_API_KEY=k\nLIVEKIT_API_SECRET=\"s\"\n",
        )
        .unwrap();
        assert_eq!(env_pair(&dir), Some(("k".into(), "s".into())));
        std::fs::remove_dir_all(&dir).ok();
    }
}
