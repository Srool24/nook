//! Voice memos: recording the default microphone (waveIn, 16-bit mono
//! PCM), reading and writing WAV files, and playing them (PlaySound).
//!
//! Recording polls a ring of buffers instead of taking callbacks, so it
//! needs no window and no callback thread; the caller runs it on a thread
//! of its own and stops it with a flag.
//!
//! On Linux, recording reads raw PCM from PipeWire's `pw-record` (or
//! PulseAudio's `parec`, which PipeWire also serves) and playback runs
//! `pw-play` / `paplay`; both ship with Fedora Workstation.

#[cfg(target_os = "linux")]
pub use linux::{play, record, stop_playback};

#[cfg(windows)]
use crate::{PlatformError, Result, wide};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
#[cfg(windows)]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(windows)]
use std::time::Duration;
#[cfg(windows)]
use windows::Win32::Media::Audio::{
    CALLBACK_NULL, HWAVEIN, PlaySoundW, SND_ASYNC, SND_FILENAME, SND_FLAGS, SND_NODEFAULT,
    WAVE_FORMAT_PCM, WAVE_MAPPER, WAVEFORMATEX, WAVEHDR, WHDR_DONE, waveInAddBuffer, waveInClose,
    waveInGetNumDevs, waveInOpen, waveInPrepareHeader, waveInReset, waveInStart,
    waveInUnprepareHeader,
};
#[cfg(windows)]
use windows::core::{PCWSTR, PSTR};

pub const SAMPLE_RATE: u32 = 44_100;
/// Samples per buffer: 50 ms.
const CHUNK: usize = SAMPLE_RATE as usize / 20;
#[cfg(windows)]
const BUFFERS: usize = 8;

#[cfg(windows)]
fn mm(call: &'static str, code: u32) -> Result<()> {
    if code == 0 {
        Ok(())
    } else {
        Err(PlatformError::Code { call, code })
    }
}

/// Records from the default microphone until `stop` is set, handing each
/// buffer of samples (about 50 ms) to `on_chunk` in order. Blocks.
#[cfg(windows)]
pub fn record(stop: &AtomicBool, mut on_chunk: impl FnMut(&[i16])) -> Result<()> {
    // SAFETY: the headers and their sample buffers live until after
    // `waveInUnprepareHeader`, neither moves while the device owns them,
    // and a buffer is read only once the driver has marked it done.
    unsafe {
        if waveInGetNumDevs() == 0 {
            return Err(PlatformError::Other("No microphone found".into()));
        }
        let fmt = WAVEFORMATEX {
            wFormatTag: WAVE_FORMAT_PCM as u16,
            nChannels: 1,
            nSamplesPerSec: SAMPLE_RATE,
            nAvgBytesPerSec: SAMPLE_RATE * 2,
            nBlockAlign: 2,
            wBitsPerSample: 16,
            cbSize: 0,
        };
        let mut hwi = HWAVEIN::default();
        mm(
            "waveInOpen",
            waveInOpen(Some(&mut hwi), WAVE_MAPPER, &fmt, None, None, CALLBACK_NULL),
        )?;
        let mut data: Vec<Box<[i16]>> = (0..BUFFERS)
            .map(|_| vec![0i16; CHUNK].into_boxed_slice())
            .collect();
        let mut headers: Box<[WAVEHDR]> = data
            .iter_mut()
            .map(|d| WAVEHDR {
                lpData: PSTR(d.as_mut_ptr().cast()),
                dwBufferLength: (CHUNK * 2) as u32,
                ..Default::default()
            })
            .collect();
        let size = std::mem::size_of::<WAVEHDR>() as u32;
        for h in headers.iter_mut() {
            waveInPrepareHeader(hwi, h, size);
            waveInAddBuffer(hwi, h, size);
        }
        let started = mm("waveInStart", waveInStart(hwi));
        let mut next = 0;
        while started.is_ok() && !stop.load(Ordering::Relaxed) {
            let h = &mut headers[next];
            if h.dwFlags & WHDR_DONE != 0 {
                let n = (h.dwBytesRecorded as usize / 2).min(CHUNK);
                on_chunk(&data[next][..n]);
                h.dwBytesRecorded = 0;
                waveInAddBuffer(hwi, h, size);
                next = (next + 1) % BUFFERS;
            } else {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        // Reset hands back every queued buffer, the partial one included.
        waveInReset(hwi);
        for _ in 0..BUFFERS {
            let h = &headers[next];
            if h.dwFlags & WHDR_DONE != 0 && h.dwBytesRecorded > 0 {
                let n = (h.dwBytesRecorded as usize / 2).min(CHUNK);
                on_chunk(&data[next][..n]);
            }
            next = (next + 1) % BUFFERS;
        }
        for h in headers.iter_mut() {
            waveInUnprepareHeader(hwi, h, size);
        }
        waveInClose(hwi);
        started
    }
}

/// Writes 16-bit mono PCM WAV, patching the sizes on [`WavWriter::finish`].
pub struct WavWriter {
    file: std::io::BufWriter<std::fs::File>,
    samples: u32,
}

fn header(samples: u32) -> [u8; 44] {
    let data = samples * 2;
    let mut h = [0u8; 44];
    let mut put = |at: usize, bytes: &[u8]| h[at..at + bytes.len()].copy_from_slice(bytes);
    put(0, b"RIFF");
    put(4, &(36 + data).to_le_bytes());
    put(8, b"WAVEfmt ");
    put(16, &16u32.to_le_bytes());
    put(20, &1u16.to_le_bytes()); // PCM
    put(22, &1u16.to_le_bytes()); // mono
    put(24, &SAMPLE_RATE.to_le_bytes());
    put(28, &(SAMPLE_RATE * 2).to_le_bytes());
    put(32, &2u16.to_le_bytes());
    put(34, &16u16.to_le_bytes());
    put(36, b"data");
    put(40, &data.to_le_bytes());
    h
}

impl WavWriter {
    pub fn create(path: &Path) -> std::io::Result<Self> {
        let mut file = std::io::BufWriter::new(std::fs::File::create(path)?);
        file.write_all(&header(0))?;
        Ok(Self { file, samples: 0 })
    }

    pub fn write(&mut self, samples: &[i16]) -> std::io::Result<()> {
        let bytes: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
        self.file.write_all(&bytes)?;
        self.samples += samples.len() as u32;
        Ok(())
    }

    pub fn finish(mut self) -> std::io::Result<()> {
        self.file.seek(SeekFrom::Start(0))?;
        self.file.write_all(&header(self.samples))?;
        self.file.flush()
    }
}

/// Sample rate and samples (first channel) of a 16-bit PCM WAV file.
pub fn read_wav(path: &Path) -> std::io::Result<(u32, Vec<i16>)> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)?.read_to_end(&mut bytes)?;
    let bad = || std::io::Error::new(std::io::ErrorKind::InvalidData, "not a PCM WAV file");
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(bad());
    }
    let u16_at = |i: usize| u16::from_le_bytes([bytes[i], bytes[i + 1]]);
    let u32_at =
        |i: usize| u32::from_le_bytes([bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]);
    let (mut rate, mut channels, mut bits) = (0, 1, 16);
    let mut at = 12;
    while at + 8 <= bytes.len() {
        let id = &bytes[at..at + 4];
        let len = u32_at(at + 4) as usize;
        let body = at + 8;
        if id == b"fmt " && body + 16 <= bytes.len() {
            channels = u16_at(body + 2).max(1) as usize;
            rate = u32_at(body + 4);
            bits = u16_at(body + 14);
        } else if id == b"data" {
            if bits != 16 {
                return Err(bad());
            }
            let end = (body + len).min(bytes.len());
            let samples = bytes[body..end]
                .chunks_exact(2 * channels)
                .map(|c| i16::from_le_bytes([c[0], c[1]]))
                .collect();
            return Ok((rate, samples));
        }
        at = body + len + (len & 1);
    }
    Err(bad())
}

/// Starts playing a WAV file, replacing any sound playing.
#[cfg(windows)]
pub fn play(path: &Path) -> Result<()> {
    let w = wide(&path.to_string_lossy());
    // SAFETY: a null-terminated path; PlaySound copies what it needs.
    let ok = unsafe {
        PlaySoundW(
            PCWSTR(w.as_ptr()),
            None,
            SND_FILENAME | SND_ASYNC | SND_NODEFAULT,
        )
    };
    if ok.as_bool() {
        Ok(())
    } else {
        Err(PlatformError::Other(format!(
            "could not play {}",
            path.display()
        )))
    }
}

/// Stops what [`play`] started.
#[cfg(windows)]
pub fn stop_playback() {
    // SAFETY: a null sound stops the current one.
    let _ = unsafe { PlaySoundW(PCWSTR::null(), None, SND_FLAGS(0)) };
}

#[cfg(target_os = "linux")]
mod linux {
    use super::{CHUNK, SAMPLE_RATE};
    use crate::sys::cmd;
    use crate::{PlatformError, Result};
    use std::io::Read;
    use std::path::Path;
    use std::process::{Child, Command, Stdio};
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};

    /// Records from the default microphone until `stop` is set, handing
    /// each buffer of samples (about 50 ms) to `on_chunk` in order. Blocks.
    pub fn record(stop: &AtomicBool, mut on_chunk: impl FnMut(&[i16])) -> Result<()> {
        let rate = SAMPLE_RATE.to_string();
        let mut child = if cmd::which("pw-record").is_some() {
            Command::new("pw-record")
                .args([
                    "--format",
                    "s16",
                    "--rate",
                    &rate,
                    "--channels",
                    "1",
                    "--latency",
                    "50ms",
                    "-",
                ])
                .stdout(Stdio::piped())
                .stdin(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
        } else if cmd::which("parec").is_some() {
            Command::new("parec")
                .args([
                    "--format=s16le",
                    &format!("--rate={rate}"),
                    "--channels=1",
                    "--raw",
                    "--latency-msec=50",
                ])
                .stdout(Stdio::piped())
                .stdin(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
        } else {
            return Err(PlatformError::Other(
                "no recorder found (install pipewire-utils or pulseaudio-utils)".into(),
            ));
        }
        .map_err(|e| PlatformError::other("starting the recorder", e))?;
        let out = child
            .stdout
            .take()
            .ok_or_else(|| PlatformError::Other("recorder has no output".into()))?;
        let (mut out, big_endian) = match pcm_stream(out) {
            Ok(s) => s,
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(e);
            }
        };
        let mut bytes = vec![0u8; CHUNK * 2];
        let mut samples = Vec::with_capacity(CHUNK);
        let mut got_any = false;
        let result = loop {
            if stop.load(Ordering::Relaxed) {
                break Ok(());
            }
            match out.read_exact(&mut bytes) {
                Ok(()) => {
                    got_any = true;
                    samples.clear();
                    samples.extend(bytes.as_chunks::<2>().0.iter().map(|b| {
                        if big_endian {
                            i16::from_be_bytes(*b)
                        } else {
                            i16::from_le_bytes(*b)
                        }
                    }));
                    on_chunk(&samples);
                }
                Err(e) => {
                    break if got_any {
                        Ok(())
                    } else {
                        Err(PlatformError::other("no microphone input", e))
                    };
                }
            }
        };
        let _ = child.kill();
        let _ = child.wait();
        result
    }

    /// The 16-bit samples in a recorder's output, past any container
    /// header, and whether they are big-endian. `pw-record` writes an AU
    /// stream (big-endian) to stdout; `parec --raw` writes bare
    /// little-endian samples. Reading AU samples as little-endian turned
    /// every memo into white noise.
    fn pcm_stream(mut out: impl Read + 'static) -> Result<(Box<dyn Read>, bool)> {
        let read = |out: &mut dyn Read, buf: &mut [u8]| {
            out.read_exact(buf)
                .map_err(|e| PlatformError::other("no microphone input", e))
        };
        let mut magic = [0u8; 4];
        read(&mut out, &mut magic)?;
        match &magic {
            b".snd" => {
                let mut head = [0u8; 20];
                read(&mut out, &mut head)?;
                let word =
                    |i: usize| u32::from_be_bytes([head[i], head[i + 1], head[i + 2], head[i + 3]]);
                let (offset, encoding) = (word(0) as usize, word(8));
                // Encoding 3 is 16-bit linear PCM.
                if encoding != 3 || offset < 24 {
                    return Err(PlatformError::Other(format!(
                        "recorder sent AU encoding {encoding}, not 16-bit PCM"
                    )));
                }
                let mut rest = vec![0u8; offset - 24];
                read(&mut out, &mut rest)?;
                Ok((Box::new(out), true))
            }
            b"RIFF" => {
                // "WAVE", then chunks up to "data" (its size is unknown on a pipe).
                let mut skip = [0u8; 8];
                read(&mut out, &mut skip)?;
                loop {
                    let mut chunk = [0u8; 8];
                    read(&mut out, &mut chunk)?;
                    if &chunk[0..4] == b"data" {
                        break Ok((Box::new(out), false));
                    }
                    let size = u32::from_le_bytes([chunk[4], chunk[5], chunk[6], chunk[7]]);
                    let mut body = vec![0u8; size as usize + (size as usize & 1)];
                    read(&mut out, &mut body)?;
                }
            }
            _ => Ok((Box::new(std::io::Cursor::new(magic).chain(out)), false)),
        }
    }

    static PLAYER: Mutex<Option<Child>> = Mutex::new(None);

    /// Starts playing a WAV file, replacing any sound playing.
    pub fn play(path: &Path) -> Result<()> {
        stop_playback();
        let program = ["pw-play", "paplay", "aplay"]
            .into_iter()
            .find(|p| cmd::which(p).is_some())
            .ok_or_else(|| PlatformError::Other("no audio player found".into()))?;
        let child = Command::new(program)
            .arg(path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| PlatformError::other(&format!("could not play {}", path.display()), e))?;
        *PLAYER.lock().unwrap_or_else(|e| e.into_inner()) = Some(child);
        Ok(())
    }

    /// Stops what [`play`] started.
    pub fn stop_playback() {
        if let Some(mut child) = PLAYER.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    #[cfg(test)]
    mod tests {
        use super::pcm_stream;
        use std::io::Read;

        fn samples(bytes: Vec<u8>) -> Vec<i16> {
            let (mut r, be) = pcm_stream(std::io::Cursor::new(bytes)).unwrap();
            let mut pcm = Vec::new();
            r.read_to_end(&mut pcm).unwrap();
            pcm.as_chunks::<2>()
                .0
                .iter()
                .map(|b| {
                    if be {
                        i16::from_be_bytes(*b)
                    } else {
                        i16::from_le_bytes(*b)
                    }
                })
                .collect()
        }

        #[test]
        fn reads_au_from_pw_record() {
            // What pw-record writes to stdout: an AU header (here with 8
            // bytes of annotation) and big-endian 16-bit samples.
            let mut au = b".snd".to_vec();
            for word in [32u32, u32::MAX, 3, 44_100, 1] {
                au.extend(word.to_be_bytes());
            }
            au.extend([0u8; 8]);
            for s in [1000i16, -2, 300] {
                au.extend(s.to_be_bytes());
            }
            assert_eq!(samples(au), [1000, -2, 300]);
        }

        #[test]
        fn reads_wav_and_raw() {
            let mut wav = b"RIFF\xff\xff\xff\xffWAVEfmt ".to_vec();
            wav.extend(16u32.to_le_bytes());
            wav.extend([0u8; 16]);
            wav.extend(b"data\xff\xff\xff\xff");
            wav.extend(7i16.to_le_bytes());
            assert_eq!(samples(wav), [7]);
            let raw: Vec<u8> = [5i16, -6, 7].iter().flat_map(|s| s.to_le_bytes()).collect();
            assert_eq!(samples(raw), [5, -6, 7]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wav_round_trip() {
        let path = std::env::temp_dir().join(format!("nook-wav-{}.wav", std::process::id()));
        let mut w = WavWriter::create(&path).unwrap();
        w.write(&[0, 1000, -1000, i16::MAX]).unwrap();
        w.write(&[5]).unwrap();
        w.finish().unwrap();
        let (rate, samples) = read_wav(&path).unwrap();
        assert_eq!(rate, SAMPLE_RATE);
        assert_eq!(samples, [0, 1000, -1000, i16::MAX, 5]);
        let _ = std::fs::remove_file(&path);
    }
}
