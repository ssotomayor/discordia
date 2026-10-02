//! Local metadata avoids sending installed-game or running-process information
//! to a third-party catalogue service.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

use crate::protocol::{Activity, ActivityKind};

/// Executable, as the process table spells it, to the name a person recognises.
/// Lowercase keys: Windows reports `Factorio.exe`, Linux reports `factorio`.
const CATALOGUE: &[(&str, &str)] = &[
    ("afop.exe", "Avatar: Frontiers of Pandora"),
    ("factorio", "Factorio"),
    ("factorio.exe", "Factorio"),
    ("stardew valley", "Stardew Valley"),
    ("stardew valley.exe", "Stardew Valley"),
    ("dota2", "Dota 2"),
    ("dota2.exe", "Dota 2"),
    ("cs2", "Counter-Strike 2"),
    ("cs2.exe", "Counter-Strike 2"),
    ("hl2_linux", "Half-Life 2"),
    ("hl2.exe", "Half-Life 2"),
    ("eu4", "Europa Universalis IV"),
    ("eu4.exe", "Europa Universalis IV"),
    ("rimworldlinux", "RimWorld"),
    ("rimworldwin64.exe", "RimWorld"),
    ("hollow_knight", "Hollow Knight"),
    ("hollow_knight.exe", "Hollow Knight"),
    ("celeste", "Celeste"),
    ("celeste.exe", "Celeste"),
    ("balatro", "Balatro"),
    ("balatro.exe", "Balatro"),
    ("minecraft", "Minecraft"),
    ("terraria", "Terraria"),
    ("terraria.exe", "Terraria"),
];

pub fn set_override(
    extra: &mut Vec<(String, String)>,
    exe: &str,
    title: &str,
) -> Result<(), &'static str> {
    let exe = exe.trim().to_ascii_lowercase();
    let title = title.trim();
    if exe.is_empty()
        || exe.len() > 240
        || exe.contains(['/', '\\'])
        || exe.chars().any(char::is_control)
        || title.is_empty()
        || title.chars().count() > 80
        || title.chars().any(char::is_control)
    {
        return Err("Enter an executable filename and a game name (up to 80 characters).");
    }
    if let Some((_, name)) = extra
        .iter_mut()
        .find(|(key, _)| key.eq_ignore_ascii_case(&exe))
    {
        *name = title.to_owned();
    } else if extra.len() < 128 {
        extra.push((exe, title.to_owned()));
    } else {
        return Err("You can add up to 128 additional games.");
    }
    Ok(())
}

pub struct Detector {
    system: System,
    catalogue: HashMap<String, String>,
    extra: Vec<(String, String)>,
    installed: Vec<super::installed::Game>,
    refreshed: Instant,
}

impl Detector {
    /// `extra` is the person's own list, and it wins: they added it because we
    /// got their machine wrong.
    pub fn new(extra: &[(String, String)]) -> Self {
        let mut catalogue: HashMap<String, String> = CATALOGUE
            .iter()
            .map(|(exe, name)| (exe.to_string(), name.to_string()))
            .collect();
        for (exe, name) in extra {
            let exe = exe.trim().to_ascii_lowercase();
            let name = name.trim();
            if !exe.is_empty() && !name.is_empty() {
                catalogue.insert(exe, name.to_string());
            }
        }
        Self {
            system: System::new(),
            catalogue,
            extra: extra.to_vec(),
            installed: super::installed::discover(),
            refreshed: Instant::now(),
        }
    }

    pub fn update_extra(&mut self, extra: &[(String, String)]) {
        if self.extra == extra {
            return;
        }
        self.catalogue = CATALOGUE
            .iter()
            .map(|(exe, name)| (exe.to_string(), name.to_string()))
            .collect();
        for (exe, name) in extra {
            let exe = exe.trim().to_ascii_lowercase();
            let name = name.trim();
            if !exe.is_empty() && !name.is_empty() {
                self.catalogue.insert(exe, name.to_owned());
            }
        }
        self.extra = extra.to_vec();
    }

    /// The longest-running match, so alt-tabbing to a launcher that is also on
    /// the list does not keep rewriting what someone has played for an hour.
    pub fn scan(&mut self) -> Option<Activity> {
        if self.refreshed.elapsed() >= Duration::from_secs(300) {
            self.installed = super::installed::discover();
            self.refreshed = Instant::now();
        }
        self.system.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing().with_exe(UpdateKind::OnlyIfNotSet),
        );

        let mut best: Option<(u64, &str)> = None;
        for process in self.system.processes().values() {
            let Some(name) = self.lookup(process) else {
                continue;
            };
            let started = process.start_time();
            if best.map(|(prev, _)| started < prev).unwrap_or(true) {
                best = Some((started, name));
            }
        }

        best.map(|(started, name)| Activity {
            kind: ActivityKind::Playing,
            name: name.to_string(),
            details: None,
            state: None,
            started_ms: Some(started as i64 * 1000),
        })
    }

    /// The file name, never the path: two people install the same game in two
    /// places, and only the leaf is the same on both.
    fn lookup(&self, process: &sysinfo::Process) -> Option<&str> {
        let from_exe = process
            .exe()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().to_ascii_lowercase());
        let from_name = process.name().to_string_lossy().to_ascii_lowercase();
        from_exe
            .and_then(|e| self.catalogue.get(&e))
            .or_else(|| self.catalogue.get(&from_name))
            .map(String::as_str)
            .or_else(|| {
                process
                    .exe()
                    .and_then(|path| super::installed::lookup(&self.installed, path))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_games_update_existing_names_and_reject_paths_or_invalid_titles() {
        let mut extra = Vec::new();
        set_override(&mut extra, " AFOP.EXE ", "Avatar").unwrap();
        set_override(&mut extra, "afop.exe", "Avatar: Frontiers of Pandora").unwrap();
        assert_eq!(extra.len(), 1);
        assert_eq!(extra[0].0, "afop.exe");
        for (exe, name) in [
            ("C:\\game.exe", "Game"),
            ("../game", "Game"),
            ("game.exe", ""),
            ("game.exe", "bad\nname"),
        ] {
            assert!(set_override(&mut extra, exe, name).is_err());
        }
        assert_eq!(extra.len(), 1);
    }

    #[test]
    fn avatar_is_recognized_and_custom_names_can_be_updated_without_restarting() {
        let mut detector = Detector::new(&[]);
        assert_eq!(
            detector.catalogue.get("afop.exe").map(String::as_str),
            Some("Avatar: Frontiers of Pandora")
        );
        detector.update_extra(&[(" AFOP.EXE ".into(), "My Avatar".into())]);
        assert_eq!(
            detector.catalogue.get("afop.exe").map(String::as_str),
            Some("My Avatar")
        );
        detector.update_extra(&[]);
        assert_eq!(
            detector.catalogue.get("afop.exe").map(String::as_str),
            Some("Avatar: Frontiers of Pandora")
        );
    }

    #[test]
    #[ignore = "requires a running game and access to native process metadata"]
    fn live_game_detection() {
        let activity = Detector::new(&[])
            .scan()
            .expect("a running game should be detected");
        println!("Detected game: {}", activity.name);
        assert_eq!(activity.name, "Avatar: Frontiers of Pandora");
    }
}
