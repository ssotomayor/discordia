use std::{collections::BTreeMap, sync::OnceLock};

use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Language {
    English,
    Spanish,
    Portuguese,
    #[default]
    #[serde(other)]
    System,
}

impl Language {
    pub fn resolve(self) -> Self {
        if self == Self::System {
            static SYSTEM: OnceLock<Language> = OnceLock::new();
            *SYSTEM.get_or_init(|| from_tag(&system_language()))
        } else {
            self
        }
    }

    pub fn tag(self) -> &'static str {
        match self.resolve() {
            Self::Spanish => "es",
            Self::Portuguese => "pt-BR",
            _ => "en",
        }
    }

    fn value(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::English => "en",
            Self::Spanish => "es",
            Self::Portuguese => "pt-BR",
        }
    }
}

fn from_tag(tag: &str) -> Language {
    match tag
        .split(['-', '_', '.', '@'])
        .next()
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "es" => Language::Spanish,
        "pt" => Language::Portuguese,
        _ => Language::English,
    }
}

#[cfg(target_os = "windows")]
fn system_language() -> String {
    use windows::{
        Win32::Globalization::{GetUserPreferredUILanguages, MUI_LANGUAGE_NAME},
        core::PWSTR,
    };
    let (mut count, mut size) = (0, 0);
    // SAFETY: the first call queries the required UTF-16 length without supplying a buffer.
    if unsafe { GetUserPreferredUILanguages(MUI_LANGUAGE_NAME, &mut count, None, &mut size) }
        .is_ok()
        && (1..=65536).contains(&size)
    {
        let mut preferred = vec![0_u16; size as usize];
        // SAFETY: the buffer has the length requested by Windows and stays live through the call.
        if unsafe {
            GetUserPreferredUILanguages(
                MUI_LANGUAGE_NAME,
                &mut count,
                Some(PWSTR(preferred.as_mut_ptr())),
                &mut size,
            )
        }
        .is_ok()
        {
            let end = preferred.iter().position(|value| *value == 0).unwrap_or(0);
            if end > 0 {
                return String::from_utf16_lossy(&preferred[..end]);
            }
        }
    }
    let mut buffer = [0_u16; 85];
    // SAFETY: Windows writes at most the length of this live UTF-16 buffer.
    let length = unsafe { windows::Win32::Globalization::GetUserDefaultLocaleName(&mut buffer) };
    String::from_utf16_lossy(&buffer[..length.saturating_sub(1) as usize])
}

#[cfg(target_os = "macos")]
fn system_language() -> String {
    objc2_foundation::NSLocale::preferredLanguages()
        .firstObject()
        .map(|language| language.to_string())
        .unwrap_or_default()
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn system_language() -> String {
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .into_iter()
        .filter_map(|name| std::env::var(name).ok())
        .find(|value| !value.is_empty())
        .unwrap_or_default()
}

#[derive(Clone, Copy)]
pub struct Locale(pub Signal<Language>);

#[derive(Clone, Copy)]
pub struct WindowTitle(pub &'static str);

pub fn current() -> Language {
    if dioxus::core::Runtime::try_current()
        .and_then(|runtime| runtime.try_current_scope_id())
        .is_some()
        && let Some(locale) = try_consume_context::<Locale>()
    {
        return *locale.0.read();
    }
    Language::English
}

fn catalog() -> &'static BTreeMap<String, [String; 2]> {
    static CATALOG: OnceLock<BTreeMap<String, [String; 2]>> = OnceLock::new();
    CATALOG.get_or_init(|| {
        serde_json::from_str(include_str!("../locales/messages.json")).unwrap_or_else(|error| {
            tracing::error!(%error, "invalid embedded translation catalog");
            BTreeMap::new()
        })
    })
}

pub fn text(source: &str, arguments: &[String]) -> String {
    translate(current(), source, arguments)
}

pub fn label(source: &str) -> &str {
    match current().resolve() {
        Language::Spanish => catalog().get(source).map(|entry| entry[0].as_str()),
        Language::Portuguese => catalog().get(source).map(|entry| entry[1].as_str()),
        _ => None,
    }
    .filter(|value| !value.is_empty())
    .unwrap_or(source)
}

pub fn translate(language: Language, source: &str, arguments: &[String]) -> String {
    let translated = match language.resolve() {
        Language::Spanish => catalog().get(source).map(|entry| entry[0].as_str()),
        Language::Portuguese => catalog().get(source).map(|entry| entry[1].as_str()),
        _ => None,
    }
    .filter(|value| !value.is_empty())
    .unwrap_or(source);
    interpolate(translated, arguments)
}

fn interpolate(template: &str, arguments: &[String]) -> String {
    let mut output = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        output.push_str(&rest[..start]);
        rest = &rest[start..];
        if let Some(end) = rest.find('}')
            && let Ok(index) = rest[1..end].parse::<usize>()
            && let Some(value) = arguments.get(index)
        {
            output.push_str(value);
            rest = &rest[end + 1..];
        } else {
            output.push('{');
            rest = &rest[1..];
        }
    }
    output.push_str(rest);
    output
}

pub fn plural(one: &str, other: &str, count: usize) -> String {
    text(if count == 1 { one } else { other }, &[count.to_string()])
}

pub fn message(source: &str) -> String {
    message_in(current(), source)
}

fn message_in(language: Language, source: &str) -> String {
    if language == Language::English || catalog().contains_key(source) {
        return translate(language, source, &[]);
    }
    // Legacy services report English strings; match only complete, known templates at the UI boundary.
    for template in catalog().keys().filter(|key| key.contains("{0}")) {
        let prefix = template.split('{').next().unwrap_or("");
        if prefix.len() < 8 || !source.starts_with(prefix) {
            continue;
        }
        if let Some(arguments) = capture(template, source) {
            return translate(language, template, &arguments);
        }
    }
    source.to_owned()
}

fn capture(template: &str, source: &str) -> Option<Vec<String>> {
    let mut rest = source;
    let mut pattern = template;
    let mut arguments = Vec::<String>::new();
    while let Some(start) = pattern.find('{') {
        rest = rest.strip_prefix(&pattern[..start])?;
        let end = pattern[start..].find('}')? + start;
        let index = pattern[start + 1..end].parse::<usize>().ok()?;
        pattern = &pattern[end + 1..];
        let next = pattern.split('{').next()?;
        let size = if next.is_empty() {
            rest.len()
        } else {
            rest.find(next)?
        };
        let value = &rest[..size];
        if index < arguments.len() {
            if arguments[index] != value {
                return None;
            }
        } else if index == arguments.len() {
            arguments.push(value.to_owned());
        } else {
            return None;
        }
        rest = &rest[size..];
    }
    (rest == pattern).then_some(arguments)
}

#[component]
pub fn LanguageSelector() -> Element {
    let mut settings = use_context::<Signal<crate::settings::ClientSettings>>();
    let mut locale = use_context::<Locale>().0;
    rsx! {
        label { class: "flex flex-wrap items-center gap-2 text-xs text-[var(--text-muted)]",
            span { {text("Language", &[])} }
            select {
                class: "bg-[var(--panel2)] text-[var(--text)] border border-[var(--border)] rounded-lg px-3 py-2 max-w-full",
                aria_label: text("Language", &[]),
                value: settings.read().language.value(),
                onchange: move |event: FormEvent| {
                    let language = match event.value().as_str() {
                        "es" => Language::Spanish,
                        "pt-BR" => Language::Portuguese,
                        "en" => Language::English,
                        _ => Language::System,
                    };
                    settings.write().language = language;
                    locale.set(language.resolve());
                    crate::settings::save(&settings.peek());
                },
                option { value: "system", {text("Use system language", &[])} }
                option { value: "en", {crate::i18n::text("English", &[])} }
                option { value: "es", {crate::i18n::text("Español", &[])} }
                option { value: "pt-BR", {crate::i18n::text("Português (Brasil)", &[])} }
            }
        }
    }
}

#[component]
pub fn LanguageDocument() -> Element {
    let window = dioxus::desktop::use_window();
    let title = try_consume_context::<WindowTitle>();
    use_effect(move || {
        let language = current();
        if let Some(title) = title {
            window.set_title(label(title.0));
        }
        document::eval(&format!(
            "document.documentElement.lang = '{}';",
            language.tag()
        ));
    });
    rsx! {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regional_languages_and_unsupported_languages() {
        for tag in ["es-AR", "es_ES.UTF-8", "ES-mx"] {
            assert_eq!(from_tag(tag), Language::Spanish);
        }
        for tag in ["pt-BR", "pt_PT.UTF-8"] {
            assert_eq!(from_tag(tag), Language::Portuguese);
        }
        for tag in ["en-US", "de-DE", "C", ""] {
            assert_eq!(from_tag(tag), Language::English);
        }
    }

    #[test]
    fn interpolation_keeps_user_content_literal() {
        assert_eq!(
            interpolate("{1}: {0} / {0}", &["Ana {1}".into(), "Hola".into()]),
            "Hola: Ana {1} / Ana {1}"
        );
        assert_eq!(
            translate(Language::Spanish, "Unknown {0}", &["value".into()]),
            "Unknown value"
        );
    }

    #[test]
    fn legacy_errors_preserve_details_and_require_a_complete_match() {
        assert_eq!(
            message_in(Language::Spanish, "Couldn't start your camera: device {0}"),
            "No se pudo iniciar tu cámara: device {0}"
        );
        assert_eq!(
            message_in(Language::Portuguese, "Couldn't start your camera: device"),
            "Não foi possível iniciar sua câmera: device"
        );
        assert_eq!(
            message_in(Language::Spanish, "Unknown error from a new server"),
            "Unknown error from a new server"
        );
        assert_eq!(capture("Hello {0} / {0}", "Hello Ana / Bob"), None);
    }

    #[test]
    fn old_and_future_settings_preserve_unrelated_preferences() {
        let mut value = serde_json::to_value(crate::settings::ClientSettings::default()).unwrap();
        value.as_object_mut().unwrap().remove("language");
        value["theme"] = serde_json::json!("paper");
        let old: crate::settings::ClientSettings = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(old.language, Language::System);
        assert_eq!(old.theme, "paper");
        value["language"] = serde_json::json!("future-language");
        let future: crate::settings::ClientSettings = serde_json::from_value(value).unwrap();
        assert_eq!(future.language, Language::System);
        assert_eq!(future.theme, "paper");
        for language in [Language::English, Language::Spanish, Language::Portuguese] {
            let saved = serde_json::to_string(&language).unwrap();
            assert_eq!(serde_json::from_str::<Language>(&saved).unwrap(), language);
        }
    }

    #[tokio::test]
    async fn one_preference_updates_multiple_windows_without_recreating_them() {
        use std::{cell::RefCell, rc::Rc};
        type Output = Rc<RefCell<Vec<String>>>;
        type Export = Rc<RefCell<Option<Signal<Language>>>>;
        fn probe() -> Element {
            let output = consume_context::<Output>();
            output.borrow_mut().push(format!(
                "{} / {}",
                text("Settings", &[]),
                plural("{0} key", "{0} keys", 1)
            ));
            rsx! { div { {text("Settings", &[])} } }
        }
        fn owner(export: Export) -> Element {
            let locale = use_hook(|| Signal::new_in_scope(Language::English, ScopeId::ROOT));
            use_drop(move || locale.manually_drop());
            use_context_provider(|| Locale(locale));
            *export.borrow_mut() = Some(locale);
            probe()
        }
        let exported = Export::default();
        let main_output = Output::default();
        let popup_output = Output::default();
        let mut main = VirtualDom::new_with_props(owner, exported.clone())
            .with_root_context(main_output.clone());
        main.rebuild_in_place();
        let mut locale = exported.borrow().unwrap();
        let mut popup = VirtualDom::new(probe)
            .with_root_context(Locale(locale))
            .with_root_context(popup_output.clone());
        popup.rebuild_in_place();
        for (language, expected) in [
            (Language::Spanish, "Ajustes / 1 clave"),
            (Language::Portuguese, "Configurações / 1 chave"),
            (Language::English, "Settings / 1 key"),
        ] {
            locale.set(language);
            for window in [&mut main, &mut popup] {
                tokio::time::timeout(std::time::Duration::from_secs(1), window.wait_for_work())
                    .await
                    .unwrap();
                window.render_immediate_to_vec();
            }
            assert_eq!(main_output.borrow().last().unwrap(), expected);
            assert_eq!(popup_output.borrow().last().unwrap(), expected);
        }
    }

    #[test]
    fn catalog_is_valid_and_preserves_arguments() {
        let parsed: BTreeMap<String, [String; 2]> =
            serde_json::from_str(include_str!("../locales/messages.json")).unwrap();
        assert!(!parsed.is_empty());
        for (source, translations) in parsed {
            for translation in translations {
                assert!(!translation.trim().is_empty(), "{source}");
                for index in 0..20 {
                    let marker = format!("{{{index}}}");
                    assert_eq!(
                        source.matches(&marker).count(),
                        translation.matches(&marker).count(),
                        "{source}: {translation}"
                    );
                }
            }
        }
    }
}
