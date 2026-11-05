//! `AsuraScans` downloader. API/Astro contract adapted from `TachiKindle` (Apache-2.0).
//! See docs/licensing.md for attribution and fixture provenance.

use anyhow::{Context, Result, anyhow, bail, ensure};
use clap::Subcommand;
use rayon::prelude::*;
use serde_json::{Value, json};
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const SITE: &str = "https://asurascans.com";
const API: &str = "https://api.asurascans.com/api";

#[derive(Debug, Subcommand)]
#[expect(clippy::redundant_pub_crate, reason = "binary entry point must be visible to main while unreachable_pub forbids pub")]
pub(super) enum Command {
    /// Busca títulos (sem consulta, lista o catálogo).
    Search {
        query: Option<String>,
        #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u32).range(1..))]
        page: u32,
    },
    /// Lista capítulos e indica os bloqueados.
    Chapters { series: String },
    /// Baixa capítulos em CBZ. Use --all ou um intervalo --from/--to.
    Download {
        series: String,
        #[arg(long)]
        all: bool,
        #[arg(long, conflicts_with = "all")]
        from: Option<f64>,
        #[arg(long, conflicts_with = "all")]
        to: Option<f64>,
        #[arg(short, long, default_value = "downloads")]
        out: PathBuf,
        /// Copia os CBZ para documents/Manhwas do Kindle montado neste caminho.
        #[arg(long)]
        kindle: Option<PathBuf>,
    },
}

#[derive(Debug)]
struct Client {
    agent: ureq::Agent,
    last: Option<Instant>,
}

impl Client {
    fn new() -> Self {
        Self {
            agent: ureq::Agent::config_builder()
                .timeout_global(Some(Duration::from_secs(45)))
                .user_agent("Mozilla/5.0 kindle-turbo/0.1 (+https://github.com/Yorizel/kindle-turbo)")
                .build()
                .into(),
            last: None,
        }
    }

    fn get(&mut self, url: &str) -> Result<Vec<u8>> {
        self.get_limited(url, 64 * 1024 * 1024)
    }

    fn get_limited(&mut self, url: &str, limit: u64) -> Result<Vec<u8>> {
        for attempt in 0..3_u32 {
            if let Some(last) = self.last {
                std::thread::sleep(Duration::from_secs(1).saturating_sub(last.elapsed()));
            }
            self.last = Some(Instant::now());
            let response = self.agent.get(url).header("Referer", SITE).config().http_status_as_error(false).build().call();
            match response {
                Ok(mut response) if response.status().as_u16() == 200 => {
                    return response.body_mut().with_config().limit(limit).read_to_vec().with_context(|| format!("lendo {url}"));
                }
                Ok(response) => {
                    let code = response.status().as_u16();
                    ensure!(attempt < 2 && (code == 429 || code >= 500), "AsuraScans: {url}: HTTP {code}");
                    let delay = match response.headers().get("retry-after") {
                        Some(header) => {
                            let value = header.to_str().context("Retry-After inválido")?;
                            value.parse::<u64>().with_context(|| format!("AsuraScans: HTTP {code}; tente novamente após {value}"))?
                        }
                        None if code == 429 => 30 * 2_u64.pow(attempt),
                        None => 2_u64.pow(attempt + 1),
                    };
                    ensure!(delay <= 120, "AsuraScans: HTTP {code}; aguarde {delay}s antes de tentar novamente");
                    eprintln!("AsuraScans: HTTP {code}; aguardando {delay}s para tentar novamente");
                    std::thread::sleep(Duration::from_secs(delay));
                }
                Err(error) => return Err(anyhow!("AsuraScans: {url}: {error}")),
            }
        }
        bail!("AsuraScans indisponível: {url}")
    }

    fn json(&mut self, path: &str) -> Result<Value> {
        serde_json::from_slice(&self.get(&format!("{API}{path}"))?).context("resposta inválida da API AsuraScans")
    }

    fn props(&mut self, path: &str, key: &str) -> Result<Value> {
        let body = self.get(&format!("{SITE}{path}"))?;
        astro_prop(&String::from_utf8(body)?, key)
    }
}

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

fn filename(input: &str) -> String {
    let mut name: String = input.chars().map(|c| if c.is_control() || "/\\:*?\"<>|".contains(c) { '_' } else { c }).take(100).collect();
    while name.len() > 100 {
        let _ = name.pop();
    }
    let name = name.trim().trim_matches('.');
    if name.is_empty() { "serie".into() } else { name.into() }
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

#[derive(Debug)]
struct Partial(PathBuf);

impl Drop for Partial {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn valid_archive(path: &Path, chapter_url: &str) -> bool {
    let check = || -> Result<()> {
        let mut zip = zip::ZipArchive::new(File::open(path)?)?;
        let mut manifest = String::new();
        zip.by_name("kindle-turbo.json")?.take(64 * 1024).read_to_string(&mut manifest)?;
        let manifest: Value = serde_json::from_str(&manifest)?;
        ensure!(manifest["chapter_url"] == chapter_url, "capítulo diferente");
        let count = manifest["pages"].as_u64().context("contagem ausente")?;
        ensure!(count > 0 && u64::try_from(zip.len())? == count + 1, "arquivo incompleto");
        for i in 0..zip.len() {
            let mut entry = zip.by_index(i)?;
            std::io::copy(&mut entry, &mut std::io::sink())?;
        }
        Ok(())
    };
    check().is_ok()
}

fn download(client: &Client, path: &Path, chapter_url: &str, urls: &[&str]) -> Result<()> {
    let temporary = path.with_extension("cbz.part");
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .context("download parcial já existe; verifique se há outro download em andamento")?;
    let _partial = Partial(temporary.clone());
    let mut zip = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    // These workers wait on network I/O, including on hosts limited to one CPU.
    let pool = rayon::ThreadPoolBuilder::new().num_threads(4).build()?;
    // Four pages at a time bounds memory while keeping the ZIP in reading order.
    // API/HTML requests still use the caller's rate-limited client.
    for (batch, urls_batch) in urls.chunks(4).enumerate() {
        let images: Vec<Vec<u8>> = pool.install(|| {
            urls_batch
                .par_iter()
                .map(|url| {
                    let mut image_client = Client { agent: client.agent.clone(), last: None };
                    let data = image_client.get_limited(url, 16 * 1024 * 1024)?;
                    image::guess_format(&data).context("a página recebida não é uma imagem")?;
                    Ok(data)
                })
                .collect::<Result<_>>()
        })?;
        for (offset, data) in images.iter().enumerate() {
            let index = batch * 4 + offset;
            let format = image::guess_format(data)?;
            let extension = format.extensions_str().first().context("formato de imagem desconhecido")?;
            zip.start_file(format!("{:05}.{extension}", index + 1), options)?;
            zip.write_all(data)?;
            println!("     página {}/{}", index + 1, urls.len());
        }
    }
    zip.start_file("kindle-turbo.json", options)?;
    zip.write_all(serde_json::to_string(&json!({"source":"asurascans","chapter_url":chapter_url,"pages":urls.len()}))?.as_bytes())?;
    zip.finish()?.sync_all()?;
    ensure!(!path.exists(), "O destino já existe: {}", path.display());
    std::fs::rename(&temporary, path).context("publicando CBZ")?;
    Ok(())
}

fn copy_archive(source: &Path, destination: &Path, chapter_url: &str) -> Result<()> {
    if destination.exists() {
        ensure!(
            valid_archive(destination, chapter_url),
            "CBZ existente no Kindle é inválido; mova-o antes de copiar: {}",
            destination.display()
        );
        return Ok(());
    }
    let temporary = destination.with_extension("cbz.part");
    let mut output = OpenOptions::new().write(true).create_new(true).open(&temporary)?;
    let _partial = Partial(temporary.clone());
    std::io::copy(&mut File::open(source)?, &mut output)?;
    output.sync_all()?;
    ensure!(!destination.exists(), "O destino já existe: {}", destination.display());
    std::fs::rename(temporary, destination)?;
    Ok(())
}

#[expect(clippy::redundant_pub_crate, reason = "binary entry point must be visible to main while unreachable_pub forbids pub")]
pub(super) fn run(command: Command) -> Result<()> {
    let mut client = Client::new();
    let (series, selection) = match command {
        Command::Search { query, page } => {
            let query = percent_encoding::utf8_percent_encode(query.as_deref().unwrap_or_default(), percent_encoding::NON_ALPHANUMERIC);
            let response = client.json(&format!("/series?offset={}&limit=20&search={query}", u64::from(page - 1) * 20))?;
            let list = response["data"].as_array().context("AsuraScans: catálogo inválido")?;
            for item in list {
                println!("{}\t{}", required(item, "slug")?, required(item, "title")?);
            }
            println!("{} resultado(s), página {page}; próxima página: {}", list.len(), response["meta"]["has_more"] == true);
            return Ok(());
        }
        Command::Chapters { series } => (series, None),
        Command::Download { series, all, from, to, out, kindle } => {
            ensure!(all || from.is_some() || to.is_some(), "Escolha --all ou um intervalo --from N --to N");
            ensure!(from.into_iter().chain(to).all(|n| n.is_finite() && n >= 0.0), "Intervalo inválido");
            ensure!(from.unwrap_or(0.0) <= to.unwrap_or(f64::MAX), "--from deve ser menor ou igual a --to");
            if let Some(root) = &kindle {
                ensure!(root.join("documents").is_dir(), "Kindle não encontrado em {}", root.display());
            }
            (series, Some((from, to, out, kindle)))
        }
    };
    let slug = slug(&series)?;
    let details = client.json(&format!("/series/{slug}"))?;
    let details = details.get("series").unwrap_or(&details);
    let title = required(details, "title")?;
    let public = required(details, "public_url")?;
    ensure!(public.starts_with("/comics/") && !public.contains('?') && !public.contains('#'), "AsuraScans: caminho público inválido");
    let response = client.props(public, "chapters")?;
    let chapters = response["chapters"].as_array().context("AsuraScans: capítulos inválidos")?;
    let mut chapters: Vec<_> = chapters.iter().map(|chapter| Ok((number(chapter)?, chapter))).collect::<Result<_>>()?;
    chapters.sort_by(|a, b| a.0.total_cmp(&b.0));
    let Some((from, to, out, kindle)) = selection else {
        for (number, chapter) in chapters {
            println!(
                "{number}\t{}\t{}",
                if locked(chapter) { "bloqueado" } else { "público" },
                chapter["title"].as_str().unwrap_or_default()
            );
        }
        return Ok(());
    };
    let folder = out.join(format!("{} [{slug}]", filename(title)));
    std::fs::create_dir_all(&folder)?;
    let mut selected = 0;
    let mut failures = 0;
    for (number, chapter) in chapters {
        if number < from.unwrap_or(0.0) || number > to.unwrap_or(f64::MAX) {
            continue;
        }
        if locked(chapter) {
            println!("ignorado: capítulo {number} bloqueado");
            continue;
        }
        selected += 1;
        let url = format!("{SITE}/series/{slug}/chapter/{number}");
        let path = folder.join(format!("Capítulo {number}.cbz"));
        let result = (|| -> Result<()> {
            if path.exists() {
                ensure!(valid_archive(&path, &url), "CBZ existente inválido; mova-o antes de tentar novamente: {}", path.display());
                println!("já baixado: capítulo {number}");
            } else {
                let props = client.props(&format!("{public}/chapter/{number}"), "pages")?;
                download(&client, &path, &url, &pages(&props)?)?;
                println!("salvo: {}", path.display());
            }
            if let Some(root) = &kindle {
                let dest = root.join("documents/Manhwas").join(folder.file_name().context("pasta sem nome")?);
                std::fs::create_dir_all(&dest)?;
                copy_archive(&path, &dest.join(path.file_name().context("CBZ sem nome")?), &url)?;
            }
            Ok(())
        })();
        if let Err(error) = result {
            eprintln!("ERRO capítulo {number}: {error:#}");
            failures += 1;
        }
    }
    ensure!(selected > 0, "Nenhum capítulo público no intervalo escolhido");
    ensure!(failures == 0, "{failures} capítulo(s) falharam; execute novamente para retomar os CBZ concluídos");
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing)]
    use super::*;

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

    fn serve_once(body: Vec<u8>, status: &str) -> (String, std::thread::JoinHandle<()>) {
        serve_responses(vec![(body, status.to_owned())])
    }

    fn serve_responses(responses: Vec<(Vec<u8>, String)>) -> (String, std::thread::JoinHandle<()>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/page", listener.local_addr().unwrap());
        let handle = std::thread::spawn(move || {
            for (body, status) in responses {
                let (mut stream, _) = listener.accept().unwrap();
                stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
                let mut request = [0; 4096];
                let _ = stream.read(&mut request).unwrap();
                write!(stream, "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
                stream.write_all(&body).unwrap();
            }
        });
        (url, handle)
    }

    #[test]
    fn retries_rate_limits_with_retry_after_and_rejects_permanent_http_errors() {
        let image = include_bytes!("../tests/fixtures/asura/page.png").to_vec();
        let (url, handle) =
            serve_responses(vec![(Vec::new(), "429 Too Many Requests\r\nRetry-After: 0".into()), (image.clone(), "200 OK".into())]);
        assert_eq!(Client::new().get(&url).unwrap(), image);
        handle.join().unwrap();
        let (url, handle) = serve_once(b"not found".to_vec(), "404 Not Found");
        assert!(Client::new().get(&url).is_err());
        handle.join().unwrap();
    }

    #[test]
    fn real_http_download_produces_verified_cbz_and_rejects_wrong_identity() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("chapter.cbz");
        let mut png = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(2, 2).write_to(&mut png, image::ImageFormat::Png).unwrap();
        let expected = png.into_inner();
        let (url, handle) = serve_once(expected.clone(), "200 OK");
        download(&Client::new(), &path, "chapter:1", &[&url]).unwrap();
        handle.join().unwrap();
        assert!(valid_archive(&path, "chapter:1"));
        assert!(!valid_archive(&path, "chapter:2"));
        assert!(!path.with_extension("cbz.part").exists());
        let mut zip = zip::ZipArchive::new(File::open(&path).unwrap()).unwrap();
        let mut actual = Vec::new();
        zip.by_name("00001.png").unwrap().read_to_end(&mut actual).unwrap();
        assert_eq!(actual, expected);
        let destination = dir.path().join("kindle.cbz");
        copy_archive(&path, &destination, "chapter:1").unwrap();
        assert!(valid_archive(&destination, "chapter:1"));
        copy_archive(&path, &path, "chapter:1").unwrap();
        assert!(valid_archive(&path, "chapter:1"));
        assert!(copy_archive(&path, &destination, "chapter:2").is_err());
    }

    #[test]
    fn failed_download_cleans_only_its_own_partial() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("chapter.cbz");
        let partial = path.with_extension("cbz.part");
        let (url, handle) = serve_once(b"<html>challenge</html>".to_vec(), "200 OK");
        assert!(download(&Client::new(), &path, "chapter:1", &[&url]).is_err());
        handle.join().unwrap();
        assert!(!path.exists());
        assert!(!partial.exists());
        std::fs::write(&partial, b"another download").unwrap();
        assert!(download(&Client::new(), &path, "chapter:1", &[]).is_err());
        assert_eq!(std::fs::read(partial).unwrap(), b"another download");
    }

    #[test]
    fn parallel_pages_keep_reading_order_and_bound_in_flight_requests() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};
        let expected: Vec<_> = (0..8_u8)
            .map(|index| {
                let mut data = std::io::Cursor::new(Vec::new());
                image::RgbImage::from_pixel(1, 1, image::Rgb([index, 0, 0])).write_to(&mut data, image::ImageFormat::Png).unwrap();
                data.into_inner()
            })
            .collect();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let peak_server = Arc::clone(&peak);
        let bodies = expected.clone();
        let server = std::thread::spawn(move || {
            let mut workers = Vec::new();
            for incoming in listener.incoming().take(8) {
                let mut stream = incoming.unwrap();
                let active = Arc::clone(&active);
                let peak = Arc::clone(&peak_server);
                let bodies = bodies.clone();
                workers.push(std::thread::spawn(move || {
                    stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
                    let mut request = [0; 4096];
                    let count = stream.read(&mut request).unwrap();
                    let request = std::str::from_utf8(&request[..count]).unwrap();
                    let index: usize = request.split_whitespace().nth(1).unwrap().trim_start_matches('/').parse().unwrap();
                    let count = active.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(count, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(20 * u64::try_from(4 - index % 4).unwrap()));
                    let body = &bodies[index];
                    write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
                    stream.write_all(body).unwrap();
                    active.fetch_sub(1, Ordering::SeqCst);
                }));
            }
            for worker in workers {
                worker.join().unwrap();
            }
        });
        let urls: Vec<_> = (0..8).map(|index| format!("http://{address}/{index}")).collect();
        let urls: Vec<_> = urls.iter().map(String::as_str).collect();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("parallel.cbz");
        download(&Client::new(), &path, "chapter:parallel", &urls).unwrap();
        server.join().unwrap();
        let count = peak.load(Ordering::SeqCst);
        assert!((2..=4).contains(&count), "observed {count} concurrent requests");
        let mut zip = zip::ZipArchive::new(File::open(path).unwrap()).unwrap();
        for (index, expected) in expected.iter().enumerate() {
            let mut actual = Vec::new();
            zip.by_name(&format!("{:05}.png", index + 1)).unwrap().read_to_end(&mut actual).unwrap();
            assert_eq!(&actual, expected);
        }
    }
}
