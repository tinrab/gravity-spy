//! Gravity Spy, a reconstruction of a platformer with gravity toward nearby terrain.

mod audio;
mod game;
mod geometry;
mod level;
mod rendering;
mod session;
mod settings;
mod svg;
mod svg_document;
mod ui;

#[cfg(not(target_arch = "wasm32"))]
use std::{
    env,
    error::Error,
    path::{Path, PathBuf},
};

#[cfg(not(target_arch = "wasm32"))]
use bevy::render::view::screenshot::{Screenshot, ScreenshotCaptured, save_to_disk};
use bevy::{asset::AssetMetaCheck, prelude::*, window::PresentMode};
#[cfg(target_arch = "wasm32")]
use wasm_bindgen::{JsValue, prelude::wasm_bindgen};

use audio::SoundPlugin;
#[cfg(not(target_arch = "wasm32"))]
use game::Status;
use game::{Run, SimulationPlugin};
use level::Level;
#[cfg(not(target_arch = "wasm32"))]
use level::LevelId;
use rendering::PresentationPlugin;
use session::{Catalog, SessionPlugin, TitleScene};
use settings::{Persistence, Progress, Settings, SettingsPlugin, StoragePaths};
use ui::InterfacePlugin;

#[cfg(not(target_arch = "wasm32"))]
const HELP: &str = "Gravity Spy
  --validate-level PATH.svg  Validate without a window or audio
  --edit PATH.svg            Preview; F5 reloads
  --level ID                 Select a campaign level
  --autostart                Start selected level directly
  --paths                    Show settings and progress paths
  --screenshot PATH          Capture after 120 frames
";

#[cfg(not(target_arch = "wasm32"))]
#[derive(Default)]
struct Options {
    validate: Option<PathBuf>,
    edit: Option<PathBuf>,
    level: Option<LevelId>,
    screenshot: Option<String>,
    autostart: bool,
    paths: bool,
    help: bool,
}

#[cfg(not(target_arch = "wasm32"))]
impl Options {
    fn parse(arguments: impl IntoIterator<Item = String>) -> Result<Self, String> {
        let mut options = Self::default();
        let mut arguments = arguments.into_iter();
        while let Some(argument) = arguments.next() {
            let mut value = || {
                arguments
                    .next()
                    .ok_or_else(|| format!("{argument} needs a value"))
            };
            match argument.as_str() {
                "--validate-level" => options.validate = Some(value()?.into()),
                "--edit" => options.edit = Some(value()?.into()),
                "--level" => options.level = Some(LevelId(value()?)),
                "--screenshot" => options.screenshot = Some(value()?),
                "--autostart" => options.autostart = true,
                "--paths" => options.paths = true,
                "--help" | "-h" => options.help = true,
                other => return Err(format!("unknown argument {other}; use --help")),
            }
        }
        Ok(options)
    }
}

/// Opens the game window and runs the application.
///
/// # Errors
/// Returns an error for invalid arguments, missing assets, or invalid campaign levels.
#[cfg(not(target_arch = "wasm32"))]
pub fn run() -> Result<(), Box<dyn Error>> {
    let options = Options::parse(env::args().skip(1))?;
    if options.help {
        print!("{HELP}");
        return Ok(());
    }
    if options.paths {
        let paths = StoragePaths::discover()
            .ok_or("no valid per-user storage directories are available")?;
        println!("Settings: {}", paths.settings.display());
        println!("Progress: {}", paths.progress.display());
        return Ok(());
    }
    if let Some(path) = &options.validate {
        print_validation(path, &Level::load(path)?);
        return Ok(());
    }

    // Prefer assets packaged next to the executable, then the source tree.
    let assets = env::current_exe()?
        .parent()
        .map(|parent| parent.join("assets"))
        .filter(|path| path.join("levels/tutorial.svg").exists())
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets"));
    // A broken campaign SVG must not prevent opening that file for repair in preview.
    let mut catalog = if options.edit.is_some() {
        Catalog::empty()
    } else {
        Catalog::discover(&assets.join("levels"))?
    };
    if let Some(id) = &options.level {
        if !catalog.entries.iter().any(|entry| entry.level.id == *id) {
            return Err(format!("unknown level {}", id.0).into());
        }
        catalog.select_id(id);
    }
    let (settings, progress, persistence) = Persistence::load(StoragePaths::discover());
    let title = TitleScene::default();
    let mut level = if options.edit.is_some() {
        Level::parse(include_str!("../assets/levels/tutorial.svg"))?
    } else if options.autostart {
        catalog.entries[catalog.selected].level.clone()
    } else {
        title.0.clone()
    };
    let mut run = Run {
        best: progress.bests.get(&level.id).copied(),
        ..default()
    };
    if let Some(path) = options.edit {
        match Level::load(&path) {
            Ok(preview) => {
                level = preview;
                run.best = None;
                run.restart();
            }
            Err(error) => {
                catalog.diagnostics = Some(error);
                run.status = Status::Paused;
            }
        }
        catalog.preview = Some(path);
    } else if options.autostart {
        run.restart();
    }
    let mut app = application(
        level,
        title,
        catalog,
        run,
        assets.to_string_lossy().into_owned(),
        Window::default(),
        (settings, progress, persistence),
    );
    if let Some(path) = options.screenshot {
        app.insert_resource(Capture { path, frames: 0 })
            .add_systems(Update, capture_frame);
    }
    app.run();
    Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
fn print_validation(path: &Path, level: &Level) {
    println!("{}: valid [{}] {}", path.display(), level.id.0, level.name);
    println!(
        "{} terrain regions, {} boundary segments, {} crates, {} agents, {} patrols, {} lights, {} hints",
        level.terrain.len(),
        level.surfaces.len(),
        level.cubes.len(),
        level.sensors.len(),
        level.sensors.iter().filter(|s| s.patrol.is_some()).count(),
        level.lights.len(),
        level.hints.len()
    );
    for reference in &level.references {
        println!(
            "Reference: {} {:.2}–{:.2}s {}",
            reference.source, reference.start, reference.end, reference.notes
        );
    }
    // Report SVG coordinates, where Y points down.
    for sensor in &level.sensors {
        println!(
            "Agent: {} at ({:.2}, {:.2}) body/patrol clearance {:.2}",
            sensor.id,
            sensor.origin.x,
            -sensor.origin.y,
            sensor.terrain_clearance(&level.terrain, &level.surfaces)
        );
    }
}

/// Starts the browser game after the player presses Play.
///
/// # Errors
/// Returns an error if a bundled campaign level is invalid.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
pub fn start_game() -> Result<(), JsValue> {
    let catalog = Catalog::bundled().map_err(|error| JsValue::from_str(&error))?;
    let title = TitleScene::default();
    let level = title.0.clone();
    let saved = Persistence::load(StoragePaths::discover());
    let window = Window {
        canvas: Some("#game-canvas".into()),
        prevent_default_event_handling: true,
        ..default()
    };
    application(
        level,
        title,
        catalog,
        Run::default(),
        "assets".into(),
        window,
        saved,
    )
    .run();
    Ok(())
}

fn application(
    level: Level,
    title: TitleScene,
    catalog: Catalog,
    run: Run,
    assets: String,
    mut window: Window,
    (settings, progress, persistence): (Settings, Progress, Persistence),
) -> App {
    window.title = "Gravity Spy".into();
    window.resolution = (1280, 720).into();
    window.present_mode = PresentMode::AutoVsync;
    let mut app = App::new();
    app.add_plugins(
        DefaultPlugins
            .set(AssetPlugin {
                file_path: assets,
                // Static hosts answer missing .meta files with HTML error pages.
                meta_check: if cfg!(target_arch = "wasm32") {
                    AssetMetaCheck::Never
                } else {
                    default()
                },
                ..default()
            })
            .set(WindowPlugin {
                primary_window: Some(window),
                ..default()
            }),
    )
    .insert_resource(ClearColor(Color::srgb(0.025, 0.055, 0.012)))
    .insert_resource(level)
    .insert_resource(title)
    .insert_resource(catalog)
    .insert_resource(settings)
    .insert_resource(progress)
    .insert_resource(persistence)
    .insert_resource(run)
    .add_plugins((
        SimulationPlugin,
        SessionPlugin,
        PresentationPlugin,
        InterfacePlugin,
        SettingsPlugin,
        SoundPlugin,
    ));
    app
}

#[cfg(not(target_arch = "wasm32"))]
#[derive(Resource)]
struct Capture {
    path: String,
    frames: u32,
}

#[cfg(not(target_arch = "wasm32"))]
fn capture_frame(mut commands: Commands, mut capture: ResMut<Capture>) {
    capture.frames += 1;
    if capture.frames == 120 {
        commands
            .spawn(Screenshot::primary_window())
            .observe(save_to_disk(capture.path.clone()))
            .observe(
                |_: On<ScreenshotCaptured>, mut exit: MessageWriter<AppExit>| {
                    exit.write(AppExit::Success);
                },
            );
    }
}
