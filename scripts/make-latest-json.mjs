// Gera o latest.json do auto-updater a partir do build de release.
//
// Uso (depois de `npm run tauri build` com a chave de assinatura):
//   node scripts/make-latest-json.mjs "Notas desta versão"
//
// Lê a versão de tauri.conf.json, encontra o instalador NSIS + a assinatura
// (.sig) e escreve src-tauri/target/release/bundle/latest.json com a URL do
// GitHub Release. Faça upload do latest.json + do instalador para a Release.

import { readFileSync, writeFileSync, readdirSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");

// Repositório do app no GitHub (ajuste se mudar).
const REPO = "fdavid90faria-glitch/gpwapp";

const conf = JSON.parse(readFileSync(join(root, "src-tauri/tauri.conf.json"), "utf8"));
const version = conf.version;
const notes = process.argv[2] || `GPW Uploader ${version}`;

const nsisDir = join(root, "src-tauri/target/release/bundle/nsis");
let files;
try {
  files = readdirSync(nsisDir);
} catch {
  console.error(`Pasta não encontrada: ${nsisDir}\nRode "npm run tauri build" primeiro.`);
  process.exit(1);
}

// Filtra pela versao atual: a pasta pode conter instaladores de releases
// anteriores, e um find() sem filtro pegaria o mais antigo (0.1.0 antes de
// 0.2.0 na ordem do diretorio) -> latest.json apontaria para a versao errada.
const tag = `_${version}_`;
const sigName = files.find((f) => f.includes(tag) && f.endsWith("-setup.exe.sig"));
const exeName = files.find((f) => f.includes(tag) && f.endsWith("-setup.exe"));
if (!sigName || !exeName) {
  console.error(`Instalador NSIS ${version} ou .sig não encontrado em ${nsisDir}.\nBuildou a versão ${version} com a chave de assinatura definida?`);
  process.exit(1);
}

const signature = readFileSync(join(nsisDir, sigName), "utf8").trim();
// O GitHub troca espaços por pontos no nome do asset baixável.
const assetName = exeName.replaceAll(" ", ".");
const url = `https://github.com/${REPO}/releases/download/v${version}/${assetName}`;

const latest = {
  version,
  notes,
  pub_date: new Date().toISOString(),
  platforms: {
    "windows-x86_64": { signature, url },
  },
};

const out = join(root, "src-tauri/target/release/bundle/latest.json");
writeFileSync(out, JSON.stringify(latest, null, 2));

console.log("latest.json gerado:", out);
console.log("\n--- conteúdo ---\n" + JSON.stringify(latest, null, 2));
console.log("\nUpload para a Release v" + version + ":");
console.log("  1) " + join(nsisDir, exeName) + "   (renomeie/garanta o nome: " + assetName + ")");
console.log("  2) " + out);
