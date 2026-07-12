// qc.rs - metricas de qualidade de audio (padrao GPW) lidas direto do WAV em
// streaming (memoria constante, sem carregar o arquivo inteiro).
//
// E o equivalente do pipeline ffmpeg do GPW ANALYZER (ebur128+silencedetect+
// astats), reimplementado porque o uploader nao empacota ffmpeg:
//   - peak / RMS         <- astats
//   - LUFS I / S max     <- ebur128 (crate ebur128, mesmo algoritmo EBU R128)
//   - silencio no final  <- silencedetect noise=-60dB
//   - vocal (heuristico) <- RMS da banda 300-3400 Hz vs RMS total
// As regras de julgamento (o que reprova) ficam no JS (src/qc.js), portadas
// do validate() do ANALYZER.

use ebur128::{EbuR128, Mode};
use serde::Serialize;

#[derive(Serialize, Default)]
pub struct QcAnalysis {
    pub duration: f64,
    pub sample_rate: u32,
    pub channels: u32,
    pub bit_depth: u32,
    pub is_float: bool,
    pub sample_peak_db: Option<f64>,
    pub rms_db: Option<f64>,
    pub lufs_integrated: Option<f64>,
    pub lufs_short_max: Option<f64>,
    /// instante (s) em que ocorre o pico de LUFS short-term
    pub lufs_short_max_time: Option<f64>,
    /// true quando o arquivo nao tem sinal nenhum (peak < -60 dB)
    pub is_silent: bool,
    /// segundos de silencio no FINAL do arquivo (bug de export do DAW)
    pub trailing_silence_secs: f64,
    /// 0..1, so preenchido para categorias instrumentais
    pub vocal_confidence: Option<f64>,
}

const SILENCE_AMP: f32 = 0.001; // -60 dBFS

fn db(amp: f64) -> Option<f64> {
    (amp > 0.0).then(|| 20.0 * amp.log10())
}

fn finite(v: f64) -> Option<f64> {
    v.is_finite().then_some(v)
}

/// Biquad RBJ com Q = 1/sqrt(2) (12 dB/oct) — igual ao highpass/lowpass
/// poles=2 do ffmpeg que o ANALYZER usa no heuristico de vocal.
struct Biquad {
    b0: f64, b1: f64, b2: f64, a1: f64, a2: f64,
    x1: f64, x2: f64, y1: f64, y2: f64,
}

impl Biquad {
    fn new(highpass: bool, f0: f64, fs: f64) -> Biquad {
        let w0 = 2.0 * std::f64::consts::PI * f0 / fs;
        let (sinw, cosw) = (w0.sin(), w0.cos());
        let alpha = sinw * std::f64::consts::FRAC_1_SQRT_2; // sin/(2Q), Q=0.7071
        let a0 = 1.0 + alpha;
        let (b0, b1) = if highpass {
            ((1.0 + cosw) / 2.0, -(1.0 + cosw))
        } else {
            ((1.0 - cosw) / 2.0, 1.0 - cosw)
        };
        Biquad {
            b0: b0 / a0,
            b1: b1 / a0,
            b2: b0 / a0,
            a1: -2.0 * cosw / a0,
            a2: (1.0 - alpha) / a0,
            x1: 0.0, x2: 0.0, y1: 0.0, y2: 0.0,
        }
    }

    fn process(&mut self, x: f64) -> f64 {
        let y = self.b0 * x + self.b1 * self.x1 + self.b2 * self.x2
            - self.a1 * self.y1 - self.a2 * self.y2;
        self.x2 = self.x1;
        self.x1 = x;
        self.y2 = self.y1;
        self.y1 = y;
        y
    }
}

/// Acumuladores do streaming: um `push(sample)` por amostra intercalada.
struct Acc {
    channels: usize,
    rate: u32,
    ebur: EbuR128,
    hop: usize, // amostras (ja com canais) por hop de ~100ms
    buf: Vec<f32>,
    frames_done: u64,
    peak: f32,
    sumsq: f64,
    n: u64,
    last_loud_frame: Option<u64>,
    sample_idx: u64,
    short_max: f64,
    short_max_t: f64,
    band: Option<(Vec<Biquad>, Vec<Biquad>, f64)>, // (hp, lp, band_sumsq) por canal
}

impl Acc {
    fn push(&mut self, s: f32) -> Result<(), String> {
        let a = s.abs();
        if a > self.peak {
            self.peak = a;
        }
        if a >= SILENCE_AMP {
            self.last_loud_frame = Some(self.sample_idx / self.channels as u64);
        }
        self.sumsq += (s as f64) * (s as f64);
        self.n += 1;
        if let Some((hp, lp, band_sumsq)) = self.band.as_mut() {
            let ch = (self.sample_idx % self.channels as u64) as usize;
            let b = lp[ch].process(hp[ch].process(s as f64));
            *band_sumsq += b * b;
        }
        self.sample_idx += 1;
        self.buf.push(s);
        if self.buf.len() == self.hop {
            self.flush()?;
        }
        Ok(())
    }

    fn flush(&mut self) -> Result<(), String> {
        if self.buf.is_empty() {
            return Ok(());
        }
        self.ebur
            .add_frames_f32(&self.buf)
            .map_err(|e| format!("ebur128: {}", e))?;
        self.frames_done += (self.buf.len() / self.channels) as u64;
        self.buf.clear();
        if let Ok(s) = self.ebur.loudness_shortterm() {
            if s.is_finite() && s > self.short_max {
                self.short_max = s;
                self.short_max_t = self.frames_done as f64 / self.rate as f64;
            }
        }
        Ok(())
    }
}

/// Analisa um WAV do disco. `category` liga o heuristico de vocal quando
/// contem "instrumental" (mesma regra do ANALYZER).
pub fn analyze(path: &str, category: &str) -> Result<QcAnalysis, String> {
    let mut reader =
        hound::WavReader::open(path).map_err(|e| format!("Could not read WAV: {}", e))?;
    let spec = reader.spec();
    let channels = spec.channels as usize;
    if channels == 0 || spec.sample_rate == 0 {
        return Err("Invalid WAV header".into());
    }
    let rate = spec.sample_rate;
    let duration = reader.duration() as f64 / rate as f64;
    let is_float = spec.sample_format == hound::SampleFormat::Float;

    let ebur = EbuR128::new(spec.channels as u32, rate, Mode::I | Mode::S)
        .map_err(|e| format!("ebur128: {}", e))?;
    let hop = (rate as usize / 10).max(1) * channels;
    let band = category.contains("instrumental").then(|| {
        let hp = (0..channels).map(|_| Biquad::new(true, 300.0, rate as f64)).collect();
        let lp = (0..channels).map(|_| Biquad::new(false, 3400.0, rate as f64)).collect();
        (hp, lp, 0.0)
    });
    let mut acc = Acc {
        channels,
        rate,
        ebur,
        hop,
        buf: Vec::with_capacity(hop),
        frames_done: 0,
        peak: 0.0,
        sumsq: 0.0,
        n: 0,
        last_loud_frame: None,
        sample_idx: 0,
        short_max: f64::NEG_INFINITY,
        short_max_t: 0.0,
        band,
    };

    if is_float {
        for s in reader.samples::<f32>() {
            acc.push(s.map_err(|e| format!("Corrupt WAV: {}", e))?)?;
        }
    } else {
        let scale = 1.0 / (1i64 << (spec.bits_per_sample - 1)) as f32;
        for s in reader.samples::<i32>() {
            acc.push(s.map_err(|e| format!("Corrupt WAV: {}", e))? as f32 * scale)?;
        }
    }
    acc.flush()?;

    let is_silent = acc.peak < SILENCE_AMP;
    let rms = (acc.n > 0).then(|| (acc.sumsq / acc.n as f64).sqrt());
    let trailing_silence_secs = match acc.last_loud_frame {
        Some(f) => (duration - (f + 1) as f64 / rate as f64).max(0.0),
        None => duration,
    };
    let vocal_confidence = match (&acc.band, rms, is_silent) {
        (Some((_, _, band_sumsq)), Some(rms), false) if acc.n > 0 => {
            let band_rms = (band_sumsq / acc.n as f64).sqrt();
            match (db(rms), db(band_rms)) {
                (Some(o), Some(b)) => {
                    // diff <= 2 dB -> 1.0 ; diff >= 10 dB -> 0.0 ; linear no meio
                    Some(((10.0 - (o - b)) / 8.0).clamp(0.0, 1.0))
                }
                _ => Some(0.0),
            }
        }
        _ => None,
    };

    Ok(QcAnalysis {
        duration,
        sample_rate: rate,
        channels: channels as u32,
        bit_depth: spec.bits_per_sample as u32,
        is_float,
        sample_peak_db: db(acc.peak as f64),
        rms_db: rms.and_then(db),
        lufs_integrated: acc.ebur.loudness_global().ok().and_then(finite),
        lufs_short_max: finite(acc.short_max),
        lufs_short_max_time: finite(acc.short_max).map(|_| acc.short_max_t),
        is_silent,
        trailing_silence_secs,
        vocal_confidence,
    })
}

#[derive(Serialize, Default)]
pub struct StemsSum {
    /// peak da SOMA de todas as stems (dBFS). Passa de 0 dB se a soma clipar.
    pub sample_peak_db: Option<f64>,
    /// LUFS integrado da SOMA — comparado com o LUFS do mixdown (devem bater a
    /// +-1 dB; senao o mixdown esta mais alto/baixo que a soma dos stems).
    pub lufs_integrated: Option<f64>,
    pub duration: f64,
    pub count: usize,
}

/// Soma N stems amostra-a-amostra e devolve o peak E o LUFS integrado da soma.
/// Rust puro (sem ffmpeg): abre todos os WAVs e percorre-os em paralelo,
/// somando; nunca carrega tudo em memoria. O sinal somado alimenta o ebur128
/// (Mode::I) em blocos de ~100ms para o LUFS. Duracoes diferentes somam ate a
/// mais longa. O formato (rate/canais) vem do 1.o stem — o QC ja acusa formato
/// errado por ficheiro, e as stems devem ser todas iguais.
pub fn analyze_stems_sum(paths: &[String]) -> Result<StemsSum, String> {
    if paths.is_empty() {
        return Err("no stems".into());
    }
    let mut iters: Vec<Box<dyn Iterator<Item = f32>>> = Vec::new();
    let mut rate = 0u32;
    let mut channels = 0usize;
    let mut max_frames = 0u64;
    for p in paths {
        let reader = hound::WavReader::open(p).map_err(|e| format!("open {}: {}", p, e))?;
        let spec = reader.spec();
        if spec.channels == 0 || spec.sample_rate == 0 {
            continue;
        }
        if rate == 0 {
            rate = spec.sample_rate;
            channels = spec.channels as usize;
        }
        max_frames = max_frames.max(reader.duration() as u64);
        if spec.sample_format == hound::SampleFormat::Float {
            iters.push(Box::new(reader.into_samples::<f32>().map(|s| s.unwrap_or(0.0))));
        } else {
            let scale = 1.0 / (1i64 << (spec.bits_per_sample - 1)) as f32;
            iters.push(Box::new(reader.into_samples::<i32>().map(move |s| s.unwrap_or(0) as f32 * scale)));
        }
    }
    if iters.is_empty() || rate == 0 {
        return Err("no readable stems".into());
    }

    let mut ebur = EbuR128::new(channels as u32, rate, Mode::I).map_err(|e| format!("ebur128: {}", e))?;
    let hop = (rate as usize / 10).max(1) * channels; // ~100ms (multiplo de canais)
    let mut buf: Vec<f32> = Vec::with_capacity(hop);
    let mut peak: f32 = 0.0;

    // percorre em lockstep: em cada posicao soma uma amostra (interleaved) de
    // cada stem; acumula para o LUFS e vai medindo o peak.
    loop {
        let mut sum = 0.0f32;
        let mut any = false;
        for it in iters.iter_mut() {
            if let Some(v) = it.next() {
                sum += v;
                any = true;
            }
        }
        if !any {
            break;
        }
        let a = sum.abs();
        if a > peak {
            peak = a;
        }
        buf.push(sum);
        if buf.len() >= hop {
            let _ = ebur.add_frames_f32(&buf);
            buf.clear();
        }
    }
    if !buf.is_empty() {
        let _ = ebur.add_frames_f32(&buf);
    }

    Ok(StemsSum {
        sample_peak_db: db(peak as f64),
        lufs_integrated: ebur.loudness_global().ok().and_then(finite),
        duration: max_frames as f64 / rate as f64,
        count: iters.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Gera um WAV senoidal (freq Hz, amp 0..1, secs) + `tail_silence` segundos
    /// de zeros no final. bits: 16/24 int ou 32 float.
    fn gen(name: &str, freq: f64, amp: f64, secs: f64, tail_silence: f64, bits: u16) -> String {
        let dir = std::env::temp_dir().join("gpw_qc_test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        let rate = 44100u32;
        let spec = hound::WavSpec {
            channels: 2,
            sample_rate: rate,
            bits_per_sample: bits,
            sample_format: if bits == 32 { hound::SampleFormat::Float } else { hound::SampleFormat::Int },
        };
        let mut w = hound::WavWriter::create(&path, spec).unwrap();
        let n = (secs * rate as f64) as u64;
        let tail = (tail_silence * rate as f64) as u64;
        for i in 0..(n + tail) {
            let v = if i < n {
                amp * (2.0 * std::f64::consts::PI * freq * i as f64 / rate as f64).sin()
            } else {
                0.0
            };
            for _ in 0..2 {
                if bits == 32 {
                    w.write_sample(v as f32).unwrap();
                } else {
                    let max = (1i64 << (bits - 1)) - 1;
                    w.write_sample((v * max as f64) as i32).unwrap();
                }
            }
        }
        w.finalize().unwrap();
        path.to_string_lossy().into_owned()
    }

    #[test]
    fn metricas_completas_wav24() {
        let p = gen("m24.wav", 220.0, 0.5, 6.0, 0.0, 24);
        let a = analyze(&p, "extended_mix").unwrap();
        assert_eq!(a.bit_depth, 24);
        assert!(!a.is_float);
        assert!((a.duration - 6.0).abs() < 0.05, "duration={}", a.duration);
        // peak de seno 0.5 = -6.02 dB
        assert!((a.sample_peak_db.unwrap() + 6.02).abs() < 0.1);
        assert!(a.rms_db.is_some());
        assert!(a.lufs_integrated.is_some());
        assert!(a.lufs_short_max.is_some());
        assert!(a.lufs_short_max_time.is_some());
        assert!(!a.is_silent);
        assert!(a.trailing_silence_secs < 0.1);
        assert!(a.vocal_confidence.is_none(), "master nao roda heuristico");
    }

    #[test]
    fn atenuacao_de_6db_reflete_nas_metricas() {
        let full = analyze(&gen("g_full.wav", 220.0, 0.8, 6.0, 0.0, 24), "x").unwrap();
        let quiet = analyze(&gen("g_quiet.wav", 220.0, 0.4, 6.0, 0.0, 24), "x").unwrap();
        for (name, d) in [
            ("peak", full.sample_peak_db.unwrap() - quiet.sample_peak_db.unwrap()),
            ("rms", full.rms_db.unwrap() - quiet.rms_db.unwrap()),
            ("lufs", full.lufs_integrated.unwrap() - quiet.lufs_integrated.unwrap()),
        ] {
            assert!((d - 6.02).abs() < 0.3, "{} delta={}", name, d);
        }
    }

    #[test]
    fn detecta_16_bit_e_float() {
        assert_eq!(analyze(&gen("m16.wav", 220.0, 0.5, 2.0, 0.0, 16), "x").unwrap().bit_depth, 16);
        let f = analyze(&gen("f32.wav", 220.0, 0.5, 2.0, 0.0, 32), "x").unwrap();
        assert!(f.is_float);
        assert_eq!(f.bit_depth, 32);
    }

    #[test]
    fn detecta_silencio_no_final() {
        let a = analyze(&gen("cauda.wav", 220.0, 0.5, 6.0, 5.0, 24), "x").unwrap();
        assert!((a.trailing_silence_secs - 5.0).abs() < 0.2, "trailing={}", a.trailing_silence_secs);
        let b = analyze(&gen("sem_cauda.wav", 220.0, 0.5, 6.0, 0.0, 24), "x").unwrap();
        assert!(b.trailing_silence_secs < 0.1);
    }

    #[test]
    fn detecta_arquivo_sem_audio() {
        let a = analyze(&gen("mudo.wav", 220.0, 0.0, 4.0, 0.0, 24), "stems").unwrap();
        assert!(a.is_silent);
        assert!(a.vocal_confidence.is_none());
        let b = analyze(&gen("baixo.wav", 220.0, 0.1, 4.0, 0.0, 24), "stems").unwrap();
        assert!(!b.is_silent);
    }

    #[test]
    fn heuristico_vocal_separa_banda_vocal_de_sub_bass() {
        // 800 Hz cai na banda de formantes (300-3400) e acende; 40 Hz nao.
        let v = analyze(&gen("vocalish.wav", 800.0, 0.5, 4.0, 0.0, 24), "extended_instrumental").unwrap();
        let s = analyze(&gen("sub.wav", 40.0, 0.5, 4.0, 0.0, 24), "extended_instrumental").unwrap();
        assert!(v.vocal_confidence.unwrap() > 0.6, "800Hz: {:?}", v.vocal_confidence);
        assert!(s.vocal_confidence.unwrap() < 0.3, "40Hz: {:?}", s.vocal_confidence);
    }

    #[test]
    fn nao_wav_da_erro() {
        let dir = std::env::temp_dir().join("gpw_qc_test");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("nao_wav.wav");
        std::fs::write(&p, b"definitely not a wav").unwrap();
        assert!(analyze(p.to_str().unwrap(), "x").is_err());
    }

    #[test]
    fn soma_de_stems_clipa() {
        // Duas stems de -6 dB (amp 0.5) somadas dao ~0 dB (amp 1.0) -> passa de -3.
        let a = gen("stemA.wav", 220.0, 0.5, 2.0, 0.0, 24);
        let b = gen("stemB.wav", 220.0, 0.5, 2.0, 0.0, 24);
        let sum = analyze_stems_sum(&[a.clone(), b.clone()]).unwrap();
        let peak = sum.sample_peak_db.unwrap();
        assert!(peak > -3.0, "soma de duas stems -6dB deve passar de -3 dB: {}", peak);
        assert_eq!(sum.count, 2);
        // Uma stem sozinha a -6 dB nao passa de -3.
        let one = analyze_stems_sum(&[a]).unwrap();
        assert!(one.sample_peak_db.unwrap() < -3.0, "uma stem -6dB nao passa de -3");
    }
}
