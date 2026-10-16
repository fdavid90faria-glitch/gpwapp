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
import { createHash, createPublicKey, verify } from "node:crypto";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");

// Repositório do app no GitHub (ajuste se mudar).
const REPO = "fdavid90faria-glitch/gpwapp-releases";

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

// Confere a .sig contra o instalador e a pubkey do tauri.conf ANTES de publicar.
// Uma .sig velha (build sem a chave) ou de outra chave passava, e o updater de
// todos os produtores recusava a versao em silencio.
{
  const pubLines = Buffer.from(conf.plugins.updater.pubkey, "base64").toString().trim().split("\n");
  const pubRaw = Buffer.from(pubLines[1], "base64"); // "Ed" | keynum(8) | pk(32)
  const pk = createPublicKey({
    key: Buffer.concat([Buffer.from("302a300506032b6570032100", "hex"), pubRaw.subarray(10)]),
    format: "der",
    type: "spki",
  });
  const sigLines = Buffer.from(signature, "base64").toString().trim().split("\n");
  const sigRaw = Buffer.from(sigLines[1], "base64"); // alg(2) | keynum(8) | sig(64)
  const data = readFileSync(join(nsisDir, exeName));
  const msg = sigRaw.subarray(0, 2).toString() === "ED" ? createHash("blake2b512").update(data).digest() : data;
  const sameKey = pubRaw.subarray(2, 10).equals(sigRaw.subarray(2, 10));
  if (!sameKey || !verify(null, msg, pk, sigRaw.subarray(10, 74))) {
    console.error(`A assinatura ${sigName} NAO corresponde a ${exeName} e a pubkey do tauri.conf.json.\nRefaca o build com TAURI_SIGNING_PRIVATE_KEY definida.`);
    process.exit(1);
  }
}
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
