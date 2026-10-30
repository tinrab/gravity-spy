//! Desktop files and browser storage share the same save format and recovery rules.
use std::{
    collections::BTreeMap,
    io,
    path::{Path, PathBuf},
};
#[cfg(not(target_arch = "wasm32"))]
use std::{fs, io::Write};

use bevy::prelude::*;
#[cfg(not(target_arch = "wasm32"))]
use directories::ProjectDirs;
#[cfg(target_arch = "wasm32")]
use js_sys::Date;
use ron::{
    de::from_bytes,
    ser::{PrettyConfig, to_string_pretty},
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
#[cfg(not(target_arch = "wasm32"))]
use tempfile::{Builder, NamedTempFile};
#[cfg(target_arch = "wasm32")]
use wasm_bindgen::JsValue;
#[cfg(target_arch = "wasm32")]
use web_sys::Storage;

use crate::level::LevelId;

const VERSION: u32 = 1;

#[derive(Resource, Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Settings {
    volume: f32,
    muted: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            volume: 0.25,
            muted: true,
        }
    }
}

impl Settings {
    pub fn volume(&self) -> f32 {
        self.volume
    }

    pub fn muted(&self) -> bool {
        self.muted
    }

    pub fn effective_volume(&self) -> f32 {
        if self.muted { 0.0 } else { self.volume }
    }

    pub fn toggle_mute(&mut self) {
        self.muted = !self.muted;
    }

    pub fn adjust_volume(&mut self, amount: f32) {
        self.volume = (self.volume + amount).clamp(0.0, 1.0);
    }

    fn validated(mut self, diagnostics: &mut Vec<String>) -> Self {
        if !self.volume.is_finite() || !(0.0..=1.0).contains(&self.volume) {
            diagnostics.push("Invalid audio volume; using the default volume.".into());
            self.volume = Self::default().volume;
        }
        self
    }
}

#[derive(Resource, Clone, Debug, Default, PartialEq)]
pub struct Progress {
    pub bests: BTreeMap<LevelId, f32>,
}

/// Desktop file paths, or browser localStorage keys.
#[derive(Clone, Debug)]
pub struct StoragePaths {
    pub settings: PathBuf,
    pub progress: PathBuf,
}

impl StoragePaths {
    #[cfg(not(target_arch = "wasm32"))]
    pub fn discover() -> Option<Self> {
        let project = ProjectDirs::from("", "", "gravity-spy")?;
        let settings = project.config_dir().join("settings.ron");
        let progress = project.data_local_dir().join("progress.ron");
        (settings.is_absolute() && progress.is_absolute()).then_some(Self { settings, progress })
    }

    #[cfg(target_arch = "wasm32")]
    pub fn discover() -> Option<Self> {
        Some(Self {
            settings: "gravity-spy.settings".into(),
            progress: "gravity-spy.progress".into(),
        })
    }
}

#[derive(Serialize, Deserialize)]
#[serde(default)]
struct SettingsFile {
    version: u32,
    audio: Settings,
}

impl Default for SettingsFile {
    fn default() -> Self {
        Self {
            version: VERSION,
            audio: Settings::default(),
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(default)]
struct ProgressFile {
    version: u32,
    bests: BTreeMap<LevelId, f32>,
}

impl Default for ProgressFile {
    fn default() -> Self {
        Self {
            version: VERSION,
            bests: BTreeMap::new(),
        }
    }
}

/// Tracks what is on disk so only changed values are written. A file that cannot be read
/// safely is blocked for the session so it is never overwritten.
#[derive(Resource)]
pub struct Persistence {
    paths: Option<StoragePaths>,
    saved_settings: Option<Settings>,
    saved_progress: Option<Progress>,
    settings_blocked: bool,
    progress_blocked: bool,
    diagnostics: Vec<String>,
    pub settings_error: Option<String>,
    pub progress_error: Option<String>,
}

impl Persistence {
    pub fn load(paths: Option<StoragePaths>) -> (Settings, Progress, Self) {
        let mut store = Self {
            paths: paths.clone(),
            saved_settings: None,
            saved_progress: None,
            settings_blocked: false,
            progress_blocked: false,
            diagnostics: Vec::new(),
            settings_error: None,
            progress_error: None,
        };
        let Some(paths) = paths else {
            let error = "No valid per-user directories are available; changes remain in memory.";
            store.diagnostics.push(error.into());
            store.block_settings(error.into());
            store.block_progress(error.into());
            return (Settings::default(), Progress::default(), store);
        };
        let progress = store.load_progress(&paths.progress);
        let settings = store.load_settings(&paths.settings);
        (settings, progress, store)
    }

    fn block_settings(&mut self, error: String) {
        self.settings_error = Some(error);
        self.settings_blocked = true;
    }

    fn block_progress(&mut self, error: String) {
        self.progress_error = Some(error);
        self.progress_blocked = true;
    }

    fn load_settings(&mut self, path: &Path) -> Settings {
        let file = match read_document::<SettingsFile>(path, &mut self.diagnostics) {
            Document::Loaded(file) => file,
            Document::Missing | Document::Recovered => return Settings::default(),
            Document::Unreadable(error) => {
                self.block_settings(error);
                return Settings::default();
            }
        };
        if file.version > VERSION {
            self.block_settings(format!(
                "{}: unsupported settings version {}",
                path.display(),
                file.version
            ));
            return Settings::default();
        }
        let settings = file.audio.validated(&mut self.diagnostics);
        if settings == file.audio {
            self.saved_settings = Some(settings);
        }
        settings
    }

    fn load_progress(&mut self, path: &Path) -> Progress {
        let file = match read_document::<ProgressFile>(path, &mut self.diagnostics) {
            Document::Loaded(file) => file,
            Document::Missing => {
                // Nothing to write until the first best time.
                self.saved_progress = Some(Progress::default());
                return Progress::default();
            }
            Document::Recovered => return Progress::default(),
            Document::Unreadable(error) => {
                self.block_progress(error);
                return Progress::default();
            }
        };
        if file.version > VERSION {
            self.block_progress(format!(
                "{}: unsupported progress version {}",
                path.display(),
                file.version
            ));
            return Progress::default();
        }
        let mut progress = Progress { bests: file.bests };
        let count = progress.bests.len();
        progress
            .bests
            .retain(|_, best| best.is_finite() && *best > 0.0);
        if progress.bests.len() == count {
            self.saved_progress = Some(progress.clone());
        } else {
            self.diagnostics
                .push("Ignoring invalid best times; valid progress is retained.".into());
        }
        progress
    }

    fn flush(&mut self, settings: &Settings, progress: &Progress) {
        let Some(paths) = &self.paths else {
            return;
        };
        if self.saved_settings.as_ref() != Some(settings) && !self.settings_blocked {
            let file = SettingsFile {
                version: VERSION,
                audio: *settings,
            };
            match write_document(&paths.settings, &file) {
                Ok(()) => {
                    self.saved_settings = Some(*settings);
                    self.settings_error = None;
                    debug!("Settings saved: {}", paths.settings.display());
                }
                Err(error) => {
                    self.settings_error = Some(format!("{}: {error}", paths.settings.display()))
                }
            }
        }
        if self.saved_progress.as_ref() != Some(progress) && !self.progress_blocked {
            let file = ProgressFile {
                version: VERSION,
                bests: progress.bests.clone(),
            };
            match write_document(&paths.progress, &file) {
                Ok(()) => {
                    self.saved_progress = Some(progress.clone());
                    self.progress_error = None;
                    debug!("Progress saved: {}", paths.progress.display());
                }
                Err(error) => {
                    self.progress_error = Some(format!("{}: {error}", paths.progress.display()))
                }
            }
        }
    }
}

enum Document<T> {
    Missing,
    Loaded(T),
    /// Corrupt contents were backed up, so the file may be replaced with defaults.
    Recovered,
    /// The file could not be read or backed up; it must not be replaced this session.
    Unreadable(String),
}

fn read_document<T: DeserializeOwned>(path: &Path, diagnostics: &mut Vec<String>) -> Document<T> {
    let bytes = match read_bytes(path) {
        Ok(bytes) => bytes,
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
            ) =>
        {
            return Document::Missing;
        }
        Err(error) => return Document::Unreadable(format!("{}: {error}", path.display())),
    };
    let error = match from_bytes(&bytes) {
        Ok(value) => return Document::Loaded(value),
        Err(error) => error,
    };
    match backup_document(path, &bytes) {
        Ok(backup) => {
            diagnostics.push(format!(
                "{}: {error}; preserved as {} and using defaults",
                path.display(),
                backup.display()
            ));
            Document::Recovered
        }
        Err(backup_error) => Document::Unreadable(format!(
            "{}: {error}; cannot preserve corrupt file: {backup_error}",
            path.display()
        )),
    }
}

fn write_document(path: &Path, value: &impl Serialize) -> io::Result<()> {
    let text = to_string_pretty(value, PrettyConfig::default()).map_err(io::Error::other)?;
    write_bytes(path, format!("{text}\n").as_bytes())
}

#[cfg(not(target_arch = "wasm32"))]
fn read_bytes(path: &Path) -> io::Result<Vec<u8>> {
    fs::read(path)
}

#[cfg(not(target_arch = "wasm32"))]
fn backup_document(path: &Path, bytes: &[u8]) -> io::Result<PathBuf> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("missing parent directory"))?;
    let prefix = format!(
        "{}.invalid-",
        path.file_name().unwrap_or_default().to_string_lossy()
    );
    let mut copy = Builder::new().prefix(&prefix).tempfile_in(parent)?;
    copy.write_all(bytes)?;
    copy.as_file().sync_all()?;
    let (_, backup) = copy.keep().map_err(io::Error::other)?;
    Ok(backup)
}

#[cfg(not(target_arch = "wasm32"))]
fn write_bytes(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("missing parent directory"))?;
    fs::create_dir_all(parent)?;
    let mut temporary = NamedTempFile::new_in(parent)?;
    temporary.write_all(bytes)?;
    temporary.as_file().sync_all()?;
    temporary.persist(path).map_err(|error| error.error)?;
    #[cfg(unix)]
    fs::File::open(parent)?.sync_all()?;
    Ok(())
}

#[cfg(target_arch = "wasm32")]
fn browser_storage() -> io::Result<Storage> {
    web_sys::window()
        .ok_or_else(|| io::Error::other("browser window unavailable"))?
        .local_storage()
        .map_err(browser_error)?
        .ok_or_else(|| io::Error::other("browser storage unavailable"))
}

#[cfg(target_arch = "wasm32")]
fn browser_error(error: JsValue) -> io::Error {
    io::Error::other(format!("browser storage: {error:?}"))
}

#[cfg(target_arch = "wasm32")]
fn read_bytes(path: &Path) -> io::Result<Vec<u8>> {
    browser_storage()?
        .get_item(&path.to_string_lossy())
        .map_err(browser_error)?
        .map(String::into_bytes)
        .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))
}

#[cfg(target_arch = "wasm32")]
fn write_bytes(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let text = str::from_utf8(bytes).map_err(io::Error::other)?;
    browser_storage()?
        .set_item(&path.to_string_lossy(), text)
        .map_err(browser_error)
}

#[cfg(target_arch = "wasm32")]
fn backup_document(path: &Path, bytes: &[u8]) -> io::Result<PathBuf> {
    let storage = browser_storage()?;
    let prefix = format!("{}.invalid-{}", path.display(), Date::now());
    let mut backup = prefix.clone();
    let mut suffix = 0_u32;
    while storage.get_item(&backup).map_err(browser_error)?.is_some() {
        suffix += 1;
        backup = format!("{prefix}-{suffix}");
    }
    write_bytes(Path::new(&backup), bytes)?;
    Ok(PathBuf::from(backup))
}

pub struct SettingsPlugin;

impl Plugin for SettingsPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, report_storage)
            .add_systems(PostUpdate, persist_changes);
    }
}

fn report_storage(store: Res<Persistence>) {
    if let Some(paths) = &store.paths {
        info!("Settings storage: {}", paths.settings.display());
        info!("Progress storage: {}", paths.progress.display());
    }
    for message in store
        .diagnostics
        .iter()
        .chain(store.settings_error.iter())
        .chain(store.progress_error.iter())
    {
        warn!("{message}");
    }
}

fn persist_changes(
    settings: Res<Settings>,
    progress: Res<Progress>,
    mut store: ResMut<Persistence>,
    mut exit: MessageReader<AppExit>,
) {
    let exiting = exit.read().next().is_some();
    if settings.is_changed() || progress.is_changed() || exiting {
        store.flush(&settings, &progress);
        for error in store
            .settings_error
            .iter()
            .chain(store.progress_error.iter())
        {
            warn!("Could not save: {error}");
        }
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use tempfile::tempdir;

    use super::*;

    fn paths(directory: &Path) -> StoragePaths {
        StoragePaths {
            settings: directory.join("config/settings.ron"),
            progress: directory.join("data/progress.ron"),
        }
    }
    fn put(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }
    fn progress_record(level: &str, best: f32) -> Progress {
        Progress {
            bests: BTreeMap::from([(LevelId(level.into()), best)]),
        }
    }

    #[test]
    fn paths_follow_platform_standards_and_never_use_a_temporary_fallback() {
        let paths = StoragePaths::discover().unwrap();
        let project = ProjectDirs::from("", "", "gravity-spy").unwrap();
        assert_eq!(paths.settings, project.config_dir().join("settings.ron"));
        assert_eq!(
            paths.progress,
            project.data_local_dir().join("progress.ron")
        );
        assert!(paths.settings.is_absolute() && paths.progress.is_absolute());
        #[cfg(target_os = "windows")]
        assert!(
            paths
                .settings
                .ends_with(Path::new("gravity-spy").join("config/settings.ron"))
        );
        #[cfg(target_os = "macos")]
        assert!(
            paths
                .settings
                .ends_with("Library/Application Support/gravity-spy/settings.ron")
        );
        #[cfg(target_os = "linux")]
        assert!(paths.settings.ends_with("gravity-spy/settings.ron"));
        let (settings, progress, mut store) = Persistence::load(None);
        store.flush(&settings, &progress);
        assert!(store.paths.is_none() && store.settings_error.is_some());
    }

    #[test]
    fn settings_replace_existing_files_and_round_trip_separately_from_progress() {
        let directory = Builder::new().prefix("gravity spy ü ").tempdir().unwrap();
        let paths = paths(directory.path());
        let (mut settings, mut progress, mut store) = Persistence::load(Some(paths.clone()));
        store.flush(&settings, &progress);
        assert!(paths.settings.is_file());
        assert!(!paths.progress.exists());
        settings.toggle_mute();
        settings.adjust_volume(0.35);
        progress.bests.insert(LevelId("level-1".into()), 20.0);
        store.flush(&settings, &progress);
        settings.adjust_volume(-0.1);
        store.flush(&settings, &progress);
        assert!(store.settings_error.is_none() && store.progress_error.is_none());
        let (loaded_settings, loaded_progress, _) = Persistence::load(Some(paths.clone()));
        assert_eq!(loaded_settings, settings);
        assert_eq!(loaded_progress, progress);
        assert!(
            !fs::read_to_string(paths.settings)
                .unwrap()
                .contains("bests")
        );
        assert!(
            !fs::read_to_string(paths.progress)
                .unwrap()
                .contains("volume")
        );
        assert_eq!(
            fs::read_dir(directory.path().join("config"))
                .unwrap()
                .count(),
            1
        );
        assert_eq!(
            fs::read_dir(directory.path().join("data")).unwrap().count(),
            1
        );
    }

    #[test]
    fn corrupt_settings_are_backed_up_without_losing_progress() {
        let directory = tempdir().unwrap();
        let paths = paths(directory.path());
        let corrupt = "(this is not RON";
        put(&paths.settings, corrupt);
        write_document(
            &paths.progress,
            &ProgressFile {
                version: VERSION,
                bests: progress_record("level-2", 9.0).bests,
            },
        )
        .unwrap();
        let (settings, progress, mut store) = Persistence::load(Some(paths.clone()));
        assert_eq!(settings, Settings::default());
        assert_eq!(progress.bests.get(&LevelId("level-2".into())), Some(&9.0));
        store.flush(&settings, &progress);
        let backup = fs::read_dir(paths.settings.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| {
                path.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .contains(".invalid-")
            })
            .unwrap();
        assert_eq!(fs::read_to_string(backup).unwrap(), corrupt);
        let _: SettingsFile = from_bytes(&fs::read(paths.settings).unwrap()).unwrap();
    }

    #[test]
    fn corrupt_progress_recovers_once_without_resetting_settings() {
        let directory = tempdir().unwrap();
        let paths = paths(directory.path());
        put(
            &paths.settings,
            "(version:1,audio:(volume:0.7,muted:false))",
        );
        put(&paths.progress, "broken progress");
        let (settings, progress, mut store) = Persistence::load(Some(paths.clone()));
        assert_eq!((settings.volume(), settings.muted()), (0.7, false));
        store.flush(&settings, &progress);
        let (_, restored, store) = Persistence::load(Some(paths.clone()));
        assert_eq!(restored, Progress::default());
        assert!(store.diagnostics.is_empty());
        assert_eq!(
            fs::read_dir(paths.progress.parent().unwrap())
                .unwrap()
                .count(),
            2
        );
    }

    #[test]
    fn invalid_volume_and_scores_do_not_discard_other_valid_values() {
        let directory = tempdir().unwrap();
        let paths = paths(directory.path());
        put(&paths.settings, "(audio:(volume:NaN,muted:false))");
        put(
            &paths.progress,
            r#"(version:1,bests:{("good"):10.0,("negative"):-2.0,("nan"):NaN})"#,
        );
        let (settings, progress, _) = Persistence::load(Some(paths));
        assert_eq!((settings.volume(), settings.muted()), (0.25, false));
        assert_eq!(progress, progress_record("good", 10.0));
    }

    #[test]
    fn partial_settings_receive_defaults_and_newer_schemas_are_never_overwritten() {
        let directory = tempdir().unwrap();
        let paths = paths(directory.path());
        put(&paths.settings, "(audio:(muted:false))");
        let (settings, _, _) = Persistence::load(Some(paths.clone()));
        assert_eq!((settings.volume(), settings.muted()), (0.25, false));
        let future_settings = "(version:99,audio:(volume:0.9,muted:false))";
        let future_progress = "(version:99,bests:{})";
        put(&paths.settings, future_settings);
        put(&paths.progress, future_progress);
        let (mut settings, mut progress, mut store) = Persistence::load(Some(paths.clone()));
        settings.toggle_mute();
        progress.bests.insert(LevelId("tutorial".into()), 1.0);
        store.flush(&settings, &progress);
        assert!(store.settings_error.is_some() && store.progress_error.is_some());
        assert_eq!(fs::read_to_string(paths.settings).unwrap(), future_settings);
        assert_eq!(fs::read_to_string(paths.progress).unwrap(), future_progress);
    }

    #[test]
    fn persistence_system_saves_resource_changes_without_rewriting_unchanged_files() {
        let directory = tempdir().unwrap();
        let paths = paths(directory.path());
        let (settings, progress, store) = Persistence::load(Some(paths.clone()));
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_message::<AppExit>()
            .insert_resource(settings)
            .insert_resource(progress)
            .insert_resource(store)
            .add_plugins(SettingsPlugin);
        app.update();
        let original = fs::read(&paths.settings).unwrap();
        app.world_mut().resource_mut::<Settings>().toggle_mute();
        app.update();
        assert_ne!(fs::read(&paths.settings).unwrap(), original);
        let formatted = String::from_utf8(fs::read(&paths.settings).unwrap()).unwrap()
            + "// preserve manual comments when nothing changes\n";
        fs::write(&paths.settings, &formatted).unwrap();
        app.update();
        assert_eq!(fs::read_to_string(paths.settings).unwrap(), formatted);
        assert!(!paths.progress.exists());
    }

    #[test]
    fn exit_retries_a_failed_save_without_requiring_another_settings_change() {
        let directory = tempdir().unwrap();
        let paths = paths(directory.path());
        fs::write(directory.path().join("config"), "blocked").unwrap();
        let (settings, progress, store) = Persistence::load(Some(paths.clone()));
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_message::<AppExit>()
            .insert_resource(settings)
            .insert_resource(progress)
            .insert_resource(store)
            .add_plugins(SettingsPlugin);
        app.update();
        assert!(
            app.world()
                .resource::<Persistence>()
                .settings_error
                .is_some()
        );
        fs::remove_file(directory.path().join("config")).unwrap();
        app.world_mut().write_message(AppExit::Success);
        app.update();
        assert!(paths.settings.is_file());
        assert!(
            app.world()
                .resource::<Persistence>()
                .settings_error
                .is_none()
        );
    }
}
