mod comic;
mod dom;
mod epub;
mod images;
mod kf8;
mod mobi;
mod palmdoc;
mod passthrough;

use anyhow::{Context, Result, anyhow};
use clap::Parser;
use rayon::prelude::*;
use std::path::{Path, PathBuf};
use std::time::Instant;

/// Conversor EPUB / manhwa -> AZW3 (Kindle) ultra rápido, usa todos os núcleos.
#[derive(Parser, Debug)]
#[command(version, about)]
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

    /// Número de threads (padrão: todos os núcleos)
    #[arg(short, long)]
    jobs: Option<usize>,
}

fn sanitize(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| if matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') { '_' } else { c })
        .collect();
    let s = s.trim().trim_matches('.').to_string();
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
    let book =if input.is_dir() || lower.ends_with(".cbz") || (lower.ends_with(".zip") && !is_epub_zip(input)) {
        comic::open(input, cli.title.as_deref())?
    } else {
        let bytes = std::fs::read(input).with_context(|| format!("lendo {}", input.display()))?;
        let mut b = epub::open(&bytes)?;
        if let Some(t) = &cli.title {
            b.meta.title = t.clone();
        }
        b
    };
    let out = kf8::convert(&book, &kf8::Options { cover: cover.cloned(), compress: !cli.no_compress })?;
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
        .map(|a| a.index_for_name("META-INF/container.xml").is_some())
        .unwrap_or(false)
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    if let Some(j) = cli.jobs {
        rayon::ThreadPoolBuilder::new().num_threads(j).build_global().ok();
    }
    let total = Instant::now();
    std::fs::create_dir_all(&cli.out)?;
    let cover = match &cli.cover {
        Some(p) => Some(std::fs::read(p).with_context(|| format!("lendo capa {}", p.display()))?),
        None => None,
    };
    let kindle = if cli.kindle { Some(find_kindle()?) } else { None };

    let results: Vec<(PathBuf, Result<Done>)> = cli
        .inputs
        .par_iter()
        .map(|i| (i.clone(), convert_one(i, &cli, cover.as_ref(), &cli.out, kindle.as_deref())))
        .collect();

    let mut failed = 0;
    for (input, r) in results {
        match r {
            Ok(d) => {
                println!("ok   {:>7.2}s  {:>7.1} MB  {}", d.secs, d.size as f64 / 1e6, d.out.display());
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
