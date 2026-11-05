//! Source browsing, bounded HTTP downloads and CBZ storage for the manga CLI.
//! See docs/licensing.md for attribution and fixture provenance.

use anyhow::{Context, Result, anyhow, bail, ensure};
use clap::Subcommand;
use rayon::prelude::*;
use serde_json::{Value, json};
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[path = "asura.rs"]
mod asura;
#[path = "weebcentral.rs"]
mod weebcentral;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[expect(clippy::redundant_pub_crate, reason = "binary entry point visibility is constrained by unreachable_pub")]
pub(super) enum Source {
    Asura,
    WeebCentral,
}

impl Source {
    const fn site(self) -> &'static str {
        match self {
            Self::Asura => "https://asurascans.com",
            Self::WeebCentral => "https://weebcentral.com",
        }
    }
    const fn name(self) -> &'static str {
        match self {
            Self::Asura => "AsuraScans",
            Self::WeebCentral => "WeebCentral",
        }
    }
    const fn id(self) -> &'static str {
        match self {
            Self::Asura => "asurascans",
            Self::WeebCentral => "weebcentral",
        }
    }
    fn interval(self) -> Duration {
        Duration::from_secs(if self == Self::WeebCentral { 2 } else { 1 })
    }
    fn parallelism(self) -> usize {
        if self == Self::WeebCentral { 1 } else { 4 }
    }
}

#[derive(Debug)]
struct Manga {
    id: String,
    title: String,
}
#[derive(Debug)]
struct Chapter {
    id: String,
    title: String,
    number: Option<f64>,
    url: String,
    locked: bool,
}
#[derive(Debug)]
struct Series {
    id: String,
    title: String,
    url: String,
    chapters: Vec<Chapter>,
}

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
        /// Identificador de um capítulo específico (inclusive volumes/extras).
        #[arg(long, conflicts_with_all = ["all", "from", "to"])]
        chapter: Option<String>,
        #[arg(short, long, default_value = "downloads")]
        out: PathBuf,
        /// Copia os CBZ para documents/Manhwas do Kindle montado neste caminho.
        #[arg(long)]
        kindle: Option<PathBuf>,
    },
}

#[derive(Debug)]
struct Client {
    source: Source,
    agent: ureq::Agent,
    last: Option<Instant>,
}

impl Client {
    fn new(source: Source) -> Self {
        Self {
            source,
            agent: ureq::Agent::config_builder()
                .timeout_global(Some(Duration::from_secs(45)))
                .user_agent("Mozilla/5.0 kindle-turbo/0.1 (+https://github.com/Yorizel/kindle-turbo)")
                .build()
                .into(),
            last: None,
        }
    }

    fn get(&mut self, url: &str) -> Result<Vec<u8>> {
        self.get_limited(url, 64 * 1024 * 1024, false)
    }

    fn get_limited(&mut self, url: &str, limit: u64, image: bool) -> Result<Vec<u8>> {
        for attempt in 0..3_u32 {
            if let Some(last) = self.last {
                std::thread::sleep(self.source.interval().saturating_sub(last.elapsed()));
            }
            self.last = Some(Instant::now());
            let response = self
                .agent
                .get(url)
                .header("Referer", &format!("{}/", self.source.site()))
                .header(
                    "Accept",
                    if image { "image/avif,image/webp,*/*" } else { "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8" },
                )
                .config()
                .http_status_as_error(false)
                .build()
                .call();
            match response {
                Ok(mut response) if response.status().as_u16() == 200 => {
                    return response.body_mut().with_config().limit(limit).read_to_vec().with_context(|| format!("lendo {url}"));
                }
                Ok(response) => {
                    let code = response.status().as_u16();
                    ensure!(attempt < 2 && (code == 429 || code >= 500), "{}: {url}: HTTP {code}", self.source.name());
                    let delay = match response.headers().get("retry-after") {
                        Some(header) => {
                            let value = header.to_str().context("Retry-After inválido")?;
                            value.parse::<u64>().with_context(|| format!("Fonte: HTTP {code}; tente novamente após {value}"))?
                        }
                        None if code == 429 => 30 * 2_u64.pow(attempt),
                        None => 2_u64.pow(attempt + 1),
                    };
                    ensure!(delay <= 120, "Fonte: HTTP {code}; aguarde {delay}s antes de tentar novamente");
                    eprintln!("Fonte: HTTP {code}; aguardando {delay}s para tentar novamente");
                    std::thread::sleep(Duration::from_secs(delay));
                }
                Err(error) => return Err(anyhow!("Fonte: {url}: {error}")),
            }
        }
        bail!("Fonte indisponível: {url}")
    }
}

fn filename(input: &str) -> String {
    let mut name: String = input.chars().map(|c| if c.is_control() || "/\\:*?\"<>|".contains(c) { '_' } else { c }).take(100).collect();
    while name.len() > 100 {
        let _ = name.pop();
    }
    let name = name.trim().trim_matches('.');
    if name.is_empty() { "serie".into() } else { name.into() }
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

fn download(client: &mut Client, path: &Path, chapter_url: &str, urls: &[&str]) -> Result<()> {
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
    let parallelism = client.source.parallelism();
    for (batch, urls_batch) in urls.chunks(parallelism).enumerate() {
        let images: Vec<Vec<u8>> = if parallelism == 1 {
            let url = urls_batch.first().context("lote de imagens vazio")?;
            let data = client.get_limited(url, 16 * 1024 * 1024, true)?;
            image::guess_format(&data).context("a página recebida não é uma imagem")?;
            vec![data]
        } else {
            pool.install(|| {
                urls_batch
                    .par_iter()
                    .map(|url| {
                        let mut image_client = Client { source: client.source, agent: client.agent.clone(), last: None };
                        let data = image_client.get_limited(url, 16 * 1024 * 1024, true)?;
                        image::guess_format(&data).context("a página recebida não é uma imagem")?;
                        Ok(data)
                    })
                    .collect::<Result<_>>()
            })?
        };
        for (offset, data) in images.iter().enumerate() {
            let index = batch * parallelism + offset;
            let format = image::guess_format(data)?;
            let extension = format.extensions_str().first().context("formato de imagem desconhecido")?;
            zip.start_file(format!("{:05}.{extension}", index + 1), options)?;
            zip.write_all(data)?;
            println!("     página {}/{}", index + 1, urls.len());
        }
    }
    zip.start_file("kindle-turbo.json", options)?;
    zip.write_all(serde_json::to_string(&json!({"source":client.source.id(),"chapter_url":chapter_url,"pages":urls.len()}))?.as_bytes())?;
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

#[derive(Debug)]
struct Selection {
    from: Option<f64>,
    to: Option<f64>,
    chapter: Option<String>,
    out: PathBuf,
    kindle: Option<PathBuf>,
}

impl Selection {
    fn new(all: bool, from: Option<f64>, to: Option<f64>, chapter: Option<String>, out: PathBuf, kindle: Option<PathBuf>) -> Result<Self> {
        ensure!(all || from.is_some() || to.is_some() || chapter.is_some(), "Escolha --all, --chapter ID ou um intervalo --from N --to N");
        ensure!(from.into_iter().chain(to).all(|n| n.is_finite() && n >= 0.0), "Intervalo inválido");
        ensure!(from.unwrap_or(0.0) <= to.unwrap_or(f64::MAX), "--from deve ser menor ou igual a --to");
        if let Some(root) = &kindle {
            ensure!(root.join("documents").is_dir(), "Kindle não encontrado em {}", root.display());
        }
        Ok(Self { from, to, chapter, out, kindle })
    }

    fn includes(&self, chapter: &Chapter) -> bool {
        if let Some(id) = &self.chapter {
            return chapter.id == *id || chapter.url == *id;
        }
        (self.from.is_none() && self.to.is_none())
            || chapter.number.is_some_and(|n| n >= self.from.unwrap_or(0.0) && n <= self.to.unwrap_or(f64::MAX))
    }
}

fn load_series(client: &mut Client, input: &str) -> Result<Series> {
    match client.source {
        Source::Asura => asura::series(client, input),
        Source::WeebCentral => weebcentral::series(client, input),
    }
}

fn chapter_filename(source: Source, chapter: &Chapter) -> String {
    if source == Source::Asura {
        format!("Capítulo {}.cbz", chapter.id)
    } else {
        format!("{} [{}].cbz", filename(&chapter.title), chapter.id)
    }
}

fn download_chapter(client: &mut Client, series: &Series, chapter: &Chapter, folder: &Path, kindle: Option<&Path>) -> Result<()> {
    let path = folder.join(chapter_filename(client.source, chapter));
    if path.exists() {
        ensure!(valid_archive(&path, &chapter.url), "CBZ existente inválido; mova-o antes de tentar novamente: {}", path.display());
        println!("já baixado: {}", chapter.id);
    } else {
        let urls = match client.source {
            Source::Asura => asura::page_urls(client, series, chapter)?,
            Source::WeebCentral => weebcentral::page_urls(client, chapter)?,
        };
        let urls: Vec<_> = urls.iter().map(String::as_str).collect();
        download(client, &path, &chapter.url, &urls)?;
        println!("salvo: {}", path.display());
    }
    if let Some(root) = kindle {
        let dest = root.join("documents/Manhwas").join(folder.file_name().context("pasta sem nome")?);
        std::fs::create_dir_all(&dest)?;
        copy_archive(&path, &dest.join(path.file_name().context("CBZ sem nome")?), &chapter.url)?;
    }
    Ok(())
}

fn download_series(client: &mut Client, series: &Series, selection: &Selection) -> Result<()> {
    let namespace = if client.source == Source::Asura { series.id.clone() } else { format!("{}-{}", client.source.id(), series.id) };
    let folder = selection.out.join(format!("{} [{namespace}]", filename(&series.title)));
    std::fs::create_dir_all(&folder)?;
    let mut selected = 0;
    let mut failures = 0;
    for chapter in series.chapters.iter().filter(|chapter| selection.includes(chapter)) {
        if chapter.locked {
            println!("ignorado: capítulo {} bloqueado", chapter.id);
            continue;
        }
        selected += 1;
        if let Err(error) = download_chapter(client, series, chapter, &folder, selection.kindle.as_deref()) {
            eprintln!("ERRO capítulo {}: {error:#}", chapter.id);
            failures += 1;
        }
    }
    ensure!(selected > 0, "Nenhum capítulo público na seleção escolhida");
    ensure!(failures == 0, "{failures} capítulo(s) falharam; execute novamente para retomar os CBZ concluídos");
    Ok(())
}

#[expect(clippy::redundant_pub_crate, reason = "binary entry point must be visible to main while unreachable_pub forbids pub")]
pub(super) fn run(source: Source, command: Command) -> Result<()> {
    let mut client = Client::new(source);
    match command {
        Command::Search { query, page } => {
            let (items, more) = match source {
                Source::Asura => asura::search(&mut client, query.as_deref().unwrap_or_default(), page)?,
                Source::WeebCentral => weebcentral::search(&mut client, query.as_deref().unwrap_or_default(), page)?,
            };
            for item in &items {
                println!("{}\t{}", item.id, item.title);
            }
            println!("{} resultado(s), página {page}; próxima página: {more}", items.len());
        }
        Command::Chapters { series } => {
            let series = load_series(&mut client, &series)?;
            for chapter in series.chapters {
                println!("{}\t{}\t{}", chapter.id, if chapter.locked { "bloqueado" } else { "público" }, chapter.title);
            }
        }
        Command::Download { series, all, from, to, chapter, out, kindle } => {
            let selection = Selection::new(all, from, to, chapter, out, kindle)?;
            let series = load_series(&mut client, &series)?;
            download_series(&mut client, &series, &selection)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing)]
    use super::*;

    #[test]
    fn selection_keeps_non_numeric_entries_and_distinct_chapter_identities() {
        let mut chapter = Chapter {
            id: "one".into(),
            title: "Volume 1".into(),
            number: None,
            url: "https://weebcentral.com/chapters/one".into(),
            locked: false,
        };
        let all = Selection::new(true, None, None, None, PathBuf::from("out"), None).unwrap();
        let range = Selection::new(false, Some(2.0), Some(3.0), None, PathBuf::from("out"), None).unwrap();
        let explicit = Selection::new(false, None, None, Some("one".into()), PathBuf::from("out"), None).unwrap();
        assert!(all.includes(&chapter) && explicit.includes(&chapter) && !range.includes(&chapter));
        chapter.title = "Chapter 2.5".into();
        chapter.number = Some(2.5);
        assert!(range.includes(&chapter));
        let first = chapter_filename(Source::WeebCentral, &chapter);
        chapter.id = "two".into();
        assert_ne!(first, chapter_filename(Source::WeebCentral, &chapter));
        assert!(!explicit.includes(&chapter));
        chapter.id = "1".into();
        assert_eq!(chapter_filename(Source::Asura, &chapter), "Capítulo 1.cbz");
    }

    #[test]
    fn weebcentral_downloads_are_paced_and_tagged_with_their_source() {
        let image = include_bytes!("../tests/fixtures/asura/page.png").to_vec();
        let (url, server) = serve_responses(vec![(image.clone(), "200 OK".into()), (image, "200 OK".into())]);
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("weeb.cbz");
        let started = Instant::now();
        download(&mut Client::new(Source::WeebCentral), &path, "chapter:weeb", &[&url, &url]).unwrap();
        server.join().unwrap();
        assert!(started.elapsed() >= Duration::from_millis(1900));
        let mut archive = zip::ZipArchive::new(File::open(path).unwrap()).unwrap();
        let mut manifest = String::new();
        archive.by_name("kindle-turbo.json").unwrap().read_to_string(&mut manifest).unwrap();
        assert_eq!(serde_json::from_str::<Value>(&manifest).unwrap()["source"], "weebcentral");
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
        assert_eq!(Client::new(Source::Asura).get(&url).unwrap(), image);
        handle.join().unwrap();
        let (url, handle) = serve_once(b"not found".to_vec(), "404 Not Found");
        assert!(Client::new(Source::Asura).get(&url).is_err());
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
        download(&mut Client::new(Source::Asura), &path, "chapter:1", &[&url]).unwrap();
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
        assert!(download(&mut Client::new(Source::Asura), &path, "chapter:1", &[&url]).is_err());
        handle.join().unwrap();
        assert!(!path.exists());
        assert!(!partial.exists());
        std::fs::write(&partial, b"another download").unwrap();
        assert!(download(&mut Client::new(Source::Asura), &path, "chapter:1", &[]).is_err());
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
        download(&mut Client::new(Source::Asura), &path, "chapter:parallel", &urls).unwrap();
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
