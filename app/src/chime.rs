//! The game-warning toast's soft two-note chime (v2 played it with WebAudio): 880 Hz,
//! then 1174.66 Hz 0.16 s later, each a 20 ms rise to 0.18 and an exponential fall to
//! 0.001 over 0.5 s. Synthesised once into an in-memory WAV and played asynchronously.

use std::sync::OnceLock;
use windows_sys::Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_MEMORY, SND_NODEFAULT};

const RATE: u32 = 44_100;

/// 16-bit mono PCM WAV of the chime.
fn wav() -> Vec<u8> {
    let len = (RATE as f32 * 0.72) as usize;
    let mut samples = vec![0f32; len];
    for (freq, start) in [(880.0f32, 0.0f32), (1174.66, 0.16)] {
        for (i, s) in samples.iter_mut().enumerate() {
            let t = i as f32 / RATE as f32 - start;
            if !(0.0..0.55).contains(&t) {
                continue;
            }
            let gain = if t < 0.02 { 0.18 * t / 0.02 } else { 0.18 * (0.001f32 / 0.18).powf((t - 0.02) / 0.48) };
            *s += gain * (std::f32::consts::TAU * freq * t).sin();
        }
    }
    let data: Vec<u8> = samples
        .iter()
        .flat_map(|s| ((s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16).to_le_bytes())
        .collect();
    let mut out = Vec::with_capacity(44 + data.len());
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes()); // fmt chunk size
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&RATE.to_le_bytes());
    out.extend_from_slice(&(RATE * 2).to_le_bytes()); // bytes per second
    out.extend_from_slice(&2u16.to_le_bytes()); // block align
    out.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(&data);
    out
}

/// Play the chime without waiting; silent if there is no audio device.
pub fn play() {
    static WAV: OnceLock<Vec<u8>> = OnceLock::new();
    // SND_ASYNC + SND_MEMORY: the buffer must outlive playback, hence the static.
    let wav = WAV.get_or_init(wav);
    unsafe { PlaySoundW(wav.as_ptr() as *const u16, std::ptr::null_mut(), SND_MEMORY | SND_ASYNC | SND_NODEFAULT) };
}

#[cfg(test)]
mod tests {
    use super::wav;

    #[test]
    fn chime_is_a_well_formed_wav_that_stays_soft() {
        let wav = wav();
        assert_eq!(&wav[..4], b"RIFF");
        assert_eq!(u32::from_le_bytes(wav[4..8].try_into().unwrap()) as usize, wav.len() - 8);
        assert_eq!(u32::from_le_bytes(wav[40..44].try_into().unwrap()) as usize, wav.len() - 44);
        let peak = wav[44..].chunks(2).map(|b| i16::from_le_bytes([b[0], b[1]]).unsigned_abs()).max().unwrap();
        // Two overlapping notes at ≤0.18 each: well below full scale, but audible.
        assert!(peak > 3_000 && peak < 13_000, "peak {peak}");
    }
}
