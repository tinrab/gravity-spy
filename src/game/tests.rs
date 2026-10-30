use std::{
    f32::consts::FRAC_PI_2,
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use bevy::time::TimeUpdateStrategy;

use super::*;
use crate::{
    level::{AGENT_PERIOD, LevelId},
    session::{Catalog, Entry, SessionPlugin, TitleScene},
    settings::Progress,
};

fn level(name: &str) -> Level {
    Level::load(Path::new(&format!("assets/levels/{name}.svg"))).unwrap()
}

fn app(level: Level) -> App {
    let mut app = App::new();
    app.add_plugins((MinimalPlugins, TransformPlugin))
        .init_resource::<ButtonInput<KeyCode>>()
        .insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs_f64(
            1.0 / 60.0,
        )))
        .insert_resource(level)
        .add_plugins(SimulationPlugin);
    tick(&mut app, &[]);
    tick(&mut app, &[]);
    app
}

/// Holds exactly `held` for one frame; keys missing from it are released.
fn tick(app: &mut App, held: &[KeyCode]) {
    let mut keys = app.world_mut().resource_mut::<ButtonInput<KeyCode>>();
    let previous: Vec<_> = keys.get_pressed().copied().collect();
    for key in previous {
        if !held.contains(&key) {
            keys.release(key);
        }
    }
    for &key in held {
        keys.press(key);
    }
    app.update();
    app.world_mut()
        .resource_mut::<ButtonInput<KeyCode>>()
        .clear();
}

fn status(app: &App) -> Status {
    app.world().resource::<Run>().status
}

fn position(app: &mut App) -> Vec2 {
    let world = app.world_mut();
    world
        .query_filtered::<&Transform, With<Spy>>()
        .single(world)
        .unwrap()
        .translation
        .truncate()
}

fn with_context<R>(app: &mut App, f: impl FnOnce(&RapierContext) -> R) -> R {
    let world = app.world_mut();
    let (simulation, colliders, joints, rigidbody_set) = world
        .query::<(
            &RapierContextSimulation,
            &RapierContextColliders,
            &RapierContextJoints,
            &RapierRigidBodySet,
        )>()
        .single(world)
        .unwrap();
    f(&RapierContext {
        simulation,
        colliders,
        joints,
        rigidbody_set,
    })
}

#[test]
fn tutorial_can_be_completed_using_only_movement_input() {
    let mut app = app(Level::tutorial());
    tick(&mut app, &[KeyCode::Space]);
    for _ in 0..300 {
        tick(&mut app, &[KeyCode::KeyD]);
        if status(&app) != Status::Playing {
            break;
        }
    }
    let position = position(&mut app);
    assert_eq!(status(&app), Status::Complete, "run ended at {position:?}");
}

#[test]
fn recorded_campaign_routes_complete_with_all_sensors_enabled() {
    for id in 1..=7 {
        let route = fs::read_to_string(format!("src/game/routes/level-{id}-route.tsv")).unwrap();
        let mut app = app(level(&format!("level-{id}")));
        tick(&mut app, &[KeyCode::Space]);
        'route: for line in route.lines() {
            let (frames, keys) = line.split_once('\t').unwrap();
            let held: Vec<_> = keys
                .split('+')
                .filter_map(|key| match key {
                    "a" => Some(KeyCode::KeyA),
                    "d" => Some(KeyCode::KeyD),
                    "space" => Some(KeyCode::Space),
                    "none" => None,
                    _ => panic!("unknown route key {key}"),
                })
                .collect();
            for _ in 0..frames.parse::<usize>().unwrap() {
                tick(&mut app, &held);
                if status(&app) != Status::Playing {
                    break 'route;
                }
            }
        }
        let p = position(&mut app);
        assert_eq!(
            status(&app),
            Status::Complete,
            "Level {id} route ended at {p:?}"
        );
    }
}

#[test]
fn switching_levels_restores_authored_movement_settings() {
    let mut app = app(level("level-2"));
    assert_eq!(app.world().resource::<MovementSettings>().jump_speed, 720.0);
    app.insert_resource(Level::tutorial());
    tick(&mut app, &[]);
    assert_eq!(app.world().resource::<MovementSettings>().jump_speed, 340.0);
    app.insert_resource(level("level-2"));
    tick(&mut app, &[]);
    tick(&mut app, &[KeyCode::Space]);
    assert_eq!(app.world().resource::<MovementSettings>().jump_speed, 720.0);
}

#[test]
fn jump_leaves_the_floor_and_cannot_repeat_in_midair() {
    let mut app = app(Level::tutorial());
    tick(&mut app, &[KeyCode::Space]);
    for _ in 0..10 {
        tick(&mut app, &[]);
    }
    let floor = position(&mut app);
    tick(&mut app, &[KeyCode::Space]);
    for _ in 0..8 {
        tick(&mut app, &[]);
    }
    let after_jump = position(&mut app);
    tick(&mut app, &[KeyCode::Space]);
    let world = app.world_mut();
    let velocity = world
        .query_filtered::<&Velocity, With<Spy>>()
        .single(world)
        .unwrap()
        .linear
        .y;
    assert!(
        after_jump.y > floor.y + 20.0 && velocity < MovementSettings::default().jump_speed - 80.0,
        "floor={floor:?}, after jump={after_jump:?}, second jump velocity={velocity}"
    );
}

#[test]
fn spy_can_jump_when_supported_by_a_loose_crate() {
    let level = Level::parse(r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="-600 -400 1200 800" data-gs-id="crate-support"><title>Crate support</title><circle id="spawn" cx="0" cy="-34" r="16"/><circle id="goal" cx="400" cy="-18" r="20"/><rect id="terrain" x="-500" y="0" width="1000" height="32"/><rect id="crate-support" x="-8" y="-16" width="16" height="16"/></svg>"#).unwrap();
    let mut app = app(level);
    tick(&mut app, &[KeyCode::Space]);
    for _ in 0..30 {
        tick(&mut app, &[]);
    }
    let supported = position(&mut app);
    let world = app.world_mut();
    assert!(
        world.query::<&Spy>().single(world).unwrap().grounded,
        "spy resting on a crate is incorrectly airborne at {supported:?}"
    );
    tick(&mut app, &[KeyCode::Space]);
    for _ in 0..8 {
        tick(&mut app, &[]);
    }
    assert!(position(&mut app).y > supported.y + 25.0);
}

#[test]
fn gameplay_requests_jump_landing_and_detection_sounds() {
    #[derive(Resource, Default)]
    struct Heard(Vec<Sound>);
    fn hear(mut sounds: MessageReader<Sound>, mut heard: ResMut<Heard>) {
        heard.0.extend(sounds.read().copied());
    }
    let mut app = app(Level::tutorial());
    app.init_resource::<Heard>().add_systems(Update, hear);
    tick(&mut app, &[KeyCode::Space]);
    for _ in 0..30 {
        tick(&mut app, &[]);
    }
    app.world_mut().resource_mut::<Heard>().0.clear();
    tick(&mut app, &[KeyCode::Space]);
    for _ in 0..1200 {
        tick(&mut app, &[]);
        if status(&app) == Status::Dead {
            break;
        }
    }
    let heard = &app.world().resource::<Heard>().0;
    assert_eq!(heard.first(), Some(&Sound::Jump), "{heard:?}");
    assert!(heard.contains(&Sound::Land), "{heard:?}");
    assert_eq!(heard.last(), Some(&Sound::Death), "{heard:?}");
    assert!(!heard.contains(&Sound::Complete), "{heard:?}");
}

#[test]
fn detection_scatters_debris_that_settles_on_terrain_until_retry() {
    fn debris(app: &mut App) -> Vec<Vec2> {
        let world = app.world_mut();
        world
            .query_filtered::<&Transform, With<Debris>>()
            .iter(world)
            .map(|transform| transform.translation.truncate())
            .collect()
    }
    let mut app = app(Level::tutorial());
    tick(&mut app, &[KeyCode::Space]);
    for _ in 0..1200 {
        tick(&mut app, &[]);
        if status(&app) == Status::Dead {
            break;
        }
    }
    assert_eq!(status(&app), Status::Dead);
    let scattered = debris(&mut app);
    assert_eq!(scattered.len(), DEBRIS_COUNT);
    for _ in 0..180 {
        tick(&mut app, &[]);
    }
    let settled = debris(&mut app);
    let level = app.world().resource::<Level>();
    let resting = settled
        .iter()
        .filter(|p| {
            let nearest = nearest_surface(&level.surfaces, **p, None).unwrap();
            nearest.distance < DEBRIS_RADIUS * 2.0 + 1.0
        })
        .count();
    assert!(
        scattered
            .iter()
            .zip(&settled)
            .all(|(a, b)| a.distance(*b) > 1.0),
        "{settled:?}"
    );
    assert!(resting * 2 >= DEBRIS_COUNT, "{settled:?}");
    tick(&mut app, &[KeyCode::KeyR]);
    tick(&mut app, &[]);
    assert!(debris(&mut app).is_empty());
}

#[test]
fn pause_freezes_bodies_timer_and_sensor_clock() {
    let mut app = app(Level::tutorial());
    tick(&mut app, &[KeyCode::Space]);
    for _ in 0..30 {
        tick(&mut app, &[KeyCode::KeyD]);
    }
    tick(&mut app, &[KeyCode::KeyP]);
    let before = position(&mut app);
    let elapsed = app.world().resource::<Run>().elapsed;
    for _ in 0..100 {
        tick(&mut app, &[KeyCode::KeyD]);
    }
    assert_eq!(
        (position(&mut app), app.world().resource::<Run>().elapsed),
        (before, elapsed)
    );
    tick(&mut app, &[KeyCode::KeyP]);
    for _ in 0..10 {
        tick(&mut app, &[KeyCode::KeyD]);
    }
    assert!(position(&mut app).x > before.x);
}

#[test]
fn patrolling_sensor_uses_the_same_clock_for_pose_pause_and_retry() {
    let level = Level::parse(include_str!("../../assets/authoring/template.svg")).unwrap();
    let mut app = app(level);
    let origin = |app: &App| {
        let elapsed = app.world().resource::<Run>().elapsed;
        app.world().resource::<Level>().sensors[0].origin_at(elapsed)
    };
    tick(&mut app, &[KeyCode::Space]);
    for _ in 0..60 {
        tick(&mut app, &[]);
    }
    tick(&mut app, &[KeyCode::KeyP]);
    let before = origin(&app);
    for _ in 0..60 {
        tick(&mut app, &[]);
    }
    assert_eq!(origin(&app), before);
    tick(&mut app, &[KeyCode::KeyR]);
    assert!(origin(&app).distance(Vec2::new(-300.0, 150.0)) < 2.0);
}

#[test]
fn retry_restores_spawn_cubes_timer_and_trail_without_leaks() {
    let mut app = app(Level::tutorial());
    tick(&mut app, &[KeyCode::Space]);
    for _ in 0..40 {
        tick(&mut app, &[KeyCode::KeyD]);
    }
    for _ in 0..5 {
        tick(&mut app, &[KeyCode::KeyR]);
        tick(&mut app, &[]);
    }
    let position = position(&mut app);
    let world = app.world_mut();
    let count = world
        .query_filtered::<Entity, With<AttemptEntity>>()
        .iter(world)
        .count();
    let trail_length = world.query::<&Spy>().single(world).unwrap().trail.len();
    assert!(
        position.distance(world.resource::<Level>().spawn) < 3.0
            && count == 3
            && trail_length <= 2
            && world.resource::<Run>().elapsed < 0.06,
        "position={position:?}, count={count}, trail={trail_length}"
    );
}

#[test]
fn level_selection_switches_and_retries_without_stale_physics() {
    let tutorial = Level::tutorial();
    let one = level("level-1");
    let mut app = app(tutorial.clone());
    app.insert_resource(Catalog {
        entries: vec![
            Entry {
                path: "tutorial.svg".into(),
                level: tutorial,
            },
            Entry {
                path: "level-1.svg".into(),
                level: one,
            },
        ],
        selected: 0,
        preview: None,
        diagnostics: None,
    });
    let mut progress = Progress::default();
    progress.bests.insert(LevelId("tutorial".into()), 3.0);
    progress.bests.insert(LevelId("level-1".into()), 20.0);
    app.insert_resource(progress).add_plugins(SessionPlugin);
    tick(&mut app, &[KeyCode::Space]);
    assert_eq!(status(&app), Status::MainMenu);
    tick(&mut app, &[]);
    tick(&mut app, &[KeyCode::Space]);
    assert_eq!(status(&app), Status::LevelSelect);
    tick(&mut app, &[]);
    for _ in 0..8 {
        tick(&mut app, &[KeyCode::ArrowRight]);
        tick(&mut app, &[]);
        tick(&mut app, &[KeyCode::Space]);
        tick(&mut app, &[]);
        let world = app.world_mut();
        let level = world.resource::<Level>();
        let expected = (level.terrain.len(), level.cubes.len() + 1);
        let terrain = world
            .query_filtered::<Entity, With<TerrainBody>>()
            .iter(world)
            .count();
        let actors = world
            .query_filtered::<Entity, With<AttemptEntity>>()
            .iter(world)
            .count();
        assert_eq!((terrain, actors), expected);
        let level = world.resource::<Level>();
        assert_eq!(
            world.resource::<Run>().best,
            world.resource::<Progress>().bests.get(&level.id).copied()
        );
        tick(&mut app, &[KeyCode::KeyR]);
        tick(&mut app, &[]);
        tick(&mut app, &[KeyCode::Tab]);
        tick(&mut app, &[]);
        assert_eq!(status(&app), Status::LevelSelect);
    }
}

#[test]
fn main_menu_starts_the_tutorial_escape_returns_and_quit_exits() {
    let mut app = app(Level::tutorial());
    app.insert_resource(Catalog {
        entries: vec![
            Entry {
                path: "level-1.svg".into(),
                level: level("level-1"),
            },
            Entry {
                path: "tutorial.svg".into(),
                level: Level::tutorial(),
            },
        ],
        selected: 0,
        preview: None,
        diagnostics: None,
    })
    .init_resource::<Progress>()
    .add_plugins(SessionPlugin);
    let level_id = |app: &App| app.world().resource::<Level>().id.0.clone();
    tick(&mut app, &[KeyCode::Space]);
    assert_eq!(status(&app), Status::MainMenu);
    tick(&mut app, &[KeyCode::ArrowDown]);
    tick(&mut app, &[KeyCode::Space]);
    assert_eq!(
        (status(&app), level_id(&app)),
        (Status::Playing, "tutorial".into())
    );
    for _ in 0..10 {
        tick(&mut app, &[KeyCode::KeyD]);
    }
    tick(&mut app, &[KeyCode::Escape]);
    let title = app.world().resource::<TitleScene>().0.id.0.clone();
    assert_eq!((status(&app), level_id(&app)), (Status::MainMenu, title));
    tick(&mut app, &[]);
    assert!(app.should_exit().is_none());
    tick(&mut app, &[KeyCode::ArrowDown]);
    tick(&mut app, &[]);
    tick(&mut app, &[KeyCode::ArrowDown]);
    tick(&mut app, &[KeyCode::Space]);
    assert!(app.should_exit().is_some());
}

#[test]
fn spy_can_travel_around_a_wall_and_under_a_platform() {
    let level = Level::parse(
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="-1200 -1200 2400 2400" data-gs-id="corner"><title>Corner fixture</title>
        <circle id="spawn" cx="0" cy="-98" r="16"/><circle id="goal" cx="1000" cy="-800" r="20"/>
        <rect id="terrain-square" x="-80" y="-80" width="160" height="160"/></svg>"#,
    )
    .unwrap();
    let mut app = app(level);
    tick(&mut app, &[KeyCode::Space]);
    let mut reached_side = false;
    let mut reached_ceiling = false;
    for _ in 0..360 {
        tick(&mut app, &[KeyCode::KeyD]);
        let world = app.world_mut();
        let down = world
            .query_filtered::<&GravityBody, With<Spy>>()
            .single(world)
            .unwrap()
            .down;
        reached_side |= down.x < -0.9;
        reached_ceiling |= down.y > 0.9;
    }
    assert!(
        reached_side && reached_ceiling,
        "wall={reached_side}, ceiling={reached_ceiling}"
    );
}

#[test]
fn movement_path_is_independent_of_render_frame_rate() {
    fn replay(frame_rate: f64, mut level: Level) -> Vec2 {
        level.sensors.clear();
        let mut app = app(level);
        app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs_f64(
            1.0 / frame_rate,
        )));
        tick(&mut app, &[KeyCode::Space]);
        // Compare at a fixed simulation count rather than rounding wall-clock durations.
        while app.world().resource::<Run>().elapsed < 1.5 {
            tick(&mut app, &[KeyCode::KeyD]);
        }
        position(&mut app)
    }
    for level in [Level::tutorial(), level("level-1")] {
        let reference = replay(60.0, level.clone());
        for fps in [30.0, 144.0] {
            let position = replay(fps, level.clone());
            assert!(
                position.distance(reference) < 5.0,
                "{} at {fps} fps: {position:?}, 60 fps: {reference:?}",
                level.name
            );
        }
    }
}

#[test]
fn terrain_blocks_sensor_rays_but_loose_cubes_do_not() {
    let mut level = Level::tutorial();
    level.sensors.clear();
    let sensor = Sensor {
        id: "agent-test".into(),
        color: Color::WHITE,
        origin: Vec2::new(-405.0, -250.0),
        angle: FRAC_PI_2,
        sweep: 0.0,
        period: 1.0,
        phase: 0.8,
        range: 500.0,
        aperture: 1.0,
        patrol: None,
        body: Vec::new(),
    };
    with_context(&mut app(level), |context| {
        assert!(!sensor_sees(
            context,
            &sensor,
            0.0,
            Vec2::new(-405.0, -90.0)
        ));
        let distance = terrain_ray(context, Vec2::new(-148.0, -80.0), Vec2::NEG_Y, 100.0);
        assert!(
            (distance - 28.0).abs() < 1.0,
            "a cube wrongly occluded the ray at {distance}"
        );
    });
}

#[test]
fn collision_boundary_preserves_the_valley_beside_the_exit() {
    with_context(&mut app(Level::tutorial()), |context| {
        let distance = terrain_ray(context, Vec2::new(400.0, -80.0), Vec2::NEG_Y, 100.0);
        assert!(
            (distance - 28.0).abs() < 1.0,
            "terrain incorrectly filled the exit valley: {distance}"
        );
    });
}

#[test]
fn hole_collision_gravity_and_render_fill_agree() {
    let level = Level::parse(r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="-300 -300 600 600" data-gs-id="hole"><title>Hole</title><circle id="spawn" r="16"/><circle id="goal" cx="200" r="20"/><path id="terrain-ring" fill-rule="evenodd" d="M -100 -100 H 100 V 100 H -100 Z M -60 -60 H 60 V 60 H -60 Z"/></svg>"#).unwrap();
    assert!(!level.terrain[0].contains(Vec2::ZERO));
    assert!(level.terrain[0].contains(Vec2::new(80.0, 0.0)));
    let nearest = nearest_surface(&level.surfaces, Vec2::ZERO, None).unwrap();
    assert!((nearest.distance - 60.0).abs() < 0.01);
    with_context(&mut app(level), |context| {
        for direction in [Vec2::X, Vec2::Y, Vec2::NEG_X, Vec2::NEG_Y] {
            assert!((terrain_ray(context, Vec2::ZERO, direction, 200.0) - 60.0).abs() < 0.01);
        }
    });
}

#[test]
fn level_seven_center_is_solid_for_rendering_collision_and_gravity() {
    let level = level("level-7");
    for x in [128.0, 624.0, 1056.0] {
        for y in [64.0, 400.0, 704.0] {
            let point = Vec2::new(x, y);
            assert!(level.terrain.iter().any(|shape| shape.contains(point)));
        }
    }
    let center = Vec2::new(624.0, 400.0);
    let nearest = nearest_surface(&level.surfaces, center, None).unwrap();
    assert!(nearest.distance > 600.0);
    with_context(&mut app(level), |context| {
        for direction in [Vec2::X, Vec2::Y, Vec2::NEG_X, Vec2::NEG_Y] {
            assert_eq!(terrain_ray(context, center, direction, 2000.0), 0.0);
        }
        let above = terrain_ray(context, Vec2::new(624.0, 1700.0), Vec2::NEG_Y, 3000.0);
        assert!((above - 548.0).abs() < 0.01);
    });
}

#[test]
fn agents_stay_safe_for_ten_seconds_then_warn_before_each_ten_second_danger() {
    let paths = fs::read_dir("assets/levels")
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .chain([PathBuf::from("assets/authoring/template.svg")]);
    for path in paths {
        for sensor in Level::load(&path).unwrap().sensors {
            assert_eq!(
                sensor.period,
                AGENT_PERIOD,
                "{} {}",
                path.display(),
                sensor.id
            );
            let phase = |time: f32| sensor.phase_at(time);
            let first_danger = (0..6000)
                .map(|step| step as f32 * 0.01)
                .find(|&time| phase(time) == SensorPhase::Danger)
                .unwrap();
            assert!(
                first_danger >= 10.0,
                "{} {} at {first_danger}",
                path.display(),
                sensor.id
            );
            assert_eq!(phase(first_danger - 0.5), SensorPhase::Warning);
            assert_eq!(
                phase(first_danger + 0.5 + AGENT_PERIOD),
                SensorPhase::Danger
            );
            assert_eq!(
                phase(first_danger - 0.5 + AGENT_PERIOD),
                SensorPhase::Warning
            );
        }
    }
}

#[test]
fn every_authored_agent_pose_clears_the_actual_terrain_colliders() {
    let paths = fs::read_dir("assets/levels")
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .chain([PathBuf::from("assets/authoring/template.svg")]);
    for path in paths {
        let level = Level::load(&path).unwrap();
        with_context(&mut app(level.clone()), |context| {
            for sensor in &level.sensors {
                let triangles = sensor
                    .body
                    .iter()
                    .flat_map(|body| body.triangles())
                    .map(|triangle| {
                        let [a, b, c] = triangle.map(|vertex| vertex - sensor.origin);
                        (Vec2::ZERO, 0.0, Collider::triangle(a, b, c))
                    })
                    .collect();
                let body = Collider::compound(triangles);
                let duration = sensor
                    .patrol
                    .as_ref()
                    .map_or(sensor.period, |patrol| sensor.period.max(patrol.period()));
                for sample in 0..=720 {
                    let elapsed = sample as f32 / 720.0 * duration;
                    context.intersect_shape(
                        sensor.origin_at(elapsed),
                        sensor.facing(elapsed) - sensor.angle,
                        body.raw.as_ref(),
                        terrain_filter(),
                        |_| {
                            panic!(
                                "{} {} intersects terrain at {elapsed:.3}s",
                                path.display(),
                                sensor.id
                            )
                        },
                    );
                }
            }
        });
    }
}
