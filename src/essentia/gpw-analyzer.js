// ============================================================
//  GPW AUDIO ANALYZER — deteção de BPM e Key (Essentia.js)
//  Requer os globais EssentiaWASM + Essentia (essentia.js .web.js
//  + .js-core.js carregados antes deste script).
//  Uso:  const r = await GPW_analyzeAudio(file)
//        → { bpm, key, scale, keyName, camelot }
// ============================================================
(function () {
  const KEY_TO_CAMELOT = {
    'C major': '8B', 'G major': '9B', 'D major': '10B', 'A major': '11B', 'E major': '12B', 'B major': '1B',
    'F# major': '2B', 'Gb major': '2B', 'Db major': '3B', 'C# major': '3B', 'Ab major': '4B', 'G# major': '4B',
    'Eb major': '5B', 'D# major': '5B', 'Bb major': '6B', 'A# major': '6B', 'F major': '7B',
    'A minor': '8A', 'E minor': '9A', 'B minor': '10A', 'F# minor': '11A', 'Gb minor': '11A',
    'C# minor': '12A', 'Db minor': '12A', 'Ab minor': '1A', 'G# minor': '1A', 'Eb minor': '2A', 'D# minor': '2A',
    'Bb minor': '3A', 'A# minor': '3A', 'F minor': '4A', 'C minor': '5A', 'G minor': '6A', 'D minor': '7A'
  }

  let _essentia = null
  async function getEssentia() {
    if (_essentia) return _essentia
    if (typeof Essentia === 'undefined' || typeof EssentiaWASM === 'undefined') {
      throw new Error('Analysis engine failed to load.')
    }
    let wasm = EssentiaWASM
    if (typeof wasm === 'function') { wasm = await wasm() }
    else if (wasm && typeof wasm.EssentiaWASM === 'function') { wasm = await wasm.EssentiaWASM() }
    else if (wasm && wasm.EssentiaWASM) { wasm = wasm.EssentiaWASM }
    _essentia = new Essentia(wasm)
    return _essentia
  }

  async function decodeToMono44k(arrayBuffer) {
    const AC = window.AudioContext || window.webkitAudioContext
    const tmp = new AC()
    const audioBuf = await tmp.decodeAudioData(arrayBuffer)
    tmp.close()
    const offline = new OfflineAudioContext(1, Math.max(1, Math.ceil(audioBuf.duration * 44100)), 44100)
    const src = offline.createBufferSource()
    src.buffer = audioBuf
    src.connect(offline.destination)
    src.start()
    const rendered = await offline.startRendering()
    return rendered.getChannelData(0)
  }

  async function GPW_analyzeAudio(file) {
    const arrayBuffer = await file.arrayBuffer()
    let data = await decodeToMono44k(arrayBuffer)
    const MAX = 44100 * 90 // cap a 90s por performance
    if (data.length > MAX) data = data.slice(0, MAX)

    const essentia = await getEssentia()
    const vec = essentia.arrayToVector(data)

    // BPM — PercivalBpmEstimator (melhor para batidas estáveis de EDM)
    let bpmRaw = 0
    try {
      const perc = essentia.PercivalBpmEstimator(vec, 1024, 2048, 128, 128, 210, 50, 44100)
      bpmRaw = perc.bpm || 0
    } catch (e) { /* fallback abaixo */ }
    if (!bpmRaw || bpmRaw < 40) {
      try { const rhythm = essentia.RhythmExtractor2013(vec, 208, 'multifeature', 40); bpmRaw = rhythm.bpm || 0 } catch (e) {}
    }
    const bpm = Math.round(bpmRaw) // EDM usa BPM inteiros

    const keyRes = essentia.KeyExtractor(vec)
    const key = keyRes.key
    const scale = keyRes.scale
    const keyName = `${key} ${scale}`
    const camelot = KEY_TO_CAMELOT[keyName] || ''

    try { vec.delete() } catch (e) {}
    return { bpm, key, scale, keyName, camelot }
  }

  window.GPW_analyzeAudio = GPW_analyzeAudio
})()
