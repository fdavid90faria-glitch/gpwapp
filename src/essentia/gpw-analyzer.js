// ============================================================
//  GPW AUDIO ANALYZER — deteção de BPM e Key (Essentia.js)
//
//  A página NÃO carrega o Essentia: o motor corre em gpw-essentia-worker.js
//  (o glue emscripten usa `new Function`, que a CSP do app já não permite).
//  Aqui fica só o que precisa do Web Audio — decodificar para PCM mono 44.1k,
//  que não existe dentro de um worker — e a tradução para Camelot.
//
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

  // Um worker só, reaproveitado: arrancar custa carregar ~2MB de WASM.
  let _worker = null
  let _seq = 0
  const _pending = new Map()

  function getWorker() {
    if (_worker) return _worker
    _worker = new Worker('/essentia/gpw-essentia-worker.js')
    _worker.onmessage = (e) => {
      const p = _pending.get(e.data.id)
      if (!p) return
      _pending.delete(e.data.id)
      if (e.data.error) p.reject(new Error(e.data.error))
      else p.resolve(e.data)
    }
    // Falha a carregar o worker (ou morte a meio): ninguém responde às
    // promessas pendentes, ficariam para sempre em "Analyzing…".
    _worker.onerror = (e) => {
      const err = new Error(e.message || 'Analysis engine failed to load.')
      for (const p of _pending.values()) p.reject(err)
      _pending.clear()
      try { _worker.terminate() } catch (x) {}
      _worker = null
    }
    return _worker
  }

  function analyzeInWorker(pcm) {
    const id = ++_seq
    return new Promise((resolve, reject) => {
      _pending.set(id, { resolve, reject })
      // pcm.buffer transferido (é uma cópia nossa): evita clonar ~16MB.
      getWorker().postMessage({ id, pcm }, [pcm.buffer])
    })
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
    const data = await decodeToMono44k(arrayBuffer)
    const MAX = 44100 * 90 // cap a 90s por performance
    // slice SEMPRE: o buffer vai ser transferido para o worker, e o original é
    // uma vista sobre o AudioBuffer que ainda está vivo aqui.
    const pcm = data.slice(0, Math.min(data.length, MAX))

    const r = await analyzeInWorker(pcm)
    const keyName = `${r.key} ${r.scale}`
    return {
      bpm: r.bpm,          // EDM usa BPM inteiros (arredondado no worker)
      key: r.key,
      scale: r.scale,
      keyName,
      camelot: KEY_TO_CAMELOT[keyName] || ''
    }
  }

  window.GPW_analyzeAudio = GPW_analyzeAudio
})()
