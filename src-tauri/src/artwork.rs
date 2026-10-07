use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use base64::Engine;
use serde_json::Value;
use windows::core::{Result, HSTRING};
use windows::Foundation::Uri;
use windows::Storage::Streams::DataReader;
use windows::Web::Http::HttpClient;

const ARTWORK_SIZE: &str = "600x600bb";

static CACHE: LazyLock<Mutex<HashMap<String, Option<String>>>> = LazyLock::new(Default::default);

pub fn lookup(artist: &str, album: &str, title: &str) -> Option<String> {
    let artist = clean(artist);
    if artist.is_empty() {
        return None;
    }
    let (entity, name) = if album.trim().is_empty() {
        ("song", clean(title))
    } else {
        ("album", clean(album))
    };
    let term = format!("{artist} {name}");
    let cache_key = format!("{entity}:{}", term.to_lowercase());

    if let Some(hit) = CACHE.lock().ok()?.get(&cache_key) {
        return hit.clone();
    }
    let found = search(entity, &term, &artist, &name).unwrap_or_else(|e| {
        eprintln!("iTunes недоступен: {e}");
        None
    });
    if let Ok(mut cache) = CACHE.lock() {
        cache.insert(cache_key, found.clone());
    }
    found
}

fn search(entity: &str, term: &str, artist: &str, name: &str) -> Result<Option<String>> {
    let client = HttpClient::new()?;
    let url = format!(
        "https://itunes.apple.com/search?media=music&limit=10&entity={entity}&term={}",
        encode(term)
    );
    let body = client.GetStringAsync(&uri(&url)?)?.get()?.to_string();
    let Ok(json) = serde_json::from_str::<Value>(&body) else {
        return Ok(None);
    };

    let name_field = if entity == "song" { "trackName" } else { "collectionName" };
    let matches = |r: &Value, field: &str, wanted: &str| {
        r[field].as_str().is_some_and(|v| similar(&normalize(&clean(v)), &normalize(wanted)))
    };
    let artwork = json["results"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|r| matches(r, "artistName", artist) && matches(r, name_field, name))
        .and_then(|r| r["artworkUrl100"].as_str());
    let Some(artwork) = artwork else {
        return Ok(None);
    };

    let buffer = client.GetBufferAsync(&uri(&artwork.replace("100x100bb", ARTWORK_SIZE))?)?.get()?;
    let mut bytes = vec![0u8; buffer.Length()? as usize];
    DataReader::FromBuffer(&buffer)?.ReadBytes(&mut bytes)?;
    let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
    Ok(Some(format!("data:image/jpeg;base64,{encoded}")))
}

fn uri(url: &str) -> Result<Uri> {
    Uri::CreateUri(&HSTRING::from(url))
}

fn similar(found: &str, wanted: &str) -> bool {
    !found.is_empty() && !wanted.is_empty() && (found.contains(wanted) || wanted.contains(found))
}

fn normalize(s: &str) -> String {
    s.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

fn clean(s: &str) -> String {
    let mut out = String::new();
    let mut depth = 0;
    for c in s.chars() {
        match c {
            '(' | '[' => depth += 1,
            ')' | ']' => depth = (depth - 1).max(0),
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    let out = out.split(" feat").next().unwrap_or_default();
    out.split(" ft.").next().unwrap_or_default().trim().to_string()
}

fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            b' ' => "+".to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}
