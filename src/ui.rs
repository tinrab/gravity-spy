use bevy::{prelude::*, text::FontSmoothing};
use bevy_rapier2d::render::DebugRenderContext;

use crate::{
    audio::Sound,
    game::{Run, Status},
    level::Level,
    rendering::{DebugView, FONT},
    session::{Catalog, MainMenu, MenuItem},
    settings::{Persistence, Progress, Settings},
};

#[derive(Component)]
enum UiText {
    Hud,
    Footer,
    Heading,
    Description,
}

#[derive(Component)]
struct Overlay;

pub struct InterfacePlugin;

impl Plugin for InterfacePlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, setup_ui)
            .add_systems(Update, (interface_keys, save_best, update_ui).chain());
    }
}

fn font(handle: &Handle<Font>, size: f32) -> TextFont {
    TextFont::from(handle.clone())
        .with_font_size(size)
        .with_font_smoothing(FontSmoothing::None)
}

/// Spawns empty text; `update_ui` fills it in every frame.
fn setup_ui(mut commands: Commands, assets: Res<AssetServer>) {
    let face = assets.load(FONT);
    commands.spawn((
        UiText::Hud,
        Text::default(),
        font(&face, 16.0),
        TextColor(Color::WHITE),
        Node {
            position_type: PositionType::Absolute,
            top: px(14),
            left: px(14),
            ..default()
        },
    ));
    commands.spawn((
        UiText::Footer,
        Text::default(),
        GlobalZIndex(1),
        font(&face, 9.0),
        TextColor(Color::srgba(0.85, 0.93, 0.75, 0.75)),
        Node {
            position_type: PositionType::Absolute,
            bottom: px(18),
            left: px(18),
            ..default()
        },
    ));
    commands
        .spawn((
            Overlay,
            Node {
                position_type: PositionType::Absolute,
                width: percent(100),
                height: percent(100),
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::Center,
                justify_content: JustifyContent::Center,
                row_gap: px(36),
                ..default()
            },
            BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.92)),
        ))
        .with_children(|parent| {
            parent.spawn((
                UiText::Heading,
                Text::default(),
                font(&face, 32.0),
                TextColor(Color::WHITE),
            ));
            parent.spawn((
                UiText::Description,
                Text::default(),
                Node {
                    max_width: percent(90),
                    ..default()
                },
                font(&face, 14.0),
                TextColor(Color::srgb(0.88, 0.95, 0.80)),
                TextLayout::justify(Justify::Center),
            ));
        });
}

#[expect(
    clippy::too_many_arguments,
    reason = "The interface reflects run, level, menu, and save state together"
)]
fn update_ui(
    run: Res<Run>,
    level: Res<Level>,
    catalog: Res<Catalog>,
    progress: Res<Progress>,
    persistence: Res<Persistence>,
    menu: Res<MainMenu>,
    mut texts: Query<(&UiText, &mut Text, &mut Node), Without<Overlay>>,
    mut overlays: Query<(&mut Node, &mut BackgroundColor), With<Overlay>>,
) {
    for (mut node, mut background) in &mut overlays {
        node.display = if run.status == Status::Playing && catalog.diagnostics.is_none() {
            Display::None
        } else {
            Display::Flex
        };
        // The welcome scene shows through untouched; the main menu dims it.
        let alpha = match run.status {
            Status::Welcome => 0.0,
            Status::MainMenu => 0.45,
            _ => 0.65,
        };
        background.0 = Color::srgba(0.0, 0.0, 0.0, alpha);
    }
    let preview = if catalog.preview.is_some() {
        "   PREVIEW  F5: RELOAD"
    } else {
        ""
    };
    let in_menu = matches!(
        run.status,
        Status::Welcome | Status::MainMenu | Status::LevelSelect
    );
    let timer = if in_menu {
        String::new()
    } else {
        format!("{}  {:.1}s{preview}", level.name, run.elapsed)
    };
    let (heading, body) = overlay_text(&run, &level, &catalog, &progress, &menu);
    let footer = footer_text(&catalog, &persistence);
    for (role, mut text, mut node) in &mut texts {
        if matches!(role, UiText::Heading) {
            node.display = if heading.is_empty() {
                Display::None
            } else {
                Display::Flex
            };
        }
        let value = match role {
            UiText::Hud => &timer,
            UiText::Footer => footer,
            UiText::Heading => heading,
            UiText::Description => &body,
        };
        if text.0 != value {
            text.0.clear();
            text.0.push_str(value);
        }
    }
}

fn overlay_text(
    run: &Run,
    level: &Level,
    catalog: &Catalog,
    progress: &Progress,
    menu: &MainMenu,
) -> (&'static str, String) {
    if let Some(error) = &catalog.diagnostics {
        let state = if run.attempts == 0 {
            "No valid preview has loaded yet."
        } else {
            "The last valid level is retained."
        };
        return (
            "SVG ERROR",
            format!("{error}\n\nFix the SVG, then press F5 to reload.\n{state}"),
        );
    }
    let best = run.best.map_or_else(
        || "Not yet completed.".into(),
        |best| format!("Best time: {best:.2}s"),
    );
    match run.status {
        Status::Welcome => ("", "Press 'SPACE' to start.".into()),
        Status::MainMenu => {
            let items: Vec<_> = MenuItem::ALL
                .iter()
                .enumerate()
                .map(|(index, item)| {
                    if index == menu.selected {
                        format!("> {} <", item.label())
                    } else {
                        item.label().into()
                    }
                })
                .collect();
            // The lit title scene behind the menu serves as its heading.
            (
                "",
                format!(
                    "\n\n\n\n{}\n\n\nA/D or arrows to move. SPACE to jump.",
                    items.join("\n\n")
                ),
            )
        }
        Status::LevelSelect => {
            let rows: Vec<_> = catalog
                .entries
                .iter()
                .enumerate()
                .map(|(index, entry)| {
                    let mark = if index == catalog.selected { ">" } else { " " };
                    let best = progress
                        .bests
                        .get(&entry.level.id)
                        .map_or_else(|| "Not yet played".into(), |t| format!("{t:.2}s"));
                    format!("{mark} {}   {best}", entry.level.name)
                })
                .collect();
            (
                "SELECT LEVEL",
                format!(
                    "{}\n\nARROWS: SELECT    SPACE: START    ESC: MENU",
                    rows.join("\n\n")
                ),
            )
        }
        Status::Paused => (
            "PAUSED",
            "Press P to resume.\n\nR restarts the level.".into(),
        ),
        Status::Dead => (
            "DETECTED",
            "Press R to restart.\n\nUse terrain as cover.\nGreen is safe. Yellow warns. Red detects."
                .into(),
        ),
        Status::Complete => {
            let has_next = catalog
                .entries
                .iter()
                .position(|entry| entry.level.id == level.id)
                .is_some_and(|index| index + 1 < catalog.entries.len());
            let action = if catalog.preview.is_some() {
                "SPACE/R: RETRY    F5: RELOAD"
            } else if has_next {
                "SPACE: NEXT LEVEL    R: RETRY    TAB: SELECT"
            } else {
                "SPACE: LEVELS    R: RETRY    TAB: SELECT"
            };
            (
                "BASE INFILTRATED",
                format!("Time: {:.2}s\n\n{best}\n\n{action}", run.elapsed),
            )
        }
        Status::Playing => ("", String::new()),
    }
}

fn footer_text(catalog: &Catalog, persistence: &Persistence) -> &'static str {
    if persistence.settings_error.is_some() || persistence.progress_error.is_some() {
        "Settings or progress could not be saved. Changes apply to this session."
    } else if catalog.preview.is_some() {
        "A/D OR ARROWS: MOVE   SPACE: JUMP   P: PAUSE   R: RETRY   F5: RELOAD   F1: GUIDES   ESC: QUIT"
    } else {
        "A/D OR ARROWS: MOVE   SPACE: JUMP   P: PAUSE   R: RETRY   TAB: LEVELS   ESC: MENU   M: MUTE   +/-: VOLUME"
    }
}

fn interface_keys(
    keys: Res<ButtonInput<KeyCode>>,
    mut settings: ResMut<Settings>,
    mut debug: ResMut<DebugView>,
    mut rapier_debug: ResMut<DebugRenderContext>,
    mut sounds: MessageWriter<Sound>,
    #[cfg(not(target_arch = "wasm32"))] catalog: Res<Catalog>,
    #[cfg(not(target_arch = "wasm32"))] mut exit: MessageWriter<AppExit>,
) {
    // Elsewhere Esc returns to the main menu; a preview has no menu to return to.
    #[cfg(not(target_arch = "wasm32"))]
    if keys.just_pressed(KeyCode::Escape) && catalog.preview.is_some() {
        exit.write(AppExit::Success);
    }
    if keys.just_pressed(KeyCode::F1) {
        debug.0 = !debug.0;
        rapier_debug.enabled = debug.0;
    }
    if keys.just_pressed(KeyCode::KeyM) {
        settings.toggle_mute();
        sounds.write(Sound::Select);
    }
    if keys.any_just_pressed([KeyCode::Equal, KeyCode::NumpadAdd]) {
        settings.adjust_volume(0.05);
        sounds.write(Sound::Select);
    }
    if keys.any_just_pressed([KeyCode::Minus, KeyCode::NumpadSubtract]) {
        settings.adjust_volume(-0.05);
        sounds.write(Sound::Select);
    }
}

fn save_best(
    run: Res<Run>,
    level: Res<Level>,
    catalog: Res<Catalog>,
    mut progress: ResMut<Progress>,
) {
    if run.status == Status::Complete
        && catalog.preview.is_none()
        && progress.bests.get(&level.id).copied() != run.best
        && let Some(best) = run.best
    {
        progress.bests.insert(level.id.clone(), best);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejected_reload_replaces_pause_text_with_diagnostics() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .insert_resource(Level::tutorial())
            .insert_resource(Run {
                status: Status::Paused,
                ..default()
            })
            .insert_resource(Catalog {
                entries: Vec::new(),
                selected: 0,
                preview: Some("preview.svg".into()),
                diagnostics: None,
            })
            .insert_resource(Progress::default())
            .insert_resource(Persistence::load(None).2)
            .init_resource::<MainMenu>()
            .add_systems(Update, update_ui);
        let heading = app.world_mut().spawn((UiText::Heading, Text::new(""))).id();
        app.update();
        assert_eq!(app.world().get::<Text>(heading).unwrap().0, "PAUSED");
        app.world_mut().resource_mut::<Catalog>().diagnostics =
            Some("preview.svg: terrain-bad: invalid geometry".into());
        app.update();
        assert_eq!(app.world().get::<Text>(heading).unwrap().0, "SVG ERROR");
    }
}
