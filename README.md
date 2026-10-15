# GPW Uploader

App desktop (Tauri 2, Windows) que automatiza o upload de tracks para o
**Ghost Producer World**. O produtor arrasta a pasta de exportação; o app
identifica os ficheiros pelo nome, deteta BPM/Key, converte os masters para MP3,
verifica o áudio contra o padrão GPW e sobe tudo como rascunho no site, que
abre no browser para o produtor terminar.

## Como funciona

- **Scanner** (`src-tauri/src/scanner.rs`): classifica cada ficheiro num slot do
  upload. WAVs mal classificados corrigem-se no dropdown da lista.
- **BPM/Key** (`src/essentia/`): Essentia.js 0.1.3 num Web Worker, offline.
- **MP3** (`src-tauri/src/converter.rs`): LAME 3.100.1 como sidecar
  (`src-tauri/binaries/`), 320k CBR, um por master (Extended e Radio).
- **QC** (`src-tauri/src/qc.rs` + `src/qc.js`): peak, LUFS, silêncio, vocal.
- **Upload** (`src-tauri/src/uploader.rs`): rascunho no site + envio direto ao
  R2 em partes de 64MB. Contrato em `CONTRATO_UPLOAD.md`.
- **Login** (`src/supabase.js`): Supabase com 2FA; sessão cifrada com DPAPI.

Os hashes dos binários de terceiros estão em `src-tauri/binaries/SHA256SUMS`.

## Desenvolvimento

Pré-requisitos: Node.js, Rust (com os Build Tools do Visual Studio). Ver
<https://tauri.app/start/prerequisites/>.

```bash
npm install
npm run tauri dev
cd src-tauri && cargo test
```

Publicar uma versão: ver `RELEASE.md`.
