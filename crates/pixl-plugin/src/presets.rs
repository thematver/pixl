//! Preset store: the embedded factory library, user presets on disk and favourites.
//!
//! - **Factory** presets are compiled into the binary (`presets/factory.json`) and parsed
//!   once per process. Their IDs are `factory:NNN` (three-digit number).
//! - **User** presets live in `~/Library/Application Support/PIXL/Presets/*.pixlpreset`,
//!   one `pixl-preset` JSON document per file. Their IDs are `user:<file stem>`. File names
//!   are derived from the preset name with characters that are unsafe in file names
//!   replaced; writes are atomic (temp file + `fsync` + rename), so a crash never leaves a
//!   truncated preset behind.
//! - **Favourites** are a list of preset IDs in `…/PIXL/favorites.json`.
//!
//! The store never touches the disk from the audio thread. The editor may call
//! [`PresetStore::list`] often, so the user folder is only re-scanned when its modification
//! time changes (checked at most once per second).
//!
//! The base folder is injectable ([`PresetStore::new`]) so tests run against a temporary
//! directory and never touch the real user library.

use crate::state::{clean_name, MAX_NAME_CHARS};
use parking_lot::Mutex;
use pixl_core::preset::{PresetState, PRESET_EXTENSION};
use pixl_core::wave;
use pixl_core::{
    Category, Lang, MacroAssignments, ParamValues, Preset, PresetId, PresetInfo, PresetLibrary, VENDOR_NAME,
};
use serde_json::Value;
use std::collections::BTreeSet;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant, SystemTime};

/// The factory library as shipped (embedded at compile time).
pub const FACTORY_JSON: &str = include_str!("../../../presets/factory.json");

/// Display number of the init preset (a fresh instance equals this factory preset).
pub const INIT_PRESET_NUMBER: u32 = 14;
/// Name of the init preset.
pub const INIT_PRESET_NAME: &str = "Hyrule Fanfare";
/// Category of the init preset.
pub const INIT_PRESET_CATEGORY: Category = Category::Lead;

/// Sub-folder of the PIXL support folder holding user presets.
pub const PRESETS_DIR: &str = "Presets";
/// Favourites file name inside the PIXL support folder.
pub const FAVORITES_FILE: &str = "favorites.json";
/// Author written into user presets.
pub const USER_AUTHOR: &str = "User";

/// Largest preset file the store will read (protects against unrelated huge files).
const MAX_PRESET_FILE_BYTES: u64 = 1 << 20;
/// How often [`PresetStore::list`] checks the disk for external changes.
const RESCAN_INTERVAL: Duration = Duration::from_secs(1);

/// The factory ID for a display number (`factory:014`).
pub fn factory_id(number: u32) -> PresetId {
    format!("factory:{number:03}")
}

/// The ID of a user preset file stem (`user:<stem>`).
pub fn user_id(stem: &str) -> PresetId {
    format!("user:{stem}")
}

// ---------------------------------------------------------------------------------------
// Factory library
// ---------------------------------------------------------------------------------------

/// One factory preset with its resolved state.
#[derive(Clone, Debug)]
pub struct FactoryPreset {
    pub id: PresetId,
    pub preset: Preset,
    pub state: PresetState,
}

/// The parsed factory library.
#[derive(Clone, Debug, Default)]
pub struct FactoryLibrary {
    /// Presets in display-number order (numbers are unique).
    pub presets: Vec<FactoryPreset>,
    /// Problems found while parsing (empty for a correct library).
    pub warnings: Vec<String>,
}

impl FactoryLibrary {
    /// Parse a library document. Never fails: problems are reported in `warnings` and the
    /// offending presets are skipped or sanitized.
    pub fn parse(json: &str) -> Self {
        let mut out = FactoryLibrary::default();
        let lib = match PresetLibrary::from_json(json) {
            Ok(lib) => lib,
            Err(e) => {
                out.warnings.push(e);
                return out;
            }
        };
        let mut seen = BTreeSet::new();
        for preset in lib.presets {
            let label = format!("factory preset {:03} '{}'", preset.number, preset.name);
            if preset.name.trim().is_empty() {
                out.warnings.push(format!("{label}: missing name; skipped"));
                continue;
            }
            if preset.number == 0 || preset.number > 999 {
                out.warnings.push(format!("{label}: invalid number; skipped"));
                continue;
            }
            if !seen.insert(preset.number) {
                out.warnings.push(format!("{label}: duplicate number; skipped"));
                continue;
            }
            let (state, warnings) = preset.to_state();
            out.warnings.extend(warnings.into_iter().map(|w| format!("{label}: {w}")));
            out.presets.push(FactoryPreset { id: factory_id(preset.number), preset, state });
        }
        out.presets.sort_by_key(|p| p.preset.number);
        out
    }

    /// Look up a factory preset by ID.
    pub fn get(&self, id: &str) -> Option<&FactoryPreset> {
        let number: u32 = id.strip_prefix("factory:")?.parse().ok()?;
        self.presets.binary_search_by_key(&number, |p| p.preset.number).ok().map(|i| &self.presets[i])
    }
}

/// The embedded factory library, parsed on first use (never on the audio thread: the first
/// call happens when a plugin instance is created).
pub fn factory_library() -> &'static FactoryLibrary {
    static LIB: OnceLock<FactoryLibrary> = OnceLock::new();
    LIB.get_or_init(|| {
        let lib = FactoryLibrary::parse(FACTORY_JSON);
        for w in &lib.warnings {
            nih_plug::nih_warn!("PIXL factory library: {w}");
        }
        lib
    })
}

/// The state of a fresh instance (parameter defaults, default wave, factory macros).
pub fn init_state() -> PresetState {
    PresetState {
        values: ParamValues::defaults(),
        wave: wave::default_wave(),
        macros: MacroAssignments::factory_default(),
    }
}

// ---------------------------------------------------------------------------------------
// Current preset metadata
// ---------------------------------------------------------------------------------------

/// What the header shows, plus the baseline the modified flag compares against.
#[derive(Clone, Debug, PartialEq)]
pub struct PresetMeta {
    /// `None` when the state did not come from a stored preset.
    pub id: Option<PresetId>,
    pub name: String,
    pub category: Category,
    pub number: u32,
    /// The state as loaded/saved; `None` if unknown (e.g. a restored, modified user preset).
    pub baseline: Option<PresetState>,
    /// Reported "modified" when there is no baseline.
    pub modified_fallback: bool,
}

/// Metadata of a fresh instance: factory 014 "Hyrule Fanfare", unmodified. The baseline is
/// the library's version of that preset (equal to [`init_state`] in a correct library).
pub fn init_meta() -> PresetMeta {
    match factory_library().presets.iter().find(|p| p.preset.number == INIT_PRESET_NUMBER) {
        Some(p) => PresetMeta {
            id: Some(p.id.clone()),
            name: p.preset.name.clone(),
            category: p.preset.category,
            number: p.preset.number,
            baseline: Some(p.state.clone()),
            modified_fallback: false,
        },
        None => PresetMeta {
            id: None,
            name: INIT_PRESET_NAME.to_string(),
            category: INIT_PRESET_CATEGORY,
            number: INIT_PRESET_NUMBER,
            baseline: Some(init_state()),
            modified_fallback: false,
        },
    }
}

// ---------------------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------------------

/// Why a preset operation failed. [`StoreError::message`] gives the user-facing text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StoreError {
    /// The name is empty after trimming.
    EmptyName,
    /// The name is longer than [`MAX_NAME_CHARS`].
    NameTooLong,
    /// The name cannot be turned into a file name.
    InvalidName,
    /// There is no user folder (no home directory).
    NoUserFolder,
    /// No preset with this ID.
    NotFound,
    /// The operation needs a user preset.
    NotUserPreset,
    /// A file could not be read or written.
    Io(String),
    /// A preset file exists but is not a valid PIXL preset.
    Invalid(String),
}

impl StoreError {
    /// Localized message for the editor.
    pub fn message(&self, lang: Lang) -> String {
        let ru = matches!(lang, Lang::Ru);
        match self {
            StoreError::EmptyName => (if ru { "Введите имя пресета" } else { "Enter a preset name" }).to_string(),
            StoreError::NameTooLong => {
                if ru {
                    format!("Имя длиннее {MAX_NAME_CHARS} символов")
                } else {
                    format!("The name is longer than {MAX_NAME_CHARS} characters")
                }
            }
            StoreError::InvalidName => {
                (if ru { "Недопустимое имя пресета" } else { "This name cannot be used" }).to_string()
            }
            StoreError::NoUserFolder => (if ru {
                "Папка пользовательских пресетов недоступна"
            } else {
                "The user preset folder is not available"
            })
            .to_string(),
            StoreError::NotFound => (if ru { "Пресет не найден" } else { "Preset not found" }).to_string(),
            StoreError::NotUserPreset => (if ru {
                "Заводские пресеты нельзя изменить или удалить"
            } else {
                "Factory presets cannot be changed or deleted"
            })
            .to_string(),
            StoreError::Io(e) => {
                if ru {
                    format!("Ошибка диска: {e}")
                } else {
                    format!("Disk error: {e}")
                }
            }
            StoreError::Invalid(e) => {
                if ru {
                    format!("Файл пресета повреждён: {e}")
                } else {
                    format!("The preset file is damaged: {e}")
                }
            }
        }
    }
}

impl From<io::Error> for StoreError {
    fn from(e: io::Error) -> Self {
        StoreError::Io(e.to_string())
    }
}

// ---------------------------------------------------------------------------------------
// The store
// ---------------------------------------------------------------------------------------

/// A preset resolved for loading.
#[derive(Clone, Debug)]
pub struct ResolvedPreset {
    pub id: PresetId,
    pub name: String,
    pub category: Category,
    pub number: u32,
    pub state: PresetState,
    /// Values that had to be ignored or clamped (user presets from other versions).
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug)]
struct UserPreset {
    stem: String,
    preset: Preset,
}

#[derive(Default)]
struct Cache {
    users: Vec<UserPreset>,
    favorites: BTreeSet<String>,
    users_stamp: Option<SystemTime>,
    favorites_stamp: Option<SystemTime>,
    loaded: bool,
    last_check: Option<Instant>,
}

/// Factory + user presets and favourites. Thread-safe; all methods are meant for the main
/// (GUI) thread.
pub struct PresetStore {
    /// `~/Library/Application Support/PIXL` (or a test directory). `None` disables user
    /// presets and favourites persistence.
    root: Option<PathBuf>,
    cache: Mutex<Cache>,
}

impl PresetStore {
    /// A store rooted at `root` (the PIXL support folder; `Presets/` and `favorites.json`
    /// live inside it). Nothing is read until first use.
    pub fn new(root: Option<PathBuf>) -> Self {
        Self { root, cache: Mutex::new(Cache::default()) }
    }

    /// The store at the standard location `~/Library/Application Support/PIXL`.
    pub fn system() -> Self {
        let root = std::env::var_os("HOME")
            .filter(|h| !h.is_empty())
            .map(|home| PathBuf::from(home).join("Library").join("Application Support").join("PIXL"));
        Self::new(root)
    }

    /// The PIXL support folder, if any.
    pub fn root(&self) -> Option<&Path> {
        self.root.as_deref()
    }

    /// The user preset folder, if any.
    pub fn presets_dir(&self) -> Option<PathBuf> {
        self.root.as_ref().map(|r| r.join(PRESETS_DIR))
    }

    fn favorites_path(&self) -> Option<PathBuf> {
        self.root.as_ref().map(|r| r.join(FAVORITES_FILE))
    }

    /// All presets for the browser: factory presets in number order, then user presets by
    /// name (case-insensitive).
    pub fn list(&self) -> Vec<PresetInfo> {
        let mut cache = self.cache.lock();
        self.refresh(&mut cache, false);
        let lib = factory_library();
        let mut out = Vec::with_capacity(lib.presets.len() + cache.users.len());
        for f in &lib.presets {
            out.push(info_for(&f.id, &f.preset, true, cache.favorites.contains(&f.id)));
        }
        for u in &cache.users {
            let id = user_id(&u.stem);
            let fav = cache.favorites.contains(&id);
            out.push(info_for(&id, &u.preset, false, fav));
        }
        out
    }

    /// Force the next [`Self::list`] to re-read the disk.
    pub fn invalidate(&self) {
        let mut cache = self.cache.lock();
        cache.loaded = false;
    }

    /// Resolve a preset ID into a loadable state.
    pub fn resolve(&self, id: &str) -> Result<ResolvedPreset, StoreError> {
        if id.starts_with("factory:") {
            let f = factory_library().get(id).ok_or(StoreError::NotFound)?;
            return Ok(ResolvedPreset {
                id: f.id.clone(),
                name: f.preset.name.clone(),
                category: f.preset.category,
                number: f.preset.number,
                state: f.state.clone(),
                warnings: Vec::new(),
            });
        }
        let stem = id.strip_prefix("user:").filter(|s| is_valid_stem(s)).ok_or(StoreError::NotFound)?;
        let dir = self.presets_dir().ok_or(StoreError::NoUserFolder)?;
        let path = dir.join(format!("{stem}.{PRESET_EXTENSION}"));
        let preset = read_preset_file(&path)?;
        let (state, warnings) = preset.to_state();
        Ok(ResolvedPreset {
            id: user_id(stem),
            name: clean_name(&preset.name),
            category: preset.category,
            number: 0,
            state,
            warnings,
        })
    }

    /// Save `state` as a user preset. A user preset with the same name (case-insensitive)
    /// is overwritten; a different preset whose name maps to the same file name (e.g. "A/B"
    /// and "A:B") is kept and the new one gets a numbered file name. Returns the stored
    /// preset's identity (its state is `state`).
    pub fn save_user(&self, name: &str, category: Category, state: &PresetState) -> Result<ResolvedPreset, StoreError> {
        let display = validate_name(name)?;
        let dir = self.presets_dir().ok_or(StoreError::NoUserFolder)?;
        let base = file_stem_for(&display);
        if base.is_empty() {
            return Err(StoreError::InvalidName);
        }
        let mut cache = self.cache.lock();
        cache.loaded = false;
        self.refresh(&mut cache, true);
        let key = display.to_lowercase();
        let stem = match cache.users.iter().find(|u| clean_name(&u.preset.name).to_lowercase() == key) {
            Some(existing) => existing.stem.clone(),
            None => free_stem(&dir, &base, &cache.users).ok_or(StoreError::InvalidName)?,
        };
        let mut preset = Preset::from_state(&display, category, state);
        preset.author = USER_AUTHOR.to_string();
        fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{stem}.{PRESET_EXTENSION}"));
        write_atomic(&path, preset.to_json().as_bytes())?;
        cache.loaded = false;
        self.refresh(&mut cache, true);
        Ok(ResolvedPreset {
            id: user_id(&stem),
            name: display,
            category,
            number: 0,
            state: state.clone(),
            warnings: Vec::new(),
        })
    }

    /// Delete a user preset (and its favourite mark).
    pub fn delete_user(&self, id: &str) -> Result<(), StoreError> {
        if id.starts_with("factory:") {
            return Err(StoreError::NotUserPreset);
        }
        let stem = id.strip_prefix("user:").filter(|s| is_valid_stem(s)).ok_or(StoreError::NotFound)?;
        let dir = self.presets_dir().ok_or(StoreError::NoUserFolder)?;
        let path = dir.join(format!("{stem}.{PRESET_EXTENSION}"));
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(StoreError::NotFound),
            Err(e) => return Err(e.into()),
        }
        {
            let mut cache = self.cache.lock();
            cache.loaded = false;
            self.refresh(&mut cache, true);
        }
        // The file is gone; failing to drop its favourite mark is not an error for the caller.
        if let Err(e) = self.update_favorites(id, false) {
            nih_plug::nih_warn!("PIXL: could not update favourites after deleting {id}: {e:?}");
        }
        Ok(())
    }

    /// Mark or unmark a preset as favourite (persisted immediately).
    pub fn set_favorite(&self, id: &str, favorite: bool) -> Result<(), StoreError> {
        if !crate::state::is_valid_preset_id(id) {
            return Err(StoreError::NotFound);
        }
        self.update_favorites(id, favorite)
    }

    /// Read-modify-write of the favourites file. The in-memory list only changes once the
    /// file was written, so a failed write never shows a mark that will not persist.
    fn update_favorites(&self, id: &str, favorite: bool) -> Result<(), StoreError> {
        let path = self.favorites_path().ok_or(StoreError::NoUserFolder)?;
        let mut cache = self.cache.lock();
        // Merge with changes other instances may have written since the last scan.
        let mut favorites = read_favorites(&path);
        let changed = if favorite { favorites.insert(id.to_string()) } else { favorites.remove(id) };
        if changed {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            let doc = serde_json::json!({
                "format": "pixl-favorites",
                "version": 1,
                "favorites": favorites.iter().collect::<Vec<_>>(),
            });
            let text = serde_json::to_string_pretty(&doc).map_err(|e| StoreError::Io(e.to_string()))?;
            write_atomic(&path, text.as_bytes())?;
        }
        cache.favorites = favorites;
        cache.favorites_stamp = modified_time(&path);
        Ok(())
    }

    /// Whether a preset is a favourite.
    pub fn is_favorite(&self, id: &str) -> bool {
        let mut cache = self.cache.lock();
        self.refresh(&mut cache, false);
        cache.favorites.contains(id)
    }

    /// Re-read user presets / favourites when they changed on disk (checked at most once
    /// per [`RESCAN_INTERVAL`] unless `force`).
    fn refresh(&self, cache: &mut Cache, force: bool) {
        let now = Instant::now();
        if cache.loaded && !force && cache.last_check.is_some_and(|t| now.duration_since(t) < RESCAN_INTERVAL) {
            return;
        }
        cache.last_check = Some(now);
        let (Some(dir), Some(fav_path)) = (self.presets_dir(), self.favorites_path()) else {
            cache.loaded = true;
            return;
        };
        let users_stamp = modified_time(&dir);
        if !cache.loaded || users_stamp != cache.users_stamp || users_stamp.is_none() {
            cache.users = scan_user_presets(&dir);
            cache.users_stamp = users_stamp;
        }
        let favorites_stamp = modified_time(&fav_path);
        if !cache.loaded || favorites_stamp != cache.favorites_stamp {
            cache.favorites = read_favorites(&fav_path);
            cache.favorites_stamp = favorites_stamp;
        }
        cache.loaded = true;
    }
}

fn info_for(id: &str, preset: &Preset, is_factory: bool, favorite: bool) -> PresetInfo {
    PresetInfo {
        id: id.to_string(),
        name: clean_name(&preset.name),
        category: preset.category,
        number: if is_factory { preset.number } else { 0 },
        chip: preset.chip(),
        icon: preset.icon,
        tags: preset.tags.clone(),
        description: preset.description.clone(),
        author: if preset.author.is_empty() && is_factory {
            format!("{VENDOR_NAME} Factory")
        } else {
            preset.author.clone()
        },
        is_factory,
        favorite,
    }
}

fn modified_time(path: &Path) -> Option<SystemTime> {
    fs::metadata(path).and_then(|m| m.modified()).ok()
}

fn scan_user_presets(dir: &Path) -> Vec<UserPreset> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let is_preset =
            path.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case(PRESET_EXTENSION));
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else { continue };
        if !is_preset || stem.starts_with('.') || !is_valid_stem(stem) {
            continue;
        }
        match read_preset_file(&path) {
            Ok(preset) => out.push(UserPreset { stem: stem.to_string(), preset }),
            Err(e) => nih_plug::nih_warn!("PIXL: skipping user preset {}: {e:?}", path.display()),
        }
    }
    out.sort_by(|a, b| {
        let (x, y) = (clean_name(&a.preset.name).to_lowercase(), clean_name(&b.preset.name).to_lowercase());
        x.cmp(&y).then_with(|| a.stem.cmp(&b.stem))
    });
    out
}

fn read_preset_file(path: &Path) -> Result<Preset, StoreError> {
    let file = match fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(StoreError::NotFound),
        Err(e) => return Err(e.into()),
    };
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(StoreError::NotFound);
    }
    if meta.len() > MAX_PRESET_FILE_BYTES {
        return Err(StoreError::Invalid("file too large".to_string()));
    }
    let mut text = String::new();
    file.take(MAX_PRESET_FILE_BYTES).read_to_string(&mut text).map_err(|e| StoreError::Invalid(e.to_string()))?;
    Preset::from_json(&text).map_err(StoreError::Invalid)
}

fn read_favorites(path: &Path) -> BTreeSet<String> {
    let Ok(text) = fs::read_to_string(path) else {
        return BTreeSet::new();
    };
    let Ok(v) = serde_json::from_str::<Value>(&text) else {
        return BTreeSet::new();
    };
    let list = match &v {
        Value::Array(a) => Some(a),
        Value::Object(o) => o.get("favorites").and_then(Value::as_array),
        _ => None,
    };
    list.map(|a| {
        a.iter()
            .filter_map(Value::as_str)
            .filter(|id| crate::state::is_valid_preset_id(id))
            .map(str::to_string)
            .collect()
    })
    .unwrap_or_default()
}

/// Validate and clean a user-entered preset name.
pub fn validate_name(name: &str) -> Result<String, StoreError> {
    let cleaned: String = name.chars().filter(|c| !c.is_control()).collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        return Err(StoreError::EmptyName);
    }
    if trimmed.chars().count() > MAX_NAME_CHARS {
        return Err(StoreError::NameTooLong);
    }
    Ok(trimmed.to_string())
}

/// File stem for a preset name: characters that are unsafe in file names (`/ \ : * ? " < > |`
/// and control characters) become `_`, leading dots and surrounding whitespace are removed,
/// and the result is limited to [`MAX_NAME_CHARS`] characters. Idempotent.
pub fn file_stem_for(name: &str) -> String {
    let mapped: String = name
        .chars()
        .filter(|c| !c.is_control())
        .map(|c| if matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') { '_' } else { c })
        .collect();
    let trimmed = mapped.trim_start_matches(|c: char| c == '.' || c.is_whitespace());
    let limited: String = trimmed.chars().take(MAX_NAME_CHARS).collect();
    limited.trim_end().to_string()
}

/// A file stem for a new preset whose sanitized name is `base`: `base` itself if no file or
/// known preset uses it (case-insensitively), else `base 2`, `base 3`, … (shortened so the
/// stem stays within [`MAX_NAME_CHARS`]).
fn free_stem(dir: &Path, base: &str, users: &[UserPreset]) -> Option<String> {
    let taken = |stem: &str| {
        let lower = stem.to_lowercase();
        users.iter().any(|u| u.stem.to_lowercase() == lower) || dir.join(format!("{stem}.{PRESET_EXTENSION}")).exists()
    };
    if !taken(base) {
        return Some(base.to_string());
    }
    (2..10_000u32)
        .map(|n| {
            let suffix = format!(" {n}");
            let head: String = base.chars().take(MAX_NAME_CHARS.saturating_sub(suffix.chars().count())).collect();
            format!("{}{suffix}", head.trim_end())
        })
        .find(|stem| is_valid_stem(stem) && !taken(stem))
}

/// Whether `stem` is a file stem this store could have produced (rejects path traversal).
pub fn is_valid_stem(stem: &str) -> bool {
    !stem.is_empty() && file_stem_for(stem) == stem
}

/// Write `bytes` to `path` atomically: write a hidden temp file in the same folder, flush it
/// to disk, then rename it over the destination.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let dir = path.parent().ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no parent folder"))?;
    let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("preset");
    let tmp = dir.join(format!(".{file_name}.{}.{}.tmp", std::process::id(), COUNTER.fetch_add(1, Ordering::Relaxed)));
    let result = (|| {
        let mut f = fs::OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use pixl_core::{ChipMode, ParamId, PresetIcon};
    use std::collections::HashSet;

    /// A unique, self-deleting temporary directory (no external crates).
    pub(crate) struct TempDir(pub PathBuf);

    impl TempDir {
        pub(crate) fn new(tag: &str) -> Self {
            static N: AtomicU64 = AtomicU64::new(0);
            let nanos = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
            let dir = std::env::temp_dir().join(format!(
                "pixl-test-{tag}-{}-{nanos}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&dir).expect("create temp dir");
            TempDir(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn some_state() -> PresetState {
        let mut values = ParamValues::defaults();
        values.set(ParamId::Chip, 0.0);
        values.set(ParamId::P1On, 1.0);
        values.set(ParamId::P1Duty, 12.5);
        values.set(ParamId::WvOn, 0.0);
        values.set(ParamId::SweepTime, 321.5);
        values.set(ParamId::ArpChord, 4.0);
        let mut macros = MacroAssignments::empty();
        macros.set(0, ParamId::P1Duty, 0.4);
        macros.set(3, ParamId::EchoMix, -0.25);
        PresetState { values, wave: wave::saw(), macros }
    }

    #[test]
    fn factory_library_loads_without_warnings() {
        let lib = factory_library();
        assert!(lib.warnings.is_empty(), "factory library warnings:\n{}", lib.warnings.join("\n"));
        assert!(!lib.presets.is_empty());
        let mut names = HashSet::new();
        let mut numbers = HashSet::new();
        for p in &lib.presets {
            assert!(names.insert(p.preset.name.to_lowercase()), "duplicate name {}", p.preset.name);
            assert!(numbers.insert(p.preset.number));
            assert!((1..=128).contains(&p.preset.number), "{} number {}", p.preset.name, p.preset.number);
            // Number ranges per category (SPEC §9).
            let expected = match p.preset.number {
                1..=32 => Category::Lead,
                33..=56 => Category::Bass,
                57..=74 => Category::Arp,
                75..=86 => Category::Pad,
                87..=108 => Category::Drum,
                _ => Category::Sfx,
            };
            assert_eq!(p.preset.category, expected, "{} ({})", p.preset.name, p.preset.number);
            assert!(!p.preset.description.ru.is_empty() && !p.preset.description.en.is_empty(), "{}", p.preset.name);
            assert_eq!(lib.get(&p.id).map(|f| f.preset.number), Some(p.preset.number));
        }
    }

    #[test]
    fn init_preset_is_factory_014_and_equals_a_fresh_instance() {
        let lib = factory_library();
        let f = lib.get("factory:014").expect("factory preset 014 exists");
        assert_eq!(f.preset.name, INIT_PRESET_NAME);
        assert_eq!(f.preset.category, INIT_PRESET_CATEGORY);
        assert!(crate::state::states_equivalent(&f.state, &init_state()), "factory 014 differs from the init state");
        let meta = init_meta();
        assert_eq!(meta.id.as_deref(), Some("factory:014"));
        assert!(!meta.is_modified(&init_state()));
    }

    #[test]
    fn library_parsing_reports_problems() {
        let lib = FactoryLibrary::parse("garbage");
        assert!(lib.presets.is_empty());
        assert_eq!(lib.warnings.len(), 1);
        let json = r#"{"format":"pixl-library","version":1,"presets":[
            {"name":"A","category":"LEAD","number":3,"params":{"bogus":1}},
            {"name":"B","category":"LEAD","number":3},
            {"name":"C","category":"BASS","number":40}]}"#;
        let lib = FactoryLibrary::parse(json);
        assert_eq!(lib.presets.len(), 2);
        assert_eq!(lib.warnings.len(), 2, "{:?}", lib.warnings);
        assert!(lib.get("factory:040").is_some());
        assert!(lib.get("factory:003").is_some());
        assert!(lib.get("factory:004").is_none());
        assert!(lib.get("factory:x").is_none());
    }

    #[test]
    fn save_list_load_delete_roundtrip() {
        let tmp = TempDir::new("store");
        let store = PresetStore::new(Some(tmp.0.clone()));
        let factory_count = factory_library().presets.len();
        assert_eq!(store.list().len(), factory_count);

        let state = some_state();
        let saved = store.save_user("  My Lead  ", Category::Lead, &state).unwrap();
        assert_eq!(saved.id, "user:My Lead");
        assert_eq!(saved.name, "My Lead");
        let file = tmp.0.join(PRESETS_DIR).join("My Lead.pixlpreset");
        assert!(file.is_file());

        let list = store.list();
        assert_eq!(list.len(), factory_count + 1);
        let info = list.last().unwrap();
        assert_eq!(info.id, "user:My Lead");
        assert!(!info.is_factory);
        assert_eq!(info.number, 0);
        assert_eq!(info.chip, ChipMode::Nes);
        assert_eq!(info.icon, PresetIcon::P12);
        assert_eq!(info.author, USER_AUTHOR);

        let loaded = store.resolve("user:My Lead").unwrap();
        assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
        assert_eq!(loaded.name, "My Lead");
        assert!(crate::state::states_equivalent(&loaded.state, &state));
        assert_eq!(loaded.state.wave, state.wave);

        // Same name (different case) overwrites instead of duplicating.
        let mut state2 = state.clone();
        state2.values.set(ParamId::P1Level, 3.0);
        let saved2 = store.save_user("my lead", Category::Bass, &state2).unwrap();
        assert_eq!(saved2.id, "user:My Lead");
        assert_eq!(store.list().len(), factory_count + 1);
        let loaded2 = store.resolve(&saved2.id).unwrap();
        assert_eq!(loaded2.state.values.get(ParamId::P1Level), 3.0);
        assert_eq!(loaded2.category, Category::Bass);

        // User presets are sorted by name after the factory presets.
        store.save_user("Alpha", Category::Pad, &state).unwrap();
        store.save_user("zeta", Category::Pad, &state).unwrap();
        let names: Vec<_> = store.list().into_iter().skip(factory_count).map(|p| p.name).collect();
        assert_eq!(names, ["Alpha", "my lead", "zeta"]);

        // No temp files are left behind.
        let leftovers: Vec<_> = fs::read_dir(tmp.0.join(PRESETS_DIR))
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty());

        store.set_favorite("user:Alpha", true).unwrap();
        store.delete_user("user:Alpha").unwrap();
        assert!(!store.is_favorite("user:Alpha"));
        assert_eq!(store.resolve("user:Alpha").unwrap_err(), StoreError::NotFound);
        assert_eq!(store.delete_user("user:Alpha").unwrap_err(), StoreError::NotFound);
        assert_eq!(store.delete_user("factory:014").unwrap_err(), StoreError::NotUserPreset);
        assert_eq!(store.list().len(), factory_count + 2);
    }

    #[test]
    fn favorites_persist_across_store_instances() {
        let tmp = TempDir::new("fav");
        let a = PresetStore::new(Some(tmp.0.clone()));
        a.set_favorite("factory:014", true).unwrap();
        a.set_favorite("factory:001", true).unwrap();
        a.set_favorite("factory:001", false).unwrap();
        assert!(a.is_favorite("factory:014"));
        assert!(a.set_favorite("../../x", true).is_err());

        let b = PresetStore::new(Some(tmp.0.clone()));
        assert!(b.is_favorite("factory:014"));
        assert!(!b.is_favorite("factory:001"));
        if factory_library().get("factory:014").is_some() {
            assert!(b.list().iter().any(|p| p.id == "factory:014" && p.favorite));
        }
        // A garbage favourites file is ignored, then replaced on the next write.
        fs::write(tmp.0.join(FAVORITES_FILE), "{{{{").unwrap();
        let c = PresetStore::new(Some(tmp.0.clone()));
        assert!(!c.is_favorite("factory:014"));
        c.set_favorite("factory:002", true).unwrap();
        let d = PresetStore::new(Some(tmp.0.clone()));
        assert!(d.is_favorite("factory:002"));
        // Plain array form is accepted too.
        fs::write(tmp.0.join(FAVORITES_FILE), r#"["factory:007", 5, "nope"]"#).unwrap();
        let e = PresetStore::new(Some(tmp.0.clone()));
        assert!(e.is_favorite("factory:007"));
    }

    #[test]
    fn names_are_validated_and_sanitized() {
        assert_eq!(validate_name("   ").unwrap_err(), StoreError::EmptyName);
        assert_eq!(validate_name(&"я".repeat(65)).unwrap_err(), StoreError::NameTooLong);
        assert_eq!(validate_name(" Ок\n ").unwrap(), "Ок");
        assert_eq!(file_stem_for("a/b\\c:d*e?f\"g<h>i|j"), "a_b_c_d_e_f_g_h_i_j");
        assert_eq!(file_stem_for(". . .hidden"), "hidden");
        assert_eq!(file_stem_for("...."), "");
        assert_eq!(file_stem_for("Лид «Тест»"), "Лид «Тест»");
        for s in ["a/b", "..", "../x", " x", "x ", "", ".x", "a\u{0}b"] {
            assert!(!is_valid_stem(s), "{s:?}");
        }
        for name in ["a/b", ". .x", "Лид", "x", "  y  ", &"q".repeat(80)] {
            let stem = file_stem_for(name);
            assert_eq!(file_stem_for(&stem), stem, "idempotent for {name:?}");
        }

        let tmp = TempDir::new("names");
        let store = PresetStore::new(Some(tmp.0.clone()));
        assert_eq!(store.save_user("....", Category::Lead, &init_state()).unwrap_err(), StoreError::InvalidName);
        let saved = store.save_user("Bass/Lead: 2", Category::Bass, &init_state()).unwrap();
        assert_eq!(saved.id, "user:Bass_Lead_ 2");
        assert_eq!(saved.name, "Bass/Lead: 2");
        assert_eq!(store.resolve(&saved.id).unwrap().name, "Bass/Lead: 2");
        // A different name with the same sanitized file name does not overwrite it.
        let other = store.save_user("Bass:Lead/ 2", Category::Lead, &init_state()).unwrap();
        assert_eq!(other.id, "user:Bass_Lead_ 2 2");
        assert_eq!(store.resolve(&saved.id).unwrap().name, "Bass/Lead: 2");
        assert_eq!(store.resolve(&other.id).unwrap().name, "Bass:Lead/ 2");
        // Maximum-length names that collide after sanitizing: the numbered stem is shortened.
        let head = "x".repeat(MAX_NAME_CHARS - 1);
        let a = store.save_user(&format!("{head}/"), Category::Lead, &init_state()).unwrap();
        let b = store.save_user(&format!("{head}:"), Category::Lead, &init_state()).unwrap();
        assert_eq!(a.id, format!("user:{head}_"));
        assert_eq!(b.id, format!("user:{} 2", "x".repeat(MAX_NAME_CHARS - 2)));
        assert!(is_valid_stem(b.id.strip_prefix("user:").unwrap()));
        assert_eq!(store.resolve(&b.id).unwrap().name, format!("{head}:"));
        // Path traversal through IDs is impossible.
        assert_eq!(store.resolve("user:../../../etc/hosts").unwrap_err(), StoreError::NotFound);
        assert_eq!(store.resolve("user:").unwrap_err(), StoreError::NotFound);
        assert_eq!(store.resolve("factory:999").unwrap_err(), StoreError::NotFound);
    }

    #[test]
    fn broken_and_foreign_files_are_skipped() {
        let tmp = TempDir::new("broken");
        let dir = tmp.0.join(PRESETS_DIR);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("Broken.pixlpreset"), "{ not json").unwrap();
        fs::write(dir.join("Foreign.pixlpreset"), r#"{"format":"other","name":"x"}"#).unwrap();
        fs::write(dir.join("notes.txt"), "hello").unwrap();
        fs::write(dir.join(".Hidden.pixlpreset"), r#"{"format":"pixl-preset","name":"Hidden"}"#).unwrap();
        fs::write(
            dir.join("Sparse.PIXLPRESET"),
            r#"{"format":"pixl-preset","name":"Sparse","params":{"chip":"FREE"}}"#,
        )
        .unwrap();
        fs::create_dir_all(dir.join("Folder.pixlpreset")).unwrap();
        let store = PresetStore::new(Some(tmp.0.clone()));
        let users: Vec<_> = store.list().into_iter().filter(|p| !p.is_factory).collect();
        assert_eq!(users.len(), 1);
        assert_eq!(users[0].name, "Sparse");
        assert_eq!(users[0].chip, ChipMode::Free);
        assert!(matches!(store.resolve("user:Broken").unwrap_err(), StoreError::Invalid(_)));
        let sparse = store.resolve("user:Sparse");
        // The extension match is case-insensitive for listing; resolving uses the canonical
        // lower-case extension, which a case-insensitive volume also finds.
        if let Ok(s) = sparse {
            assert_eq!(s.state.values.chip(), ChipMode::Free);
            assert_eq!(s.state.wave, wave::default_wave());
        }
    }

    #[test]
    fn no_user_folder_degrades_gracefully() {
        let store = PresetStore::new(None);
        assert_eq!(store.list().len(), factory_library().presets.len());
        assert_eq!(store.save_user("X", Category::Lead, &init_state()).unwrap_err(), StoreError::NoUserFolder);
        assert!(store.set_favorite("factory:014", true).is_err());
        assert!(!store.is_favorite("factory:014"));
    }

    #[test]
    fn external_changes_are_picked_up() {
        let tmp = TempDir::new("ext");
        let a = PresetStore::new(Some(tmp.0.clone()));
        let b = PresetStore::new(Some(tmp.0.clone()));
        let n = b.list().len();
        a.save_user("Shared", Category::Arp, &init_state()).unwrap();
        b.invalidate();
        assert_eq!(b.list().len(), n + 1);
    }

    #[test]
    fn error_messages_are_localized() {
        for e in [
            StoreError::EmptyName,
            StoreError::NameTooLong,
            StoreError::InvalidName,
            StoreError::NoUserFolder,
            StoreError::NotFound,
            StoreError::NotUserPreset,
            StoreError::Io("x".into()),
            StoreError::Invalid("y".into()),
        ] {
            let ru = e.message(Lang::Ru);
            let en = e.message(Lang::En);
            assert!(!ru.is_empty() && !en.is_empty());
            assert_ne!(ru, en);
        }
    }

    #[test]
    fn atomic_write_replaces_content() {
        let tmp = TempDir::new("atomic");
        let path = tmp.0.join("file.json");
        write_atomic(&path, b"one").unwrap();
        write_atomic(&path, b"two").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "two");
        assert_eq!(fs::read_dir(&tmp.0).unwrap().count(), 1);
        assert!(write_atomic(&tmp.0.join("missing").join("x"), b"z").is_err());
    }
}
