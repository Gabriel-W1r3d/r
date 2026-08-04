# Ohm Player

Um player de MP3 nativo, leve e multiplataforma, feito em **Rust** com **Slint UI**.

## Destaques

- Interface profissional e responsiva
- Barra de player fixa na parte inferior
- Página inicial com recentes e atalhos
- Busca offline local
- Biblioteca com playlists, músicas, álbuns e artistas
- Histórico com estatísticas e gráficos
- Shuffle, repeat, volume e fila de reprodução
- Banco SQLite embutido
- Seleção nativa de arquivos
- Execução em CPU ou GPU via Slint

## Stack

- `slint` / `slint-build`
- `rodio`
- `rusqlite`
- `rfd`
- `id3`
- `rand`
- `image`
- `mp3-duration`

## Requisitos

- Rust toolchain
- Windows, Linux ou macOS

## Como executar

```bash
cargo run
```

Se você quiser forçar o backend de software da UI:

```bash
SLINT_BACKEND=winit-software cargo run
```

No Windows, você também pode usar o script incluso:

```powershell
.\run.ps1
```

Ou dar duplo clique em `Ohm Player.cmd`.

Se você baixar apenas o `.cmd`, ele baixa a última release automaticamente.

## Download e execução em 2 cliques

O download pronto atualmente é o pacote Windows:

- **Windows:** `.zip` com `ohm_player.exe`, `Ohm Player.cmd` e `Logo.jpg`

Ao publicar uma tag `v*`, o workflow em `.github/workflows/release.yml` anexa o zip ao release do GitHub.

## Banco de dados

O app cria automaticamente o arquivo `ohm_player.db` no diretório do projeto.

## Estrutura

- `src/main.rs` — interface entre Slint, SQLite e Rodio
- `src/db.rs` — persistência SQLite
- `ui/appwindow.slint` — interface visual
- `build.rs` — compilação do UI Slint

## Observação

O projeto foi pensado para funcionar bem em máquinas modestas e em VMs, com foco em baixo consumo e renderização flexível.
