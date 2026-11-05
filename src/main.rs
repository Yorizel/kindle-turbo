#![forbid(unsafe_code)]

use kindle_turbo::{comic, epub, images, kf8, passthrough, reader};

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

use anyhow::{Context, Result, anyhow};
use clap::Parser;
use rayon::prelude::*;
use std::path::{Path, PathBuf};
use std::time::Instant;

/// Conversor EPUB / manhwa -> AZW3 (Kindle) ultra rápido, usa todos os núcleos.
#[derive(Parser, Debug)]
#[command(version, about)]
#[expect(clippy::struct_excessive_bools, reason = "each bool is an independent CLI switch; the flag names are part of the interface")]
struct Cli {
    /// Arquivos .epub, .cbz/.zip ou pastas de imagens (manhwa/mangá)
    #[arg(required = true)]
    inputs: Vec<PathBuf>,

    /// Pasta de saída (padrão: pasta atual)
    #[arg(short, long, default_value = ".")]
    out: PathBuf,

    /// Copia direto para o Kindle conectado por USB (e grava a capa da biblioteca)
    #[arg(short, long)]
    kindle: bool,

    /// Imagem de capa personalizada (jpg/png/webp)
    #[arg(short, long)]
    cover: Option<PathBuf>,

    /// Título (útil para manhwa a partir de pasta)
    #[arg(short, long)]
    title: Option<String>,

    /// Não comprimir o texto (arquivo maior, conversão um pouco mais rápida)
    #[arg(long)]
    no_compress: bool,

    /// Não buscar capa na internet quando o EPUB não tiver
    #[arg(long)]
    offline: bool,

    /// Modo `KOReader`: manhwa vira um CBZ por capítulo em `documents/Manhwas/<Série>`;
    /// EPUB vai como está para `documents/Novels`.
    #[arg(long)]
    koreader: bool,

    /// Só verifica arquivos AZW3 (decodifica tudo e mostra um resumo)
    #[arg(long)]
    check: bool,

    /// Número de threads (padrão: todos os núcleos)
    #[arg(short, long)]
    jobs: Option<usize>,
}

fn sanitize(name: &str) -> String {
    let s: String = name.chars().map(|c| if matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') { '_' } else { c }).collect();
    let s = s.trim().trim_matches('.').to_owned();
    let s: String = s.chars().take(150).collect();
    if s.is_empty() { "livro".into() } else { s }
}

fn find_kindle() -> Result<PathBuf> {
    let user = std::env::var("USER").unwrap_or_default();
    let candidates = [format!("/run/media/{user}/Kindle"), format!("/media/{user}/Kindle"), "/mnt/kindle".into()];
    for c in &candidates {
        let p = PathBuf::from(c);
        if p.join("documents").is_dir() {
            return Ok(p);
        }
    }
    // try to mount it
    if Path::new("/dev/disk/by-label/Kindle").exists() {
        let st = std::process::Command::new("udisksctl").args(["mount", "-b", "/dev/disk/by-label/Kindle"]).output();
        if st.is_ok() {
            for c in &candidates {
                let p = PathBuf::from(c);
                if p.join("documents").is_dir() {
                    return Ok(p);
                }
            }
        }
    }
    Err(anyhow!("Kindle não encontrado. Conecte por USB e desbloqueie a tela."))
}

struct Done {
    input: PathBuf,
    out: PathBuf,
    secs: f64,
    size: usize,
    warnings: Vec<String>,
}

/// Bytes as megabytes (10^6). Truncating to whole kilobytes first keeps the
/// result exact at the one-decimal boundaries used for display.
fn megabytes(bytes: usize) -> f64 {
    f64::from(u32::try_from(bytes / 1000).unwrap_or(u32::MAX)) / 1e3
}

fn convert_one(input: &Path, cli: &Cli, cover: Option<&Vec<u8>>, out_dir: &Path, kindle: Option<&Path>) -> Result<Done> {
    let t = Instant::now();
    let lower = input.to_string_lossy().to_lowercase();
    if [".azw3", ".mobi", ".azw"].iter().any(|e| lower.ends_with(e)) {
        let data = std::fs::read(input).with_context(|| format!("lendo {}", input.display()))?;
        let info = passthrough::read(&data)?;
        let ext = lower.rsplit('.').next().unwrap_or("azw3");
        let name = format!("{}.{ext}", sanitize(cli.title.as_deref().unwrap_or(&info.title)));
        let path = out_dir.join(&name);
        if path.canonicalize().ok() != input.canonicalize().ok() {
            std::fs::write(&path, &data)?;
        }
        let mut warnings = Vec::new();
        let thumb = match cover {
            Some(c) => images::thumbnail(c, 330, 470, 64 * 1024).ok(),
            None => info.thumb,
        };
        if let Some(k) = kindle {
            std::fs::write(k.join("documents").join(&name), &data)?;
            match (&info.asin, &thumb) {
                (Some(asin), Some(th)) => {
                    let dir = k.join("system").join("thumbnails");
                    if dir.is_dir() {
                        std::fs::write(dir.join(format!("thumbnail_{asin}_{}_portrait.jpg", info.cdetype)), th)?;
                    }
                }
                _ => warnings.push("arquivo sem ASIN/capa: o Kindle pode mostrar capa genérica (converta do EPUB se tiver)".into()),
            }
        }
        return Ok(Done { input: input.to_path_buf(), out: path, secs: t.elapsed().as_secs_f64(), size: data.len(), warnings });
    }
    if cli.koreader {
        return koreader_one(input, cli, out_dir, kindle, t);
    }
    let ends_with = |ext: &str| lower.ends_with(ext);
    let book = if input.is_dir() || ends_with(".cbz") || (ends_with(".zip") && !is_epub_zip(input)) {
        comic::open(input, cli.title.as_deref())?
    } else {
        let bytes = std::fs::read(input).with_context(|| format!("lendo {}", input.display()))?;
        let mut b = epub::open(&bytes)?;
        if let Some(t) = &cli.title {
            b.meta.title.clone_from(t);
        }
        b
    };
    let out = kf8::convert(&book, &kf8::Options { cover: cover.cloned(), compress: !cli.no_compress, fetch_cover: !cli.offline })?;
    let name = format!("{}.azw3", sanitize(&out.title));
    let path = out_dir.join(&name);
    std::fs::write(&path, &out.data).with_context(|| format!("gravando {}", path.display()))?;
    if let Some(k) = kindle {
        std::fs::write(k.join("documents").join(&name), &out.data)?;
        if let Some(th) = &out.library_thumb {
            let dir = k.join("system").join("thumbnails");
            if dir.is_dir() {
                std::fs::write(dir.join(format!("thumbnail_{}_EBOK_portrait.jpg", out.asin)), th)?;
            }
        }
    }
    Ok(Done { input: input.to_path_buf(), out: path, secs: t.elapsed().as_secs_f64(), size: out.data.len(), warnings: out.warnings })
}

fn is_epub_zip(p: &Path) -> bool {
    std::fs::File::open(p)
        .ok()
        .and_then(|f| zip::ZipArchive::new(f).ok())
        .is_some_and(|a| a.index_for_name("META-INF/container.xml").is_some())
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    if let Some(j) = cli.jobs {
        rayon::ThreadPoolBuilder::new().num_threads(j).build_global().ok();
    }
    let total = Instant::now();
    if cli.check {
        check(&cli.inputs);
        return Ok(());
    }
    std::fs::create_dir_all(&cli.out)?;
    let cover = match &cli.cover {
        Some(p) => Some(std::fs::read(p).with_context(|| format!("lendo capa {}", p.display()))?),
        None => None,
    };
    let kindle = if cli.kindle { Some(find_kindle()?) } else { None };

    let results: Vec<(PathBuf, Result<Done>)> =
        cli.inputs.par_iter().map(|i| (i.clone(), convert_one(i, &cli, cover.as_ref(), &cli.out, kindle.as_deref()))).collect();

    let mut failed = 0;
    for (input, r) in results {
        match r {
            Ok(d) => {
                println!("ok   {:>7.2}s  {:>7.1} MB  {}", d.secs, megabytes(d.size), d.out.display());
                for w in d.warnings.iter().take(5) {
                    println!("     aviso: {w}");
                }
                if d.warnings.len() > 5 {
                    println!("     (+{} avisos)", d.warnings.len() - 5);
                }
                let _ = d.input;
            }
            Err(e) => {
                failed += 1;
                println!("ERRO {}: {e:#}", input.display());
            }
        }
    }
    if kindle.is_some() {
        let _ = std::process::Command::new("sync").status();
        println!("copiado para o Kindle (ejete antes de desconectar)");
    }
    println!("total: {:.2}s", total.elapsed().as_secs_f64());
    if failed > 0 {
        std::process::exit(1);
    }
    Ok(())
}

/// Decode each input and print a summary; exits with status 1 if any input fails.
fn check(inputs: &[PathBuf]) {
    let mut bad = 0;
    for p in inputs {
        let res = std::fs::read(p).map_err(anyhow::Error::from).and_then(|d| reader::read(&d));
        match res {
            Ok(b) => println!(
                "ok   {}: \"{}\", {} arquivos, {} entradas no sumário, {:.1} MB de texto, capa: {}",
                p.display(),
                b.title,
                b.files.len(),
                b.toc.len(),
                megabytes(b.text.len()),
                if b.exth_u32(201).is_some() { "sim" } else { "não" }
            ),
            Err(e) => {
                bad += 1;
                println!("ERRO {}: {e:#}", p.display());
            }
        }
    }
    if bad > 0 {
        std::process::exit(1);
    }
}

fn is_comic(input: &Path) -> bool {
    let ext = input.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase);
    input.is_dir() || ext.as_deref() == Some("cbz") || (ext.as_deref() == Some("zip") && !is_epub_zip(input))
}

/// `KOReader` reads EPUB and CBZ natively: copy EPUBs, pre-render comics.
fn koreader_one(input: &Path, cli: &Cli, out_dir: &Path, kindle: Option<&Path>, t: Instant) -> Result<Done> {
    // manhwa -> documents/Manhwas/<Série>/<Capítulo>.cbz (one file per chapter)
    // novel  -> documents/Novels/<Título>.epub (as-is)
    let mut files: Vec<(PathBuf, Vec<u8>)> = Vec::new();
    if is_comic(input) {
        let rendered = comic::render(input, cli.title.as_deref())?;
        let series = sanitize(&rendered.title);
        let single = rendered.chapters.len() == 1;
        for ch in rendered.chapters.iter().filter(|c| !c.pages.is_empty()) {
            let rel = if single {
                PathBuf::from("Manhwas").join(format!("{series}.cbz"))
            } else {
                PathBuf::from("Manhwas").join(&series).join(format!("{}.cbz", sanitize(&ch.title)))
            };
            files.push((rel, comic::chapter_cbz(&rendered.title, ch)?));
        }
    } else {
        let data = std::fs::read(input).with_context(|| format!("lendo {}", input.display()))?;
        let title = cli.title.clone().map_or_else(|| epub::open(&data).map(|b| b.meta.title), Ok)?;
        files.push((PathBuf::from("Novels").join(format!("{}.epub", sanitize(&title))), data));
    }
    let mut size = 0;
    for (rel, data) in &files {
        for root in std::iter::once(out_dir.to_path_buf()).chain(kindle.map(|k| k.join("documents"))) {
            let path = root.join(rel);
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            std::fs::write(&path, data).with_context(|| format!("gravando {}", path.display()))?;
        }
        size += data.len();
    }
    let out = files.first().map_or_else(|| out_dir.to_path_buf(), |(rel, _)| out_dir.join(rel));
    let out = if files.len() > 1 { out.parent().map_or_else(|| out.clone(), Path::to_path_buf) } else { out };
    Ok(Done { input: input.to_path_buf(), out, secs: t.elapsed().as_secs_f64(), size, warnings: Vec::new() })
}
