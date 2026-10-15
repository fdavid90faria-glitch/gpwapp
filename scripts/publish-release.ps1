# Publica a release da versao atual no GitHub num comando so:
# build assinado -> latest.json -> copia de nome fixo -> GitHub Release.
#
# Uso (PowerShell, na raiz do repo):
#   $env:TAURI_SIGNING_PRIVATE_KEY = Get-Content -Raw .tauri/gpwapp_updater.key
#   $env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD = "<senha da chave>"
#   ./scripts/publish-release.ps1 "O que mudou nesta versao"
#
# A versao sai de src-tauri/tauri.conf.json. Sobe 3 assets: o instalador, o
# latest.json (auto-updater) e a copia GPW-Uploader-setup.exe (link do site).

param([string]$Notes = "")
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

if (-not $env:TAURI_SIGNING_PRIVATE_KEY -or -not $env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD) {
  throw "Defina TAURI_SIGNING_PRIVATE_KEY e TAURI_SIGNING_PRIVATE_KEY_PASSWORD antes de rodar (ver RELEASE.md)."
}

$version = (Get-Content -Raw "src-tauri/tauri.conf.json" | ConvertFrom-Json).version
$tag = "v$version"
if (-not $Notes) { $Notes = "GPW Uploader $version" }

# Verificacoes ANTES do build (que demora): republicar uma tag ja instalada
# pelos produtores, ou publicar de uma arvore suja/diferente do main, deixa
# instaladores que nao correspondem a nenhum commit.
gh release view $tag --repo fdavid90faria-glitch/gpwapp *> $null
if ($LASTEXITCODE -eq 0) { throw "$tag ja esta publicada: suba a versao em tauri.conf.json (e Cargo.toml). Nunca republique a mesma tag." }
$global:LASTEXITCODE = 0
if (git status --porcelain --untracked-files=no) { throw "Arvore com alteracoes por commitar: faca commit (ou stash) antes de publicar." }
git fetch -q origin main
if ((git rev-parse HEAD) -ne (git rev-parse origin/main)) { throw "HEAD nao e o origin/main: publique a partir do main atualizado." }
$cargoVersion = (Select-String -Path "src-tauri/Cargo.toml" -Pattern '^version = "(.+)"' | Select-Object -First 1).Matches.Groups[1].Value
if ($cargoVersion -ne $version) { throw "Versoes diferentes: tauri.conf.json $version, Cargo.toml $cargoVersion." }

# Binarios de terceiros que vao no instalador tem de bater com o SHA256SUMS.
foreach ($line in Get-Content "src-tauri/binaries/SHA256SUMS") {
  if ($line -match '^\s*(#|$)') { continue }
  $hash, $file = $line -split '\s+', 2
  if ((Get-FileHash $file -Algorithm SHA256).Hash -ne $hash.ToUpper()) { throw "$file nao bate com src-tauri/binaries/SHA256SUMS." }
}

Write-Host "==> Build assinado $tag (pode demorar)..."
npm run tauri build
if ($LASTEXITCODE -ne 0) { throw "build falhou" }

Write-Host "==> Gerando latest.json..."
node scripts/make-latest-json.mjs $Notes
if ($LASTEXITCODE -ne 0) { throw "make-latest-json falhou" }

$nsis = "src-tauri/target/release/bundle/nsis"
$exe = Get-ChildItem $nsis -Filter "*_${version}_*-setup.exe" | Select-Object -First 1
if (-not $exe) { throw "Instalador $version nao encontrado em $nsis." }
$latest = "src-tauri/target/release/bundle/latest.json"
$fixed = Join-Path $nsis "GPW-Uploader-setup.exe"
Copy-Item $exe.FullName $fixed -Force

Write-Host "==> Publicando release $tag no GitHub..."
gh release create $tag $exe.FullName $latest $fixed `
  --repo fdavid90faria-glitch/gpwapp --title "GPW Uploader $version" --notes $Notes --latest `
  --target (git rev-parse HEAD)
if ($LASTEXITCODE -ne 0) { throw "gh release create falhou. Se a tag $tag ja existir, suba a versao; nunca republique a mesma tag." }

Write-Host "OK: release $tag publicada. Aba Tools e auto-updater ja apontam para ela."
