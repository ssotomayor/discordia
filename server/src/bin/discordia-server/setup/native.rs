use std::path::{Path, PathBuf};

use super::{Answers, Layout, Port, Role, Step, rendezvous_ports, run_step, server_ports};

pub const LAYOUT: Layout = Layout {
    server_addr: "127.0.0.1:9000",
    server_data: "/var/lib/discordia/server",
    rendezvous_data: "/var/lib/discordia/rendezvous",
    external_sfu: false,
    keys_in_file: true,
};

const BIN: &str = "/usr/local/bin/discordia-server";
const UNITS: &str = "/etc/systemd/system";

pub fn plan(a: &Answers) -> Result<(Vec<Step>, Vec<Port>), String> {
    let exe = std::env::current_exe().map_err(|e| format!("cannot find this binary: {e}"))?;
    let mut steps = Vec::new();

    if exe.canonicalize().ok() != Path::new(BIN).canonicalize().ok() {
        steps.push(run_step(&[
            "install",
            "-m",
            "0755",
            &exe.to_string_lossy(),
            BIN,
        ]));
    }
    steps.push(Step::Run {
        argv: [
            "useradd",
            "--system",
            "--user-group",
            "--home-dir",
            "/var/lib/discordia",
            "--shell",
            "/usr/sbin/nologin",
            "discordia",
        ]
        .map(String::from)
        .to_vec(),
        unless: Some(vec!["id".into(), "-u".into(), "discordia".into()]),
    });

    let config = a.config_path.to_string_lossy().to_string();
    steps.push(Step::Write {
        path: a.config_path.clone(),
        contents: super::config_for(a, LAYOUT).render(),
        mode: 0o640,
    });
    steps.push(run_step(&["chgrp", "discordia", &config]));

    let mut ports = Vec::new();
    let mut enable = Vec::new();
    for (wanted, name, sub, what) in [
        (a.role.server(), "discordia-server", "", "server"),
        (
            a.role.rendezvous(),
            "discordia-rendezvous",
            "rendezvous ",
            "rendezvous",
        ),
    ] {
        let path = PathBuf::from(UNITS).join(format!("{name}.service"));
        if wanted {
            steps.push(Step::Write {
                contents: unit(what, &format!("{BIN} {sub}--config {config}")),
                path,
                mode: 0o644,
            });
            enable.push(name);
        } else if path.exists() {
            steps.push(run_step(&["systemctl", "disable", "--now", name]));
        }
    }
    if a.role.server() {
        ports.extend(server_ports(false));
    }
    if a.role.rendezvous() {
        ports.extend(rendezvous_ports(a.turn));
    }

    steps.push(run_step(&["systemctl", "daemon-reload"]));
    let mut argv = vec!["systemctl", "enable"];
    argv.extend(&enable);
    steps.push(run_step(&argv));
    let mut argv = vec!["systemctl", "restart"];
    argv.extend(&enable);
    steps.push(run_step(&argv));
    Ok((steps, ports))
}

/// The bundled SFU unpacks into the state directory, the one place it may write.
fn unit(what: &str, exec: &str) -> String {
    format!(
        "# Written by `discordia-server setup`.\n\
         [Unit]\n\
         Description=Discordia {what}\n\
         After=network-online.target\n\
         Wants=network-online.target\n\
         \n\
         [Service]\n\
         ExecStart={exec}\n\
         User=discordia\n\
         Group=discordia\n\
         StateDirectory=discordia/{what}\n\
         StateDirectoryMode=0700\n\
         Restart=on-failure\n\
         RestartSec=5\n\
         LimitNOFILE=65536\n\
         NoNewPrivileges=yes\n\
         ProtectSystem=strict\n\
         ProtectHome=yes\n\
         PrivateTmp=yes\n\
         PrivateDevices=yes\n\
         ProtectKernelTunables=yes\n\
         ProtectKernelModules=yes\n\
         ProtectControlGroups=yes\n\
         RestrictSUIDSGID=yes\n\
         \n\
         [Install]\n\
         WantedBy=multi-user.target\n"
    )
}

pub fn report(a: &Answers) {
    println!();
    if a.role.server() {
        let opts = super::config_for(a, LAYOUT).server.unwrap_or_default();
        let key = Path::new(LAYOUT.server_data).join("quic-secret");
        let share = super::wait_for(30, || key.exists())
            .then(|| crate::share(&opts).ok())
            .flatten();
        match share {
            Some(s) => {
                println!("Server      {s}\n            paste it in the client: + → Join a server")
            }
            None => println!("Server      not up after 30 s: journalctl -u discordia-server -n 50"),
        }
    }
    if a.role.rendezvous() {
        let up = super::wait_for(15, || {
            std::net::TcpStream::connect(("127.0.0.1", 7700)).is_ok()
        });
        let host = crate::serve::bare_host(&a.public_address);
        match up {
            true => println!(
                "Rendezvous  ws://{host}:7700\n            add it in the client: Settings → Rendezvous servers"
            ),
            false => {
                println!("Rendezvous  not up after 15 s: journalctl -u discordia-rendezvous -n 50")
            }
        }
    }
    let units = match a.role {
        Role::Both => "discordia-server discordia-rendezvous",
        Role::Server => "discordia-server",
        Role::Rendezvous => "discordia-rendezvous",
    };
    println!(
        "\nConfig      {}  (edit, then: systemctl restart {units})\n\
         Logs        journalctl -fu {}\n\
         Back up     {}/quic-secret: it is the key in the share string",
        a.config_path.display(),
        units.split(' ').next().unwrap_or(units),
        LAYOUT.server_data,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::setup::tests::answers;

    #[test]
    fn the_server_binds_plaintext_to_loopback_only() {
        let s = crate::setup::config_for(&answers(Role::Server), LAYOUT)
            .server
            .unwrap();
        assert!(s.addr.unwrap().ip().is_loopback());
        assert_eq!(s.livekit_url, None);
    }

    #[test]
    fn units_run_the_installed_binary_against_the_written_config() {
        let (steps, ports) = plan(&answers(Role::Both)).unwrap();
        let units: Vec<&String> = steps
            .iter()
            .filter_map(|s| match s {
                Step::Write { path, contents, .. } if path.starts_with(UNITS) => Some(contents),
                _ => None,
            })
            .collect();
        assert_eq!(units.len(), 2);
        assert!(units[1].contains(
            "ExecStart=/usr/local/bin/discordia-server rendezvous --config /etc/discordia/discordia.toml"
        ));
        assert!(ports.iter().any(|p| p.number == "9001" && p.proto == "udp"));
        assert!(!ports.iter().any(|p| p.number == "9000"));
    }
}
