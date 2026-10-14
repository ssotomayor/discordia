mod docker;
mod native;
mod stun;

use std::io::{BufRead, IsTerminal, Write};
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::process::Command;

use discordia_server::livekit_bundle::Credentials;

use crate::options::ConfigFile;

#[derive(clap::Args, Debug, Clone)]
pub struct SetupArgs {
    /// Write a docker compose deployment, pulling the published images, instead of systemd units
    #[arg(long)]
    pub docker: bool,
    /// What this machine runs
    #[arg(long, value_enum)]
    pub role: Option<Role>,
    /// IP or DNS name clients dial [default: this machine's public IPv4]
    #[arg(long)]
    pub public_address: Option<String>,
    /// Hex pubkey allowed to moderate system guilds; repeat for several
    #[arg(long = "operator")]
    pub operators: Vec<String>,
    /// Run the rendezvous's TURN relay, so friends reach the voice of hosts behind NAT [default: true]
    #[arg(long, value_parser = clap::builder::BoolishValueParser::new())]
    pub turn: Option<bool>,
    /// Let the rendezvous hand this machine's SFU to hosts nobody can reach [default: true]
    #[arg(long, value_parser = clap::builder::BoolishValueParser::new())]
    pub shared_voice: Option<bool>,
    /// Docker: where the compose project lives [default: /opt/discordia]
    #[arg(long)]
    pub dir: Option<PathBuf>,
    /// Docker: image tag to pull, `latest` or a short sha
    #[arg(long, default_value = "latest")]
    pub image_tag: String,
    /// Take the default for every question no flag answered
    #[arg(long, short)]
    pub yes: bool,
    /// Print every file and command, change nothing
    #[arg(long)]
    pub dry_run: bool,
}

impl SetupArgs {
    pub fn default_config_path(&self) -> PathBuf {
        if self.docker {
            self.docker_dir().join("discordia.toml")
        } else {
            PathBuf::from(crate::SYSTEM_CONFIG)
        }
    }

    fn docker_dir(&self) -> PathBuf {
        self.dir
            .clone()
            .unwrap_or_else(|| PathBuf::from("/opt/discordia"))
    }
}

#[derive(clap::ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Both,
    Server,
    Rendezvous,
}

impl Role {
    pub fn server(self) -> bool {
        self != Role::Rendezvous
    }
    pub fn rendezvous(self) -> bool {
        self != Role::Server
    }
}

/// Everything setup decided, before anything is written.
#[derive(Debug, Clone)]
pub struct Answers {
    pub role: Role,
    pub public_address: String,
    pub public_ip: Option<IpAddr>,
    pub operators: Vec<String>,
    pub turn: bool,
    pub shared_voice: bool,
    /// Kept across reruns: a fresh pair would orphan every token a host holds.
    pub livekit: Credentials,
    /// What a rerun found, so hand-edited keys survive it.
    pub existing: ConfigFile,
    pub config_path: PathBuf,
}

pub enum Step {
    Write {
        path: PathBuf,
        contents: String,
        mode: u32,
    },
    Run {
        argv: Vec<String>,
        /// Skipped when this succeeds: what makes a step safe to rerun.
        unless: Option<Vec<String>>,
    },
    Note(String),
}

pub fn run_step(argv: &[&str]) -> Step {
    Step::Run {
        argv: argv.iter().map(|s| s.to_string()).collect(),
        unless: None,
    }
}

pub struct Port {
    pub number: String,
    pub proto: &'static str,
    pub why: &'static str,
}

pub fn port(number: impl ToString, proto: &'static str, why: &'static str) -> Port {
    Port {
        number: number.to_string(),
        proto,
        why,
    }
}

pub fn run(args: SetupArgs, config_path: &Path) -> Result<(), String> {
    if !cfg!(unix) {
        return Err(
            "setup installs on Linux; on this machine run `discordia-server` directly".into(),
        );
    }
    if !args.dry_run && !is_root() {
        return Err("setup writes /etc and starts services: run it with sudo".into());
    }
    if !args.docker && !args.dry_run && !Path::new("/run/systemd/system").exists() {
        return Err("no systemd on this machine: use `setup --docker`".into());
    }
    let mut ask = Prompter::new(args.yes)?;
    let answers = gather(&args, config_path, &mut ask)?;

    let (steps, ports) = if args.docker {
        docker::plan(&answers, &args.docker_dir(), &args.image_tag)
    } else {
        native::plan(&answers)?
    };
    let mut steps = steps;
    steps.extend(firewall(&ports));

    println!("\nPlan:");
    for step in &steps {
        match step {
            Step::Write { path, .. } => println!("  write  {}", path.display()),
            Step::Run { argv, .. } => println!("  run    {}", argv.join(" ")),
            Step::Note(note) => println!("  note   {note}"),
        }
    }
    if args.dry_run {
        for step in &steps {
            if let Step::Write { path, contents, .. } = step {
                println!("\n--- {}\n{contents}", path.display());
            }
        }
        return Ok(());
    }
    if !ask.yes_no("Proceed?", true)? {
        return Err("nothing changed".into());
    }
    for step in &steps {
        execute(step)?;
    }
    if args.docker {
        docker::report(&answers, &args.docker_dir());
    } else {
        native::report(&answers);
    }
    Ok(())
}

fn gather(args: &SetupArgs, config_path: &Path, ask: &mut Prompter) -> Result<Answers, String> {
    let existing = if config_path.exists() {
        println!(
            "Found {}; its answers are the defaults.",
            config_path.display()
        );
        ConfigFile::load(config_path)?
    } else {
        ConfigFile::default()
    };
    let old_server = existing.server.clone().unwrap_or_default();
    let old_rendezvous = existing.rendezvous.clone().unwrap_or_default();

    let role = match args.role {
        Some(role) => role,
        None => {
            let default = match (existing.server.is_some(), existing.rendezvous.is_some()) {
                (true, false) => 2,
                (false, true) => 3,
                _ => 1,
            };
            let picked = ask.choice(
                "What should this machine run?",
                &[
                    "server and rendezvous",
                    "server only — a community others join by its quic:// address",
                    "rendezvous only — discovery and relays for hosts behind NAT",
                ],
                default,
            )?;
            [Role::Both, Role::Server, Role::Rendezvous][picked - 1]
        }
    };

    let public_address = match &args.public_address {
        Some(a) => a.clone(),
        None => {
            let previous = old_server
                .public_address
                .clone()
                .or_else(|| old_rendezvous.turn_relay_ip.map(|ip| ip.to_string()));
            let detected = previous.or_else(|| detect_public_ipv4().map(|ip| ip.to_string()));
            ask.text(
                "Public address clients dial (IP or DNS name)",
                detected.as_deref(),
            )?
        }
    };
    let public_ip = resolve_now(&public_address);
    if public_ip.is_none() {
        println!("  {public_address} does not resolve from here; the TURN relay needs it to.");
    }

    let operators = if !args.operators.is_empty() {
        args.operators.clone()
    } else if role.server() {
        let previous = old_server.operators.clone().unwrap_or_default().join(",");
        let typed = ask.text(
            "Operator pubkeys, hex, comma-separated (moderate system guilds; empty for none)",
            Some(previous.as_str()),
        )?;
        typed
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    } else {
        Vec::new()
    };
    if let Some(bad) = operators.iter().find(|k| !is_hex_pubkey(k)) {
        return Err(format!("operator '{bad}' is not a 64-character hex pubkey"));
    }

    let turn = match args.turn {
        Some(t) => t,
        None if role.rendezvous() => ask.yes_no(
            "Run a TURN relay, so friends reach the voice of hosts behind NAT?",
            existing.rendezvous.is_none() || old_rendezvous.turn_url.is_some(),
        )?,
        None => false,
    };
    let shared_voice = match args.shared_voice {
        Some(s) => s && role == Role::Both,
        None if role == Role::Both => ask.yes_no(
            "Offer this machine's voice server to hosts nobody can reach (their calls pass through it)?",
            existing.rendezvous.is_none() || old_rendezvous.livekit_url.is_some(),
        )?,
        None => false,
    };

    let livekit = match old_server
        .livekit_api_key
        .clone()
        .zip(old_server.livekit_api_secret.clone())
        .or_else(|| {
            old_rendezvous
                .livekit_api_key
                .clone()
                .zip(old_rendezvous.livekit_api_secret.clone())
        })
        .or_else(|| docker::env_pair(&args.docker_dir()))
    {
        Some((key, secret)) => Credentials { key, secret },
        None => Credentials::generate(),
    };

    Ok(Answers {
        role,
        public_address,
        public_ip,
        operators,
        turn,
        shared_voice,
        livekit,
        existing,
        config_path: config_path.to_path_buf(),
    })
}

/// Shared by both layouts: they differ in paths and ports, not in what a role means.
pub fn config_for(a: &Answers, layout: Layout) -> ConfigFile {
    let host = crate::serve::bare_host(&a.public_address);
    let relay_url = format!("http://{host}:7701");
    let sfu_url = format!("ws://{host}:7880");

    let server = a.role.server().then(|| {
        let mut s = a.existing.server.clone().unwrap_or_default();
        s.addr = Some(layout.server_addr.parse().unwrap());
        s.quic_port = Some(9001);
        s.data_dir = Some(layout.server_data.into());
        s.public_address = Some(a.public_address.clone());
        s.operators = (!a.operators.is_empty()).then(|| a.operators.clone());
        s.relay_url = a.role.rendezvous().then(|| relay_url.clone());
        match layout.external_sfu {
            true => {
                s.livekit_url = Some(sfu_url.clone());
                s.livekit_autospawn = Some(false);
            }
            false => {
                s.livekit_url = None;
                s.livekit_autospawn = None;
            }
        }
        let pair = layout.keys_in_file && (a.shared_voice || layout.external_sfu);
        s.livekit_api_key = pair.then(|| a.livekit.key.clone());
        s.livekit_api_secret = pair.then(|| a.livekit.secret.clone());
        s
    });

    let rendezvous = a.role.rendezvous().then(|| {
        let mut r = a.existing.rendezvous.clone().unwrap_or_default();
        r.addr = Some("0.0.0.0:7700".parse().unwrap());
        r.data_dir = Some(layout.rendezvous_data.into());
        r.relay_addr = Some("0.0.0.0:7701".parse().unwrap());
        r.relay_url = Some(relay_url.clone());
        r.turn_url = a.turn.then(|| format!("turn:{host}:7702?transport=udp"));
        r.turn_addr = a.turn.then(|| "0.0.0.0:7702".parse().unwrap());
        r.turn_ports = a.turn.then(|| "7710-7809".into());
        r.turn_relay_ip = a.turn.then_some(a.public_ip).flatten();
        r.livekit_url = a.shared_voice.then(|| sfu_url.clone());
        let pair = layout.keys_in_file && a.shared_voice;
        r.livekit_api_key = pair.then(|| a.livekit.key.clone());
        r.livekit_api_secret = pair.then(|| a.livekit.secret.clone());
        r
    });

    ConfigFile { server, rendezvous }
}

pub struct Layout {
    pub server_addr: &'static str,
    pub server_data: &'static str,
    pub rendezvous_data: &'static str,
    /// The SFU is its own container rather than the server's child.
    pub external_sfu: bool,
    /// Otherwise the pair rides in `.env`, where the SFU container reads it too.
    pub keys_in_file: bool,
}

pub fn server_ports(external_sfu: bool) -> Vec<Port> {
    let mut ports = vec![
        port(9001, "udp", "QUIC gateway"),
        port(7880, "tcp", "SFU signaling"),
        port(7881, "tcp", "SFU media over TCP"),
    ];
    ports.push(match external_sfu {
        true => port("50000:50100", "udp", "SFU media"),
        false => port(7882, "udp", "SFU media"),
    });
    ports
}

pub fn rendezvous_ports(turn: bool) -> Vec<Port> {
    let mut ports = vec![
        port(7700, "tcp", "rendezvous"),
        port(7701, "tcp", "iroh relay"),
    ];
    if turn {
        ports.push(port(7702, "udp", "TURN"));
        ports.push(port("7710:7809", "udp", "TURN relayed media"));
    }
    ports
}

fn firewall(ports: &[Port]) -> Vec<Step> {
    let ufw_active = Command::new("ufw")
        .arg("status")
        .output()
        .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains("Status: active"));
    let firewalld = Command::new("firewall-cmd")
        .arg("--state")
        .output()
        .is_ok_and(|o| o.status.success());
    if ufw_active {
        ports
            .iter()
            .map(|p| run_step(&["ufw", "allow", &format!("{}/{}", p.number, p.proto)]))
            .collect()
    } else if firewalld {
        let mut steps: Vec<Step> = ports
            .iter()
            .map(|p| {
                let spec = format!("{}/{}", p.number.replace(':', "-"), p.proto);
                run_step(&["firewall-cmd", "--permanent", &format!("--add-port={spec}")])
            })
            .collect();
        steps.push(run_step(&["firewall-cmd", "--reload"]));
        steps
    } else {
        let list: Vec<String> = ports
            .iter()
            .map(|p| format!("{}/{} ({})", p.number, p.proto, p.why))
            .collect();
        vec![Step::Note(format!(
            "no ufw or firewalld here; open these in your provider's firewall: {}",
            list.join(", ")
        ))]
    }
}

fn execute(step: &Step) -> Result<(), String> {
    match step {
        Step::Write {
            path,
            contents,
            mode,
        } => {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("{}: {e}", parent.display()))?;
            }
            if let Ok(old) = std::fs::read_to_string(path) {
                if old == *contents {
                    return Ok(());
                }
                let backup = path.with_extension(format!(
                    "{}.bak",
                    path.extension().and_then(|e| e.to_str()).unwrap_or("old")
                ));
                std::fs::copy(path, &backup).map_err(|e| format!("{}: {e}", backup.display()))?;
                println!("  kept the previous one as {}", backup.display());
            }
            std::fs::write(path, contents).map_err(|e| format!("{}: {e}", path.display()))?;
            set_mode(path, *mode)
        }
        Step::Run { argv, unless } => {
            if let Some(check) = unless
                && Command::new(&check[0])
                    .args(&check[1..])
                    .output()
                    .is_ok_and(|o| o.status.success())
            {
                return Ok(());
            }
            println!("$ {}", argv.join(" "));
            let status = Command::new(&argv[0])
                .args(&argv[1..])
                .status()
                .map_err(|e| format!("{}: {e}", argv[0]))?;
            if status.success() {
                Ok(())
            } else {
                Err(format!("`{}` failed ({status})", argv.join(" ")))
            }
        }
        Step::Note(note) => {
            println!("note: {note}");
            Ok(())
        }
    }
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
        .map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(not(unix))]
fn set_mode(_: &Path, _: u32) -> Result<(), String> {
    Ok(())
}

fn is_root() -> bool {
    Command::new("id")
        .arg("-u")
        .output()
        .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).trim() == "0")
}

fn is_hex_pubkey(key: &str) -> bool {
    key.len() == 64 && key.bytes().all(|b| b.is_ascii_hexdigit())
}

fn detect_public_ipv4() -> Option<IpAddr> {
    let on_interface = local_ip_address::list_afinet_netifas()
        .ok()?
        .into_iter()
        .map(|(_, ip)| ip)
        .find(|ip| ip.is_ipv4() && !discordia_server::protocol::is_private_ip(*ip));
    on_interface.or_else(stun::public_ipv4)
}

fn resolve_now(name: &str) -> Option<IpAddr> {
    if let Ok(ip) = name.parse() {
        return Some(ip);
    }
    use std::net::ToSocketAddrs;
    let found: Vec<IpAddr> = (name, 0).to_socket_addrs().ok()?.map(|a| a.ip()).collect();
    found
        .iter()
        .find(|ip| ip.is_ipv4())
        .or(found.first())
        .copied()
}

/// Waits up to `secs` for `ready`; false on timeout.
pub fn wait_for(secs: u64, mut ready: impl FnMut() -> bool) -> bool {
    for _ in 0..secs * 2 {
        if ready() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    false
}

struct Prompter {
    yes: bool,
    input: Option<std::io::StdinLock<'static>>,
}

impl Prompter {
    fn new(yes: bool) -> Result<Prompter, String> {
        let stdin = std::io::stdin();
        if !yes && !stdin.is_terminal() {
            return Err(
                "stdin is not a terminal: pass --yes, with the answers as flags (see `setup --help`)"
                    .into(),
            );
        }
        Ok(Prompter {
            yes,
            input: (!yes).then(|| stdin.lock()),
        })
    }

    fn line(&mut self, prompt: &str) -> Result<String, String> {
        print!("{prompt}");
        std::io::stdout().flush().ok();
        let mut line = String::new();
        let input = self.input.as_mut().expect("interactive");
        if input.read_line(&mut line).map_err(|e| e.to_string())? == 0 {
            return Err("input closed".into());
        }
        Ok(line.trim().to_string())
    }

    fn text(&mut self, question: &str, default: Option<&str>) -> Result<String, String> {
        let default = default.filter(|d| !d.is_empty());
        if self.yes {
            return match default {
                Some(d) => Ok(d.to_string()),
                None if question.contains("empty for none") => Ok(String::new()),
                None => Err(format!("no default for \"{question}\": pass it as a flag")),
            };
        }
        loop {
            let shown = default.map(|d| format!(" [{d}]")).unwrap_or_default();
            let typed = self.line(&format!("{question}{shown}: "))?;
            match (typed.is_empty(), default) {
                (false, _) => return Ok(typed),
                (true, Some(d)) => return Ok(d.to_string()),
                (true, None) if question.contains("empty for none") => return Ok(typed),
                (true, None) => continue,
            }
        }
    }

    fn yes_no(&mut self, question: &str, default: bool) -> Result<bool, String> {
        if self.yes {
            return Ok(default);
        }
        let hint = if default { "[Y/n]" } else { "[y/N]" };
        loop {
            match self
                .line(&format!("{question} {hint} "))?
                .to_lowercase()
                .as_str()
            {
                "" => return Ok(default),
                "y" | "yes" => return Ok(true),
                "n" | "no" => return Ok(false),
                _ => continue,
            }
        }
    }

    /// 1-based, as shown.
    fn choice(
        &mut self,
        question: &str,
        options: &[&str],
        default: usize,
    ) -> Result<usize, String> {
        if self.yes {
            return Ok(default);
        }
        println!("{question}");
        for (i, option) in options.iter().enumerate() {
            println!("  {}) {option}", i + 1);
        }
        loop {
            let typed = self.line(&format!("> [{default}] "))?;
            if typed.is_empty() {
                return Ok(default);
            }
            if let Ok(n) = typed.parse::<usize>()
                && (1..=options.len()).contains(&n)
            {
                return Ok(n);
            }
        }
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::options::ServerOptions;

    pub fn answers(role: Role) -> Answers {
        Answers {
            role,
            public_address: "203.0.113.7".into(),
            public_ip: Some("203.0.113.7".parse().unwrap()),
            operators: vec!["ab".repeat(32)],
            turn: true,
            shared_voice: role == Role::Both,
            livekit: Credentials {
                key: "APIkey".into(),
                secret: "secret".into(),
            },
            existing: ConfigFile::default(),
            config_path: PathBuf::from("/etc/discordia/discordia.toml"),
        }
    }

    #[test]
    fn a_rerun_keeps_keys_setup_does_not_manage() {
        let mut a = answers(Role::Server);
        a.existing.server = Some(ServerOptions {
            media_max_bytes: Some(42),
            ..ServerOptions::default()
        });
        let file = config_for(&a, native::LAYOUT);
        assert_eq!(file.server.unwrap().media_max_bytes, Some(42));
        assert!(file.rendezvous.is_none());
    }

    #[test]
    fn both_roles_point_the_server_at_its_own_relay_and_share_the_sfu() {
        let file = config_for(&answers(Role::Both), native::LAYOUT);
        let (s, r) = (file.server.unwrap(), file.rendezvous.unwrap());
        assert_eq!(s.relay_url.as_deref(), Some("http://203.0.113.7:7701"));
        assert_eq!(r.livekit_url.as_deref(), Some("ws://203.0.113.7:7880"));
        assert_eq!(s.livekit_api_key, r.livekit_api_key);
        assert_eq!(
            r.turn_url.as_deref(),
            Some("turn:203.0.113.7:7702?transport=udp")
        );
    }

    #[test]
    fn an_ipv6_address_is_bracketed_in_urls() {
        let mut a = answers(Role::Rendezvous);
        a.public_address = "2001:db8::7".into();
        let r = config_for(&a, native::LAYOUT).rendezvous.unwrap();
        assert_eq!(r.relay_url.as_deref(), Some("http://[2001:db8::7]:7701"));
    }

    #[test]
    fn hex_pubkeys_only() {
        assert!(is_hex_pubkey(&"0f".repeat(32)));
        assert!(!is_hex_pubkey("npub1xyz"));
    }
}
