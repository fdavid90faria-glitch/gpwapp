# GPW Uploader

App desktop (Tauri 2) que automatiza o upload de tracks para o **Ghost Producer World**.
Lê uma pasta de exportação, identifica os arquivos pela nomenclatura, detecta BPM/Key,
converte WAV→MP3 (só os masters), preenche os campos com IA e envia para o GPW via API.

Veja a arquitetura completa em `../ARQUITETURA_GPWAPP_v2.md`.

## Status das fases

- [x] **Fase 1 — Esqueleto Tauri**: janela configurada, tema GPW (amarelo/escuro),
      drag-drop de pasta, ponte JS↔Rust (`ping`), plugin `fs` registrado.
- [x] **Fase 2 — Scanner** (atual): `scanner.rs` classifica os arquivos pela
      nomenclatura e mapeia para os campos reais do upload (ver `CONTRATO_UPLOAD.md`);
      comando `scan_folder`; UI lista detectados/indefinidos + avisos de faltantes.
      Testes unitários em `scanner.rs` (4/4 ok).
- [x] **Fase 3 — Análise de áudio** (atual): Essentia.js (0.1.3) reaproveitado do
      GPW em `src/essentia/` (offline, com `.wasm` local); comando `read_file_bytes`
      lê o WAV → `ArrayBuffer`; `GPW_analyzeAudio` detecta BPM/Key/Camelot no
      Extended Mix e mostra no card de análise. Mesma lógica/resultado do site.
- [x] **Fase 4 — Conversão** (atual): FFmpeg sidecar (`binaries/ffmpeg-<triple>.exe`)
      + `tauri-plugin-shell`; `converter.rs` gera 1 MP3 por master/instrumental
      presente (Extended Mix, Extended Instrumental, e Radio equivalentes) em
      320k CBR, em paralelo, emitindo `convert:progress`. Comando `convert_masters`;
      card de conversão na UI. (Dev: ffmpeg copiado p/ `target/debug/`.)
      O instrumental MP3 vai em `xf_extended_instrumental_mp3` (backend aceita
      qualquer `xf_*`; campo a adicionar no form do site depois).
- [x] Fase 5 — Login + memória
- [ ] Fase 6 — Wizard + revisão
- [ ] Fase 7 — Upload (`uploader.rs`)
- [ ] Fase 8 — Build & polish

## Pré-requisitos

- **Node.js** (instalado — testado com v24)
- **Rust + Cargo** — **NÃO instalado nesta máquina**. Necessário para compilar/rodar.
  Instale em <https://www.rust-lang.org/tools/install> (no Windows requer também os
  *Build Tools do Visual Studio* com o workload "Desktop development with C++").
  Veja os pré-requisitos do Tauri: <https://tauri.app/start/prerequisites/>

## Rodar em desenvolvimento

```bash
cd gpwapp
npm install            # já executado
npm run tauri dev      # requer Rust/Cargo instalados
```

## Estrutura

```
gpwapp/
├── src/                  # Frontend (webview)
│   ├── index.html        # Tela principal (dropzone)
│   ├── app.js            # Orquestração da UI + drag-drop
│   └── styles.css        # Tema GPW (amarelo/escuro)
└── src-tauri/            # Backend Rust
    ├── Cargo.toml
    ├── tauri.conf.json   # Janela, drag-drop, bundle
    ├── capabilities/     # Permissões (core, opener, fs)
    └── src/
        ├── main.rs
        ├── lib.rs        # Entrypoint + registro de plugins
        └── commands.rs   # Comandos expostos ao frontend (ping)
```
