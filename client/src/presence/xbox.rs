use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

use quick_xml::Reader;
use quick_xml::events::Event;

pub(super) struct Game {
    pub name: Option<String>,
    pub artwork: Vec<PathBuf>,
}

fn relative_path(value: &str) -> Option<PathBuf> {
    let path = PathBuf::from(value.replace('\\', "/"));
    (!value.contains(':')
        && !path.as_os_str().is_empty()
        && path
            .components()
            .all(|part| matches!(part, Component::Normal(_))))
    .then_some(path)
}

fn title(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty() && !value.starts_with("ms-resource:"))
        .then(|| value.chars().filter(|c| !c.is_control()).take(80).collect())
}

fn game_config(root: &Path, exe: &Path, xml: &str) -> Option<Game> {
    let mut reader = Reader::from_str(xml.trim_start_matches('\u{feff}'));
    let mut parents = Vec::<Vec<u8>>::new();
    let mut shell = None;
    let mut executable = None;
    loop {
        let event = reader.read_event().ok()?;
        let is_start = matches!(event, Event::Start(_));
        match event {
            Event::Start(node) | Event::Empty(node) => {
                let name = node.local_name().as_ref().to_vec();
                let mut attributes = HashMap::new();
                for attribute in node.attributes() {
                    let attribute = attribute.ok()?;
                    attributes.insert(
                        String::from_utf8(attribute.key.as_ref().to_vec()).ok()?,
                        attribute
                            .decoded_and_normalized_value(
                                quick_xml::XmlVersion::Implicit1_0,
                                reader.decoder(),
                            )
                            .ok()?
                            .into_owned(),
                    );
                }
                if parents.len() == 1 && parents[0] == b"Game" && name == b"ShellVisuals" {
                    shell = Some(attributes);
                } else if parents.len() == 2
                    && parents[0] == b"Game"
                    && parents[1] == b"ExecutableList"
                    && name == b"Executable"
                    && attributes
                        .get("Name")
                        .and_then(|n| relative_path(n))
                        .is_some_and(|p| same_path(&root.join(p), exe))
                {
                    executable = Some(attributes);
                }
                // Empty elements must not become parents of their next sibling.
                if is_start {
                    parents.push(name);
                    if parents.len() > 32 {
                        return None;
                    }
                }
            }
            Event::End(_) => {
                parents.pop()?;
            }
            Event::DocType(_) => return None,
            Event::Eof => break,
            _ => {}
        }
    }
    if !parents.is_empty() {
        return None;
    }
    let shell = shell?;
    let executable = executable.unwrap_or_default();
    let name = executable
        .get("OverrideDisplayName")
        .or_else(|| shell.get("DefaultDisplayName"))
        .and_then(|s| title(s));
    let mut artwork = Vec::new();
    for (overridden, standard) in [
        ("OverrideLogo", "Square150x150Logo"),
        ("OverrideSquare480x480Logo", "Square480x480Logo"),
        ("OverrideSquare44x44Logo", "Square44x44Logo"),
        ("", "StoreLogo"),
    ] {
        if let Some(path) = executable
            .get(overridden)
            .or_else(|| shell.get(standard))
            .and_then(|s| relative_path(s))
        {
            artwork.push(root.join(path));
        }
    }
    Some(Game { name, artwork })
}

fn same_path(a: &Path, b: &Path) -> bool {
    #[cfg(windows)]
    {
        a.to_string_lossy()
            .eq_ignore_ascii_case(&b.to_string_lossy())
    }
    #[cfg(not(windows))]
    {
        a == b
    }
}

pub(super) fn from_executable(exe: &Path) -> Option<Game> {
    if !exe.is_absolute() || super::installed::helper(&exe.file_name()?.to_string_lossy()) {
        return None;
    }
    for root in exe
        .parent()?
        .ancestors()
        .take(8)
        .filter(|p| p.parent().is_some())
    {
        let Some(xml) = super::installed::read_manifest(&root.join("MicrosoftGame.config")) else {
            continue;
        };
        if let Some(mut game) = game_config(root, exe, &xml) {
            game.artwork.retain(|asset| contained(root, asset));
            return Some(game);
        }
    }
    None
}

fn contained(root: &Path, asset: &Path) -> bool {
    root.canonicalize()
        .ok()
        .zip(asset.canonicalize().ok())
        .is_some_and(|(root, asset)| asset.starts_with(root))
}

fn package_logos(root: &Path, exe: &Path, xml: &str) -> Option<Vec<PathBuf>> {
    let mut reader = Reader::from_str(xml.trim_start_matches('\u{feff}'));
    let mut matches = false;
    let mut logos = Vec::new();
    loop {
        match reader.read_event().ok()? {
            Event::Start(node) | Event::Empty(node) => {
                let mut attrs = HashMap::new();
                for attr in node.attributes() {
                    let attr = attr.ok()?;
                    attrs.insert(
                        String::from_utf8(attr.key.as_ref().to_vec()).ok()?,
                        attr.decoded_and_normalized_value(
                            quick_xml::XmlVersion::Implicit1_0,
                            reader.decoder(),
                        )
                        .ok()?
                        .into_owned(),
                    );
                }
                if node.local_name().as_ref() == b"Application" {
                    matches = attrs
                        .get("Executable")
                        .and_then(|s| relative_path(s))
                        .is_some_and(|p| same_path(&root.join(p), exe));
                }
                if matches && node.local_name().as_ref() == b"VisualElements" {
                    for key in ["Square150x150Logo", "Square44x44Logo"] {
                        if let Some(path) = attrs.get(key).and_then(|s| relative_path(s)) {
                            logos.push(root.join(path));
                        }
                    }
                }
            }
            Event::End(node) if node.local_name().as_ref() == b"Application" => {
                matches = false;
            }
            Event::DocType(_) => return None,
            Event::Eof => return Some(logos),
            _ => {}
        }
    }
}

pub(super) fn package_artwork(exe: &Path) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    let Some(parent) = exe.parent() else {
        return candidates;
    };
    for root in parent.ancestors().take(8).filter(|p| p.parent().is_some()) {
        let Some(logos) = super::installed::read_manifest(&root.join("AppxManifest.xml"))
            .and_then(|xml| package_logos(root, exe, &xml))
        else {
            continue;
        };
        for path in logos {
            if contained(root, &path) {
                candidates.push(path.clone());
            }
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str())
                && let Some(dir) = path.parent()
                && let Ok(entries) = std::fs::read_dir(dir)
            {
                let mut variants: Vec<_> = entries
                    .flatten()
                    .take(256)
                    .map(|entry| entry.path())
                    .filter(|p| {
                        p.extension().is_some_and(|e| e == "png")
                            && p.file_stem().is_some_and(|name| {
                                let name = name.to_string_lossy();
                                name.starts_with(&format!("{stem}.scale-"))
                                    || name.starts_with(&format!("{stem}.targetsize-"))
                            })
                            && contained(root, p)
                    })
                    .collect();
                variants.sort();
                candidates.extend(variants);
            }
        }
        break;
    }
    candidates
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xbox_uses_the_matching_executable_override_and_decodes_entities() {
        let root = std::env::temp_dir().join("XboxGames/Game/Content");
        let game = game_config(&root, &root.join("Binaries/game.exe"), r#"<Game>
            <ShellVisuals DefaultDisplayName="Game &amp; Friends" Square150x150Logo="Assets\logo.png"/>
            <ExecutableList>
                <Executable Name="other.exe" OverrideDisplayName="Wrong game" OverrideLogo="wrong.png"/>
                <Executable Name="Binaries\game.exe" OverrideDisplayName="Correct game" OverrideLogo="correct.png"/>
            </ExecutableList></Game>"#).unwrap();
        assert_eq!(game.name.as_deref(), Some("Correct game"));
        assert_eq!(game.artwork[0], root.join("correct.png"));
        let game = game_config(&root, &root.join("game.exe"), r#"<Game><ShellVisuals DefaultDisplayName="Game &amp; Friends" Square150x150Logo="Assets\logo.png"/></Game>"#).unwrap();
        assert_eq!(game.name.as_deref(), Some("Game & Friends"));
        assert_eq!(game.artwork[0], root.join("Assets/logo.png"));
    }

    #[test]
    fn xbox_rejects_escaping_assets_malformed_xml_and_external_entities() {
        for path in [
            "../logo.png",
            "Assets/../../secret.png",
            "C:\\secret.png",
            "//server/logo.png",
        ] {
            assert!(relative_path(path).is_none(), "{path}");
        }
        let root = std::env::temp_dir();
        for xml in [
            "<Game><ShellVisuals/></Wrong>",
            "<!DOCTYPE Game SYSTEM 'file:///secret'><Game/>",
            "<Other><ShellVisuals DefaultDisplayName='Wrong'/></Other>",
        ] {
            assert!(game_config(&root, &root.join("game.exe"), xml).is_none());
        }
    }

    #[test]
    fn installed_xbox_metadata_works_above_the_binary_and_skips_helpers() {
        let root = std::env::temp_dir().join(format!("discordia-xbox-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("Binaries")).unwrap();
        std::fs::write(root.join("MicrosoftGame.config"), "<Game><ShellVisuals DefaultDisplayName='Xbox Game' Square150x150Logo='logo.png'/></Game>").unwrap();
        std::fs::write(root.join("logo.png"), [0]).unwrap();
        let game = from_executable(&root.join("Binaries/game.exe")).unwrap();
        assert_eq!(game.name.as_deref(), Some("Xbox Game"));
        assert_eq!(game.artwork, [root.join("logo.png")]);
        assert!(from_executable(&root.join("gamelaunchhelper.exe")).is_none());
        std::fs::remove_file(root.join("logo.png")).unwrap();
        std::fs::remove_file(root.join("MicrosoftGame.config")).unwrap();
        std::fs::remove_dir(root.join("Binaries")).unwrap();
        std::fs::remove_dir(root).unwrap();
    }

    #[test]
    fn legacy_xbox_package_art_matches_the_application_not_its_neighbour() {
        let root = std::env::temp_dir();
        let xml = r#"<Package xmlns:uap="urn:schemas-microsoft-com:universal-application-package">
            <Applications>
                <Application Executable="other.exe"><uap:VisualElements Square150x150Logo="wrong.png"/></Application>
                <Application Executable="Game.exe"><uap:VisualElements Square150x150Logo="Assets\Logo.png"/></Application>
            </Applications></Package>"#;
        assert_eq!(
            package_logos(&root, &root.join("Game.exe"), xml).unwrap(),
            [root.join("Assets/Logo.png")]
        );
        assert!(
            package_logos(&root, &root.join("unknown.exe"), xml)
                .unwrap()
                .is_empty()
        );
    }
}
