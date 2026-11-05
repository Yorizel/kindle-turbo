//! `AsuraScans` API/Astro adapter. Contract adapted from `TachiKindle` (Apache-2.0).
use super::{Chapter, Client, Manga, Series};
use anyhow::{Context, Result, bail, ensure};
use serde_json::Value;
const SITE: &str = "https://asurascans.com";
const API: &str = "https://api.asurascans.com/api";

fn unwrap_astro(value: Value) -> Value {
    match value {
        Value::Array(mut values) if values.len() == 2 && values.first().is_some_and(Value::is_number) => {
            values.pop().map_or(Value::Null, unwrap_astro)
        }
        Value::Array(values) => Value::Array(values.into_iter().map(unwrap_astro).collect()),
        Value::Object(values) => Value::Object(values.into_iter().map(|(key, value)| (key, unwrap_astro(value))).collect()),
        other => other,
    }
}

fn astro_prop(html: &str, key: &str) -> Result<Value> {
    let pattern = regex::Regex::new(r#"\bprops\s*=\s*"([^"]*)""#)?;
    for capture in pattern.captures_iter(html) {
        let raw = capture.get(1).context("atributo props ausente")?.as_str();
        let decoded = html_escape::decode_html_entities(raw);
        if let Ok(value) = serde_json::from_str::<Value>(&decoded) {
            let value = unwrap_astro(value);
            if value.get(key).is_some() {
                return Ok(value);
            }
        }
    }
    bail!("AsuraScans: dados '{key}' ausentes; o site pode ter mudado ou bloqueado a requisição")
}

fn required<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value.get(key).and_then(Value::as_str).filter(|s| !s.is_empty()).with_context(|| format!("AsuraScans: campo '{key}' ausente"))
}

fn slug(input: &str) -> Result<&str> {
    ensure!(
        !input.is_empty() && input.len() <= 120 && input.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'),
        "Use o slug exibido pela busca (ex.: solo-leveling)"
    );
    Ok(input)
}

fn number(chapter: &Value) -> Result<f64> {
    chapter["number"].as_f64().filter(|n| n.is_finite() && *n >= 0.0).context("AsuraScans: número de capítulo inválido")
}

fn locked(chapter: &Value) -> bool {
    chapter["is_locked"].as_bool().unwrap_or(false) || chapter["is_premium"].as_bool().unwrap_or(false)
}

fn pages(value: &Value) -> Result<Vec<&str>> {
    let list =
        value["pages"].as_array().filter(|list| !list.is_empty()).context("Capítulo sem páginas públicas (bloqueado ou indisponível)")?;
    list.iter()
        .map(|page| {
            ensure!(
                page["tiles"].as_array().is_none_or(Vec::is_empty),
                "Capítulo com imagens embaralhadas; suporte ainda não implementado"
            );
            let url = required(page, "url")?;
            ensure!(url.starts_with("https://") && !url.contains('#'), "URL de imagem não suportada: {url}");
            Ok(url)
        })
        .collect()
}

pub(super) fn search(client: &mut Client, query: &str, page: u32) -> Result<(Vec<Manga>, bool)> {
    let query = percent_encoding::utf8_percent_encode(query, percent_encoding::NON_ALPHANUMERIC);
    let response: Value =
        serde_json::from_slice(&client.get(&format!("{API}/series?offset={}&limit=20&search={query}", u64::from(page - 1) * 20))?)?;
    let items = response["data"].as_array().context("AsuraScans: catálogo inválido")?;
    let list = items
        .iter()
        .map(|item| Ok(Manga { id: required(item, "slug")?.into(), title: required(item, "title")?.into() }))
        .collect::<Result<_>>()?;
    Ok((list, response["meta"]["has_more"] == true))
}

pub(super) fn series(client: &mut Client, input: &str) -> Result<Series> {
    let id = slug(input)?;
    let details: Value = serde_json::from_slice(&client.get(&format!("{API}/series/{id}"))?)?;
    let details = series_details(&details);
    let public = required(details, "public_url")?;
    ensure!(public.starts_with("/comics/") && !public.contains('?') && !public.contains('#'), "AsuraScans: caminho público inválido");
    let url = format!("{SITE}{public}");
    let value = astro_prop(&String::from_utf8(client.get(&url)?)?, "chapters")?;
    let list = value["chapters"].as_array().context("AsuraScans: capítulos inválidos")?;
    let mut chapters = list
        .iter()
        .map(|item| {
            let number = number(item)?;
            Ok(Chapter {
                id: number.to_string(),
                number: Some(number),
                title: item["title"].as_str().unwrap_or_default().into(),
                url: format!("{SITE}/series/{id}/chapter/{number}"),
                locked: locked(item),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    chapters.sort_by(|a, b| a.number.unwrap_or_default().total_cmp(&b.number.unwrap_or_default()));
    Ok(Series { id: id.into(), title: required(details, "title")?.into(), url, chapters })
}

fn series_details(response: &Value) -> &Value {
    let payload = response.get("data").filter(|value| value.is_object()).unwrap_or(response);
    payload.get("series").unwrap_or(payload)
}

pub(super) fn page_urls(client: &mut Client, series: &Series, chapter: &Chapter) -> Result<Vec<String>> {
    let html = String::from_utf8(client.get(&format!("{}/chapter/{}", series.url, chapter.id))?)?;
    let props = astro_prop(&html, "pages")?;
    Ok(pages(&props)?.into_iter().map(str::to_owned).collect())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing)]
    use super::*;
    use serde_json::json;

    #[test]
    fn series_details_accept_the_live_data_wrapper_and_legacy_response() {
        let legacy: Value = serde_json::from_str(include_str!("../tests/fixtures/asura/details.json")).unwrap();
        let wrapped = json!({"data":legacy});
        assert_eq!(series_details(&wrapped), series_details(&legacy));
        assert!(required(series_details(&wrapped), "public_url").is_ok());
    }
    #[test]
    fn astro_real_shapes_preserve_arrays_and_locks() {
        let value = astro_prop(include_str!("../tests/fixtures/asura/chapters.html"), "chapters").unwrap();
        let chapters = value["chapters"].as_array().unwrap();
        assert_eq!(chapters.len(), 2);
        assert!((number(&chapters[0]).unwrap() - 82.0).abs() < f64::EPSILON);
        assert!(!locked(&chapters[0]));
        assert!(locked(&chapters[1]));
        let value = astro_prop(include_str!("../tests/fixtures/asura/pages.html"), "pages").unwrap();
        assert_eq!(pages(&value).unwrap().len(), 2);
    }

    #[test]
    fn refuses_missing_locked_and_scrambled_content() {
        assert!(astro_prop("<html>challenge</html>", "pages").is_err());
        assert!(pages(&json!({"pages":[]})).is_err());
        assert!(pages(&json!({"pages":[{"url":"https://example.com/page.webp","tiles":[1]}]})).is_err());
        assert!(slug("../../out").is_err());
    }
}
