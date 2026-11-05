# kindle-turbo

Conversor EPUB / manhwa → AZW3 (Kindle) escrito em Rust. Usa todos os núcleos do processador.

| Livro | Calibre | kindle-turbo |
|---|---|---|
| Lord of the Mysteries (1432 caps, 328 ilustrações) | 12 min 51 s, 517 MB | **2,7 s, 86 MB** |
| Pursuit of the Truth (1506 caps) | 3 min 39 s | **1,1 s** |
| The Second Coming of Gluttony | 2 min 17 s | **2,5 s** |
| 7 livros de uma vez | ~13 min | **2,7 s** |

Medido num processador de 28 threads. O texto convertido foi conferido com o leitor do próprio Calibre: mesma contagem de palavras do EPUB original.

## O que faz

- **EPUB → AZW3 (KF8)**: arquivo único, sumário (NCX), links internos, capa, CSS, ilustrações. Cada capítulo, imagem e registro de texto é processado em paralelo.
- **Manhwa / webtoon / mangá**: pasta de imagens (uma subpasta por capítulo) ou `.cbz`. A tira longa é emendada e recortada em páginas do tamanho da tela (1236×1648), cortando de preferência em faixas em branco para não partir balões. Capítulos viram o sumário, em ordem natural (Cap 2 antes de Cap 10).
- **Capa na biblioteca do Kindle**: livros copiados por USB costumam aparecer sem capa. Com `--kindle`, o programa grava também a miniatura em `system/thumbnails/`, que é onde o Kindle procura.
- **AZW3/MOBI prontos**: são copiados como estão, com a capa extraída para a biblioteca.
- **Capa personalizada**: `--cover imagem.jpg`.
- **EPUB sem capa**: busca a capa oficial no AniList / Open Library; sem internet (`--offline`) gera uma capa com título e autor.
- **Verificação**: `kindle-turbo --check livro.azw3` decodifica o arquivo inteiro com um leitor KF8 independente (incluso) e mostra um resumo.
- Imagens WebP/BMP viram JPEG ajustado à tela; JPEG/PNG/GIF passam sem recompressão (PNG otimizado sem perdas).
- **Manhwa para KOReader** (`--koreader`): o capítulo inteiro vira uma tira contínua; os quadros são detectados por bordas (funciona em fundo escuro e gradiente), cada página recebe quadros inteiros (balão junto da cena) e só quadros maiores que a tela são cortados, com sobreposição para não perder o contexto. Todas as páginas têm o mesmo tamanho (sem zoom pulando), sem upscaling, autocontraste para e-ink, cinza JPEG q55 com Huffman otimizado. Aceita também uma pasta com um `.cbz` por capítulo (como os downloaders salvam): converte em streaming, 4 capítulos por vez, e pula os que já foram convertidos. 196 capítulos (2,5 GB) em ~2,5 min. Algoritmo inspirado no `comic2panel` do KCC.

## Instalar

```sh
cargo install --path .
```

## Usar

```sh
# converte e salva na pasta atual
kindle-turbo livro.epub

# vários livros de uma vez, direto para o Kindle conectado por USB (com capa)
kindle-turbo --kindle ~/Downloads/*.epub

# manhwa a partir de uma pasta (Cap 1/, Cap 2/, ...)
kindle-turbo --kindle -t "Solo Leveling" ~/manhwa/solo-leveling/

# manhwa em .cbz com capa própria
kindle-turbo --kindle --cover capa.jpg volume1.cbz
```

Opções:

| Opção | |
|---|---|
| `-o, --out DIR` | pasta de saída (padrão: atual) |
| `-k, --kindle` | copia para o Kindle montado em `/run/media/$USER/Kindle` e grava a capa da biblioteca |
| `-c, --cover IMG` | capa personalizada |
| `-t, --title T` | título (use com uma entrada só) |
| `-j, --jobs N` | limita o número de threads |
| `--no-compress` | não comprime o texto |
| `--offline` | não busca capa na internet |
| `--check` | só verifica arquivos AZW3 |
| `--koreader` | para KOReader: manhwa vira **um CBZ por capítulo** em `documents/Manhwas/<Série>/` (lê passando a página); EPUB vai como está para `documents/Novels` |

## AsuraScans

O CLI Rust oferece busca, lista de capítulos e download em CBZ, com até quatro imagens em paralelo e retomada dos capítulos concluídos. Capítulos bloqueados e imagens embaralhadas são indicados como indisponíveis.

```sh
kindle-turbo asura search "solo leveling" --page 1
kindle-turbo asura chapters solo-leveling
kindle-turbo asura download solo-leveling --all -o downloads
kindle-turbo asura download solo-leveling --from 1 --to 5 -o downloads --kindle /mount/Kindle
```

No Kindle, o plugin KOReader em `koplugin/kindle-turbo.koplugin/` oferece busca, catálogo, download de todos os capítulos e leitura offline com escolha da pasta destino. A leitura usa o ReaderUI nativo do KOReader, que guarda o progresso. Para instalar por USB, execute `scripts/install-koplugin.sh /mount/Kindle`; o instalador exige a raiz reconhecível do Kindle, valida `documents` e `koreader/plugins`, não sobrescreve nada e instala também os avisos de licença e proveniência. Ejete o Kindle com segurança após a cópia.

O plugin também roda no KOReader para Linux, permitindo baixar no PC e copiar os CBZ para o Kindle. Os testes usam os módulos reais do KOReader no PC; a validação no Kindle físico ainda está pendente. Veja [o guia PC/Kindle](docs/asura.md) e [as atribuições](docs/licensing.md).

Depois de usar `--kindle`, ejete antes de desconectar:

```sh
udisksctl unmount -b /dev/disk/by-label/Kindle && udisksctl power-off -b /dev/disk/by-label/Kindle
```

## Qualidade

- `unsafe` proibido (`forbid(unsafe_code)`).
- Clippy `all` + `pedantic` + `nursery` + `cargo` + regras de `restriction` (sem `unwrap`/`expect`/`panic` fora de testes), tudo como erro. Zero `#[allow]` no código de produção.
- Compressão PalmDOC com parsing ótimo (programação dinâmica), não gulosa.
- Testes do conversor: EPUBs e imagens reais gerados no teste, convertidos e decodificados de volta pelo leitor KF8 próprio; testes de propriedade (proptest) na compressão; o binário real é executado nos testes de CLI. Testes com livros reais e com o AniList de verdade: `cargo test -- --ignored`.
- CI local: `./ci.sh` (fmt, clippy `-D warnings`, testes) e `./ci.sh --full` (inclui livros reais e AniList). O mesmo roda no GitHub Actions.

## Limitações

- Fontes embutidas no EPUB são ignoradas (o Kindle usa as dele).
- CBR (RAR) não é suportado; converta para CBZ.
- O formato KF8 foi portado do writer8 do Calibre (GPLv3), por isso este projeto também é GPLv3.

## Como funciona

1. Lê o EPUB inteiro na memória, descompactando as entradas em paralelo.
2. Processa o XHTML de cada capítulo em paralelo: links de imagem viram `kindle:embed`, CSS vira `kindle:flow`, atributos `aid` para âncoras.
3. Divide cada arquivo em esqueleto + fragmentos de até 8 KB (formato KF8), em paralelo.
4. Comprime os registros de texto de 4 KB com PalmDoc, em paralelo.
5. Gera os índices SKEL, FRAG, NCX e guia, os trailing byte sequences, o cabeçalho EXTH e o contêiner PalmDB.
