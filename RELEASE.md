# Como publicar uma atualização (auto-updater)

O app verifica o GitHub Releases ao abrir e instala atualizações sozinho. Este
guia é o passo a passo de **cada nova versão**. O build é **local** (o FFmpeg
tem 217 MB e fica só na sua máquina).

## Pré-requisitos (uma vez só)

1. **Repositório no GitHub** para o app: `fdavid90faria-glitch/gpwapp` (público).
   - Se mudar o nome/owner, atualize em **dois** lugares:
     - `src-tauri/tauri.conf.json` → `plugins.updater.endpoints`
     - `scripts/make-latest-json.mjs` → `REPO`
2. **Guarde a chave privada** com segurança (backup offline!):
   `.tauri/gpwapp_updater.key` — se perdê-la, os updates param de funcionar.
   (A pública já está no `tauri.conf.json`. A pasta `.tauri/` está no `.gitignore`.)

> A primeira versão que você distribuir já contém o updater, então a partir da
> **próxima** as atualizações são automáticas. Usuários de uma versão antiga
> SEM updater precisariam reinstalar uma última vez na mão.

## A cada release

### 1. Suba o número da versão
Edite **`src-tauri/tauri.conf.json`** → `"version"` (ex.: `0.1.0` → `0.2.0`).
(O `Cargo.toml` lê a versão do pacote; manter os dois iguais é boa prática.)

### 2. Build assinado (PowerShell)
```powershell
$env:TAURI_SIGNING_PRIVATE_KEY = Get-Content -Raw .tauri/gpwapp_updater.key
$env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD = "<a senha que voce definiu ao gerar a chave>"
npm run tauri build
```
Isso gera, em `src-tauri/target/release/bundle/nsis/`:
- `GPW Uploader_<versão>_x64-setup.exe`  (instalador)
- `GPW Uploader_<versão>_x64-setup.exe.sig`  (assinatura)

### 3. Gere o manifesto
```powershell
node scripts/make-latest-json.mjs "O que mudou nesta versão"
```
Cria `src-tauri/target/release/bundle/latest.json`.

### 4. Crie a Release no GitHub
- Tag: `v<versão>` (ex.: `v0.2.0`) — **tem de começar com `v`**.
- Faça upload de **2 arquivos**:
  1. o instalador `...-setup.exe`
  2. o `latest.json`
- Publique (não marque como *pre-release*).

> O GitHub troca espaços por pontos no nome do arquivo baixável
> (`GPW Uploader_...` → `GPW.Uploader_...`). O `latest.json` já usa esse nome
> com pontos — não renomeie o instalador manualmente, só suba como está.

### 5. Pronto
Na próxima vez que um usuário abrir o app, aparece a barra **“Version X is
available — Update & restart”**. Ele clica, baixa, instala e reinicia.

---

## Resumo rápido
```powershell
# 1. bump version em src-tauri/tauri.conf.json
$env:TAURI_SIGNING_PRIVATE_KEY = Get-Content -Raw .tauri/gpwapp_updater.key
$env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD = "<sua senha da chave>"
npm run tauri build
node scripts/make-latest-json.mjs "notas"
# 2. GitHub Release tag vX.Y.Z + upload do setup.exe e do latest.json
```
