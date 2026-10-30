use bevy::prelude::*;

#[derive(Clone, Copy, Debug)]
pub struct Surface {
    pub a: Vec2,
    pub b: Vec2,
}

impl Surface {
    pub fn nearest(self, point: Vec2) -> Vec2 {
        let edge = self.b - self.a;
        self.a + edge * ((point - self.a).dot(edge) / edge.length_squared()).clamp(0.0, 1.0)
    }

    pub fn outward(self) -> Vec2 {
        let edge = self.b - self.a;
        Vec2::new(edge.y, -edge.x).normalize()
    }
}

#[derive(Debug, Clone, Copy)]
pub struct NearestSurface {
    pub id: usize,
    pub point: Vec2,
    pub down: Vec2,
    pub distance: f32,
}

pub fn nearest_surface(
    surfaces: &[Surface],
    position: Vec2,
    previous: Option<usize>,
) -> Option<NearestSurface> {
    let mut best: Option<NearestSurface> = None;
    for (id, surface) in surfaces.iter().enumerate() {
        let point = surface.nearest(position);
        let offset = point - position;
        let distance = offset.length();
        if best.is_some_and(|old| {
            distance > old.distance + 0.0001
                || ((distance - old.distance).abs() <= 0.0001 && previous != Some(id))
        }) {
            continue;
        }
        best = Some(NearestSurface {
            id,
            point,
            down: offset.try_normalize().unwrap_or(-surface.outward()),
            distance,
        });
    }
    best
}

pub fn polygon_surfaces(vertices: &[Vec2]) -> impl Iterator<Item = Surface> + '_ {
    vertices.iter().enumerate().map(|(index, &a)| Surface {
        a,
        b: vertices[(index + 1) % vertices.len()],
    })
}

pub fn segment_distance(a: Surface, b: Surface) -> f32 {
    let first = a.b - a.a;
    let second = b.b - b.a;
    let denominator = first.perp_dot(second);
    if denominator.abs() > f32::EPSILON {
        let offset = b.a - a.a;
        let t = offset.perp_dot(second) / denominator;
        let u = offset.perp_dot(first) / denominator;
        if (0.0..=1.0).contains(&t) && (0.0..=1.0).contains(&u) {
            return 0.0;
        }
    }
    [
        a.nearest(b.a).distance(b.a),
        a.nearest(b.b).distance(b.b),
        b.nearest(a.a).distance(a.a),
        b.nearest(a.b).distance(a.b),
    ]
    .into_iter()
    .fold(f32::INFINITY, f32::min)
}

pub fn signed_area(vertices: &[Vec2]) -> f32 {
    polygon_surfaces(vertices)
        .map(|edge| edge.a.perp_dot(edge.b))
        .sum::<f32>()
        * 0.5
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segment_distance_covers_crossings_parallel_edges_and_endpoints() {
        let a = Surface {
            a: Vec2::ZERO,
            b: Vec2::new(10.0, 0.0),
        };
        for (b, expected) in [
            (
                Surface {
                    a: Vec2::new(5.0, -2.0),
                    b: Vec2::new(5.0, 2.0),
                },
                0.0,
            ),
            (
                Surface {
                    a: Vec2::new(2.0, 3.0),
                    b: Vec2::new(8.0, 3.0),
                },
                3.0,
            ),
            (
                Surface {
                    a: Vec2::new(5.0, 0.0),
                    b: Vec2::new(15.0, 0.0),
                },
                0.0,
            ),
            (
                Surface {
                    a: Vec2::new(13.0, 4.0),
                    b: Vec2::new(13.0, 8.0),
                },
                5.0,
            ),
        ] {
            assert!((segment_distance(a, b) - expected).abs() < 0.001);
        }
    }

    #[test]
    fn gravity_points_toward_the_nearest_surface_in_each_fixture() {
        fn down(vertices: &[Vec2], position: Vec2) -> Vec2 {
            let surfaces = polygon_surfaces(vertices).collect::<Vec<_>>();
            nearest_surface(&surfaces, position, None).unwrap().down
        }
        // A counter-clockwise box from (0, 0) to (100, 40).
        let block = [
            Vec2::ZERO,
            Vec2::new(100.0, 0.0),
            Vec2::new(100.0, 40.0),
            Vec2::new(0.0, 40.0),
        ];
        let diamond = [
            Vec2::new(0.0, -50.0),
            Vec2::new(50.0, 0.0),
            Vec2::new(0.0, 50.0),
            Vec2::new(-50.0, 0.0),
        ];
        let slope = [Vec2::ZERO, Vec2::new(100.0, 0.0), Vec2::new(100.0, 100.0)];
        // An L-shaped basin whose inner corner sits at (20, 20).
        let corner = [
            Vec2::ZERO,
            Vec2::new(100.0, 0.0),
            Vec2::new(100.0, 20.0),
            Vec2::new(20.0, 20.0),
            Vec2::new(20.0, 100.0),
            Vec2::new(0.0, 100.0),
        ];
        let diagonal = Vec2::new(1.0, -1.0).normalize();
        for (name, vertices, position, expected) in [
            ("floor", &block[..], Vec2::new(50.0, 60.0), Vec2::NEG_Y),
            ("ceiling", &block[..], Vec2::new(50.0, -20.0), Vec2::Y),
            ("left wall", &block[..], Vec2::new(-20.0, 20.0), Vec2::X),
            (
                "right wall",
                &block[..],
                Vec2::new(120.0, 20.0),
                Vec2::NEG_X,
            ),
            (
                "outside corner",
                &block[..],
                Vec2::new(110.0, 50.0),
                -diagonal.perp(),
            ),
            (
                "diamond face",
                &diamond[..],
                Vec2::new(40.0, 40.0),
                -Vec2::ONE.normalize(),
            ),
            (
                "diamond tip",
                &diamond[..],
                Vec2::new(0.0, 80.0),
                Vec2::NEG_Y,
            ),
            ("slope", &slope[..], Vec2::new(30.0, 70.0), diagonal),
            (
                "concave floor",
                &corner[..],
                Vec2::new(60.0, 30.0),
                Vec2::NEG_Y,
            ),
            (
                "concave wall",
                &corner[..],
                Vec2::new(30.0, 60.0),
                Vec2::NEG_X,
            ),
        ] {
            let actual = down(vertices, position);
            assert!(actual.abs_diff_eq(expected, 0.001), "{name}: {actual}");
        }
    }

    #[test]
    fn narrow_gap_pulls_toward_the_closer_side() {
        let surfaces = [
            Surface {
                a: Vec2::new(100.0, 0.0),
                b: Vec2::ZERO,
            },
            Surface {
                a: Vec2::new(0.0, 40.0),
                b: Vec2::new(100.0, 40.0),
            },
        ];
        for (y, expected) in [(15.0, Vec2::NEG_Y), (25.0, Vec2::Y)] {
            let nearest = nearest_surface(&surfaces, Vec2::new(50.0, y), None).unwrap();
            assert_eq!(nearest.down, expected);
        }
        // The exact midpoint keeps whichever side was selected last.
        for previous in [0, 1] {
            let nearest = nearest_surface(&surfaces, Vec2::new(50.0, 20.0), Some(previous));
            assert_eq!(nearest.unwrap().id, previous);
        }
    }
}
