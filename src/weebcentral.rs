//! HTML source adapter for `WeebCentral`, retaining opaque series/chapter IDs.

use super::{Chapter, Client, Manga, Series};
use anyhow::{Context, Result, anyhow, ensure};
use scraper::{ElementRef, Html, Selector};
use std::collections::HashSet;

const SITE: &str = "https://weebcentral.com";

fn selector(value: &str) -> Result<Selector> {
    Selector::parse(value).map_err(|_| anyhow!("seletor HTML inválido: {value}"))
}

fn text(node: ElementRef<'_>) -> String {
    node.text().collect::<String>().split_whitespace().collect::<Vec<_>>().join(" ")
}

fn id<'a>(value: &'a str, kind: &str) -> Result<&'a str> {
    let path = value.strip_prefix(SITE).unwrap_or(value);
    let prefix = format!("/{kind}/");
    let value = path.strip_prefix(&prefix).unwrap_or(path).split(['/', '?', '#']).next().unwrap_or_default();
    ensure!(
        value.len() == 26 && value.bytes().all(|c| c.is_ascii_alphanumeric()),
        "WeebCentral: use o ID ou URL de {kind} exibido na busca/lista"
    );
    Ok(value)
}

fn series_link(value: &str) -> Result<&str> {
    ensure!(value.starts_with("/series/") || value.starts_with("https://weebcentral.com/series/"), "WeebCentral: link de série inválido");
    id(value, "series")
}

fn chapter_link(value: &str) -> Result<&str> {
    ensure!(
        value.starts_with("/chapters/") || value.starts_with("https://weebcentral.com/chapters/"),
        "WeebCentral: link de capítulo inválido"
    );
    id(value, "chapters")
}

fn parse_search(html: &str) -> Result<(Vec<Manga>, bool)> {
    let doc = Html::parse_fragment(html);
    let title_selector = selector("div:not([class])")?;
    let image_selector = selector("img")?;
    let mut seen = HashSet::new();
    let mut items = Vec::new();
    for node in doc.select(&selector("article > section > a[href*='/series/']")?) {
        let href = node.value().attr("href").context("WeebCentral: URL ausente")?;
        let id = series_link(href)?;
        if !seen.insert(id.to_owned()) {
            continue;
        }
        let title = node
            .select(&title_selector)
            .map(text)
            .find(|text| !text.is_empty())
            .or_else(|| {
                node.select(&image_selector)
                    .find_map(|image| image.value().attr("alt").map(|alt| alt.trim_end_matches(" cover").to_owned()))
            })
            .filter(|text| !text.is_empty())
            .context("WeebCentral: título ausente")?;
        items.push(Manga { id: id.into(), title });
    }
    let more = doc
        .select(&selector("button[hx-get]")?)
        .any(|node| node.value().attr("hx-get").is_some_and(|url| url.contains("/search/data?") && url.contains("offset=")));
    ensure!(
        !items.is_empty() || html.trim().is_empty() || html.to_lowercase().contains("no results"),
        "WeebCentral: resposta de busca desconhecida ou bloqueada"
    );
    Ok((items, more))
}

fn chapter_number(label: &str) -> Option<f64> {
    let lower = label.to_ascii_lowercase();
    let value = lower.strip_prefix("chapter ").or_else(|| lower.strip_prefix("episode "))?.split_whitespace().next()?;
    value.parse::<f64>().ok().filter(|n| n.is_finite() && *n >= 0.0)
}

fn parse_chapters(html: &str) -> Result<Vec<Chapter>> {
    let doc = Html::parse_fragment(html);
    let label_selector = selector("span.flex > span")?;
    let mut seen = HashSet::new();
    let mut chapters = Vec::new();
    for node in doc.select(&selector("div[x-data] > a[href*='/chapters/']")?) {
        let href = node.value().attr("href").context("WeebCentral: link de capítulo ausente")?;
        let id = chapter_link(href)?;
        if !seen.insert(id.to_owned()) {
            continue;
        }
        let title =
            node.select(&label_selector).map(text).find(|value| !value.is_empty()).context("WeebCentral: rótulo de capítulo ausente")?;
        chapters.push(Chapter {
            id: id.into(),
            number: chapter_number(&title),
            title,
            url: format!("{SITE}/chapters/{id}"),
            locked: false,
        });
    }
    ensure!(!chapters.is_empty(), "WeebCentral: lista de capítulos vazia ou bloqueada");
    // The full-chapter-list endpoint is newest-first; retain all labels, including volumes/extras.
    chapters.reverse();
    Ok(chapters)
}

fn parse_pages(html: &str) -> Result<Vec<String>> {
    let doc = Html::parse_fragment(html);
    let mut pages = Vec::new();
    for image in doc.select(&selector("section#chapter-images img")?) {
        let url = image.value().attr("src").filter(|url| !url.is_empty()).context("WeebCentral: URL de imagem ausente")?;
        ensure!(url.starts_with("https://") && !url.contains('#'), "WeebCentral: URL de imagem inválida");
        pages.push(url.to_owned());
    }
    ensure!(!pages.is_empty(), "WeebCentral: capítulo sem imagens ou resposta bloqueada");
    Ok(pages)
}

pub(super) fn search(client: &mut Client, query: &str, page: u32) -> Result<(Vec<Manga>, bool)> {
    let sort = if query.is_empty() { "&sort=Popularity" } else { "" };
    let query = percent_encoding::utf8_percent_encode(query, percent_encoding::NON_ALPHANUMERIC);
    let url = format!("{SITE}/search/data?text={query}&limit=32&offset={}&display_mode=Full+Display{sort}", u64::from(page - 1) * 32);
    parse_search(&String::from_utf8(client.get(&url)?)?)
}

pub(super) fn series(client: &mut Client, input: &str) -> Result<Series> {
    let id = id(input, "series")?;
    let url = format!("{SITE}/series/{id}");
    let html = String::from_utf8(client.get(&url)?)?;
    let title = series_title(&html)?;
    let html = String::from_utf8(client.get(&format!("{url}/full-chapter-list"))?)?;
    Ok(Series { id: id.into(), title, url, chapters: parse_chapters(&html)? })
}

fn series_title(html: &str) -> Result<String> {
    let doc = Html::parse_document(html);
    doc.select(&selector("section[x-data] h1")?).map(text).find(|value| !value.is_empty()).context("WeebCentral: título da série ausente")
}

pub(super) fn page_urls(client: &mut Client, chapter: &Chapter) -> Result<Vec<String>> {
    chapter_link(&chapter.url)?;
    let url = format!("{}/images?is_prev=False&reading_style=long_strip", chapter.url);
    parse_pages(&String::from_utf8(client.get(&url)?)?)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing)]
    use super::*;

    #[test]
    fn explicit_ids_and_labels_do_not_collapse_different_chapters() {
        assert_eq!(id("https://weebcentral.com/series/01J76XYF85TPSFGEH5GCC1VWAW/Title", "series").unwrap(), "01J76XYF85TPSFGEH5GCC1VWAW");
        assert!(id("../../output", "series").is_err());
        assert!(series_link("https://evil.test/series/01J76XYF85TPSFGEH5GCC1VWAW").is_err());
        assert!(chapter_number("Chapter 2.5").is_some_and(|n| (n - 2.5).abs() < f64::EPSILON));
        assert!(chapter_number("Volume 1").is_none());
        assert!(parse_pages("<html>challenge</html>").is_err());
        assert!(parse_chapters("<html>challenge</html>").is_err());
        assert!(parse_search("<html>challenge</html>").is_err());
    }

    #[test]
    fn observed_html_shapes_keep_identity_pagination_and_image_order() {
        let (items, more) = parse_search(include_str!("../tests/fixtures/weebcentral/search.html")).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].id, "01J76XYF85TPSFGEH5GCC1VWAW");
        assert_eq!(items[1].title, "Series & Examples");
        assert!(more);
        let chapters = parse_chapters(include_str!("../tests/fixtures/weebcentral/chapters.html")).unwrap();
        assert_eq!(chapters.len(), 4);
        assert_eq!(chapters[0].title, "Chapter 2.5");
        assert_eq!(chapters[1].title, "Special");
        assert_eq!(chapters[2].title, "Volume 1");
        assert_ne!(chapters[0].id, chapters[3].id);
        assert_eq!(series_title(include_str!("../tests/fixtures/weebcentral/details.html")).unwrap(), "Series & Examples");
        let pages = parse_pages(include_str!("../tests/fixtures/weebcentral/pages.html")).unwrap();
        assert_eq!(pages, ["https://official.lowee.us/fixture/001.png?x=1&y=2", "https://official.lowee.us/fixture/002.png"]);
    }
}
