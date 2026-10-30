use std::f32::consts::{PI, TAU};

use bevy::{
    asset::RenderAssetUsages,
    camera::ScalingMode,
    mesh::{Indices, PrimitiveTopology},
    prelude::*,
    render::render_resource::AsBindGroup,
    shader::ShaderRef,
    sprite::Anchor,
    sprite_render::{AlphaMode2d, Material2d, Material2dPlugin},
    text::{FontSmoothing, LineHeight},
    transform::TransformSystems,
};
use bevy_rapier2d::prelude::*;

use crate::{
    game::{
        AttemptEntity, DEBRIS_RADIUS, Debris, GravityBody, LooseCube, PLAYER_RADIUS, Run, Spy,
        Status, TRAIL_SECONDS, terrain_ray,
    },
    geometry::{nearest_surface, polygon_surfaces},
    level::{Level, LightDefinition, Palette, SensorPhase},
};

pub const FONT: &str = "fonts/PressStart2P-Regular.ttf";
const LABEL_OFFSET: Vec2 = Vec2::new(0.0, 18.0);
const PARTICLE_LIFE: f32 = 1.4;

/// Shared render assets. Per-level meshes and materials are owned here and freed on rebuild.
#[derive(Resource)]
struct Art {
    ball: Handle<Mesh>,
    half_ball: Handle<Mesh>,
    particle: Handle<Mesh>,
    debris: Handle<Mesh>,
    trail: Handle<Mesh>,
    goal: Handle<ColorMaterial>,
    death: Handle<ColorMaterial>,
    success: Handle<ColorMaterial>,
    dark: Handle<ColorMaterial>,
    light: Handle<ColorMaterial>,
    terrain: Handle<ColorMaterial>,
    outline: Handle<ColorMaterial>,
    owned_meshes: Vec<Handle<Mesh>>,
    owned_colors: Vec<Handle<ColorMaterial>>,
    harmless: Vec<(Handle<Mesh>, Handle<LightMaterial>)>,
    cones: Vec<(Handle<Mesh>, Handle<LightMaterial>)>,
}

impl Art {
    fn release(
        &mut self,
        meshes: &mut Assets<Mesh>,
        materials: &mut Assets<ColorMaterial>,
        light_materials: &mut Assets<LightMaterial>,
    ) {
        for handle in self.owned_meshes.drain(..) {
            meshes.remove(handle.id());
        }
        for handle in self.owned_colors.drain(..) {
            materials.remove(handle.id());
        }
        for (_, handle) in self.cones.drain(..).chain(self.harmless.drain(..)) {
            light_materials.remove(handle.id());
        }
    }

    fn apply_palette(&self, palette: &Palette, materials: &mut Assets<ColorMaterial>) {
        for (handle, color) in [
            (&self.goal, palette.goal),
            (&self.death, palette.death),
            (&self.success, palette.success),
            (&self.dark, palette.spy_dark),
            (&self.light, palette.spy_light),
            (&self.terrain, palette.terrain),
            (&self.outline, palette.outline),
        ] {
            if let Some(mut material) = materials.get_mut(handle) {
                material.color = color;
            }
        }
    }
}

#[derive(Asset, TypePath, AsBindGroup, Clone)]
struct LightMaterial {
    #[uniform(0)]
    color: LinearRgba,
    #[uniform(1)]
    origin_and_range: Vec4,
}

impl Material2d for LightMaterial {
    fn fragment_shader() -> ShaderRef {
        "shaders/light.wgsl".into()
    }

    fn alpha_mode(&self) -> AlphaMode2d {
        AlphaMode2d::Blend
    }
}

struct LightShape {
    origin: Vec2,
    facing: f32,
    aperture: f32,
    range: f32,
}

#[derive(Default)]
struct LightCache {
    static_ready: bool,
    last_elapsed: Option<f32>,
}

#[derive(Component)]
struct SpyVisual;

#[derive(Component)]
struct LevelVisual;

#[derive(Component)]
struct AgentVisual(usize);

#[derive(Component)]
struct DebugLabel;

#[derive(Component)]
struct Particle {
    velocity: Vec2,
    life: f32,
}

#[derive(Resource, Default)]
pub struct DebugView(pub bool);

pub struct PresentationPlugin;

impl Plugin for PresentationPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<DebugView>()
            .add_plugins(Material2dPlugin::<LightMaterial>::default())
            .add_plugins(RapierDebugRenderPlugin {
                enabled: false,
                ..default()
            })
            .add_systems(Startup, setup_art)
            .add_systems(
                PostUpdate,
                (
                    rebuild_art,
                    attach_body_art,
                    follow_camera,
                    update_agents,
                    update_spy_art,
                    draw_lights,
                    update_trail,
                    animate_particles,
                    draw_debug,
                )
                    .chain()
                    .before(TransformSystems::Propagate),
            );
    }
}

fn mesh(vertices: Vec<[f32; 3]>, colors: Vec<[f32; 4]>, indices: Vec<u32>) -> Mesh {
    if vertices.is_empty() {
        return placeholder_mesh();
    }
    let count = vertices.len();
    Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    )
    .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, vertices)
    .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, colors)
    .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, vec![[0.0, 0.0, 1.0]; count])
    .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, vec![[0.0, 0.0]; count])
    .with_inserted_indices(Indices::U32(indices))
}

/// A degenerate triangle that keeps GPU buffers allocated while a trail or light has no samples.
fn placeholder_mesh() -> Mesh {
    mesh(vec![[0.0; 3]; 3], vec![[0.0; 4]; 3], vec![0, 1, 2])
}

fn polygon_mesh(vertices: &[Vec2], indices: &[u32]) -> Mesh {
    mesh(
        vertices.iter().map(|p| [p.x, p.y, 0.0]).collect(),
        vec![[1.0; 4]; vertices.len()],
        indices.to_vec(),
    )
}

fn circle_mesh(radius: f32, half: bool) -> Mesh {
    let steps = 48;
    let sweep = if half { PI } else { TAU };
    let mut points = vec![Vec2::ZERO];
    points.extend((0..=steps).map(|i| Vec2::from_angle(i as f32 / steps as f32 * sweep) * radius));
    let indices: Vec<u32> = (1..=steps).flat_map(|i| [0, i, i + 1]).collect();
    polygon_mesh(&points, &indices)
}

/// Quads of the given half-width along each segment.
fn strip_mesh(segments: impl Iterator<Item = (Vec2, Vec2, [f32; 4])>, half_width: f32) -> Mesh {
    let mut vertices = Vec::new();
    let mut colors = Vec::new();
    let mut indices = Vec::new();
    for (a, b, color) in segments {
        let normal = (b - a).normalize_or_zero().perp() * half_width;
        let start = vertices.len() as u32;
        vertices.extend([a - normal, b - normal, b + normal, a + normal].map(|p| [p.x, p.y, 0.0]));
        colors.extend([color; 4]);
        indices.extend([start, start + 1, start + 2, start, start + 2, start + 3]);
    }
    mesh(vertices, colors, indices)
}

fn setup_art(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<ColorMaterial>>,
) {
    commands.spawn((
        Camera2d,
        Projection::Orthographic(OrthographicProjection {
            scaling_mode: ScalingMode::FixedVertical {
                viewport_height: 720.0,
            },
            ..OrthographicProjection::default_2d()
        }),
        Transform::default(),
    ));
    let mut blended = || {
        materials.add(ColorMaterial {
            color: Color::WHITE,
            alpha_mode: AlphaMode2d::Blend,
            ..default()
        })
    };
    let trail_material = blended();
    let outline = blended();
    let trail = meshes.add(placeholder_mesh());
    commands.spawn((
        Mesh2d(trail.clone()),
        MeshMaterial2d(trail_material),
        Transform::from_xyz(0.0, 0.0, 1.5),
    ));
    // Palette colors are applied whenever a level loads.
    let mut white = || materials.add(ColorMaterial::from(Color::WHITE));
    commands.insert_resource(Art {
        ball: meshes.add(circle_mesh(PLAYER_RADIUS, false)),
        half_ball: meshes.add(circle_mesh(PLAYER_RADIUS, true)),
        particle: meshes.add(circle_mesh(2.5, false)),
        debris: meshes.add(circle_mesh(DEBRIS_RADIUS, false)),
        trail,
        goal: white(),
        death: white(),
        success: white(),
        dark: white(),
        light: white(),
        terrain: white(),
        outline,
        owned_meshes: Vec::new(),
        owned_colors: Vec::new(),
        harmless: Vec::new(),
        cones: Vec::new(),
    });
}

#[expect(
    clippy::too_many_arguments,
    reason = "Bevy injects independently owned render resources"
)]
fn rebuild_art(
    mut commands: Commands,
    level: Res<Level>,
    mut art: ResMut<Art>,
    old: Query<Entity, With<LevelVisual>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<ColorMaterial>>,
    mut light_materials: ResMut<Assets<LightMaterial>>,
    mut clear: ResMut<ClearColor>,
    assets: Res<AssetServer>,
) {
    if !level.is_changed() {
        return;
    }
    for entity in &old {
        commands.entity(entity).despawn();
    }
    art.release(&mut meshes, &mut materials, &mut light_materials);
    art.apply_palette(&level.palette, &mut materials);
    clear.0 = level.palette.background;
    if let Some(mut trail) = meshes.get_mut(&art.trail) {
        *trail = placeholder_mesh();
    }
    let mut builder = LevelArt {
        commands: &mut commands,
        meshes: &mut meshes,
        materials: &mut materials,
        light_materials: &mut light_materials,
        art: &mut art,
        font: assets.load(FONT),
    };
    builder.terrain(&level);
    builder.goal_arrow(level.destination);
    builder.lights(&level);
    builder.agents(&level);
    builder.hints(&level);
    builder.labels(&level);
}

/// Spawns one level's visuals and records every asset it creates in [`Art`].
struct LevelArt<'a, 'w, 's> {
    commands: &'a mut Commands<'w, 's>,
    meshes: &'a mut Assets<Mesh>,
    materials: &'a mut Assets<ColorMaterial>,
    light_materials: &'a mut Assets<LightMaterial>,
    art: &'a mut Art,
    font: Handle<Font>,
}

impl LevelArt<'_, '_, '_> {
    fn add_mesh(&mut self, mesh: Mesh) -> Handle<Mesh> {
        let handle = self.meshes.add(mesh);
        self.art.owned_meshes.push(handle.clone());
        handle
    }

    fn add_color(&mut self, color: Color) -> Handle<ColorMaterial> {
        let handle = self.materials.add(ColorMaterial::from(color));
        self.art.owned_colors.push(handle.clone());
        handle
    }

    fn add_light(&mut self, color: LinearRgba, origin: Vec2, range: f32) -> Handle<LightMaterial> {
        self.light_materials.add(LightMaterial {
            color,
            origin_and_range: origin.extend(range).extend(0.0),
        })
    }

    fn spawn<M: Material2d>(
        &mut self,
        mesh: Handle<Mesh>,
        material: Handle<M>,
        transform: Transform,
    ) -> EntityCommands<'_> {
        self.commands.spawn((
            LevelVisual,
            Mesh2d(mesh),
            MeshMaterial2d(material),
            transform,
        ))
    }

    fn terrain(&mut self, level: &Level) {
        for terrain in &level.terrain {
            let mesh = self.add_mesh(polygon_mesh(&terrain.vertices, &terrain.triangles));
            self.spawn(mesh, self.art.terrain.clone(), Transform::default());
        }
        // Colored shapes draw over the base terrain, in document order.
        let layers = (level.terrain_paint.len() + 1) as f32;
        for (index, paint) in level.terrain_paint.iter().enumerate() {
            let material = self.add_color(paint.color);
            let depth = 0.1 * (index + 1) as f32 / layers;
            for shape in &paint.shapes {
                let mesh = self.add_mesh(polygon_mesh(&shape.vertices, &shape.triangles));
                self.spawn(mesh, material.clone(), Transform::from_xyz(0.0, 0.0, depth));
            }
        }
        let white = [1.0; 4];
        let edges = level.surfaces.iter().map(|s| (s.a, s.b, white));
        let mesh = self.add_mesh(strip_mesh(edges, 0.5));
        self.spawn(
            mesh,
            self.art.outline.clone(),
            Transform::from_xyz(0.0, 0.0, 0.2),
        );
    }

    fn goal_arrow(&mut self, destination: Vec2) {
        let arrow = [
            Vec2::new(-10.0, 28.0),
            Vec2::new(10.0, 28.0),
            Vec2::new(10.0, 0.0),
            Vec2::new(19.0, 0.0),
            Vec2::new(0.0, -28.0),
            Vec2::new(-19.0, 0.0),
            Vec2::new(-10.0, 0.0),
        ];
        let indices = [0, 1, 2, 0, 2, 6, 6, 2, 4, 2, 3, 4, 4, 5, 6];
        let mesh = self.add_mesh(polygon_mesh(&arrow, &indices));
        let position = (destination + Vec2::Y * 15.0).extend(2.0);
        self.spawn(
            mesh,
            self.art.goal.clone(),
            Transform::from_translation(position),
        );
    }

    fn lights(&mut self, level: &Level) {
        for light in &level.lights {
            let mesh = self.add_mesh(placeholder_mesh());
            let material = self.add_light(light.color.to_linear(), light.origin, light.range);
            let depth = if light.background { -20.0 } else { -9.0 };
            self.spawn(
                mesh.clone(),
                material.clone(),
                Transform::from_xyz(0.0, 0.0, depth),
            );
            self.art.harmless.push((mesh, material));
        }
    }

    fn agents(&mut self, level: &Level) {
        for (index, sensor) in level.sensors.iter().enumerate() {
            let cone = self.add_mesh(placeholder_mesh());
            let material = self.add_light(LinearRgba::WHITE, sensor.origin, sensor.range);
            let depth = -8.0 + index as f32 * 0.01;
            self.spawn(
                cone.clone(),
                material.clone(),
                Transform::from_xyz(0.0, 0.0, depth),
            );
            self.art.cones.push((cone, material));
            // Bodies are meshed around the agent's origin so they can rotate in place.
            let color = self.add_color(sensor.color);
            for shape in &sensor.body {
                let vertices: Vec<_> = shape.vertices.iter().map(|p| *p - sensor.origin).collect();
                let mesh = self.add_mesh(polygon_mesh(&vertices, &shape.triangles));
                let transform = Transform::from_translation(sensor.origin.extend(0.5));
                self.spawn(mesh, color.clone(), transform)
                    .insert(AgentVisual(index));
            }
        }
    }

    fn hints(&mut self, level: &Level) {
        for hint in &level.hints {
            self.commands.spawn((
                LevelVisual,
                Text2d::new(hint.text.clone()),
                TextFont::from(self.font.clone())
                    .with_font_size(hint.size)
                    .with_font_smoothing(FontSmoothing::None),
                TextColor(hint.color),
                LineHeight::Px(hint.size * 1.875),
                TextLayout::justify(Justify::Left),
                Anchor::TOP_LEFT,
                Transform::from_translation(hint.position.extend(5.0)),
            ));
        }
    }

    /// Object IDs shown with F1. Agent and crate labels follow their objects.
    fn labels(&mut self, level: &Level) {
        let fixed = [
            ("spawn", level.spawn),
            ("goal", level.destination),
            ("camera-bounds", level.camera.max),
        ];
        let labels = fixed
            .into_iter()
            .chain(level.sensors.iter().map(|s| (s.id.as_str(), s.origin)))
            .chain(level.cubes.iter().map(|c| (c.id.as_str(), c.position)))
            .chain(level.lights.iter().map(|l| (l.id.as_str(), l.origin)))
            .chain(level.hints.iter().map(|h| (h.id.as_str(), h.position)))
            .chain(
                level
                    .terrain_markers
                    .iter()
                    .map(|(id, p)| (id.as_str(), *p)),
            );
        for (id, position) in labels {
            self.commands.spawn((
                LevelVisual,
                DebugLabel,
                Text2d::new(id),
                TextFont::from(self.font.clone()).with_font_size(10.0),
                TextColor(Color::WHITE),
                Transform::from_translation((position + LABEL_OFFSET).extend(10.0)),
                Visibility::Hidden,
            ));
        }
    }
}

/// Spy position between fixed physics steps, so motion stays smooth at any frame rate.
fn interpolated(spy: &Spy, run: &Run, time: &Time<Fixed>) -> Vec2 {
    if run.status == Status::Playing {
        spy.previous.lerp(spy.current, time.overstep_fraction())
    } else {
        spy.current
    }
}

fn follow_camera(
    level: Res<Level>,
    run: Res<Run>,
    time: Res<Time<Fixed>>,
    spies: Query<&Spy>,
    windows: Query<&Window>,
    mut cameras: Query<&mut Transform, With<Camera2d>>,
) {
    let Ok(spy) = spies.single() else {
        return;
    };
    let aspect = windows
        .single()
        .map_or(1280.0 / 720.0, |w| w.width() / w.height().max(1.0));
    let center = level.camera.camera_center(
        interpolated(spy, &run, &time),
        Vec2::new(720.0 * aspect, 720.0),
    );
    for mut camera in &mut cameras {
        camera.translation.x = center.x;
        camera.translation.y = center.y;
    }
}

#[expect(
    clippy::type_complexity,
    reason = "Disjoint queries prevent mutable Transform aliasing"
)]
fn update_agents(
    level: Res<Level>,
    run: Res<Run>,
    debug: Res<DebugView>,
    mut agents: Query<(&AgentVisual, &mut Transform)>,
    cubes: Query<(&LooseCube, &Transform), (Without<DebugLabel>, Without<AgentVisual>)>,
    mut labels: Query<
        (&mut Visibility, &Text2d, &mut Transform),
        (With<DebugLabel>, Without<AgentVisual>),
    >,
) {
    for (agent, mut transform) in &mut agents {
        let sensor = &level.sensors[agent.0];
        transform.translation = sensor.origin_at(run.elapsed).extend(0.5);
        transform.rotation = Quat::from_rotation_z(sensor.facing(run.elapsed) - sensor.angle);
    }
    for (mut visibility, text, mut transform) in &mut labels {
        *visibility = if debug.0 {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };
        let sensor = level.sensors.iter().find(|s| s.id == text.0);
        let cube = cubes.iter().find(|(cube, _)| cube.id == text.0);
        let position = sensor
            .map(|sensor| sensor.origin_at(run.elapsed))
            .or(cube.map(|(_, body)| body.translation.truncate()));
        if let Some(position) = position {
            transform.translation = (position + LABEL_OFFSET).extend(10.0);
        }
    }
}

fn attach_body_art(
    mut commands: Commands,
    art: Res<Art>,
    level: Res<Level>,
    spies: Query<Entity, Added<Spy>>,
    cubes: Query<(Entity, &LooseCube), Added<LooseCube>>,
    debris: Query<Entity, Added<Debris>>,
) {
    for entity in &spies {
        commands.entity(entity).with_children(|parent| {
            parent
                .spawn((SpyVisual, Transform::default(), Visibility::default()))
                .with_children(|ball| {
                    ball.spawn((
                        Mesh2d(art.ball.clone()),
                        MeshMaterial2d(art.dark.clone()),
                        Transform::default(),
                    ));
                    ball.spawn((
                        Mesh2d(art.half_ball.clone()),
                        MeshMaterial2d(art.light.clone()),
                        Transform::from_xyz(0.0, 0.0, 0.1),
                    ));
                });
        });
    }
    for (entity, cube) in &cubes {
        commands
            .entity(entity)
            .insert(Sprite::from_color(level.palette.crate_color, cube.size));
    }
    for entity in &debris {
        commands.entity(entity).insert((
            Mesh2d(art.debris.clone()),
            MeshMaterial2d(art.death.clone()),
        ));
    }
}

fn update_spy_art(
    time: Res<Time<Fixed>>,
    run: Res<Run>,
    spies: Query<&Spy>,
    mut visuals: Query<(&ChildOf, &mut Transform, &mut Visibility), With<SpyVisual>>,
) {
    for (parent, mut transform, mut visibility) in &mut visuals {
        let Ok(spy) = spies.get(parent.parent()) else {
            continue;
        };
        transform.translation = (interpolated(spy, &run, &time) - spy.current).extend(0.0);
        transform.rotation = Quat::from_rotation_z(spy.roll + 0.7);
        // Menus show the title scene or a dimmed level, without the spy.
        let hidden = matches!(
            run.status,
            Status::Dead | Status::Welcome | Status::MainMenu | Status::LevelSelect
        );
        *visibility = if hidden {
            Visibility::Hidden
        } else {
            Visibility::Inherited
        };
    }
}

/// Light from a lamp behind the terrain plane, like a backlit sign. Terrain absorbs light rather than
/// blocking it outright, so thin shapes cast soft streaks while rows of them block most light, and
/// gaps between shapes become rays radiating from the lamp. Built from boundary segments, without
/// physics queries.
fn backlight_mesh(level: &Level, light: &LightDefinition) -> Mesh {
    const DIRECTIONS: usize = 1440;
    // Dense rings near the light resolve the terrain it shines through.
    const RINGS: [f32; 12] = [
        0.0, 0.02, 0.04, 0.07, 0.1, 0.15, 0.22, 0.32, 0.45, 0.62, 0.8, 1.0,
    ];
    // Depth of terrain that dims the light to 1/e.
    const ABSORPTION: f32 = 6.0;
    let mut vertices = Vec::with_capacity(DIRECTIONS * RINGS.len());
    let mut colors = Vec::with_capacity(DIRECTIONS * RINGS.len());
    for i in 0..DIRECTIONS {
        // Half-step angles keep rays off exact grid diagonals, where they would graze corners.
        let direction = Vec2::from_angle((i as f32 + 0.5) / DIRECTIONS as f32 * TAU);
        let blocked = blocked_spans(level, light.origin, direction);
        for ring in RINGS {
            let distance = ring * light.range;
            let depth: f32 = blocked
                .iter()
                .map(|&(start, end)| end.min(distance) - start.min(distance))
                .sum();
            let point = light.origin + direction * distance;
            vertices.push([point.x, point.y, 0.0]);
            colors.push([1.0, 1.0, 1.0, (-depth / ABSORPTION).exp()]);
        }
    }
    let rings = RINGS.len() as u32;
    let indices = (0..DIRECTIONS as u32)
        .flat_map(|i| {
            let next = (i + 1) % DIRECTIONS as u32;
            (0..rings - 1).flat_map(move |j| {
                let [a, b] = [i * rings + j, i * rings + j + 1];
                let [c, d] = [next * rings + j, next * rings + j + 1];
                [a, c, d, a, d, b]
            })
        })
        .collect();
    mesh(vertices, colors, indices)
}

/// Distances along a ray where it is inside terrain, as sorted (enter, exit) pairs.
fn blocked_spans(level: &Level, origin: Vec2, direction: Vec2) -> Vec<(f32, f32)> {
    let mut crossings: Vec<f32> = level
        .surfaces
        .iter()
        .filter_map(|surface| {
            let edge = surface.b - surface.a;
            let denominator = direction.perp_dot(edge);
            if denominator.abs() < 1e-6 {
                return None;
            }
            let offset = surface.a - origin;
            let distance = offset.perp_dot(edge) / denominator;
            // Half-open edges count a ray through a shared vertex once.
            let along = offset.perp_dot(direction) / denominator;
            (distance > 0.0 && (0.0..1.0).contains(&along)).then_some(distance)
        })
        .collect();
    crossings.sort_by(f32::total_cmp);
    // An odd count means the ray starts inside terrain.
    if crossings.len() % 2 == 1 {
        crossings.insert(0, 0.0);
    }
    crossings
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&[enter, exit]| (enter, exit))
        .collect()
}

/// Fan of terrain ray hits around a light. Rays pass through and beside every terrain vertex
/// so shadow edges stay sharp, with regular samples filling the arc between them.
fn visibility_mesh(level: &Level, context: &RapierContext, light: &LightShape) -> Mesh {
    let LightShape {
        origin,
        facing,
        aperture,
        range,
    } = *light;
    let half = aperture * 0.5;
    let mut angles: Vec<f32> = (0..=96)
        .map(|i| -half + aperture * i as f32 / 96.0)
        .collect();
    for surface in &level.surfaces {
        let relative = ((surface.a - origin).to_angle() - facing + PI).rem_euclid(TAU) - PI;
        for offset in [-0.0001, 0.0, 0.0001] {
            let angle = relative + offset;
            if (-half..=half).contains(&angle) {
                angles.push(angle);
            }
        }
    }
    angles.sort_by(f32::total_cmp);
    let mut vertices = vec![[origin.x, origin.y, 0.0]];
    for angle in angles {
        let direction = Vec2::from_angle(facing + angle);
        let point = origin + direction * terrain_ray(context, origin, direction, range);
        vertices.push([point.x, point.y, 0.0]);
    }
    let indices = (1..vertices.len() as u32 - 1)
        .flat_map(|i| [0, i, i + 1])
        .collect();
    let colors = vec![[1.0; 4]; vertices.len()];
    mesh(vertices, colors, indices)
}

fn draw_lights(
    level: Res<Level>,
    run: Res<Run>,
    art: Res<Art>,
    context: ReadRapierContext,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<LightMaterial>>,
    mut cache: Local<LightCache>,
) {
    if level.is_changed() {
        *cache = LightCache::default();
    }
    if !run.ready || (cache.static_ready && cache.last_elapsed == Some(run.elapsed)) {
        return;
    }
    let Ok(context) = context.single() else {
        return;
    };
    cache.last_elapsed = Some(run.elapsed);
    // Harmless lights never move, so their shadows are built once per level.
    if !cache.static_ready {
        for (light, (handle, _)) in level.lights.iter().zip(&art.harmless) {
            let shape = LightShape {
                origin: light.origin,
                facing: light.angle,
                aperture: light.aperture,
                range: light.range,
            };
            if let Some(mut mesh) = meshes.get_mut(handle) {
                *mesh = if light.behind {
                    backlight_mesh(&level, light)
                } else {
                    visibility_mesh(&level, &context, &shape)
                };
            }
        }
        cache.static_ready = true;
    }
    for (sensor, (handle, material)) in level.sensors.iter().zip(&art.cones) {
        let color = match sensor.phase_at(run.elapsed) {
            SensorPhase::Safe => level.palette.sensor_safe,
            SensorPhase::Warning => level.palette.sensor_warning,
            SensorPhase::Danger => level.palette.sensor_danger,
        };
        let shape = LightShape {
            origin: sensor.origin_at(run.elapsed),
            facing: sensor.facing(run.elapsed),
            aperture: sensor.aperture,
            range: sensor.range,
        };
        if let Some(mut material) = materials.get_mut(material) {
            material.color = color.to_linear();
            material.origin_and_range = shape.origin.extend(shape.range).extend(0.0);
        }
        if let Some(mut mesh) = meshes.get_mut(handle) {
            *mesh = visibility_mesh(&level, &context, &shape);
        }
    }
}

fn update_trail(
    level: Res<Level>,
    run: Res<Run>,
    art: Res<Art>,
    spies: Query<Ref<Spy>>,
    mut meshes: ResMut<Assets<Mesh>>,
) {
    let Ok(spy) = spies.single() else {
        return;
    };
    if !spy.is_changed() {
        return;
    }
    let trail = level.palette.trail;
    let points: Vec<_> = spy.trail.iter().collect();
    let segments = points.windows(2).map(|pair| {
        let (a, time) = *pair[0];
        let (b, _) = *pair[1];
        let fade = (1.0 - (run.elapsed - time) / TRAIL_SECONDS).clamp(0.0, 1.0) * 0.65;
        let color = trail.with_alpha(trail.alpha() * fade).to_linear();
        (a, b, color.to_f32_array())
    });
    if let Some(mut mesh) = meshes.get_mut(&art.trail) {
        *mesh = strip_mesh(segments, 0.45);
    }
}

fn animate_particles(
    mut commands: Commands,
    time: Res<Time>,
    run: Res<Run>,
    art: Res<Art>,
    level: Res<Level>,
    mut previous: Local<Status>,
    mut particles: Query<(Entity, &mut Transform, &mut Particle)>,
) {
    if run.status == Status::Complete && *previous != run.status {
        for i in 0..32 {
            // Golden-angle spacing spreads particles evenly without randomness.
            let angle = i as f32 * 2.399_963_1;
            commands.spawn((
                AttemptEntity,
                Particle {
                    velocity: Vec2::from_angle(angle) * (35.0 + (i % 9) as f32 * 15.0),
                    life: PARTICLE_LIFE,
                },
                Mesh2d(art.particle.clone()),
                MeshMaterial2d(art.success.clone()),
                Transform::from_translation(level.destination.extend(3.0)),
            ));
        }
    }
    *previous = run.status;
    if run.status == Status::Paused {
        return;
    }
    for (entity, mut transform, mut particle) in &mut particles {
        particle.life -= time.delta_secs();
        transform.translation += (particle.velocity * time.delta_secs()).extend(0.0);
        transform.scale = Vec3::splat(particle.life.max(0.0) / PARTICLE_LIFE);
        if particle.life <= 0.0 {
            commands.entity(entity).despawn();
        }
    }
}

fn draw_debug(
    debug: Res<DebugView>,
    level: Res<Level>,
    context: ReadRapierContext,
    bodies: Query<(Entity, &Transform, &GravityBody, &Velocity)>,
    mut gizmos: Gizmos,
) {
    if !debug.0 {
        return;
    }
    let route = Color::srgb(0.2, 0.8, 1.0);
    for patrol in level.sensors.iter().filter_map(|s| s.patrol.as_ref()) {
        for segment in patrol.segments() {
            gizmos.line_2d(segment.a, segment.b, route);
        }
    }
    for contour in level.goal.iter().flat_map(|shape| &shape.contours) {
        for edge in polygon_surfaces(contour) {
            gizmos.line_2d(edge.a, edge.b, Color::srgb(0.7, 0.3, 1.0));
        }
    }
    let camera = level.camera;
    gizmos.rect_2d(
        Isometry2d::from_translation(camera.center()),
        camera.max - camera.min,
        route,
    );
    let context = context.single().ok();
    for (entity, transform, body, velocity) in &bodies {
        let p = transform.translation.truncate();
        let arrow = body.radius * 4.0;
        if let Some(nearest) = nearest_surface(&level.surfaces, p, body.surface) {
            let surface = level.surfaces[nearest.id];
            gizmos.line_2d(surface.a, surface.b, Color::srgb(1.0, 0.3, 0.6));
            gizmos.line_2d(p, nearest.point, Color::srgb(1.0, 0.8, 0.2));
            gizmos.circle_2d(nearest.point, 3.0, Color::srgb(1.0, 0.8, 0.2));
            gizmos.arrow_2d(p, p + nearest.down * arrow, Color::srgb(0.3, 0.8, 1.0));
        }
        gizmos.arrow_2d(p, p + velocity.linear * 0.15, Color::srgb(0.4, 1.0, 0.4));
        let Some(context) = &context else {
            continue;
        };
        for pair in context.contact_pairs_with(entity) {
            // Rapier normals point from the first collider toward the second.
            let sign = if pair.collider1() == Some(entity) {
                -1.0
            } else {
                1.0
            };
            for manifold in pair.manifolds() {
                for contact in manifold.solver_contacts() {
                    gizmos.arrow_2d(
                        contact.point(),
                        contact.point() + manifold.normal() * sign * arrow * 0.5,
                        Color::srgb(1.0, 0.5, 0.1),
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backlight_spans_measure_terrain_depth_from_outside_and_inside() {
        let level = Level::parse(r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="-300 -300 600 600" data-gs-id="spans"><title>Spans</title><circle id="spawn" cx="-200" cy="-200" r="16"/><circle id="goal" cx="200" cy="-200" r="20"/><rect id="terrain" x="20" y="-10" width="30" height="20"/></svg>"#).unwrap();
        let outside = blocked_spans(&level, Vec2::ZERO, Vec2::X);
        assert_eq!(outside.len(), 1);
        assert!((outside[0].0 - 20.0).abs() < 1e-4 && (outside[0].1 - 50.0).abs() < 1e-4);
        let inside = blocked_spans(&level, Vec2::new(30.0, 0.0), Vec2::X);
        assert_eq!(inside.len(), 1);
        assert!(inside[0].0 == 0.0 && (inside[0].1 - 20.0).abs() < 1e-4);
        assert!(blocked_spans(&level, Vec2::ZERO, Vec2::Y).is_empty());
    }

    #[test]
    fn switching_levels_releases_generated_meshes_materials_and_labels() {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()))
            .init_asset::<Mesh>()
            .init_asset::<ColorMaterial>()
            .init_asset::<LightMaterial>()
            .init_asset::<Font>()
            .insert_resource(ClearColor(Color::BLACK))
            .insert_resource(Level::tutorial())
            .add_systems(Startup, setup_art)
            .add_systems(Update, rebuild_art);
        app.update();
        fn counts(app: &mut App) -> (usize, usize, usize, usize) {
            let world = app.world_mut();
            (
                world.resource::<Assets<Mesh>>().len(),
                world.resource::<Assets<ColorMaterial>>().len(),
                world.resource::<Assets<LightMaterial>>().len(),
                world
                    .query_filtered::<Entity, With<LevelVisual>>()
                    .iter(world)
                    .count(),
            )
        }
        let tutorial = counts(&mut app);
        app.insert_resource(Level::parse(include_str!("../assets/levels/level-1.svg")).unwrap());
        app.update();
        let one = counts(&mut app);
        for _ in 0..10 {
            app.insert_resource(Level::tutorial());
            app.update();
            assert_eq!(counts(&mut app), tutorial);
            app.insert_resource(
                Level::parse(include_str!("../assets/levels/level-1.svg")).unwrap(),
            );
            app.update();
            assert_eq!(counts(&mut app), one);
            app.insert_resource(
                Level::parse(include_str!("../assets/authoring/template.svg")).unwrap(),
            );
            app.update();
            assert!(!app.world().resource::<Art>().owned_colors.is_empty());
        }
    }
}
