// ============================================================
//  GPW ESSENTIA WORKER — corre o Essentia.js (BPM + Key) fora da pagina.
//
//  Porque um worker: o glue emscripten do Essentia usa `new Function`, que
//  exige 'unsafe-eval' na CSP. Em vez de abrir a CSP do app inteiro, o motor
//  passa a correr aqui: o Tauri so poe o header Content-Security-Policy nas
//  respostas .html (ver manager/mod.rs: `is_html`), por isso um dedicated
//  worker carregado deste .js nao recebe CSP nenhuma. Mesma solucao que o
//  site ja usava em public/gpw-essentia-worker.js.
//
//  O Web Audio nao existe em workers, por isso a pagina decodifica e manda
//  PCM mono 44.1k ja pronto.
//
//  Mensagens:  { id, pcm: Float32Array }  ->  { id, bpm, key, scale } | { id, error }
// ============================================================

// Ficheiros locais (o app funciona offline), ao lado deste worker.
var BASE = self.location.href.replace(/[^/]*$/, '')

// A build .web.js foi compilada so para pagina (ENVIRONMENT_IS_WORKER=false) e
// le document.currentScript.src para saber de onde buscar o .wasm. Este stub
// da-lhe esse URL; sem ele rebenta com "document is not defined".
self.document = { currentScript: { src: BASE + 'essentia-wasm.web.js' }, title: '' }
importScripts('essentia-wasm.web.js', 'essentia.js-core.js')

var ready = null
function getEssentia() {
  if (ready) return ready
  var wasm = self.EssentiaWASM
  ready = Promise.resolve(
    typeof wasm === 'function'
      ? wasm()
      : wasm && typeof wasm.EssentiaWASM === 'function'
        ? wasm.EssentiaWASM()
        : (wasm && wasm.EssentiaWASM) || wasm
  ).then(function (w) { return new self.Essentia(w) })
  return ready
}

self.onmessage = function (e) {
  var id = e.data.id
  var pcm = e.data.pcm
  getEssentia().then(function (essentia) {
    var vec = essentia.arrayToVector(pcm)
    // BPM — PercivalBpmEstimator (melhor para batidas estaveis de EDM);
    // RhythmExtractor2013 de reserva.
    var bpmRaw = 0
    try { bpmRaw = essentia.PercivalBpmEstimator(vec, 1024, 2048, 128, 128, 210, 50, 44100).bpm || 0 } catch (x) {}
    if (!bpmRaw || bpmRaw < 40) {
      try { bpmRaw = essentia.RhythmExtractor2013(vec, 208, 'multifeature', 40).bpm || 0 } catch (x) {}
    }
    var k = essentia.KeyExtractor(vec)
    try { vec.delete() } catch (x) {}
    self.postMessage({ id: id, bpm: Math.round(bpmRaw), key: k.key, scale: k.scale })
  }).catch(function (err) {
    self.postMessage({ id: id, error: (err && err.message) || String(err) })
  })
}
