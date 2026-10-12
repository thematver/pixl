//! Every user-facing string of the editor in Russian (default, as in the design) and English.
//!
//! Strings that are identical in both languages (parameter units such as `MS`, chip names such
//! as `GB`, the `PULSE 1` tab labels) are part of the design's technical vocabulary and are
//! written inline; everything a user reads as language goes through [`S`].

use pixl_core::Lang;

/// A translatable string.
#[allow(missing_docs)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum S {
    // Header / tabs
    ChipNameNes,
    ChipNameGb,
    ChipNameFree,
    NesTriangleNoVolume,
    // Hero views
    Back,
    WaveSub,
    TriSub,
    TriHint,
    PulseSub,
    Harmonics,
    NoiseSubLong,
    NoiseSubShort,
    Register,
    Period,
    SweepSubChip,
    SweepSubFree,
    GlideSubLegato,
    GlideSubAlways,
    EnvSubChip,
    EnvSubFree,
    ArpSub,
    ArpExample,
    EchoSub,
    // Macro row
    Assign,
    Done,
    // Browser
    Search,
    All,
    User,
    AllPresets,
    Found,
    Load,
    SaveCurrent,
    Delete,
    SaveTitle,
    SaveName,
    Save,
    Cancel,
    NoResults,
    Favorite,
}

impl S {
    /// The string in `lang`.
    pub fn get(self, lang: Lang) -> &'static str {
        let (ru, en) = match self {
            S::ChipNameNes => ("RICOH 2A03 · 4 КАНАЛА", "RICOH 2A03 · 4 CHANNELS"),
            S::ChipNameGb => ("DMG-CPU · 4 КАНАЛА", "DMG-CPU · 4 CHANNELS"),
            S::ChipNameFree => ("БЕЗ ОГРАНИЧЕНИЙ ЧИПА", "NO CHIP LIMITS"),
            S::NesTriangleNoVolume => ("треугольник NES без громкости", "the NES triangle has no volume"),
            S::Back => ("← ", "← "),
            S::WaveSub => ("рисуйте мышью", "draw with the mouse"),
            S::TriSub => ("фиксированная 4-бит форма, без громкости", "fixed 4-bit shape, no volume"),
            S::TriHint => ("В режиме FREE форму можно нарисовать", "Switch to FREE to draw your own shape"),
            S::PulseSub => ("тяните фронт или выберите скважность", "drag the edge or pick a duty cycle"),
            S::Harmonics => ("ГАРМОНИКИ", "HARMONICS"),
            S::NoiseSubLong => ("белый шум · хэты, взрывы", "white noise · hats, explosions"),
            S::NoiseSubShort => ("короткий цикл · металлический тон", "short loop · metallic tone"),
            S::Register => ("РЕГИСТР · ОТВОДЫ XOR", "REGISTER · XOR TAPS"),
            S::Period => ("ПЕРИОД", "PERIOD"),
            S::SweepSubChip => ("пунктир — кривая, оранжевый — что сыграет чип", "dashed — curve, orange — what the chip plays"),
            S::SweepSubFree => ("пунктир — кривая, оранжевый — без шагов (FREE)", "dashed — curve, orange — unstepped (FREE)"),
            S::GlideSubLegato => ("только при legato-нотах", "only between legato notes"),
            S::GlideSubAlways => ("на каждой ноте", "on every note"),
            S::EnvSubChip => ("пунктир — ADSR, оранжевый — 16 уровней по кадрам", "dashed — ADSR, orange — 16 levels per frame"),
            S::EnvSubFree => ("пунктир — ADSR, оранжевый — плавная огибающая", "dashed — ADSR, orange — smooth envelope"),
            S::ArpSub => ("высота без перезапуска огибающей", "pitch only, no envelope retrigger"),
            S::ArpExample => ("пример: аккорд C4", "example: C4 chord"),
            S::EchoSub => ("эхо SPC700 · 32 кГц", "SPC700 echo · 32 kHz"),
            S::Assign => ("ASSIGN", "ASSIGN"),
            S::Done => ("ГОТОВО", "DONE"),
            S::Search => ("Поиск…", "Search…"),
            S::All => ("Все", "All"),
            S::User => ("Мои", "User"),
            S::AllPresets => ("ВСЕ ПРЕСЕТЫ", "ALL PRESETS"),
            S::Found => ("НАЙДЕНО", "FOUND"),
            S::Load => ("Загрузить", "Load"),
            S::SaveCurrent => ("Сохранить текущий…", "Save current…"),
            S::Delete => ("Удалить", "Delete"),
            S::SaveTitle => ("СОХРАНИТЬ ПРЕСЕТ", "SAVE PRESET"),
            S::SaveName => ("Название", "Name"),
            S::Save => ("Сохранить", "Save"),
            S::Cancel => ("Отмена", "Cancel"),
            S::NoResults => ("Ничего не найдено", "Nothing found"),
            S::Favorite => ("В избранное", "Favourite"),
        };
        match lang {
            Lang::Ru => ru,
            Lang::En => en,
        }
    }
}

/// Russian plural form for `n`: `one` (1, 21…), `few` (2–4, 22–24…) or `many` (everything else).
pub fn plural_ru(n: u64, one: &'static str, few: &'static str, many: &'static str) -> &'static str {
    let (m10, m100) = (n % 10, n % 100);
    if m10 == 1 && m100 != 11 {
        one
    } else if (2..=4).contains(&m10) && !(12..=14).contains(&m100) {
        few
    } else {
        many
    }
}

/// "64 уровня" / "64 levels" — the number of quantizer levels.
pub fn levels(lang: Lang, n: u64) -> String {
    match lang {
        Lang::Ru => format!("{n} {}", plural_ru(n, "уровень", "уровня", "уровней")),
        Lang::En => format!("{n} {}", if n == 1 { "level" } else { "levels" }),
    }
}

/// "18 центов" / "18 cents".
pub fn cents(lang: Lang, n: i64) -> String {
    match lang {
        Lang::Ru => format!("{n} {}", plural_ru(n.unsigned_abs(), "цент", "цента", "центов")),
        Lang::En => format!("{n} {}", if n.abs() == 1 { "cent" } else { "cents" }),
    }
}

/// Sweep subtitle with the chip's frame rate, e.g. "… что сыграет чип (60 Гц)".
pub fn sweep_sub(lang: Lang, frame_hz: Option<f64>) -> String {
    match frame_hz {
        Some(hz) => {
            let unit = if lang == Lang::Ru { "Гц" } else { "Hz" };
            format!("{} ({:.0} {unit})", S::SweepSubChip.get(lang), hz)
        }
        None => S::SweepSubFree.get(lang).to_string(),
    }
}

/// Vibrato subtitle: "5.5 Гц · 18 центов · задержка 120 мс".
pub fn vibrato_sub(lang: Lang, rate_hz: f32, depth_c: f32, delay_ms: f32) -> String {
    let rate = trim_float(rate_hz, 1);
    let depth = depth_c.round() as i64;
    let delay = delay_ms.round() as i64;
    match lang {
        Lang::Ru => format!("{rate} Гц · {} · задержка {delay} мс", cents(lang, depth)),
        Lang::En => format!("{rate} Hz · {} · delay {delay} ms", cents(lang, depth)),
    }
}

/// Unison subtitle: "расстройка 14 центов · ширина 60%".
pub fn unison_sub(lang: Lang, detune_c: f32, width_pct: f32) -> String {
    let d = detune_c.round() as i64;
    let w = width_pct.round() as i64;
    match lang {
        Lang::Ru => format!("расстройка {} · ширина {w}%", cents(lang, d)),
        Lang::En => format!("detune {} · width {w}%", cents(lang, d)),
    }
}

/// Formats `v` with at most `decimals` decimals, dropping a trailing `.0` ("5.5", "12").
pub fn trim_float(v: f32, decimals: usize) -> String {
    let s = format!("{v:.decimals$}");
    if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        s
    }
}

/// Replaces an ASCII hyphen-minus at the start with the typographic minus `−` (U+2212) the
/// design uses for negative numbers.
pub fn minus(s: String) -> String {
    match s.strip_prefix('-') {
        Some(rest) => format!("−{rest}"),
        None => s,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn russian_plurals() {
        assert_eq!(levels(Lang::Ru, 64), "64 уровня");
        assert_eq!(levels(Lang::Ru, 2), "2 уровня");
        assert_eq!(levels(Lang::Ru, 16), "16 уровней");
        assert_eq!(levels(Lang::Ru, 21), "21 уровень");
        assert_eq!(cents(Lang::Ru, 18), "18 центов");
        assert_eq!(cents(Lang::En, 1), "1 cent");
    }

    #[test]
    fn number_formatting() {
        assert_eq!(trim_float(5.5, 1), "5.5");
        assert_eq!(trim_float(12.0, 1), "12");
        assert_eq!(minus("-12".into()), "−12");
        assert_eq!(vibrato_sub(Lang::Ru, 5.5, 18.0, 120.0), "5.5 Гц · 18 центов · задержка 120 мс");
        assert_eq!(unison_sub(Lang::Ru, 14.0, 60.0), "расстройка 14 центов · ширина 60%");
    }
}
