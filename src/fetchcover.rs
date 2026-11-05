//! Look up an official cover online when the book has none.
//! Sources: `AniList` (light novels, manhwa) and Open Library (books).

use serde_json::{Value, json};
use std::time::Duration;

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(12)))
        .user_agent("kindle-turbo (+https://github.com/Yorizel/kindle-turbo)")
        .build()
        .into()
}

/// Title variants, from most to least specific: "A - Site Name" -> "A", "A: B" -> "A".
fn variants(title: &str) -> Vec<String> {
    let mut v = vec![title.trim().to_owned()];
    for sep in [" - ", " – ", ": ", ", ", " (", " ["] {
        let Some((head, _)) = v.last().and_then(|t| t.split_once(sep)) else {
            continue;
        };
        let h = head.trim().to_owned();
        if h.len() >= 4 && !v.contains(&h) {
            v.push(h);
        }
    }
    v
}

fn norm(s: &str) -> String {
    s.to_lowercase().chars().filter(|c| c.is_alphanumeric()).collect()
}

fn anilist(a: &ureq::Agent, q: &str, comic: bool) -> Option<String> {
    let query = "query($s:String,$f:[MediaFormat]){Page(perPage:5){media(search:$s,format_in:$f){title{romaji english native} synonyms format coverImage{extraLarge large}}}}";
    let formats = if comic { json!(["MANGA", "ONE_SHOT"]) } else { json!(["NOVEL", "MANGA"]) };
    let body = json!({"query": query, "variables": {"s": q, "f": formats}});
    let v: Value = a.post("https://graphql.anilist.co").send_json(&body).ok()?.body_mut().read_json().ok()?;
    let media = v["data"]["Page"]["media"].as_array()?;
    let nq = norm(q);
    // prefer exact-ish title match, then novel format, then first hit
    let score = |m: &Value| -> i32 {
        let mut s = 0;
        let titles: Vec<&str> = ["romaji", "english", "native"]
            .iter()
            .filter_map(|k| m["title"][k].as_str())
            .chain(m["synonyms"].as_array().into_iter().flatten().filter_map(|x| x.as_str()))
            .collect();
        if titles.iter().any(|t| norm(t) == nq) {
            s += 10;
        } else if titles.iter().any(|t| norm(t).contains(&nq) || nq.contains(&norm(t))) {
            s += 5;
        }
        if !comic && m["format"] == "NOVEL" {
            s += 2;
        }
        s
    };
    let best = media.iter().max_by_key(|m| score(m))?;
    best["coverImage"]["extraLarge"].as_str().or_else(|| best["coverImage"]["large"].as_str()).map(str::to_owned)
}

fn openlibrary(a: &ureq::Agent, q: &str, author: Option<&str>) -> Option<String> {
    let enc = |s: &str| percent_encoding::utf8_percent_encode(s, percent_encoding::NON_ALPHANUMERIC).to_string();
    let mut url = format!("https://openlibrary.org/search.json?limit=3&fields=cover_i,title&title={}", enc(q));
    if let Some(au) = author {
        url.push_str("&author=");
        url.push_str(&enc(au));
    }
    let v: Value = a.get(&url).call().ok()?.body_mut().read_json().ok()?;
    let id = v["docs"].as_array()?.iter().find_map(|d| d["cover_i"].as_i64())?;
    Some(format!("https://covers.openlibrary.org/b/id/{id}-L.jpg"))
}

fn download(a: &ureq::Agent, url: &str) -> Option<Vec<u8>> {
    let mut r = a.get(url).call().ok()?;
    let data = r.body_mut().with_config().limit(15 * 1024 * 1024).read_to_vec().ok()?;
    // must decode and not be a 1x1 placeholder
    let img = image::load_from_memory(&data).ok()?;
    (img.width() >= 100 && img.height() >= 100).then_some(data)
}

/// Returns (image bytes, source name).
#[must_use]
pub fn find(title: &str, author: Option<&str>, comic: bool) -> Option<(Vec<u8>, &'static str)> {
    let a = agent();
    for q in variants(title) {
        if let Some(url) = anilist(&a, &q, comic)
            && let Some(d) = download(&a, &url)
        {
            return Some((d, "AniList"));
        }
        if !comic
            && let Some(url) = openlibrary(&a, &q, author)
            && let Some(d) = download(&a, &url)
        {
            return Some((d, "Open Library"));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    #[test]
    fn title_variants() {
        let v = super::variants("Got Dropped into a Ghost Story, Still Gotta Work - Mythic Regressor");
        assert_eq!(v[1], "Got Dropped into a Ghost Story, Still Gotta Work");
        assert_eq!(v[2], "Got Dropped into a Ghost Story");
    }
}
