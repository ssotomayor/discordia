use serde::Serialize;
use std::{error::Error, fmt};
use url::Url;

const MAX_LINK_BYTES: usize = 2048;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    Youtube,
    Soundcloud,
    Spotify,
    Tidal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Track,
    Playlist,
    Album,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Source {
    provider: Provider,
    kind: Kind,
    canonical_url: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InvalidSource;

impl fmt::Display for InvalidSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("use a public HTTPS track, playlist or album link from YouTube, SoundCloud, Spotify or TIDAL; shortened redirect links are not supported")
    }
}

impl Error for InvalidSource {}

impl Source {
    pub fn parse(input: &str) -> Result<Self, InvalidSource> {
        if input.len() > MAX_LINK_BYTES || input.chars().any(char::is_control) {
            return Err(InvalidSource);
        }
        let input = input.trim();
        let url = Url::parse(input).map_err(|_| InvalidSource)?;
        if url.scheme() != "https"
            || !url.username().is_empty()
            || url.password().is_some()
            || url.port().is_some()
        {
            return Err(InvalidSource);
        }
        let host = url.host_str().ok_or(InvalidSource)?;
        let path: Vec<_> = url.path().trim_matches('/').split('/').collect();
        let (provider, kind, canonical_url) = match host {
            "youtube.com" | "www.youtube.com" | "m.youtube.com" | "music.youtube.com" => {
                let pairs: Vec<_> = url.query_pairs().collect();
                let field = |name: &str| {
                    let values: Vec<_> = pairs.iter().filter(|(k, _)| k == name).collect();
                    (values.len() == 1).then(|| values[0].1.as_ref())
                };
                let video = match path.as_slice() {
                    ["watch"] => field("v"),
                    ["shorts" | "live" | "embed", id] => Some(*id),
                    _ => None,
                };
                if let Some(id) = video {
                    if !youtube_id(id, 11, 11) {
                        return Err(InvalidSource);
                    }
                    (
                        Provider::Youtube,
                        Kind::Track,
                        format!("https://www.youtube.com/watch?v={id}"),
                    )
                } else if path == ["playlist"] {
                    let id = field("list").ok_or(InvalidSource)?;
                    if !youtube_id(id, 10, 128) {
                        return Err(InvalidSource);
                    }
                    (
                        Provider::Youtube,
                        Kind::Playlist,
                        format!("https://www.youtube.com/playlist?list={id}"),
                    )
                } else {
                    return Err(InvalidSource);
                }
            }
            "youtu.be" => {
                let [id] = path.as_slice() else {
                    return Err(InvalidSource);
                };
                if !youtube_id(id, 11, 11) {
                    return Err(InvalidSource);
                }
                (
                    Provider::Youtube,
                    Kind::Track,
                    format!("https://www.youtube.com/watch?v={id}"),
                )
            }
            "open.spotify.com" => {
                let path = match path.as_slice() {
                    [locale, rest @ ..] if locale.starts_with("intl-") => rest,
                    rest => rest,
                };
                let [kind, id] = path else {
                    return Err(InvalidSource);
                };
                if id.len() != 22 || !id.bytes().all(|c| c.is_ascii_alphanumeric()) {
                    return Err(InvalidSource);
                }
                let kind = parse_kind(kind)?;
                (
                    Provider::Spotify,
                    kind,
                    format!("https://open.spotify.com/{}/{id}", kind.path()),
                )
            }
            "tidal.com" | "www.tidal.com" | "listen.tidal.com" => {
                let path = path.strip_prefix(&["browse"]).unwrap_or(&path);
                let [kind, id] = path else {
                    return Err(InvalidSource);
                };
                let kind = parse_kind(kind)?;
                let valid = if kind == Kind::Playlist {
                    id.len() == 36
                        && id.bytes().enumerate().all(|(i, c)| {
                            if [8, 13, 18, 23].contains(&i) {
                                c == b'-'
                            } else {
                                c.is_ascii_hexdigit()
                            }
                        })
                } else {
                    !id.is_empty() && id.len() <= 20 && id.bytes().all(|c| c.is_ascii_digit())
                };
                if !valid {
                    return Err(InvalidSource);
                }
                (
                    Provider::Tidal,
                    kind,
                    format!(
                        "https://tidal.com/browse/{}/{}",
                        kind.path(),
                        id.to_ascii_lowercase()
                    ),
                )
            }
            "soundcloud.com" | "www.soundcloud.com" => {
                let (kind, parts) = match path.as_slice() {
                    [user, "sets", list] => (Kind::Playlist, vec![*user, "sets", *list]),
                    [user, track] => (Kind::Track, vec![*user, *track]),
                    _ => return Err(InvalidSource),
                };
                if !parts.iter().all(|part| slug(part))
                    || ["discover", "search", "you", "settings", "oauth", "charts"]
                        .contains(&parts[0])
                {
                    return Err(InvalidSource);
                }
                (
                    Provider::Soundcloud,
                    kind,
                    format!("https://soundcloud.com/{}", parts.join("/")),
                )
            }
            _ => return Err(InvalidSource),
        };
        Ok(Self {
            provider,
            kind,
            canonical_url,
        })
    }

    pub fn provider(&self) -> Provider {
        self.provider
    }
    pub fn kind(&self) -> Kind {
        self.kind
    }
    pub fn canonical_url(&self) -> &str {
        &self.canonical_url
    }

    pub(crate) fn metadata_endpoint(&self) -> Option<&'static str> {
        match (self.provider, self.kind) {
            (Provider::Youtube, Kind::Track) => Some("https://www.youtube.com/oembed"),
            (Provider::Soundcloud, _) => Some("https://soundcloud.com/oembed"),
            (Provider::Spotify, _) => Some("https://open.spotify.com/oembed"),
            _ => None,
        }
    }
}

impl Kind {
    fn path(self) -> &'static str {
        match self {
            Self::Track => "track",
            Self::Playlist => "playlist",
            Self::Album => "album",
        }
    }
}

fn parse_kind(value: &str) -> Result<Kind, InvalidSource> {
    match value {
        "track" => Ok(Kind::Track),
        "playlist" => Ok(Kind::Playlist),
        "album" => Ok(Kind::Album),
        _ => Err(InvalidSource),
    }
}

fn youtube_id(value: &str, min: usize, max: usize) -> bool {
    (min..=max).contains(&value.len())
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
}

fn slug(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 255
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_platforms_and_collections() {
        for (link, provider, kind) in [
            (
                "https://youtu.be/dQw4w9WgXcQ?si=tracking",
                Provider::Youtube,
                Kind::Track,
            ),
            (
                "https://music.youtube.com/playlist?list=PL1234567890",
                Provider::Youtube,
                Kind::Playlist,
            ),
            (
                "https://soundcloud.com/forss/flickermood",
                Provider::Soundcloud,
                Kind::Track,
            ),
            (
                "https://soundcloud.com/forss/sets/example",
                Provider::Soundcloud,
                Kind::Playlist,
            ),
            (
                "https://open.spotify.com/intl-es/track/4uLU6hMCjMI75M1A2tKUQC?si=tracking",
                Provider::Spotify,
                Kind::Track,
            ),
            (
                "https://open.spotify.com/album/4uLU6hMCjMI75M1A2tKUQC",
                Provider::Spotify,
                Kind::Album,
            ),
            (
                "https://listen.tidal.com/album/123",
                Provider::Tidal,
                Kind::Album,
            ),
            (
                "https://tidal.com/browse/playlist/01234567-89ab-cdef-0123-456789abcdef",
                Provider::Tidal,
                Kind::Playlist,
            ),
        ] {
            let source = Source::parse(link).unwrap();
            assert_eq!((source.provider(), source.kind()), (provider, kind));
            assert!(!source.canonical_url().contains("tracking"));
        }
    }

    #[test]
    fn rejects_untrusted_or_ambiguous_links() {
        for link in [
            "http://www.youtube.com/watch?v=dQw4w9WgXcQ",
            "file:///music.wav",
            "https://127.0.0.1/track/123",
            "https://[::1]/track/123",
            "https://soundcloud.com.evil.test/user/song",
            "https://soundcloud.com@evil.test/user/song",
            "https://user:password@soundcloud.com/user/song",
            "https://soundcloud.com:444/user/song",
            "https://soundcloud.com/user/%2e%2e",
            "https://soundcloud.com/user/track%2fsecret",
            "https://soundcloud.com/user/track\n",
            "https://soundcloud.com/search/sounds",
            "https://spotify.link/redirect",
            "https://on.soundcloud.com/redirect",
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ&v=AAAAAAAAAAA",
            "https://www.youtube.com/watch?v=invalid",
            "https://open.spotify.com/artist/4uLU6hMCjMI75M1A2tKUQC",
            "https://tidal.com/track/not-a-number",
            "https://tidal.com/playlist/not-a-uuid",
        ] {
            assert!(Source::parse(link).is_err(), "accepted {link}");
        }
    }

    #[test]
    fn video_with_playlist_keeps_only_selected_track() {
        let source = Source::parse(
            "https://youtube.com/watch?v=dQw4w9WgXcQ&list=PL1234567890&t=30#fragment",
        )
        .unwrap();
        assert_eq!(source.kind(), Kind::Track);
        assert_eq!(
            source.canonical_url(),
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ"
        );
    }

    #[test]
    fn rejects_oversize_input() {
        assert!(
            Source::parse(&format!(
                "https://soundcloud.com/user/{}",
                "x".repeat(MAX_LINK_BYTES)
            ))
            .is_err()
        );
    }
}
