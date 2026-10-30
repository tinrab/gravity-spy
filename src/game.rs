use std::collections::VecDeque;

use bevy::{input::InputSystems, prelude::*};
use bevy_rapier2d::{parry::shape::Ball, prelude::*};

use crate::{
    audio::Sound,
    geometry::nearest_surface,
    level::{Level, MovementSettings, Sensor, SensorPhase},
    session::Catalog,
};

pub const STEP: f32 = 1.0 / 60.0;
pub const PLAYER_RADIUS: f32 = 16.0;
/// How long a trail point stays visible.
pub const TRAIL_SECONDS: f32 = 2.5;
const SPY_MASS: f32 = 1.0;
const CRATE_MASS: f32 = 0.15;
pub const DEBRIS_RADIUS: f32 = 3.5;
const DEBRIS_MASS: f32 = 0.05;
const DEBRIS_COUNT: usize = 24;
/// Falls slower than this roll over bumps silently instead of landing.
const LAND_SPEED: f32 = 140.0;
const BUMP_SPEED: f32 = 80.0;
const TERRAIN_GROUP: Group = Group::GROUP_1;
const BODY_GROUP: Group = Group::GROUP_2;
const DEBRIS_GROUP: Group = Group::GROUP_3;

#[derive(Resource, Default, Debug)]
pub struct InputIntent {
    pub movement: f32,
    pub jump: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Status {
    #[default]
    Welcome,
    MainMenu,
    LevelSelect,
    Playing,
    Paused,
    Dead,
    Complete,
}

#[derive(Resource, Debug)]
pub struct Run {
    pub status: Status,
    pub elapsed: f32,
    pub best: Option<f32>,
    pub attempts: u32,
    pub reset_requested: bool,
    /// Physics has stepped at least once since the level loaded, so scene queries work.
    pub ready: bool,
}

impl Default for Run {
    fn default() -> Self {
        Self {
            status: Status::Welcome,
            elapsed: 0.0,
            best: None,
            attempts: 0,
            reset_requested: true,
            ready: false,
        }
    }
}

impl Run {
    pub fn restart(&mut self) {
        self.status = Status::Playing;
        self.elapsed = 0.0;
        self.attempts += 1;
        self.reset_requested = true;
        info!("Attempt {} started", self.attempts);
    }
}

/// A body pulled toward its nearest terrain surface.
#[derive(Component, Debug)]
pub struct GravityBody {
    pub down: Vec2,
    pub surface: Option<usize>,
    pub radius: f32,
    pub mass: f32,
}

impl GravityBody {
    fn new(radius: f32, mass: f32) -> Self {
        Self {
            down: Vec2::NEG_Y,
            surface: None,
            radius,
            mass,
        }
    }
}

#[derive(Component, Debug)]
pub struct Spy {
    pub grounded: bool,
    /// Fastest fall toward the surface since leaving the ground.
    pub fall_speed: f32,
    pub roll: f32,
    pub previous: Vec2,
    pub current: Vec2,
    pub trail: VecDeque<(Vec2, f32)>,
}

#[derive(Component)]
pub struct LooseCube {
    pub size: Vec2,
    pub id: String,
}

/// A fragment of the spy, scattered on detection.
#[derive(Component)]
pub struct Debris;

#[derive(Component)]
pub struct TerrainBody;

/// Removed and respawned whenever an attempt restarts.
#[derive(Component)]
pub struct AttemptEntity;

pub struct SimulationPlugin;

impl Plugin for SimulationPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(Time::<Fixed>::from_hz(60.0))
            .insert_resource(TimestepMode::Fixed {
                dt: STEP,
                substeps: 2,
            })
            .init_resource::<InputIntent>()
            .init_resource::<MovementSettings>()
            .init_resource::<Run>()
            .add_message::<Sound>()
            .add_plugins(
                RapierPhysicsPlugin::<NoUserData>::pixels_per_meter(100.0).in_fixed_schedule(),
            )
            .add_systems(PreUpdate, read_controls.after(InputSystems))
            .add_systems(
                FixedUpdate,
                (
                    rebuild_terrain,
                    reset_attempt,
                    configure_physics,
                    move_bodies,
                )
                    .chain()
                    .before(PhysicsSet::SyncBackend),
            )
            .add_systems(
                FixedUpdate,
                (finish_tick, bump_sounds).after(PhysicsSet::Writeback),
            );
    }
}

fn rebuild_terrain(
    mut commands: Commands,
    level: Res<Level>,
    mut run: ResMut<Run>,
    old: Query<Entity, With<TerrainBody>>,
    mut settings: ResMut<MovementSettings>,
) {
    if !level.is_changed() {
        return;
    }
    for entity in &old {
        commands.entity(entity).despawn();
    }
    run.ready = false;
    run.reset_requested = true;
    *settings = level.movement;
    for terrain in &level.terrain {
        let triangles = terrain
            .triangles()
            .map(|[a, b, c]| (Vec2::ZERO, 0.0, Collider::triangle(a, b, c)))
            .collect();
        commands.spawn((
            TerrainBody,
            RigidBody::Fixed,
            Collider::compound(triangles),
            CollisionGroups::new(TERRAIN_GROUP, Group::ALL),
            Friction::coefficient(0.0),
            Restitution::coefficient(0.0),
            Transform::default(),
        ));
    }
}

pub fn read_controls(
    keys: Res<ButtonInput<KeyCode>>,
    mut input: ResMut<InputIntent>,
    mut run: ResMut<Run>,
    catalog: Option<Res<Catalog>>,
) {
    let start = keys.any_just_pressed([KeyCode::Space, KeyCode::Enter]);
    let menu_handled = catalog.as_ref().is_some_and(|c| c.preview.is_none());
    let invalid_preview = catalog
        .as_ref()
        .is_some_and(|c| c.preview.is_some() && c.diagnostics.is_some() && run.attempts == 0);
    if invalid_preview {
        input.jump = false;
        input.movement = 0.0;
        return;
    }
    let retry = keys.just_pressed(KeyCode::KeyR)
        && !matches!(
            run.status,
            Status::Welcome | Status::MainMenu | Status::LevelSelect
        );
    let start_attempt = start
        && (run.status == Status::Dead
            || (!menu_handled
                && matches!(
                    run.status,
                    Status::Welcome | Status::MainMenu | Status::Complete
                )));
    if retry || start_attempt {
        run.restart();
    } else if keys.just_pressed(KeyCode::KeyP) {
        run.status = match run.status {
            Status::Playing => Status::Paused,
            Status::Paused => Status::Playing,
            other => other,
        };
        info!("Run is {:?} at {:.2}s", run.status, run.elapsed);
    } else if run.status == Status::Playing {
        input.jump |= start || keys.any_just_pressed([KeyCode::KeyW, KeyCode::ArrowUp]);
    }
    input.movement = f32::from(keys.any_pressed([KeyCode::KeyD, KeyCode::ArrowRight]))
        - f32::from(keys.any_pressed([KeyCode::KeyA, KeyCode::ArrowLeft]));
    if run.status != Status::Playing {
        input.jump = false;
    }
}

fn reset_attempt(
    mut commands: Commands,
    mut run: ResMut<Run>,
    mut input: ResMut<InputIntent>,
    level: Res<Level>,
    old_entities: Query<Entity, With<AttemptEntity>>,
) {
    if !run.reset_requested {
        return;
    }
    for entity in &old_entities {
        commands.entity(entity).despawn();
    }
    run.reset_requested = false;
    input.jump = false;
    commands.spawn((
        AttemptEntity,
        Spy {
            grounded: false,
            fall_speed: 0.0,
            roll: 0.0,
            previous: level.spawn,
            current: level.spawn,
            trail: VecDeque::new(),
        },
        GravityBody::new(PLAYER_RADIUS, SPY_MASS),
        Collider::ball(PLAYER_RADIUS),
        LockedAxes::ROTATION_LOCKED,
        ActiveEvents::COLLISION_EVENTS,
        Sleeping::disabled(),
        Transform::from_translation(level.spawn.extend(2.0)),
        Visibility::default(),
        dynamic_body(SPY_MASS, 0.0, 0.0),
        CollisionGroups::new(BODY_GROUP, Group::ALL),
    ));
    for cube in &level.cubes {
        commands.spawn((
            AttemptEntity,
            LooseCube {
                size: cube.size,
                id: cube.id.clone(),
            },
            GravityBody::new(cube.size.min_element() * 0.5, CRATE_MASS),
            Collider::cuboid(cube.size.x * 0.5, cube.size.y * 0.5),
            Transform::from_translation(cube.position.extend(1.0))
                .with_rotation(Quat::from_rotation_z(cube.angle)),
            Visibility::default(),
            dynamic_body(CRATE_MASS, 0.15, 0.05),
            CollisionGroups::new(BODY_GROUP, Group::ALL),
        ));
    }
}

fn dynamic_body(mass: f32, friction: f32, restitution: f32) -> impl Bundle {
    (
        RigidBody::Dynamic,
        ColliderMassProperties::Mass(mass),
        Friction::coefficient(friction),
        Restitution::coefficient(restitution),
        Ccd::enabled(),
        Velocity::zero(),
        ExternalForce::default(),
    )
}

fn configure_physics(run: Res<Run>, mut configs: Query<&mut RapierConfiguration>) {
    for mut config in &mut configs {
        config.gravity = Vec2::ZERO;
        // Debris keeps falling after detection. Scene queries initialize once even on the title.
        config.physics_pipeline_active =
            matches!(run.status, Status::Playing | Status::Dead) || !run.ready;
    }
}

#[expect(
    clippy::type_complexity,
    reason = "Physics updates query identity, motion, force, and optional player control together"
)]
fn move_bodies(
    level: Res<Level>,
    settings: Res<MovementSettings>,
    run: Res<Run>,
    mut input: ResMut<InputIntent>,
    mut sounds: MessageWriter<Sound>,
    context: ReadRapierContext,
    mut bodies: Query<(
        Entity,
        &Transform,
        &mut GravityBody,
        &mut Velocity,
        &mut ExternalForce,
        Option<&mut Spy>,
    )>,
) {
    if !matches!(run.status, Status::Playing | Status::Dead) {
        return;
    }
    let context = context.single().ok();
    for (entity, transform, mut gravity, mut velocity, mut force, spy) in &mut bodies {
        let position = transform.translation.truncate();
        let Some(nearest) = nearest_surface(&level.surfaces, position, gravity.surface) else {
            continue;
        };
        gravity.down = nearest.down;
        gravity.surface = Some(nearest.id);
        force.force = gravity.down * settings.gravity * gravity.mass;
        let Some(mut spy) = spy else {
            continue;
        };
        if run.status != Status::Playing {
            continue;
        }
        let down = gravity.down;
        let supported = nearest.distance <= gravity.radius + 3.0
            || context
                .as_ref()
                .is_some_and(|context| on_body(context, entity, position, &gravity));
        let was_grounded = spy.grounded;
        spy.grounded = supported && velocity.linear.dot(down) > -30.0;
        if !spy.grounded {
            spy.fall_speed = spy.fall_speed.max(velocity.linear.dot(down));
        } else {
            if !was_grounded && spy.fall_speed > LAND_SPEED {
                sounds.write(Sound::Land);
            }
            spy.fall_speed = 0.0;
        }

        let tangent = down.perp();
        let speed = velocity.linear.dot(tangent);
        let acceleration = if spy.grounded {
            settings.acceleration
        } else {
            settings.acceleration * 0.4
        };
        // Without input the spy brakes on the ground and keeps its momentum in the air.
        let desired = if input.movement != 0.0 {
            input.movement * settings.max_speed
        } else if spy.grounded {
            0.0
        } else {
            speed
        };
        velocity.linear +=
            tangent * (desired - speed).clamp(-acceleration * STEP, acceleration * STEP);
        if input.jump && spy.grounded {
            let falling_speed = velocity.linear.dot(down).max(0.0);
            velocity.linear -= down * (settings.jump_speed + falling_speed);
            spy.grounded = false;
            sounds.write(Sound::Jump);
        }
        spy.roll -= speed * STEP / PLAYER_RADIUS;
    }
    input.jump = false;
}

/// Gravity only targets terrain, but loose bodies under the spy can still support a jump.
fn on_body(context: &RapierContext, entity: Entity, position: Vec2, gravity: &GravityBody) -> bool {
    let feet = Ball::new(gravity.radius - 0.5);
    context
        .cast_shape(
            position,
            0.0,
            gravity.down,
            &feet,
            ShapeCastOptions::with_max_time_of_impact(3.5),
            QueryFilter::default()
                .exclude_rigid_body(entity)
                .groups(CollisionGroups::new(BODY_GROUP, BODY_GROUP)),
        )
        .and_then(|(_, hit)| hit.details)
        .is_some_and(|hit| hit.normal1.dot(gravity.down) < -0.2)
}

pub fn terrain_filter() -> QueryFilter<'static> {
    QueryFilter::default().groups(CollisionGroups::new(Group::ALL, TERRAIN_GROUP))
}

/// Distance to the first terrain hit along a ray, or `range` if nothing is hit.
pub fn terrain_ray(context: &RapierContext, origin: Vec2, direction: Vec2, range: f32) -> f32 {
    context
        .cast_ray(origin, direction, range, true, terrain_filter())
        .map_or(range, |(_, distance)| distance)
}

pub fn sensor_sees(context: &RapierContext, sensor: &Sensor, elapsed: f32, position: Vec2) -> bool {
    if sensor.phase_at(elapsed) != SensorPhase::Danger {
        return false;
    }
    let origin = sensor.origin_at(elapsed);
    let delta = position - origin;
    let distance = delta.length();
    if distance < 0.001 {
        return true;
    }
    if distance > sensor.range + PLAYER_RADIUS {
        return false;
    }
    let direction = Vec2::from_angle(sensor.facing(elapsed));
    let angular_radius = (PLAYER_RADIUS / distance).min(1.0).asin();
    if direction.angle_to(delta).abs() > sensor.aperture * 0.5 + angular_radius {
        return false;
    }
    // Sample the disk as well as its center so exposed edges cannot hide behind a thin wall.
    let perpendicular = delta.normalize().perp() * PLAYER_RADIUS * 0.8;
    [position, position + perpendicular, position - perpendicular]
        .into_iter()
        .any(|target| {
            let ray = target - origin;
            let length = ray.length();
            direction.angle_to(ray).abs() <= sensor.aperture * 0.5
                && terrain_ray(context, origin, ray / length, length) >= length - 0.5
        })
}

fn finish_tick(
    mut commands: Commands,
    mut run: ResMut<Run>,
    level: Res<Level>,
    context: ReadRapierContext,
    mut spies: Query<(Entity, &Transform, &mut Spy, &Velocity)>,
    mut sounds: MessageWriter<Sound>,
) {
    run.ready = true;
    if run.status != Status::Playing {
        return;
    }
    let Ok((entity, transform, mut spy, velocity)) = spies.single_mut() else {
        return;
    };
    run.elapsed += STEP;
    let elapsed = run.elapsed;
    let position = transform.translation.truncate();
    spy.previous = spy.current;
    spy.current = position;
    debug!(
        target: "gravity_spy::state",
        "SPY {elapsed:.3} {:.3} {:.3} {:.3} {:.3} {}",
        position.x, position.y, velocity.linear.x, velocity.linear.y, spy.grounded
    );
    spy.trail.push_back((position, elapsed));
    while spy
        .trail
        .front()
        .is_some_and(|(_, time)| elapsed - time > TRAIL_SECONDS)
    {
        spy.trail.pop_front();
    }
    let Ok(context) = context.single() else {
        return;
    };
    let caught = level
        .sensors
        .iter()
        .any(|sensor| sensor_sees(&context, sensor, elapsed, position));
    if caught || !level.bounds.contains(position, 256.0) {
        run.status = Status::Dead;
        sounds.write(Sound::Death);
        // The spy stays for the camera and trail, but no longer takes part in physics.
        commands
            .entity(entity)
            .insert((RigidBodyDisabled, ColliderDisabled));
        scatter_debris(&mut commands, position);
        info!("Detected at {elapsed:.2}s");
    } else if level.goal.iter().any(|shape| shape.contains(position)) {
        run.status = Status::Complete;
        run.best = Some(run.best.map_or(elapsed, |best| best.min(elapsed)));
        sounds.write(Sound::Complete);
        info!("{} complete in {elapsed:.2}s", level.name);
    }
}

/// Breaks the spy into small bodies that fly apart, then fall toward terrain like everything else.
fn scatter_debris(commands: &mut Commands, position: Vec2) {
    for i in 0..DEBRIS_COUNT {
        // Golden-angle spacing spreads fragments evenly without randomness.
        let direction = Vec2::from_angle(i as f32 * 2.399_963_1);
        let speed = 90.0 + (i % 5) as f32 * 25.0;
        let offset = direction * (PLAYER_RADIUS - DEBRIS_RADIUS) * ((i % 3 + 1) as f32 / 3.0);
        commands
            .spawn((
                AttemptEntity,
                Debris,
                GravityBody::new(DEBRIS_RADIUS, DEBRIS_MASS),
                Collider::ball(DEBRIS_RADIUS),
                CollisionGroups::new(DEBRIS_GROUP, TERRAIN_GROUP | DEBRIS_GROUP),
                Transform::from_translation((position + offset).extend(2.5)),
                Visibility::default(),
                dynamic_body(DEBRIS_MASS, 0.4, 0.3),
            ))
            .insert(Velocity::linear(direction * speed));
    }
}

fn bump_sounds(
    mut collisions: MessageReader<CollisionEvent>,
    spies: Query<&Velocity, With<Spy>>,
    cubes: Query<&Velocity, With<LooseCube>>,
    mut sounds: MessageWriter<Sound>,
) {
    for &event in collisions.read() {
        let CollisionEvent::Started(a, b, _) = event else {
            continue;
        };
        let pair = spies
            .get(a)
            .ok()
            .zip(cubes.get(b).ok())
            .or_else(|| spies.get(b).ok().zip(cubes.get(a).ok()));
        if let Some((spy, cube)) = pair
            && spy.linear.length().max(cube.linear.length()) > BUMP_SPEED
        {
            sounds.write(Sound::Bump);
        }
    }
}

#[cfg(test)]
mod tests;
