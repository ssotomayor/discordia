use std::collections::HashSet;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use serde_json::Value;

const MAX_MANIFEST_BYTES: u64 = 2 * 1024 * 1024;
const MAX_GAMES: usize = 4096;

pub(super) struct Game {
    root: PathBuf,
    pub name: String,
}

impl Game {
    fn new(root: PathBuf, name: &str) -> Option<Self> {
        let name: String = name
            .trim()
            .chars()
            .filter(|c| !c.is_control())
            .take(80)
            .collect();
        if !root.is_absolute() || root.parent().is_none() || name.is_empty() {
            return None;
        }
        Some(Self {
            root: normalized(&root),
            name,
        })
    }

    #[cfg(test)]
    fn matches(&self, exe: &Path) -> bool {
        lookup(std::slice::from_ref(self), exe).is_some()
    }
}

pub(super) fn lookup<'a>(games: &'a [Game], exe: &Path) -> Option<&'a str> {
    if games.is_empty()
        || exe
            .file_name()
            .is_none_or(|name| helper(&name.to_string_lossy()))
    {
        return None;
    }
    let exe = normalized(exe);
    games
        .iter()
        .find(|game| exe.starts_with(&game.root))
        .map(|game| game.name.as_str())
}

fn normalized(path: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        PathBuf::from(path.to_string_lossy().replace('/', "\\").to_lowercase())
    }
    #[cfg(not(windows))]
    {
        path.to_path_buf()
    }
}

fn helper(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    [
        "launcher",
        "crashreport",
        "crash_report",
        "crashhandler",
        "crashpad",
        "crashclient",
        "helper",
        "updater",
        "uninstall",
        "unins",
        "anticheat",
        "anti-cheat",
        "overlay",
        "redist",
        "vcredist",
        "dxsetup",
        "unitycrashhandler",
        "steamservice",
        "beservice",
    ]
    .iter()
    .any(|needle| name.contains(needle))
        || matches!(
            name.as_str(),
            "chrome.exe" | "msedge.exe" | "firefox.exe" | "cef.exe" | "upc.exe" | "steam.exe"
        )
}

fn read_manifest(path: &Path) -> Option<String> {
    let file = std::fs::File::open(path).ok()?;
    if file.metadata().ok()?.len() > MAX_MANIFEST_BYTES {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(MAX_MANIFEST_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > MAX_MANIFEST_BYTES {
        return None;
    }
    String::from_utf8(bytes).ok()
}

fn relative(path: &str) -> Option<&Path> {
    let path = Path::new(path);
    (!path.as_os_str().is_empty() && path.components().all(|c| matches!(c, Component::Normal(_))))
        .then_some(path)
}

fn steam_game(library: &Path, manifest: &str) -> Option<Game> {
    let doc = keyvalues(manifest)?;
    let app = doc.get("AppState").or_else(|| doc.get("appstate"))?;
    let name = app.get("name")?.as_str()?;
    let lower = name.to_ascii_lowercase();
    if lower.contains("redistributables")
        || lower.starts_with("proton")
        || lower.starts_with("steam linux runtime")
        || [
            "blender",
            "wallpaper engine",
            "lossless scaling",
            "aseprite",
            "krita",
            "obs studio",
            "animaze",
            "rpg maker",
            "game maker",
            "gamemaker",
            "godot",
            "unreal engine",
            "unity",
        ]
        .iter()
        .any(|software| lower == *software || lower.starts_with(&format!("{software} ")))
    {
        return None;
    }
    Game::new(
        library
            .join("steamapps/common")
            .join(relative(app.get("installdir")?.as_str()?)?),
        name,
    )
}

#[cfg(any(windows, test))]
fn epic_game(manifest: &str) -> Option<Game> {
    let doc: Value = serde_json::from_str(manifest).ok()?;
    if doc.get("bIsIncompleteInstall").and_then(Value::as_bool) == Some(true)
        || !doc
            .get("AppCategories")?
            .as_array()?
            .iter()
            .any(|c| c.as_str() == Some("games"))
    {
        return None;
    }
    relative(doc.get("LaunchExecutable")?.as_str()?)?;
    Game::new(
        PathBuf::from(doc.get("InstallLocation")?.as_str()?),
        doc.get("DisplayName")?.as_str()?,
    )
}

pub(super) fn discover() -> Vec<Game> {
    let mut roots = steam_roots();
    let mut libraries = roots.clone();
    for root in roots.drain(..) {
        for location in ["steamapps/libraryfolders.vdf", "config/libraryfolders.vdf"] {
            let Some(doc) = read_manifest(&root.join(location)).and_then(|text| keyvalues(&text))
            else {
                continue;
            };
            let Some(folders) = doc
                .get("libraryfolders")
                .or_else(|| doc.get("LibraryFolders"))
                .and_then(Value::as_object)
            else {
                continue;
            };
            for (key, folder) in folders {
                if key.parse::<u32>().is_err() {
                    continue;
                }
                if let Some(path) = folder
                    .as_str()
                    .or_else(|| folder.get("path").and_then(Value::as_str))
                {
                    let path = PathBuf::from(path);
                    if path.is_absolute() && libraries.len() < 64 {
                        libraries.push(path);
                    }
                }
            }
        }
    }
    let mut seen = HashSet::new();
    let mut games = Vec::new();
    for library in libraries {
        if !seen.insert(normalized(&library)) {
            continue;
        }
        collect_manifests(&library.join("steamapps"), "acf", &mut games, |text| {
            steam_game(&library, text)
        });
    }
    #[cfg(windows)]
    {
        if let Some(base) = std::env::var_os("PROGRAMDATA") {
            collect_manifests(
                &PathBuf::from(base).join("Epic/EpicGamesLauncher/Data/Manifests"),
                "item",
                &mut games,
                epic_game,
            );
        }
        for root in registry::ubisoft_roots() {
            if games.len() >= MAX_GAMES {
                break;
            }
            if let Some(game) = root
                .file_name()
                .and_then(|name| Game::new(root.clone(), &name.to_string_lossy()))
            {
                games.push(game);
            }
        }
    }
    games.sort_by_key(|game| std::cmp::Reverse(game.root.components().count()));
    games
}

fn collect_manifests(
    dir: &Path,
    extension: &str,
    games: &mut Vec<Game>,
    parse: impl Fn(&str) -> Option<Game>,
) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten().take(MAX_GAMES) {
        if games.len() >= MAX_GAMES {
            break;
        }
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some(extension)
            || (extension == "acf"
                && !path
                    .file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with("appmanifest_")))
        {
            continue;
        }
        if let Some(game) = read_manifest(&path).and_then(|text| parse(&text)) {
            games.push(game);
        }
    }
}

fn steam_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    #[cfg(windows)]
    {
        roots.extend(registry::steam_roots());
        for key in ["ProgramFiles(x86)", "ProgramFiles"] {
            if let Some(base) = std::env::var_os(key) {
                roots.push(PathBuf::from(base).join("Steam"));
            }
        }
    }
    #[cfg(not(windows))]
    if let Some(base) = std::env::var_os("HOME") {
        let base = PathBuf::from(base);
        for path in [
            ".steam/steam",
            ".local/share/Steam",
            ".var/app/com.valvesoftware.Steam/.local/share/Steam",
            "Library/Application Support/Steam",
        ] {
            roots.push(base.join(path));
        }
    }
    roots
}

fn keyvalues(text: &str) -> Option<Value> {
    enum Token {
        Text(String),
        Open,
        Close,
    }
    let mut chars = text.chars().peekable();
    let mut tokens = Vec::new();
    while let Some(c) = chars.next() {
        match c {
            c if c.is_whitespace() => {}
            '/' if chars.peek() == Some(&'/') => {
                for c in chars.by_ref() {
                    if c == '\n' {
                        break;
                    }
                }
            }
            '{' => tokens.push(Token::Open),
            '}' => tokens.push(Token::Close),
            '"' => {
                let mut value = String::new();
                let mut closed = false;
                while let Some(c) = chars.next() {
                    match c {
                        '"' => {
                            closed = true;
                            break;
                        }
                        '\\' => {
                            let escaped = chars.next()?;
                            if escaped != '\\' && escaped != '"' {
                                value.push('\\');
                            }
                            value.push(escaped);
                        }
                        c => value.push(c),
                    }
                }
                if !closed {
                    return None;
                }
                tokens.push(Token::Text(value));
            }
            _ => return None,
        }
    }
    fn object(tokens: &[Token], offset: &mut usize, depth: usize) -> Option<Value> {
        if depth > 16 {
            return None;
        }
        let mut map = serde_json::Map::new();
        while let Some(key) = tokens.get(*offset) {
            let key = match key {
                Token::Text(key) => key,
                Token::Close => break,
                Token::Open => return None,
            };
            *offset += 1;
            let value = tokens.get(*offset)?;
            *offset += 1;
            let value = match value {
                Token::Open => {
                    let value = object(tokens, offset, depth + 1)?;
                    if !matches!(tokens.get(*offset)?, Token::Close) {
                        return None;
                    }
                    *offset += 1;
                    value
                }
                Token::Close => return None,
                Token::Text(value) => Value::String(value.clone()),
            };
            map.insert(key.clone(), value);
        }
        Some(Value::Object(map))
    }
    let mut offset = 0;
    let value = object(&tokens, &mut offset, 0)?;
    (offset == tokens.len()).then_some(value)
}

#[cfg(windows)]
mod registry {
    use super::*;
    use windows::Win32::Foundation::ERROR_SUCCESS;
    use windows::Win32::System::Registry::*;
    use windows::core::{PCWSTR, PWSTR};

    fn wide(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(Some(0)).collect()
    }

    struct Key(HKEY);
    impl Drop for Key {
        fn drop(&mut self) {
            // SAFETY: Only successfully opened keys are owned here.
            let result = unsafe { RegCloseKey(self.0) };
            if result != ERROR_SUCCESS {
                tracing::debug!(code = result.0, "presence registry key close failed");
            }
        }
    }

    fn open(root: HKEY, path: &str, access: REG_SAM_FLAGS) -> Option<Key> {
        let path = wide(path);
        let mut key = HKEY::default();
        // SAFETY: The terminated path and output handle remain valid for this call.
        let result =
            unsafe { RegOpenKeyExW(root, PCWSTR(path.as_ptr()), Some(0), access, &mut key) };
        if result == ERROR_SUCCESS {
            Some(Key(key))
        } else {
            None
        }
    }

    fn value(key: HKEY, child: &str, name: &str) -> Option<PathBuf> {
        let child = wide(child);
        let name = wide(name);
        let mut data = [0u16; 4096];
        let mut bytes = std::mem::size_of_val(&data) as u32;
        // SAFETY: The byte capacity matches the live UTF-16 buffer; inputs are terminated.
        let result = unsafe {
            RegGetValueW(
                key,
                PCWSTR(child.as_ptr()),
                PCWSTR(name.as_ptr()),
                RRF_RT_REG_SZ,
                None,
                Some(data.as_mut_ptr().cast()),
                Some(&mut bytes),
            )
        };
        if result != ERROR_SUCCESS
            || bytes as usize > std::mem::size_of_val(&data)
            || !bytes.is_multiple_of(2)
        {
            return None;
        }
        let data = &data[..bytes as usize / 2];
        let len = data.iter().position(|c| *c == 0).unwrap_or(data.len());
        let text = String::from_utf16(&data[..len]).ok()?;
        (!text.is_empty()).then(|| PathBuf::from(text))
    }

    pub fn steam_roots() -> Vec<PathBuf> {
        let mut roots = Vec::new();
        if let Some(path) = value(HKEY_CURRENT_USER, "Software\\Valve\\Steam", "SteamPath") {
            roots.push(path);
        }
        if let Some(key) = open(
            HKEY_LOCAL_MACHINE,
            "SOFTWARE\\Valve\\Steam",
            KEY_READ | KEY_WOW64_32KEY,
        ) && let Some(path) = value(key.0, "", "InstallPath")
        {
            roots.push(path);
        }
        roots
    }

    pub fn ubisoft_roots() -> Vec<PathBuf> {
        let mut roots = Vec::new();
        let Some(key) = open(
            HKEY_LOCAL_MACHINE,
            "SOFTWARE\\Ubisoft\\Launcher\\Installs",
            KEY_READ | KEY_WOW64_32KEY,
        ) else {
            return roots;
        };
        for index in 0..MAX_GAMES as u32 {
            let mut name = [0u16; 256];
            let mut len = name.len() as u32;
            // SAFETY: The name buffer matches the advertised capacity; the key is owned.
            let result = unsafe {
                RegEnumKeyExW(
                    key.0,
                    index,
                    Some(PWSTR(name.as_mut_ptr())),
                    &mut len,
                    None,
                    None,
                    None,
                    None,
                )
            };
            if result != ERROR_SUCCESS || len as usize > name.len() {
                break;
            }
            if let Ok(child) = String::from_utf16(&name[..len as usize])
                && let Some(path) = value(key.0, &child, "InstallDir")
            {
                roots.push(path);
            }
        }
        roots
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires Avatar running and access to native installation metadata"]
    fn live_game_installation_matches_current_process() {
        let games = discover();
        let processes = super::super::processes::ProcessTable::default().scan();
        let matched = processes
            .iter()
            .filter(|process| process.name.eq_ignore_ascii_case("afop.exe"))
            .filter_map(|process| process.exe.as_deref().and_then(|exe| lookup(&games, exe)))
            .next();
        assert!(
            matched.is_some(),
            "Avatar's process must match the local Ubisoft installation"
        );
        println!("Local installation matched: {}", matched.unwrap());
    }

    #[test]
    fn steam_metadata_matches_game_binaries_but_not_helpers_or_neighbouring_folders() {
        let root = std::env::temp_dir().join("SteamLibrary");
        let game = steam_game(
            &root,
            r#""AppState" { "name" "A Game" "installdir" "A Game" }"#,
        )
        .unwrap();
        let install = root.join("steamapps/common/A Game");
        assert!(game.matches(&install.join("Binaries/Win64/Game-Win64-Shipping.exe")));
        assert!(!game.matches(&install.join("crash_reporter.exe")));
        assert!(!game.matches(&install.join("Launcher.exe")));
        assert!(game.matches(&install.join("CrashBandicoot.exe")));
        assert!(!game.matches(&root.join("steamapps/common/A Game Other/game.exe")));
    }

    #[test]
    fn manifests_reject_traversal_tools_and_incomplete_installs() {
        let root = std::env::temp_dir();
        assert!(
            steam_game(
                &root,
                r#""AppState" { "name" "Game" "installdir" "../escape" }"#
            )
            .is_none()
        );
        for software in [
            "Blender",
            "WallPaper Engine",
            "RPG Maker MZ",
            "Steam Linux Runtime 3.0 (sniper)",
        ] {
            let manifest =
                format!(r#""AppState" {{ "name" "{software}" "installdir" "Software" }}"#);
            assert!(steam_game(&root, &manifest).is_none());
        }
        assert!(
            steam_game(
                &root,
                r#""AppState" { "name" "Proton 9" "installdir" "Proton" }"#
            )
            .is_none()
        );
        let metadata = serde_json::json!({ "DisplayName": "Game", "InstallLocation": root, "LaunchExecutable": "game.exe", "AppCategories": ["games"], "bIsIncompleteInstall": true });
        assert!(epic_game(&metadata.to_string()).is_none());
    }

    #[test]
    fn epic_metadata_names_games_and_rejects_non_game_applications() {
        let root = std::env::temp_dir().join("EpicGame");
        let mut metadata = serde_json::json!({ "DisplayName": "Shogun Showdown", "InstallLocation": root, "LaunchExecutable": "ShogunShowdown.exe", "AppCategories": ["games"] });
        let game = epic_game(&metadata.to_string()).unwrap();
        assert_eq!(game.name, "Shogun Showdown");
        assert!(game.matches(&root.join("ShogunShowdown.exe")));
        metadata["AppCategories"] = serde_json::json!(["applications"]);
        assert!(epic_game(&metadata.to_string()).is_none());
    }

    #[test]
    fn steam_keyvalues_handles_nested_libraries_escapes_comments_and_invalid_input() {
        let doc = keyvalues(
            r#"// libraries
            "libraryfolders" { "0" { "path" "D:\\SteamLibrary" "apps" { "123" "10" } } }"#,
        )
        .unwrap();
        assert_eq!(doc["libraryfolders"]["0"]["path"], r"D:\SteamLibrary");
        assert_eq!(keyvalues(r#""brace" "{""#).unwrap()["brace"], "{");
        for malformed in ["\"unterminated", "\"a\" {", "\"a\" }", "}", "\"a\" \"b\" }"] {
            assert!(keyvalues(malformed).is_none());
        }
    }
}
