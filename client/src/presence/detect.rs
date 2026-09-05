//! What is running right now, named. The scan is ours; the catalogue is a
//! small committed list plus whatever the person added themselves, because we
//! host no equivalent of Discord's `applications/detectable` and fetching
//! theirs at runtime would tell them what our users play.

use std::collections::HashMap;

use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

use crate::protocol::{Activity, ActivityKind};

/// Executable, as the process table spells it, to the name a person recognises.
/// Lowercase keys: Windows reports `Factorio.exe`, Linux reports `factorio`.
const CATALOGUE: &[(&str, &str)] = &[
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
    ("javaw.exe", "Minecraft"),
    ("terraria", "Terraria"),
    ("terraria.exe", "Terraria"),
];

pub struct Detector {
    system: System,
    catalogue: HashMap<String, String>,
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
        }
    }

    /// The longest-running match, so alt-tabbing to a launcher that is also on
    /// the list does not keep rewriting what someone has played for an hour.
    pub fn scan(&mut self) -> Option<Activity> {
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
    }
}
