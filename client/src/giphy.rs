use std::sync::Mutex;
use std::time::{Duration, Instant};

const API: &str = "https://api.giphy.com/v1/gifs";
const LIMIT: u32 = 30;
const TIMEOUT: Duration = Duration::from_secs(10);
/// The built-in key's free tier is 100 calls an hour for every install at once.
const TRENDING_FOR: Duration = Duration::from_secs(15 * 60);

#[derive(Clone, Debug, PartialEq)]
pub struct Gif {
    pub id: String,
    pub title: String,
    pub preview: String,
    pub width: u32,
    pub height: u32,
}

fn key() -> Option<String> {
    std::env::var("GIPHY_API_KEY")
        .ok()
        .or_else(|| option_env!("GIPHY_API_KEY").map(str::to_owned))
        .map(|k| k.trim().to_owned())
        .filter(|k| !k.is_empty())
}

pub fn available() -> bool {
    key().is_some()
}

pub async fn search(query: &str) -> Result<Vec<Gif>, String> {
    let query = query.trim();
    if query.is_empty() {
        return trending().await;
    }
    fetch("search", &[("q", query)]).await
}

async fn trending() -> Result<Vec<Gif>, String> {
    static CACHE: Mutex<Option<(Instant, Vec<Gif>)>> = Mutex::new(None);
    if let Some((at, gifs)) = CACHE.lock().ok().and_then(|c| c.clone())
        && at.elapsed() < TRENDING_FOR
    {
        return Ok(gifs);
    }
    let gifs = fetch("trending", &[]).await?;
    if let Ok(mut cache) = CACHE.lock() {
        *cache = Some((Instant::now(), gifs.clone()));
    }
    Ok(gifs)
}

async fn fetch(endpoint: &str, extra: &[(&str, &str)]) -> Result<Vec<Gif>, String> {
    let key = key().ok_or("GIFs are not set up in this build.")?;
    let limit = LIMIT.to_string();
    let mut query = vec![
        ("api_key", key.as_str()),
        ("limit", limit.as_str()),
        ("rating", "pg-13"),
        ("bundle", "messaging_non_clips"),
    ];
    query.extend_from_slice(extra);
    let res = reqwest::Client::builder()
        .timeout(TIMEOUT)
        .build()
        .map_err(|e| e.to_string())?
        .get(format!("{API}/{endpoint}"))
        .query(&query)
        .send()
        .await
        .map_err(|_| "Couldn't reach GIPHY.".to_string())?;
    match res.status().as_u16() {
        200 => {}
        429 => return Err("GIPHY is busy. Try again in a while.".into()),
        _ => return Err("Couldn't reach GIPHY.".into()),
    }
    let body: serde_json::Value = res
        .json()
        .await
        .map_err(|_| "GIPHY sent an unreadable answer.".to_string())?;
    Ok(parse(&body))
}

fn parse(body: &serde_json::Value) -> Vec<Gif> {
    let number = |v: &serde_json::Value| {
        v.as_u64()
            .or_else(|| v.as_str()?.parse().ok())
            .and_then(|n| u32::try_from(n).ok())
    };
    body["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let id = item["id"].as_str().filter(|id| valid_id(id))?;
            let small = &item["images"]["fixed_height_small"];
            Some(Gif {
                id: id.to_owned(),
                title: item["title"].as_str().unwrap_or_default().to_owned(),
                preview: preview_url(id),
                width: number(&small["width"]).unwrap_or(100),
                height: number(&small["height"]).unwrap_or(100),
            })
        })
        .collect()
}

fn valid_id(id: &str) -> bool {
    (1..=64).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// What a message carries: opens the GIF on GIPHY for a client without this.
pub fn link(id: &str) -> String {
    format!("https://media.giphy.com/media/{id}/giphy.gif")
}

fn preview_url(id: &str) -> String {
    format!("https://media.giphy.com/media/{id}/100.webp")
}

/// Rebuilt from the id alone, so a message can only ever make a reader fetch
/// a GIPHY rendition, never an address the sender wrote.
pub fn embed_url(id: &str) -> String {
    format!("https://media.giphy.com/media/{id}/200.webp")
}

/// The GIF a message is, when its whole text is one GIPHY link.
pub fn message_gif(content: &str) -> Option<String> {
    let url = content.trim();
    if url.contains(char::is_whitespace) {
        return None;
    }
    let rest = url.strip_prefix("https://")?;
    let (host, path) = rest.split_once('/')?;
    let path = path.split(['?', '#']).next()?;
    let mut parts = path.split('/').filter(|p| !p.is_empty());
    let media_host = host == "i.giphy.com"
        || host
            .strip_prefix("media")
            .and_then(|h| h.strip_suffix(".giphy.com"))
            .is_some_and(|n| n.is_empty() || (n.len() == 1 && n.as_bytes()[0].is_ascii_digit()));
    let id = if media_host {
        let first = parts.next()?;
        if first == "media" {
            let next = parts.next()?;
            if next.starts_with("v1.") {
                parts.next()?
            } else {
                next
            }
        } else if host == "i.giphy.com" {
            first.rsplit_once('.').map_or(first, |(stem, _)| stem)
        } else {
            return None;
        }
    } else if host == "giphy.com" || host == "www.giphy.com" {
        if parts.next()? != "gifs" {
            return None;
        }
        let slug = parts.next()?;
        slug.rsplit_once('-').map_or(slug, |(_, id)| id)
    } else {
        return None;
    };
    valid_id(id).then(|| id.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_giphy_link_shape_names_its_gif() {
        for url in [
            "https://media.giphy.com/media/3o7TKSjRrfIPjeiVyM/giphy.gif",
            "https://media2.giphy.com/media/v1.Y2lkPTc5MGI3NjEx/3o7TKSjRrfIPjeiVyM/200.webp?cid=abc",
            "https://i.giphy.com/3o7TKSjRrfIPjeiVyM.webp",
            "https://i.giphy.com/media/3o7TKSjRrfIPjeiVyM/giphy.gif",
            "https://giphy.com/gifs/cat-funny-3o7TKSjRrfIPjeiVyM",
            "  https://giphy.com/gifs/3o7TKSjRrfIPjeiVyM  ",
        ] {
            assert_eq!(
                message_gif(url).as_deref(),
                Some("3o7TKSjRrfIPjeiVyM"),
                "{url}"
            );
        }
        assert_eq!(message_gif(&link("abc123")).as_deref(), Some("abc123"));
    }

    #[test]
    fn nothing_but_a_lone_giphy_link_is_a_gif() {
        for text in [
            "look https://media.giphy.com/media/abc/giphy.gif",
            "http://media.giphy.com/media/abc/giphy.gif",
            "https://media.giphy.com.evil.example/media/abc/giphy.gif",
            "https://evil.example/media.giphy.com/media/abc/giphy.gif",
            "https://mediax.giphy.com/media/abc/giphy.gif",
            "https://media.giphy.com/media/../x/giphy.gif",
            "https://media.giphy.com/media/a%2Fb/giphy.gif",
            "https://giphy.com/stickers/abc",
            "https://media.giphy.com/media/",
        ] {
            assert_eq!(message_gif(text), None, "{text}");
        }
    }

    #[test]
    fn results_keep_only_well_formed_ids() {
        let body = serde_json::json!({ "data": [
            { "id": "abc123", "title": "Cat", "images": { "fixed_height_small": { "width": "178", "height": "100" } } },
            { "id": "../evil", "images": {} },
            { "title": "no id" },
        ]});
        let gifs = parse(&body);
        assert_eq!(gifs.len(), 1);
        assert_eq!((gifs[0].width, gifs[0].height), (178, 100));
        assert_eq!(
            gifs[0].preview,
            "https://media.giphy.com/media/abc123/100.webp"
        );
    }
}
