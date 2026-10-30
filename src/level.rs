use std::{f32::consts::TAU, fs, path::Path};

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use crate::{
    geometry::{Surface, nearest_surface, segment_distance},
    svg,
};

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct LevelId(pub String);

#[derive(Clone, Debug)]
pub struct Patrol {
    pub points: Vec<Vec2>,
    pub closed: bool,
    pub speed: f32,
}

impl Patrol {
    /// Route segments in travel order; closed routes include the segment back to the start.
    pub fn segments(&self) -> impl Iterator<Item = Surface> + '_ {
        let count = self.points.len() - usize::from(!self.closed);
        (0..count).map(|i| Surface {
            a: self.points[i],
            b: self.points[(i + 1) % self.points.len()],
        })
    }

    pub fn length(&self) -> f32 {
        self.segments().map(|s| s.a.distance(s.b)).sum()
    }

    /// Distance traveled before returning to the starting pose. Open routes travel out and back.
    fn lap(&self) -> f32 {
        self.length() * if self.closed { 1.0 } else { 2.0 }
    }

    #[cfg(test)]
    pub fn period(&self) -> f32 {
        self.lap() / self.speed
    }

    pub fn pose(&self, elapsed: f32) -> (Vec2, f32) {
        let length = self.length();
        let mut distance = (elapsed * self.speed).rem_euclid(self.lap());
        let reverse = !self.closed && distance >= length;
        if reverse {
            distance = 2.0 * length - distance;
        }
        let mut segments = self.segments().peekable();
        while let Some(Surface { a, b }) = segments.next() {
            let segment = a.distance(b);
            if distance < segment || segments.peek().is_none() {
                let heading = if reverse { a - b } else { b - a };
                return (a.lerp(b, distance / segment), heading.to_angle());
            }
            distance -= segment;
        }
        unreachable!("validated patrol has at least one nonzero segment")
    }
}

#[derive(Clone, Debug)]
pub struct Sensor {
    pub id: String,
    pub color: Color,
    pub origin: Vec2,
    pub angle: f32,
    pub sweep: f32,
    pub period: f32,
    pub phase: f32,
    pub range: f32,
    pub aperture: f32,
    pub patrol: Option<Patrol>,
    pub body: Vec<Terrain>,
}

impl Sensor {
    /// Smallest distance between the agent body and terrain at any pose; negative when buried.
    ///
    /// A bounding disk covers the body at every facing, including patrol turns and sweeps.
    pub fn terrain_clearance(&self, terrain: &[Terrain], surfaces: &[Surface]) -> f32 {
        let radius = self
            .body
            .iter()
            .flat_map(|shape| &shape.vertices)
            .map(|vertex| vertex.distance(self.origin))
            .fold(0.0, f32::max);
        let distance = nearest_surface(surfaces, self.origin, None)
            .map_or(f32::INFINITY, |nearest| nearest.distance);
        if terrain.iter().any(|shape| shape.contains(self.origin)) {
            return -distance - radius;
        }
        let route_distance = self
            .patrol
            .iter()
            .flat_map(Patrol::segments)
            .flat_map(|segment| {
                surfaces
                    .iter()
                    .map(move |surface| segment_distance(segment, *surface))
            });
        route_distance.fold(distance, f32::min) - radius
    }

    pub fn origin_at(&self, elapsed: f32) -> Vec2 {
        self.patrol
            .as_ref()
            .map_or(self.origin, |patrol| patrol.pose(elapsed).0)
    }

    pub fn facing(&self, elapsed: f32) -> f32 {
        let angle = self
            .patrol
            .as_ref()
            .map_or(self.angle, |patrol| patrol.pose(elapsed).1);
        angle + self.sweep * ((elapsed / self.period + self.phase) * TAU).sin()
    }

    /// Agents cycle safe, warning, then danger. Each clock starts at the beginning of the safe
    /// part and `phase` delays it further, so the first danger begins at `period * (1 + phase)`.
    pub fn phase_at(&self, elapsed: f32) -> SensorPhase {
        let cycle = elapsed / self.period - (1.0 - DANGER_START) - self.phase;
        if cycle < 0.0 {
            return SensorPhase::Safe;
        }
        let cycle = cycle.fract();
        if cycle < WARNING_START {
            SensorPhase::Safe
        } else if cycle < DANGER_START {
            SensorPhase::Warning
        } else {
            SensorPhase::Danger
        }
    }
}

/// Seconds in an agent's safe, warning, and danger cycle.
pub const AGENT_PERIOD: f32 = 10.0;
/// Fractions of the cycle where warning and danger begin.
const WARNING_START: f32 = 0.52;
const DANGER_START: f32 = 0.68;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SensorPhase {
    Safe,
    Warning,
    Danger,
}

/// A triangulated filled region. `contours` holds the outer boundary first, then holes.
#[derive(Clone, Debug)]
pub struct Terrain {
    pub vertices: Vec<Vec2>,
    pub triangles: Vec<u32>,
    pub contours: Vec<Vec<Vec2>>,
}

impl Terrain {
    pub fn triangles(&self) -> impl Iterator<Item = [Vec2; 3]> + '_ {
        self.triangles
            .as_chunks::<3>()
            .0
            .iter()
            .map(|triangle| triangle.map(|index| self.vertices[index as usize]))
    }

    pub fn contains(&self, point: Vec2) -> bool {
        self.triangles().any(|[a, b, c]| {
            let cross = [
                (b - a).perp_dot(point - a),
                (c - b).perp_dot(point - b),
                (a - c).perp_dot(point - c),
            ];
            cross.iter().all(|x| *x >= -0.001) || cross.iter().all(|x| *x <= 0.001)
        })
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Bounds {
    pub min: Vec2,
    pub max: Vec2,
}

impl Bounds {
    pub fn around<'a>(points: impl IntoIterator<Item = &'a Vec2>) -> Self {
        points.into_iter().fold(
            Self {
                min: Vec2::INFINITY,
                max: Vec2::NEG_INFINITY,
            },
            |bounds, point| Self {
                min: bounds.min.min(*point),
                max: bounds.max.max(*point),
            },
        )
    }

    pub fn of_shapes(shapes: &[Terrain]) -> Self {
        Self::around(shapes.iter().flat_map(|shape| &shape.vertices))
    }

    pub fn center(self) -> Vec2 {
        (self.min + self.max) * 0.5
    }

    pub fn contains(self, point: Vec2, margin: f32) -> bool {
        point.cmpge(self.min - Vec2::splat(margin)).all()
            && point.cmple(self.max + Vec2::splat(margin)).all()
    }

    pub fn camera_center(self, target: Vec2, viewport: Vec2) -> Vec2 {
        let available = ((self.max - self.min - viewport) * 0.5).max(Vec2::ZERO);
        target.clamp(self.center() - available, self.center() + available)
    }
}

#[derive(Clone, Debug)]
pub struct CrateDefinition {
    pub id: String,
    pub position: Vec2,
    pub size: Vec2,
    pub angle: f32,
}

#[derive(Clone, Debug)]
pub struct LightDefinition {
    pub id: String,
    pub origin: Vec2,
    pub range: f32,
    pub angle: f32,
    pub aperture: f32,
    pub color: Color,
    pub background: bool,
    /// Shines from behind the terrain plane, so terrain casts streaks instead of enclosing it.
    pub behind: bool,
}

#[derive(Clone, Debug)]
pub struct Hint {
    pub id: String,
    pub color: Color,
    pub position: Vec2,
    pub text: String,
    pub size: f32,
}

#[derive(Clone, Debug)]
pub struct Palette {
    pub sensor_safe: Color,
    pub sensor_warning: Color,
    pub sensor_danger: Color,
    pub crate_color: Color,
    pub trail: Color,
    pub goal: Color,
    pub death: Color,
    pub success: Color,
    pub background: Color,
    pub terrain: Color,
    pub outline: Color,
    pub spy_dark: Color,
    pub spy_light: Color,
}

#[derive(Clone, Debug)]
#[cfg_attr(
    target_arch = "wasm32",
    expect(
        dead_code,
        reason = "Source references are used by desktop authoring and validation."
    )
)]
pub struct Reference {
    pub source: String,
    pub start: f32,
    pub end: f32,
    pub notes: String,
}

#[derive(Clone, Debug)]
pub struct TerrainPaint {
    pub shapes: Vec<Terrain>,
    pub color: Color,
}

#[derive(Resource, Clone, Copy, Debug)]
pub struct MovementSettings {
    pub gravity: f32,
    pub acceleration: f32,
    pub max_speed: f32,
    pub jump_speed: f32,
}

impl Default for MovementSettings {
    fn default() -> Self {
        Self {
            gravity: 850.0,
            acceleration: 1450.0,
            max_speed: 225.0,
            jump_speed: 340.0,
        }
    }
}

#[derive(Resource, Clone, Debug)]
pub struct Level {
    pub id: LevelId,
    pub order: i32,
    pub name: String,
    pub spawn: Vec2,
    pub destination: Vec2,
    pub goal: Vec<Terrain>,
    pub terrain: Vec<Terrain>,
    pub surfaces: Vec<Surface>,
    /// IDs of named terrain shapes and their centers, shown as debug labels.
    pub terrain_markers: Vec<(String, Vec2)>,
    pub cubes: Vec<CrateDefinition>,
    pub sensors: Vec<Sensor>,
    pub lights: Vec<LightDefinition>,
    pub hints: Vec<Hint>,
    pub bounds: Bounds,
    pub camera: Bounds,
    pub palette: Palette,
    #[cfg_attr(
        target_arch = "wasm32",
        expect(
            dead_code,
            reason = "Source references are used by desktop authoring and validation."
        )
    )]
    pub references: Vec<Reference>,
    pub movement: MovementSettings,
    pub terrain_paint: Vec<TerrainPaint>,
}

impl Level {
    pub fn parse(source: &str) -> Result<Self, String> {
        svg::parse(source)
    }

    pub fn load(path: &Path) -> Result<Self, String> {
        let source = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        Self::parse(&source).map_err(|e| format!("{}: {e}", path.display()))
    }

    #[cfg(test)]
    pub fn tutorial() -> Self {
        Self::parse(include_str!("../assets/levels/tutorial.svg")).unwrap()
    }
}

#[cfg(test)]
mod tests {
    use std::f32::consts::FRAC_PI_2;

    use super::*;

    #[test]
    fn patrol_distance_reverses_and_closed_routes_loop() {
        let mut patrol = Patrol {
            points: vec![Vec2::ZERO, Vec2::new(100.0, 0.0), Vec2::new(100.0, 100.0)],
            closed: false,
            speed: 50.0,
        };
        assert_eq!(patrol.pose(1.0), (Vec2::new(50.0, 0.0), 0.0));
        assert_eq!(patrol.pose(3.0), (Vec2::new(100.0, 50.0), FRAC_PI_2));
        assert_eq!(patrol.pose(5.0), (Vec2::new(100.0, 50.0), -FRAC_PI_2));
        assert_eq!(patrol.pose(8.0), (Vec2::ZERO, 0.0));
        patrol.closed = true;
        assert!(patrol.pose(patrol.period()).0.distance(Vec2::ZERO) < 0.0001);
    }

    #[test]
    fn camera_centers_small_bounds_and_clamps_large_bounds() {
        let bounds = Bounds {
            min: Vec2::new(-100.0, -100.0),
            max: Vec2::new(100.0, 1000.0),
        };
        assert_eq!(
            bounds.camera_center(Vec2::new(90.0, 999.0), Vec2::new(1280.0, 720.0)),
            Vec2::new(0.0, 640.0)
        );
    }
}
