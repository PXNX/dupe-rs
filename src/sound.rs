use std::sync::OnceLock;

/// Short audible cues for long-running work finishing, so the user can look
/// away from a big scan or delete and still notice when it's done.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sound {
    /// Bright rising three-note chime.
    ScanFinished,
    /// Lower descending two-note chime, distinct from `ScanFinished`.
    DeleteFinished,
}

const SAMPLE_RATE: u32 = 44_100;

impl Sound {
    /// `(frequency Hz, duration s)` for each note, played back to back.
    fn notes(self) -> &'static [(f32, f32)] {
        match self {
            // C6, E6, G6
            Sound::ScanFinished => &[(1046.5, 0.09), (1318.5, 0.09), (1568.0, 0.22)],
            // G5, C5
            Sound::DeleteFinished => &[(784.0, 0.11), (523.3, 0.26)],
        }
    }

    /// The synthesized WAV, built once and kept alive for the process's
    /// lifetime, as asynchronous in-memory playback requires.
    fn wav(self) -> &'static [u8] {
        static SCAN: OnceLock<Vec<u8>> = OnceLock::new();
        static DELETE: OnceLock<Vec<u8>> = OnceLock::new();
        let cell = match self {
            Sound::ScanFinished => &SCAN,
            Sound::DeleteFinished => &DELETE,
        };
        cell.get_or_init(|| encode_wav(&synthesize(self.notes())))
    }
}

/// Plays `sound` without blocking. A no-op off Windows.
pub fn play(sound: Sound) {
    let wav = sound.wav();
    #[cfg(windows)]
    {
        use windows_sys::Win32::Media::Audio::{
            PlaySoundW, SND_ASYNC, SND_MEMORY, SND_NODEFAULT,
        };
        // SAFETY: with SND_MEMORY the "name" is a pointer to a complete WAV
        // image, which `wav()` keeps alive for the whole process, so it
        // outlives the asynchronous playback.
        unsafe {
            PlaySoundW(
                wav.as_ptr().cast(),
                std::ptr::null_mut(),
                SND_MEMORY | SND_ASYNC | SND_NODEFAULT,
            );
        }
    }
    #[cfg(not(windows))]
    let _ = wav;
}

/// Renders sine-tone notes with a quick attack and exponential decay (a soft
/// bell-like "ding" rather than a harsh beep) as 16-bit mono samples.
fn synthesize(notes: &[(f32, f32)]) -> Vec<i16> {
    let mut samples = Vec::new();
    for &(freq, secs) in notes {
        let len = (secs * SAMPLE_RATE as f32) as usize;
        let attack = (0.005 * SAMPLE_RATE as f32) as usize;
        for i in 0..len {
            let t = i as f32 / SAMPLE_RATE as f32;
            let envelope = if i < attack {
                i as f32 / attack as f32
            } else {
                (-(t - attack as f32 / SAMPLE_RATE as f32) * 5.0 / secs).exp()
            };
            let tone = (std::f32::consts::TAU * freq * t).sin()
                + 0.3 * (std::f32::consts::TAU * freq * 2.0 * t).sin();
            samples.push((tone / 1.3 * envelope * 0.35 * i16::MAX as f32) as i16);
        }
    }
    samples
}

fn encode_wav(samples: &[i16]) -> Vec<u8> {
    let data_len = (samples.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes()); // fmt chunk size
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    out.extend_from_slice(&(SAMPLE_RATE * 2).to_le_bytes()); // byte rate
    out.extend_from_slice(&2u16.to_le_bytes()); // block align
    out.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_a_well_formed_wav_for_each_sound() {
        for sound in [Sound::ScanFinished, Sound::DeleteFinished] {
            let wav = sound.wav();
            assert_eq!(&wav[0..4], b"RIFF");
            assert_eq!(&wav[8..12], b"WAVE");
            let riff_len = u32::from_le_bytes(wav[4..8].try_into().unwrap()) as usize;
            assert_eq!(riff_len + 8, wav.len());
            let data_len = u32::from_le_bytes(wav[40..44].try_into().unwrap()) as usize;
            assert_eq!(data_len + 44, wav.len());
            assert!(data_len > 0);
        }
    }

    #[test]
    fn the_two_cues_sound_different() {
        assert_ne!(Sound::ScanFinished.wav(), Sound::DeleteFinished.wav());
    }
}
