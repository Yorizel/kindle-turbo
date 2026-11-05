# AsuraScans e WeebCentral no PC e no Kindle

O kindle-turbo busca títulos, lista capítulos e baixa CBZ do AsuraScans e do WeebCentral. O programa Rust faz isso no computador; o plugin Lua faz isso dentro do KOReader, tanto no computador quanto no Kindle com jailbreak.

Os CBZ preservam as imagens do site. No PC, AsuraScans baixa até quatro imagens em paralelo; WeebCentral mantém uma requisição a cada dois segundos, incluindo as imagens. No Kindle, o plugin baixa uma imagem por vez e usa o gravador ZIP nativo do KOReader sem recompressão. A leitura, o zoom, os gestos e o progresso ficam com o KOReader.

## Baixar pelo CLI no PC

```sh
kindle-turbo asura search "return of the disaster"
kindle-turbo asura chapters return-of-the-disaster-class-hero
kindle-turbo asura download return-of-the-disaster-class-hero --all -o "$HOME/Downloads/Kindle Turbo"
kindle-turbo asura download return-of-the-disaster-class-hero --from 1 --to 5 -o downloads --kindle /mount/Kindle

kindle-turbo weebcentral search "return of the disaster"
kindle-turbo weebcentral chapters 01J76XYF85TPSFGEH5GCC1VWAW
kindle-turbo weebcentral download 01J76XYF85TPSFGEH5GCC1VWAW --all -o downloads
# Um capítulo específico pelo ID retornado pela lista:
kindle-turbo weebcentral download 01J76XYF85TPSFGEH5GCC1VWAW --chapter 01J76XZ5Q84XEMX0NDR6SKRCSM
```

Use o slug retornado pela busca AsuraScans ou o ID retornado pelo WeebCentral. WeebCentral também aceita a URL da série. `--from N` seleciona de N em diante; `--to N` seleciona até N; os limites são inclusivos e aceitam capítulos decimais. `--all` inclui todos os capítulos públicos, inclusive volumes e extras sem número. No WeebCentral, os intervalos se aplicam aos rótulos numéricos Chapter/Episode; use `--chapter ID` para selecionar um volume/extra ou uma edição específica. O `download --kindle` dessas fontes recebe o caminho do Kindle montado; o `--kindle` do conversor original continua detectando o aparelho automaticamente.

Cada capítulo vira um CBZ. As pastas WeebCentral incluem a fonte e o ID da série; seus arquivos incluem o ID do capítulo, preservando registros com o mesmo número ou rótulos de Volume/Special. Os caminhos AsuraScans existentes são mantidos. Ao repetir o comando, os CBZ concluídos são verificados e reaproveitados, incluindo os já copiados para o Kindle. Arquivos inválidos não são sobrescritos silenciosamente. Downloads e cópias incompletos usam `.part`; se um encerramento forçado deixar esse arquivo, mova-o antes de retomar. Um cancelamento normal no plugin limpa apenas o parcial criado por aquele download.

## Instalar o plugin no Kindle

Feche o KOReader e conecte o Kindle por USB. Na raiz do repositório:

```sh
sh scripts/install-koplugin.sh /run/media/"$USER"/Kindle
```

Troque o caminho pelo ponto de montagem real, contendo `documents/` e `koreader/plugins/`. O instalador Linux copia o plugin com licenças e proveniência, recusa instalações existentes e não modifica configurações ou downloads. Para substituir uma instalação existente, guarde uma cópia dela antes de movê-la para outro lugar.

Ejete o armazenamento, desconecte o USB e reinicie o KOReader. Abra **Kindle Turbo** no menu, possivelmente dentro de **More tools**:

1. Escolha **AsuraScans** ou **WeebCentral**, depois **Buscar manhwa** ou **Catálogo**.
2. Toque num capítulo público para baixar e abrir, ou escolha **Baixar todos os capítulos públicos**.
3. **Downloads / ler offline** para abrir os CBZ existentes.
4. **Pasta dos downloads** para escolher o destino. O padrão fica em `koreader/kindle-turbo/downloads/`.

O download tem botão de cancelamento e mantém os capítulos concluídos. Para lotes longos no Kindle, o recurso nativo **Keep alive / Manter ligado** pode ser necessário para evitar a suspensão do sistema. A suspensão interrompe o download atual com limpeza do parcial; reexecute para continuar pelos capítulos restantes.

## Usar o plugin no PC e transferir depois

No KOReader para Linux, coloque a pasta `koplugin/kindle-turbo.koplugin/` no diretório de plugins da instalação ou no diretório `plugins/` da pasta de dados do KOReader. Inclua as licenças da raiz e de `third-party/`, como faz o instalador USB.

Nesta instalação local foi criado o atalho **Kindle Turbo · KOReader**, com comando `koreader-kindle-turbo`. Os dados do aplicativo ficam em `~/.local/share/kindle-turbo/koreader/` e os downloads em `~/Downloads/Kindle Turbo/`. Feche e reabra o aplicativo depois de atualizar os arquivos do plugin.

Copie as pastas de CBZ para `documents/Manhwas/` no Kindle e abra pelo navegador nativo do KOReader. Se quiser usar o menu **Downloads / ler offline**, configure **Pasta dos downloads** apontando para essa mesma pasta. Essa cópia transfere os quadrinhos; o progresso de leitura de cada aparelho continua sendo administrado pelo próprio KOReader.

## Verificações e limites

```sh
# Módulos JSON/libarchive reais de uma instalação desktop do KOReader:
python3 scripts/test-koreader.py /caminho/do/koreader
```

O diretório informado precisa conter `reader.lua` e `luajit`. No host compartilhado, execute esse check por `resource-run --wait`. Ele valida fixtures Astro, capítulos bloqueados, respostas sem páginas, CBZ real, ordem das imagens, cancelamento, arquivos parciais, retomada e instalação USB. A interface e a rede são simuladas nesse teste; JSON, utilitários e libarchive são os módulos reais do KOReader.

As fontes integradas são AsuraScans e WeebCentral. Capítulos pagos/bloqueados não são baixados; imagens embaralhadas ainda não são reconstruídas. Mudanças no site ou bloqueios de rede geram erro. O plugin foi verificado com o KOReader no PC; o funcionamento no Kindle físico ainda precisa ser validado.
