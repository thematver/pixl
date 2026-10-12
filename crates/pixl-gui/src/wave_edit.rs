//! Lossless 4-bit wave editing and a small, portable clipboard format.
use pixl_core::{
    wave::{WAVE_LEN, WAVE_MAX},
    WaveRam,
};

/// Operations available in the wave editor's toolbar and context menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaveEdit {
    ShiftLeft,
    ShiftRight,
    Reverse,
    Normalize,
    Copy,
    Paste,
    Reset,
}

pub(crate) fn transform(mut wave: WaveRam, operation: WaveEdit) -> WaveRam {
    match operation {
        WaveEdit::ShiftLeft => wave.rotate_left(1),
        WaveEdit::ShiftRight => wave.rotate_right(1),
        WaveEdit::Reverse => wave.reverse(),
        WaveEdit::Normalize => {
            let lo = *wave.iter().min().unwrap();
            let hi = *wave.iter().max().unwrap();
            // A constant wave has no AC signal to amplify; retain its DC level.
            if hi > lo {
                let range = (hi - lo) as u16;
                for value in &mut wave {
                    *value = (((*value - lo) as u16 * WAVE_MAX as u16 + range / 2) / range) as u8;
                }
            }
        }
        WaveEdit::Reset => wave = pixl_core::wave::default_wave(),
        WaveEdit::Copy | WaveEdit::Paste => {}
    }
    wave
}

pub(crate) fn encode(wave: &WaveRam) -> String {
    let mut text = String::from("PIXL-WAVE/1 ");
    for &value in wave {
        text.push(char::from(b"0123456789ABCDEF"[value.min(WAVE_MAX) as usize]));
    }
    text
}

pub(crate) fn decode(text: &str) -> Option<WaveRam> {
    let text = text.trim();
    let text = text.strip_prefix("PIXL-WAVE/1 ").unwrap_or(text);
    let mut wave = [0; WAVE_LEN];
    let mut index = 0;
    for byte in text.bytes().filter(|b| !b.is_ascii_whitespace()) {
        if index == WAVE_LEN {
            return None;
        }
        wave[index] = match byte {
            b'0'..=b'9' => byte - b'0',
            b'A'..=b'F' => byte - b'A' + 10,
            b'a'..=b'f' => byte - b'a' + 10,
            _ => return None,
        };
        index += 1;
    }
    (index == WAVE_LEN).then_some(wave)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn clipboard_roundtrip_and_invalid_input() {
        let wave = pixl_core::wave::default_wave();
        assert_eq!(decode(&encode(&wave)), Some(wave));
        assert_eq!(decode("0123456789abcdef 0123456789ABCDEF"), Some(std::array::from_fn(|i| (i % 16) as u8)));
        for invalid in [
            "",
            "0123",
            "PIXL-WAVE/2 0123456789ABCDEF0123456789ABCDEF",
            "0123456789ABCDEF0123456789ABCDEG",
            "00123456789ABCDEF0123456789ABCDEF",
        ] {
            assert_eq!(decode(invalid), None);
        }
    }
    #[test]
    fn phase_edits_are_reversible_and_normalization_preserves_order() {
        let wave = pixl_core::wave::default_wave();
        assert_eq!(transform(transform(wave, WaveEdit::ShiftLeft), WaveEdit::ShiftRight), wave);
        assert_eq!(transform(transform(wave, WaveEdit::Reverse), WaveEdit::Reverse), wave);
        assert_eq!(transform([7; 32], WaveEdit::Normalize), [7; 32]);
        let quiet = std::array::from_fn(|i| 4 + (i % 8) as u8);
        let normalized = transform(quiet, WaveEdit::Normalize);
        assert_eq!(*normalized.iter().min().unwrap(), 0);
        assert_eq!(*normalized.iter().max().unwrap(), 15);
        for i in 0..31 {
            assert_eq!(quiet[i].cmp(&quiet[i + 1]), normalized[i].cmp(&normalized[i + 1]));
        }
    }
}
