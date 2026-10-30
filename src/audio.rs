use std::collections::HashMap;

use bevy::{
    audio::{AudioSinkPlayback, Volume},
    prelude::*,
};

use crate::{
    game::{Run, Status},
    settings::Settings,
};

/// A one-shot sound effect requested by gameplay or menus.
#[derive(Message, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Sound {
    Jump,
    Land,
    Bump,
    Death,
    Complete,
    Select,
}

impl Sound {
    const ALL: [Self; 6] = [
        Self::Jump,
        Self::Land,
        Self::Bump,
        Self::Death,
        Self::Complete,
        Self::Select,
    ];

    fn path(self) -> &'static str {
        match self {
            Self::Jump => "audio/jump.wav",
            Self::Land => "audio/land-on-ground.wav",
            Self::Bump => "audio/pickup.wav",
            Self::Death => "audio/death.wav",
            Self::Complete => "audio/level-complete.wav",
            Self::Select => "audio/ui-select.wav",
        }
    }

    /// Relative loudness against the music volume setting.
    fn gain(self) -> f32 {
        match self {
            Self::Land | Self::Bump => 0.5,
            Self::Jump | Self::Select => 0.7,
            Self::Death | Self::Complete => 1.0,
        }
    }
}

#[derive(Resource)]
struct Sounds(HashMap<Sound, Handle<AudioSource>>);

#[derive(Component)]
struct Music;

pub struct SoundPlugin;

impl Plugin for SoundPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<Sound>()
            .add_systems(Startup, load_audio)
            .add_systems(
                Update,
                (
                    apply_audio_settings,
                    play_music_in_menus,
                    play_sounds,
                    report_audio_ready,
                )
                    .chain(),
            );
    }
}

fn load_audio(mut commands: Commands, assets: Res<AssetServer>, settings: Res<Settings>) {
    commands.insert_resource(Sounds(
        Sound::ALL
            .into_iter()
            .map(|sound| (sound, assets.load(sound.path())))
            .collect(),
    ));
    commands.spawn((
        Music,
        AudioPlayer::new(assets.load("audio/main-theme.mp3")),
        PlaybackSettings {
            volume: Volume::Linear(settings.effective_volume()),
            ..PlaybackSettings::LOOP
        },
    ));
}

fn apply_audio_settings(settings: Res<Settings>, mut sinks: Query<&mut AudioSink, With<Music>>) {
    for mut sink in &mut sinks {
        if settings.is_changed() || sink.is_added() {
            sink.set_volume(Volume::Linear(settings.effective_volume()));
            info!(
                "Audio settings applied: volume={:.2}, muted={}",
                settings.volume(),
                settings.muted()
            );
        }
    }
}

/// Music plays on the welcome, main menu, and level selection screens and pauses everywhere else.
fn play_music_in_menus(run: Res<Run>, sinks: Query<&AudioSink, With<Music>>) {
    let in_menu = matches!(
        run.status,
        Status::Welcome | Status::MainMenu | Status::LevelSelect
    );
    for sink in &sinks {
        if in_menu && sink.is_paused() {
            sink.play();
        } else if !in_menu && !sink.is_paused() {
            sink.pause();
        }
    }
}

fn play_sounds(
    mut commands: Commands,
    mut requests: MessageReader<Sound>,
    sounds: Res<Sounds>,
    settings: Res<Settings>,
) {
    let mut played = Vec::new();
    for &sound in requests.read() {
        // Several fixed ticks can share a frame; one copy of each cue is enough.
        if played.contains(&sound) || settings.effective_volume() <= 0.0 {
            continue;
        }
        played.push(sound);
        commands.spawn((
            AudioPlayer::new(sounds.0[&sound].clone()),
            PlaybackSettings {
                volume: Volume::Linear(settings.effective_volume() * sound.gain()),
                ..PlaybackSettings::DESPAWN
            },
        ));
    }
}

fn report_audio_ready(sinks: Query<Entity, (Added<AudioSink>, With<Music>)>) {
    if !sinks.is_empty() {
        info!("Music playback ready");
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use std::{fs, path::Path, sync::Arc};

    use bevy::audio::Decodable;

    use super::*;

    #[test]
    fn every_sound_effect_decodes() {
        let assets = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets");
        for sound in Sound::ALL {
            let source = AudioSource {
                bytes: Arc::from(fs::read(assets.join(sound.path())).unwrap()),
            };
            assert!(source.decoder().count() > 0, "{sound:?} has no samples");
        }
    }
}
