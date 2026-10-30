use std::{collections::HashSet, path::PathBuf};
#[cfg(not(target_arch = "wasm32"))]
use std::{fs, path::Path};

use bevy::{input::InputSystems, prelude::*};

use crate::{
    audio::Sound,
    game::{Run, Status, read_controls},
    level::{Level, LevelId},
    settings::Progress,
};

#[derive(Debug)]
pub struct Entry {
    pub path: PathBuf,
    pub level: Level,
}

#[derive(Resource, Debug)]
pub struct Catalog {
    pub entries: Vec<Entry>,
    pub selected: usize,
    pub preview: Option<PathBuf>,
    pub diagnostics: Option<String>,
}

impl Catalog {
    /// A catalog for previewing a single SVG outside the campaign.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn empty() -> Self {
        Self {
            entries: Vec::new(),
            selected: 0,
            preview: None,
            diagnostics: None,
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn discover(directory: &Path) -> Result<Self, String> {
        let mut entries = Vec::new();
        for path in fs::read_dir(directory).map_err(|e| e.to_string())? {
            let path = path.map_err(|e| e.to_string())?.path();
            if path.extension().is_some_and(|e| e == "svg") {
                entries.push(Entry {
                    level: Level::load(&path)?,
                    path,
                });
            }
        }
        Self::from_entries(entries)
    }

    #[cfg(any(target_arch = "wasm32", test))]
    pub fn bundled() -> Result<Self, String> {
        let sources: &[(&str, &str)] = include!(concat!(env!("OUT_DIR"), "/bundled_levels.rs"));
        let entries = sources
            .iter()
            .map(|(name, source)| {
                Ok(Entry {
                    path: PathBuf::from(name),
                    level: Level::parse(source).map_err(|error| format!("{name}: {error}"))?,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        Self::from_entries(entries)
    }

    fn from_entries(mut entries: Vec<Entry>) -> Result<Self, String> {
        entries.sort_by(|a, b| (a.level.order, &a.level.id).cmp(&(b.level.order, &b.level.id)));
        if entries.is_empty() {
            return Err("no playable SVG levels".into());
        }
        let mut ids = HashSet::new();
        for entry in &entries {
            if !ids.insert(&entry.level.id) {
                return Err(format!(
                    "{}: duplicate level id {}",
                    entry.path.display(),
                    entry.level.id.0
                ));
            }
        }
        Ok(Self {
            entries,
            selected: 0,
            preview: None,
            diagnostics: None,
        })
    }

    pub fn select_id(&mut self, id: &LevelId) {
        if let Some(index) = self.entries.iter().position(|entry| entry.level.id == *id) {
            self.selected = index;
        }
    }
}

/// The welcome and main menu backdrop: a lit title rendered with the level renderer.
#[derive(Resource)]
pub struct TitleScene(pub Level);

impl Default for TitleScene {
    fn default() -> Self {
        Self(
            Level::parse(include_str!("../assets/title.svg"))
                .expect("bundled title scene is valid"),
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuItem {
    Start,
    Tutorial,
    #[cfg(not(target_arch = "wasm32"))]
    Quit,
}

impl MenuItem {
    pub const ALL: &[Self] = &[
        Self::Start,
        Self::Tutorial,
        #[cfg(not(target_arch = "wasm32"))]
        Self::Quit,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Start => "START",
            Self::Tutorial => "TUTORIAL",
            #[cfg(not(target_arch = "wasm32"))]
            Self::Quit => "QUIT",
        }
    }
}

#[derive(Resource, Default)]
pub struct MainMenu {
    pub selected: usize,
}

pub struct SessionPlugin;

impl Plugin for SessionPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<Sound>()
            .init_resource::<TitleScene>()
            .init_resource::<MainMenu>()
            .add_systems(
                PreUpdate,
                navigate.after(InputSystems).before(read_controls),
            );
    }
}

fn activate(level: Level, current: &mut Level, run: &mut Run, progress: &Progress) {
    run.best = progress.bests.get(&level.id).copied();
    *current = level;
    run.ready = false;
    run.restart();
    info!("Loaded level {}", current.id.0);
}

fn go_to_main_menu(title: &TitleScene, level: &mut Level, run: &mut Run, menu: &mut MainMenu) {
    run.status = Status::MainMenu;
    menu.selected = 0;
    run.elapsed = 0.0;
    run.best = None;
    if level.id != title.0.id {
        *level = title.0.clone();
        run.ready = false;
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "Menus switch levels, runs, and selections together"
)]
fn navigate(
    keys: Res<ButtonInput<KeyCode>>,
    mut catalog: ResMut<Catalog>,
    mut menu: ResMut<MainMenu>,
    title: Res<TitleScene>,
    mut level: ResMut<Level>,
    mut run: ResMut<Run>,
    progress: Res<Progress>,
    mut sounds: MessageWriter<Sound>,
    #[cfg(not(target_arch = "wasm32"))] mut exit: MessageWriter<AppExit>,
) {
    let start = keys.any_just_pressed([KeyCode::Space, KeyCode::Enter]);
    if let Some(path) = catalog.preview.clone() {
        if keys.just_pressed(KeyCode::F5) {
            match Level::load(&path) {
                Ok(next) => {
                    activate(next, &mut level, &mut run, &progress);
                    catalog.diagnostics = None;
                    info!("Preview reloaded: {}", path.display());
                }
                Err(error) => {
                    warn!("Preview reload rejected: {error}");
                    catalog.diagnostics = Some(error);
                }
            }
        }
        return;
    }
    let in_menu = matches!(run.status, Status::Welcome | Status::MainMenu);
    if keys.just_pressed(KeyCode::Escape) && !in_menu {
        go_to_main_menu(&title, &mut level, &mut run, &mut menu);
        sounds.write(Sound::Select);
        return;
    }
    if keys.just_pressed(KeyCode::Tab) && run.status != Status::Welcome {
        run.status = Status::LevelSelect;
        catalog.select_id(&level.id);
        sounds.write(Sound::Select);
        return;
    }
    match run.status {
        Status::Welcome if start => {
            run.status = Status::MainMenu;
            menu.selected = 0;
            sounds.write(Sound::Select);
        }
        Status::MainMenu => {
            let count = MenuItem::ALL.len();
            if keys.any_just_pressed([KeyCode::ArrowDown, KeyCode::KeyS]) {
                menu.selected = (menu.selected + 1) % count;
                sounds.write(Sound::Select);
            }
            if keys.any_just_pressed([KeyCode::ArrowUp, KeyCode::KeyW]) {
                menu.selected = (menu.selected + count - 1) % count;
                sounds.write(Sound::Select);
            }
            if !start {
                return;
            }
            sounds.write(Sound::Select);
            match MenuItem::ALL[menu.selected] {
                MenuItem::Start => run.status = Status::LevelSelect,
                MenuItem::Tutorial => {
                    catalog.selected = catalog
                        .entries
                        .iter()
                        .position(|entry| entry.level.id.0 == "tutorial")
                        .unwrap_or(0);
                    activate(
                        catalog.entries[catalog.selected].level.clone(),
                        &mut level,
                        &mut run,
                        &progress,
                    );
                }
                #[cfg(not(target_arch = "wasm32"))]
                MenuItem::Quit => {
                    exit.write(AppExit::Success);
                }
            }
        }
        Status::LevelSelect => {
            let count = catalog.entries.len();
            if keys.any_just_pressed([KeyCode::ArrowRight, KeyCode::ArrowDown, KeyCode::KeyD]) {
                catalog.selected = (catalog.selected + 1) % count;
                sounds.write(Sound::Select);
            }
            if keys.any_just_pressed([KeyCode::ArrowLeft, KeyCode::ArrowUp, KeyCode::KeyA]) {
                catalog.selected = (catalog.selected + count - 1) % count;
                sounds.write(Sound::Select);
            }
            if start {
                sounds.write(Sound::Select);
                activate(
                    catalog.entries[catalog.selected].level.clone(),
                    &mut level,
                    &mut run,
                    &progress,
                );
            }
        }
        Status::Complete if start => {
            sounds.write(Sound::Select);
            catalog.select_id(&level.id);
            if catalog.selected + 1 < catalog.entries.len() {
                catalog.selected += 1;
                activate(
                    catalog.entries[catalog.selected].level.clone(),
                    &mut level,
                    &mut run,
                    &progress,
                );
            } else {
                run.status = Status::LevelSelect;
            }
        }
        _ => {}
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    #[test]
    fn browser_bundle_matches_the_desktop_campaign() {
        let desktop =
            Catalog::discover(&PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/levels"))
                .unwrap();
        let browser = Catalog::bundled().unwrap();
        assert_eq!(browser.entries.len(), desktop.entries.len());
        for (browser, desktop) in browser.entries.iter().zip(&desktop.entries) {
            assert_eq!(browser.level.id, desktop.level.id);
            assert_eq!(browser.level.name, desktop.level.name);
            assert_eq!(browser.level.surfaces.len(), desktop.level.surfaces.len());
        }
    }
}
