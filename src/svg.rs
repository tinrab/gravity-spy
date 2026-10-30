//! SVG authoring boundary. Gameplay uses user coordinates, independent of SVG paint visibility.
use std::{
    collections::{BTreeMap, HashSet},
    f32::consts::TAU,
    fmt::Display,
};

use bevy::prelude::*;
use earcutr::earcut;
use i_overlay::{core::fill_rule::FillRule, float::simplify::SimplifyShape};
use kurbo::{Affine, BezPath, Ellipse, PathEl, Point, Rect, Shape, flatten};
use roxmltree::{Document, Node};
use svgtypes::{
    Color as SvgColor, NumberListParser, SimplePathSegment, SimplifyingPathParser,
    TransformListParser, TransformListToken, ViewBox,
};

use crate::{
    geometry::{polygon_surfaces, signed_area},
    level::{
        AGENT_PERIOD, Bounds, CrateDefinition, Hint, Level, LevelId, LightDefinition,
        MovementSettings, Palette, Patrol, Reference, Sensor, Terrain, TerrainPaint,
    },
    svg_document::{LEVEL_NAMESPACE, is_svg, normalize},
};

/// Largest supported coordinate magnitude, in world units.
const WORLD_LIMIT: f64 = 1_000_000.0;

type Contours = Vec<Vec<[f64; 2]>>;

pub fn parse(source: &str) -> Result<Level, String> {
    let normalized = normalize(source)?;
    let document = Document::parse(&normalized).map_err(|e| e.to_string())?;
    let root = document.root_element();
    if !root.has_tag_name("svg") {
        return Err("root element must be svg".into());
    }
    let id = root
        .attribute("data-gs-id")
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| error(root, "root needs a stable data-gs-id"))?;
    let name = root
        .children()
        .find(|n| n.has_tag_name("title"))
        .and_then(|n| n.text())
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| error(root, "root needs a title"))?;
    let order = root
        .attribute("data-gs-order")
        .unwrap_or("100")
        .parse::<i32>()
        .map_err(|e| error(root, e))?;
    let bounds = view_bounds(root)?;
    check_document(root)?;
    let palette = palette(root)?;

    let mut objects = Objects::default();
    for node in root.descendants().filter(Node::is_element).skip(1) {
        let hidden = node
            .ancestors()
            .any(|a| a.has_tag_name("defs") || a.has_tag_name("metadata"));
        if !hidden && is_svg(node) {
            objects.add(node, &palette)?;
        }
    }
    let Objects {
        terrain,
        terrain_markers,
        terrain_paint,
        spawn,
        goal,
        camera,
        cubes,
        mut sensors,
        routes,
        lights,
        hints,
    } = objects;
    attach_patrols(&mut sensors, routes)?;
    if terrain.is_empty() {
        return Err("level needs terrain in a terrain layer or terrain-* shapes".into());
    }
    // Resolve each element's fill first. NonZero then unions oriented outer/hole contours
    // across elements without cancelling their overlaps as global EvenOdd would.
    let contours: Contours = terrain.into_iter().flatten().collect();
    let terrain = triangulate(contours.simplify_shape(FillRule::NonZero))?;
    let surfaces: Vec<_> = terrain
        .iter()
        .flat_map(|t| &t.contours)
        .flat_map(|c| polygon_surfaces(c))
        .collect();
    let misplaced: Vec<_> = sensors
        .iter()
        .filter_map(|sensor| {
            let clearance = sensor.terrain_clearance(&terrain, &surfaces);
            (clearance <= 0.0).then(|| {
                format!(
                    "{}: agent body or swept patrol overlaps terrain, clearance {clearance:.2}; move the entire body outside solid surfaces",
                    sensor.id
                )
            })
        })
        .collect();
    if !misplaced.is_empty() {
        return Err(misplaced.join("\n"));
    }
    let spawn = spawn.ok_or("level needs one circle with id=spawn")?;
    let goal = goal.ok_or("level needs one closed shape with id=goal")?;
    if terrain.iter().any(|t| t.contains(spawn)) {
        return Err("spawn: center lies inside solid terrain".into());
    }
    if !bounds.contains(spawn, 0.0) {
        return Err("spawn: center lies outside viewBox".into());
    }
    Ok(Level {
        id: LevelId(id.into()),
        order,
        name: name.trim().into(),
        spawn,
        destination: Bounds::of_shapes(&goal).center(),
        goal,
        terrain,
        surfaces,
        terrain_markers,
        cubes,
        sensors,
        lights,
        hints,
        bounds,
        camera: camera.unwrap_or(bounds),
        palette,
        references: references(root)?,
        movement: movement(root)?,
        terrain_paint,
    })
}

/// Gameplay objects collected from the document, before terrain is unioned.
#[derive(Default)]
struct Objects<'a, 'input> {
    terrain: Vec<Contours>,
    terrain_markers: Vec<(String, Vec2)>,
    terrain_paint: Vec<TerrainPaint>,
    spawn: Option<Vec2>,
    goal: Option<Vec<Terrain>>,
    camera: Option<Bounds>,
    cubes: Vec<CrateDefinition>,
    sensors: Vec<Sensor>,
    routes: BTreeMap<String, (Patrol, Node<'a, 'input>)>,
    lights: Vec<LightDefinition>,
    hints: Vec<Hint>,
}

impl<'a, 'input> Objects<'a, 'input> {
    fn add(&mut self, node: Node<'a, 'input>, palette: &Palette) -> Result<(), String> {
        let id = node.attribute("id").unwrap_or("");
        if node.has_tag_name("g") {
            return check_group(node, id);
        }
        if id == "spawn" {
            self.spawn = Some(spawn(node)?);
        } else if id == "goal" {
            self.goal = Some(filled_mesh(node)?);
        } else if id == "camera-bounds" {
            if !node.has_tag_name("rect") {
                return Err(error(node, "camera-bounds must be a rectangle"));
            }
            self.camera = Some(Bounds::of_shapes(&filled_mesh(node)?));
        } else if id.starts_with("crate-") {
            self.cubes.push(crate_definition(node, id)?);
        } else if id.starts_with("agent-") {
            self.sensors.push(agent(node, id)?);
        } else if let Some(name) = id.strip_prefix("patrol-") {
            self.routes.insert(name.into(), (patrol(node)?, node));
        } else if id.starts_with("light-") {
            self.lights.push(light(node, id)?);
        } else if id.starts_with("hint-") {
            self.hints.push(hint(node, id)?);
        } else if in_terrain(node)
            && !matches!(node.tag_name().name(), "title" | "desc" | "metadata")
        {
            self.add_terrain(node, id, palette)?;
        }
        Ok(())
    }

    fn add_terrain(&mut self, node: Node, id: &str, palette: &Palette) -> Result<(), String> {
        let shapes = filled(node)?;
        if property(node, "fill").is_some_and(|fill| fill != "none") {
            let paint = color(node, "fill", "#456621")?;
            if paint != palette.terrain {
                self.terrain_paint.push(TerrainPaint {
                    shapes: triangulate(shapes.clone()).map_err(|e| error(node, e))?,
                    color: paint,
                });
            }
        }
        if !id.is_empty() {
            let points: Vec<_> = shapes.iter().flatten().flat_map(|c| points(c)).collect();
            self.terrain_markers
                .push((id.into(), Bounds::around(&points).center()));
        }
        self.terrain.extend(shapes);
        Ok(())
    }
}

fn is_terrain_id(id: &str) -> bool {
    id == "terrain" || id.starts_with("terrain-")
}

fn in_terrain(node: Node) -> bool {
    node.ancestors()
        .any(|n| n.attribute("id").is_some_and(is_terrain_id))
}

fn check_group(node: Node, id: &str) -> Result<(), String> {
    if is_terrain_id(id) {
        return check_effects(node);
    }
    let gameplay = matches!(id, "spawn" | "goal" | "camera-bounds")
        || ["crate-", "agent-", "patrol-", "light-", "hint-"]
            .iter()
            .any(|prefix| id.starts_with(prefix));
    if gameplay {
        return Err(error(
            node,
            "gameplay IDs belong on shapes; use terrain for a terrain group",
        ));
    }
    Ok(())
}

fn check_document(root: Node) -> Result<(), String> {
    let mut ids = HashSet::new();
    for node in root.descendants().filter(Node::is_element) {
        if node.ancestors().any(|a| a.has_tag_name("metadata")) {
            continue;
        }
        if let Some(id) = node.attribute("id")
            && !ids.insert(id)
        {
            return Err(error(node, "duplicate SVG id"));
        }
        if node.has_tag_name("style") && node.text().is_some_and(|s| !s.trim().is_empty()) {
            return Err(error(
                node,
                "CSS stylesheets are unsupported; use inline styles or presentation attributes",
            ));
        }
    }
    Ok(())
}

fn view_bounds(root: Node) -> Result<Bounds, String> {
    let vb: ViewBox = root
        .attribute("viewBox")
        .ok_or_else(|| error(root, "root needs viewBox"))?
        .parse()
        .map_err(|e| error(root, e))?;
    if [vb.x, vb.y, vb.w, vb.h]
        .iter()
        .any(|n| !n.is_finite() || n.abs() > WORLD_LIMIT)
        || vb.w <= 0.0
        || vb.h <= 0.0
    {
        return Err(error(
            root,
            "viewBox must have finite coordinates and positive dimensions",
        ));
    }
    Ok(Bounds {
        min: Vec2::new(vb.x as f32, (-vb.y - vb.h) as f32),
        max: Vec2::new((vb.x + vb.w) as f32, -vb.y as f32),
    })
}

fn palette(root: Node) -> Result<Palette, String> {
    let spy_light = root.attribute("data-gs-spy-light").unwrap_or("#a8ed52");
    let paint = |key: &str, default: &str| color(root, key, default);
    Ok(Palette {
        sensor_safe: paint("data-gs-sensor-safe", "#59ff1a")?.with_alpha(0.43),
        sensor_warning: paint("data-gs-sensor-warning", "#ffe040")?.with_alpha(0.35),
        sensor_danger: paint("data-gs-sensor-danger", "#ff381f")?.with_alpha(0.65),
        crate_color: paint("data-gs-crate", spy_light)?,
        trail: paint("data-gs-trail", "#d6f29e")?,
        goal: paint("data-gs-goal", spy_light)?,
        death: paint("data-gs-death", "#ffffff")?,
        success: paint("data-gs-success", "#f78ee9")?,
        background: paint("data-gs-background", "#060e03")?,
        terrain: paint("data-gs-terrain", "#456621")?,
        outline: paint("data-gs-outline", "#889f52")?.with_alpha(0.32),
        spy_dark: paint("data-gs-spy-dark", "#2e4c1a")?,
        spy_light: paint("data-gs-spy-light", "#a8ed52")?,
    })
}

fn movement(root: Node) -> Result<MovementSettings, String> {
    let defaults = MovementSettings::default();
    let setting = |key: &str, default: f32| positive(root, key, default as f64);
    Ok(MovementSettings {
        gravity: setting("data-gs-gravity", defaults.gravity)?,
        acceleration: setting("data-gs-acceleration", defaults.acceleration)?,
        max_speed: setting("data-gs-max-speed", defaults.max_speed)?,
        jump_speed: setting("data-gs-jump-speed", defaults.jump_speed)?,
    })
}

fn references(root: Node) -> Result<Vec<Reference>, String> {
    root.descendants()
        .filter(|n| n.has_tag_name((LEVEL_NAMESPACE, "reference")))
        .map(|node| {
            let start = number(node, "start", 0.0)? as f32;
            let end = number(node, "end", 0.0)? as f32;
            if start < 0.0 || end <= start {
                return Err(error(node, "reference needs 0 <= start < end, in seconds"));
            }
            Ok(Reference {
                source: node.attribute("source").unwrap_or("original-video").into(),
                start,
                end,
                notes: node.text().unwrap_or_default().trim().into(),
            })
        })
        .collect()
}

fn spawn(node: Node) -> Result<Vec2, String> {
    check_effects(node)?;
    if !node.has_tag_name("circle") {
        return Err(error(node, "spawn must be a circle"));
    }
    positive(node, "r", 0.0)?;
    point(node, "cx", "cy")
}

fn crate_definition(node: Node, id: &str) -> Result<CrateDefinition, String> {
    check_effects(node)?;
    if !node.has_tag_name("rect") {
        return Err(error(node, "crates must be rectangles"));
    }
    if number(node, "rx", 0.0)? != 0.0 || number(node, "ry", 0.0)? != 0.0 {
        return Err(error(node, "crates cannot have rounded corners"));
    }
    let x = number(node, "x", 0.0)?;
    let y = number(node, "y", 0.0)?;
    let w = positive(node, "width", 0.0)? as f64;
    let h = positive(node, "height", 0.0)? as f64;
    let t = transform(node)?;
    let [origin, right, down, corner] =
        [(x, y), (x + w, y), (x, y + h), (x + w, y + h)].map(|(x, y)| t * Point::new(x, y));
    let center = t * Point::new(x + w * 0.5, y + h * 0.5);
    let dx = offset(origin, right);
    let dy = offset(origin, down);
    if ![origin, right, down, corner, center]
        .into_iter()
        .all(in_world)
        || dx.length() < 0.001
        || dy.length() < 0.001
    {
        return Err(error(node, "invalid transformed crate"));
    }
    if dx.normalize().dot(dy.normalize()).abs() > 0.001 {
        return Err(error(node, "crate transforms cannot skew rectangles"));
    }
    Ok(CrateDefinition {
        id: id.into(),
        position: vec2(center),
        size: Vec2::new(dx.length(), dy.length()),
        angle: dx.to_angle(),
    })
}

fn agent(node: Node, id: &str) -> Result<Sensor, String> {
    let body = filled_mesh(node)?;
    Ok(Sensor {
        id: id.into(),
        color: color(node, "fill", "#ffffff")?,
        origin: Bounds::of_shapes(&body).center(),
        angle: facing(node, transform(node)?)?,
        sweep: (number(node, "data-gs-sweep", 0.0)? as f32).to_radians(),
        period: positive(node, "data-gs-period", AGENT_PERIOD as f64)?,
        phase: number(node, "data-gs-phase", 0.0)? as f32,
        range: positive(node, "data-gs-range", 600.0)?,
        aperture: field_of_view(node, 60.0)?,
        patrol: None,
        body,
    })
}

fn patrol(node: Node) -> Result<Patrol, String> {
    if !matches!(
        node.tag_name().name(),
        "line" | "polyline" | "path" | "polygon"
    ) {
        return Err(error(
            node,
            "patrol must be a line, polyline, polygon, or path",
        ));
    }
    let (contours, closed) = contours(node, false)?;
    if contours.len() != 1 {
        return Err(error(node, "patrol must contain one continuous route"));
    }
    let points = points(&contours[0]);
    if points.windows(2).any(|p| p[0].distance(p[1]) < 0.001) {
        return Err(error(node, "patrol contains a zero-length segment"));
    }
    Ok(Patrol {
        points,
        closed,
        speed: positive(node, "data-gs-speed", 60.0)?,
    })
}

fn attach_patrols(
    sensors: &mut [Sensor],
    mut routes: BTreeMap<String, (Patrol, Node)>,
) -> Result<(), String> {
    for sensor in sensors {
        let Some((patrol, node)) = routes.remove(&sensor.id["agent-".len()..]) else {
            continue;
        };
        if patrol.points[0].distance(sensor.origin) > 1.0 {
            return Err(error(
                node,
                "first route point must match the paired agent center within one unit",
            ));
        }
        sensor.patrol = Some(patrol);
    }
    match routes.into_values().next() {
        Some((_, node)) => Err(error(node, "patrol has no matching agent-*")),
        None => Ok(()),
    }
}

fn light(node: Node, id: &str) -> Result<LightDefinition, String> {
    check_effects(node)?;
    if !node.has_tag_name("circle") {
        return Err(error(node, "light must be a circle; radius defines range"));
    }
    let t = transform(node)?;
    let scale = uniform_scale(node, t)?;
    let angle = facing(node, t)?;
    let aperture = field_of_view(node, 360.0)?;
    let intensity = number(node, "data-gs-intensity", 0.4)? as f32;
    if !(0.0..=1.0).contains(&intensity) {
        return Err(error(node, "data-gs-intensity must be between 0 and 1"));
    }
    let behind = match node.attribute("data-gs-behind") {
        None | Some("false") => false,
        Some("true") => true,
        Some(_) => return Err(error(node, "data-gs-behind must be true or false")),
    };
    let range = positive(node, "r", 0.0)? * scale;
    if !range.is_finite() || !(0.001..=WORLD_LIMIT as f32).contains(&range) {
        return Err(error(
            node,
            "transformed light range must be between 0.001 and 1000000",
        ));
    }
    Ok(LightDefinition {
        id: id.into(),
        origin: point(node, "cx", "cy")?,
        range,
        angle,
        aperture,
        color: color(node, "fill", "#ffffff")?.with_alpha(intensity),
        background: id == "light-background",
        behind,
    })
}

fn hint(node: Node, id: &str) -> Result<Hint, String> {
    check_effects(node)?;
    if !node.has_tag_name("text") {
        return Err(error(node, "hint must be text"));
    }
    Ok(Hint {
        id: id.into(),
        color: color(node, "fill", "#e0f5c9")?,
        position: point(node, "x", "y")?,
        text: node
            .descendants()
            .filter(Node::is_text)
            .filter_map(|n| n.text())
            .collect(),
        size: positive(node, "data-gs-size", 14.0)?,
    })
}

fn error(node: Node, message: impl Display) -> String {
    let position = node.document().text_pos_at(node.range().start);
    format!(
        "{} at {}:{}: {message}",
        node.attribute("id").unwrap_or(node.tag_name().name()),
        node.attribute("data-gs-source-line")
            .unwrap_or(&position.row.to_string()),
        node.attribute("data-gs-source-column")
            .unwrap_or(&position.col.to_string())
    )
}

fn in_world(p: Point) -> bool {
    p.x.is_finite() && p.y.is_finite() && p.x.abs() <= WORLD_LIMIT && p.y.abs() <= WORLD_LIMIT
}

fn vec2(p: Point) -> Vec2 {
    Vec2::new(p.x as f32, p.y as f32)
}

fn offset(from: Point, to: Point) -> Vec2 {
    Vec2::new((to.x - from.x) as f32, (to.y - from.y) as f32)
}

fn points(contour: &[[f64; 2]]) -> Vec<Vec2> {
    contour
        .iter()
        .map(|p| Vec2::new(p[0] as f32, p[1] as f32))
        .collect()
}

fn number(node: Node, key: &str, default: f64) -> Result<f64, String> {
    let value = match node.attribute(key) {
        Some(text) => text
            .trim()
            .strip_suffix("px")
            .unwrap_or(text.trim())
            .parse::<f64>()
            .map_err(|_| error(node, format!("{key} needs a number in user units")))?,
        None => default,
    };
    if !value.is_finite() || value.abs() > WORLD_LIMIT {
        return Err(error(
            node,
            format!("{key} must be finite and between -1000000 and 1000000"),
        ));
    }
    Ok(value)
}

fn positive(node: Node, key: &str, default: f64) -> Result<f32, String> {
    let value = number(node, key, default)? as f32;
    if value <= 0.0 {
        return Err(error(node, format!("{key} must be positive")));
    }
    Ok(value)
}

fn field_of_view(node: Node, default_degrees: f64) -> Result<f32, String> {
    let aperture = positive(node, "data-gs-fov", default_degrees)?.to_radians();
    if aperture > TAU {
        return Err(error(node, "data-gs-fov must be at most 360 degrees"));
    }
    Ok(aperture)
}

// SVG transforms compose in document order. Flip Y only after every ancestor transform.
fn transform(node: Node) -> Result<Affine, String> {
    let mut result = Affine::scale_non_uniform(1.0, -1.0);
    let ancestors: Vec<_> = node.ancestors().filter(Node::is_element).collect();
    for ancestor in ancestors.into_iter().rev() {
        let Some(text) = ancestor.attribute("transform") else {
            continue;
        };
        for token in TransformListParser::from(text) {
            result *= match token.map_err(|e| error(ancestor, e))? {
                TransformListToken::Matrix { a, b, c, d, e, f } => Affine::new([a, b, c, d, e, f]),
                TransformListToken::Translate { tx, ty } => Affine::translate((tx, ty)),
                TransformListToken::Scale { sx, sy } => Affine::scale_non_uniform(sx, sy),
                TransformListToken::Rotate { angle } => Affine::rotate(angle.to_radians()),
                TransformListToken::SkewX { angle } => {
                    Affine::new([1.0, 0.0, angle.to_radians().tan(), 1.0, 0.0, 0.0])
                }
                TransformListToken::SkewY { angle } => {
                    Affine::new([1.0, angle.to_radians().tan(), 0.0, 1.0, 0.0, 0.0])
                }
            };
        }
    }
    if result.as_coeffs().iter().any(|n| !n.is_finite())
        || !result.determinant().is_finite()
        || result.determinant().abs() < 1e-12
    {
        return Err(error(node, "transform must be finite and invertible"));
    }
    Ok(result)
}

/// World angle of the object's `data-gs-angle`, measured from its local positive X axis.
fn facing(node: Node, transform: Affine) -> Result<f32, String> {
    let angle = number(node, "data-gs-angle", 0.0)?.to_radians();
    let direction = offset(
        transform * Point::ZERO,
        transform * Point::new(angle.cos(), angle.sin()),
    );
    Ok(direction.y.atan2(direction.x))
}

fn uniform_scale(node: Node, transform: Affine) -> Result<f32, String> {
    let origin = transform * Point::ZERO;
    let x = offset(origin, transform * Point::new(1.0, 0.0));
    let y = offset(origin, transform * Point::new(0.0, 1.0));
    if !x.length().is_finite()
        || !y.length().is_finite()
        || x.length() < 0.001
        || (x.length() - y.length()).abs() > 0.001 * x.length()
        || x.normalize().dot(y.normalize()).abs() > 0.001
    {
        return Err(error(node, "lights require a uniform scale without skew"));
    }
    Ok(x.length())
}

fn point(node: Node, x: &str, y: &str) -> Result<Vec2, String> {
    let p = transform(node)? * Point::new(number(node, x, 0.0)?, number(node, y, 0.0)?);
    if !in_world(p) {
        return Err(error(node, "position outside supported world coordinates"));
    }
    Ok(vec2(p))
}

fn property<'a>(node: Node<'a, 'a>, key: &str) -> Option<&'a str> {
    for ancestor in node.ancestors().filter(Node::is_element) {
        if let Some(style) = ancestor.attribute("style") {
            // Inline style has precedence over presentation attributes.
            if let Some(value) = style.split(';').rev().find_map(|entry| {
                let (name, value) = entry.split_once(':')?;
                (name.trim() == key).then_some(value.trim())
            }) {
                return Some(value);
            }
        }
        if let Some(value) = ancestor.attribute(key) {
            return Some(value);
        }
    }
    None
}

fn color(node: Node, key: &str, default: &str) -> Result<Color, String> {
    let text = if key == "fill" {
        property(node, key)
    } else {
        node.attribute(key)
    }
    .unwrap_or(default);
    let parsed = text
        .parse::<SvgColor>()
        .map_err(|e| error(node, format!("{key}: {e}")))?;
    Ok(Color::srgba_u8(
        parsed.red,
        parsed.green,
        parsed.blue,
        parsed.alpha,
    ))
}

fn check_effects(node: Node) -> Result<(), String> {
    for ancestor in node.ancestors().filter(Node::is_element) {
        for effect in ["clip-path", "mask", "filter"] {
            if property(ancestor, effect).is_some_and(|v| v != "none") {
                return Err(error(
                    node,
                    format!("{effect} is unsupported on gameplay objects"),
                ));
            }
        }
        if matches!(
            ancestor.tag_name().name(),
            "use" | "symbol" | "defs" | "clipPath" | "mask"
        ) {
            return Err(error(
                node,
                "referenced or definition geometry is unsupported on gameplay objects",
            ));
        }
        if ancestor.tag_name().name() == "svg" && ancestor.parent_element().is_some() {
            return Err(error(
                node,
                "nested SVG viewports are unsupported; use a transformed group",
            ));
        }
    }
    Ok(())
}

fn path(node: Node) -> Result<BezPath, String> {
    let mut path = BezPath::new();
    match node.tag_name().name() {
        "path" => {
            let d = node
                .attribute("d")
                .ok_or_else(|| error(node, "path needs d"))?;
            return path_from_data(node, d);
        }
        "rect" => {
            let x = number(node, "x", 0.0)?;
            let y = number(node, "y", 0.0)?;
            let w = positive(node, "width", 0.0)? as f64;
            let h = positive(node, "height", 0.0)? as f64;
            let rx = number(node, "rx", number(node, "ry", 0.0)?)?.min(w * 0.5);
            let ry = number(node, "ry", rx)?.min(h * 0.5);
            if rx < 0.0 || ry < 0.0 {
                return Err(error(node, "corner radii cannot be negative"));
            }
            if rx == 0.0 || ry == 0.0 {
                path = Rect::new(x, y, x + w, y + h).to_path(0.5);
            } else {
                // Elliptical corner radii are retained, including unequal rx/ry.
                let d = format!(
                    "M {} {y} H {} A {rx} {ry} 0 0 1 {} {} V {} A {rx} {ry} 0 0 1 {} {} H {} A {rx} {ry} 0 0 1 {x} {} V {} A {rx} {ry} 0 0 1 {} {y} Z",
                    x + rx,
                    x + w - rx,
                    x + w,
                    y + ry,
                    y + h - ry,
                    x + w - rx,
                    y + h,
                    x + rx,
                    y + h - ry,
                    y + ry,
                    x + rx,
                );
                return path_from_data(node, &d);
            }
        }
        "circle" | "ellipse" => {
            let x = number(node, "cx", 0.0)?;
            let y = number(node, "cy", 0.0)?;
            let (rx, ry) = if node.has_tag_name("circle") {
                let r = positive(node, "r", 0.0)? as f64;
                (r, r)
            } else {
                (
                    positive(node, "rx", 0.0)? as f64,
                    positive(node, "ry", 0.0)? as f64,
                )
            };
            // Cubic approximation is refined in world units after transforms below.
            path = Ellipse::new((x, y), (rx, ry), 0.0).to_path(0.01);
            path.close_path();
        }
        "polygon" | "polyline" => {
            let text = node
                .attribute("points")
                .ok_or_else(|| error(node, "shape needs points"))?;
            let values = NumberListParser::from(text)
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| error(node, e))?;
            if values.len() < 4 || values.len() % 2 != 0 {
                return Err(error(node, "points need at least two x,y pairs"));
            }
            for (i, p) in values.as_chunks::<2>().0.iter().enumerate() {
                if i == 0 {
                    path.move_to((p[0], p[1]));
                } else {
                    path.line_to((p[0], p[1]));
                }
            }
            if node.has_tag_name("polygon") {
                path.close_path();
            }
        }
        "line" => {
            path.move_to((number(node, "x1", 0.0)?, number(node, "y1", 0.0)?));
            path.line_to((number(node, "x2", 0.0)?, number(node, "y2", 0.0)?));
        }
        other => return Err(error(node, format!("unsupported gameplay shape <{other}>"))),
    }
    Ok(path)
}

fn path_from_data(node: Node, data: &str) -> Result<BezPath, String> {
    let mut result = BezPath::new();
    for segment in SimplifyingPathParser::from(data) {
        match segment.map_err(|e| error(node, e))? {
            SimplePathSegment::MoveTo { x, y } => result.move_to((x, y)),
            SimplePathSegment::LineTo { x, y } => result.line_to((x, y)),
            SimplePathSegment::CurveTo {
                x1,
                y1,
                x2,
                y2,
                x,
                y,
            } => result.curve_to((x1, y1), (x2, y2), (x, y)),
            SimplePathSegment::Quadratic { x1, y1, x, y } => result.quad_to((x1, y1), (x, y)),
            SimplePathSegment::ClosePath => result.close_path(),
        }
    }
    Ok(result)
}

/// Flattens a shape into world-space contours and reports whether every contour is closed.
fn contours(node: Node, require_closed: bool) -> Result<(Contours, bool), String> {
    check_effects(node)?;
    let path = transform(node)? * path(node)?;
    let finite = path.elements().iter().all(|element| match *element {
        PathEl::MoveTo(p) | PathEl::LineTo(p) => in_world(p),
        PathEl::QuadTo(a, b) => in_world(a) && in_world(b),
        PathEl::CurveTo(a, b, c) => in_world(a) && in_world(b) && in_world(c),
        PathEl::ClosePath => true,
    });
    if !finite {
        return Err(error(
            node,
            "geometry must have finite world coordinates within +/-1000000",
        ));
    }
    let mut result: Contours = Vec::new();
    let mut closed = true;
    let mut current_closed = true;
    flatten(
        path.elements().iter().copied(),
        0.5,
        |element| match element {
            PathEl::MoveTo(p) => {
                closed &= current_closed;
                current_closed = false;
                result.push(vec![[p.x, p.y]]);
            }
            PathEl::LineTo(p) => {
                if let Some(contour) = result.last_mut() {
                    contour.push([p.x, p.y]);
                }
            }
            PathEl::ClosePath => current_closed = true,
            _ => unreachable!("flatten emits only straight segments"),
        },
    );
    closed &= current_closed;
    for contour in &mut result {
        contour.dedup();
        if contour.len() > 1 && contour.first() == contour.last() {
            contour.pop();
        }
        if contour.len() < if require_closed { 3 } else { 2 } {
            return Err(error(node, "geometry has too few distinct points"));
        }
    }
    if result.is_empty() || (require_closed && !closed) {
        return Err(error(
            node,
            "filled gameplay geometry must contain closed contours",
        ));
    }
    Ok((result, closed))
}

/// Resolves the element's fill rule into non-overlapping shapes, each an outer contour and holes.
fn filled(node: Node) -> Result<Vec<Contours>, String> {
    let (contours, _) = contours(node, true)?;
    let rule = match property(node, "fill-rule").unwrap_or("nonzero") {
        "nonzero" => FillRule::NonZero,
        "evenodd" => FillRule::EvenOdd,
        _ => return Err(error(node, "fill-rule must be nonzero or evenodd")),
    };
    let shapes = contours.simplify_shape(rule);
    if shapes.is_empty() {
        return Err(error(node, "filled shape has no area"));
    }
    Ok(shapes)
}

fn filled_mesh(node: Node) -> Result<Vec<Terrain>, String> {
    triangulate(filled(node)?).map_err(|message| error(node, message))
}

fn triangulate(shapes: Vec<Contours>) -> Result<Vec<Terrain>, String> {
    shapes.into_iter().map(triangulate_shape).collect()
}

fn triangulate_shape(shape: Contours) -> Result<Terrain, String> {
    let mut holes = Vec::new();
    let mut vertices = Vec::new();
    let mut coordinates = Vec::new();
    let mut contours = Vec::new();
    for (i, contour) in shape.iter().enumerate() {
        if i > 0 {
            holes.push(vertices.len());
        }
        let mut points = points(contour);
        if signed_area(&points).abs() < 0.0001
            || polygon_surfaces(&points).any(|edge| edge.a.distance_squared(edge.b) < 0.00000001)
        {
            return Err("geometry collapses at game precision; use larger shapes and edges".into());
        }
        // Outer contours wind counter-clockwise and holes clockwise.
        if (signed_area(&points) > 0.0) != (i == 0) {
            points.reverse();
        }
        coordinates.extend(points.iter().flat_map(|p| [p.x as f64, p.y as f64]));
        vertices.extend_from_slice(&points);
        contours.push(points);
    }
    let triangles: Vec<u32> = earcut(&coordinates, &holes, 2)
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|i| i as u32)
        .collect();
    if triangles.is_empty() {
        return Err("filled geometry could not be triangulated".into());
    }
    Ok(Terrain {
        vertices,
        triangles,
        contours,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(objects: &str) -> String {
        format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="-200 -200 800 800" data-gs-id="fixture"><title>Fixture</title><circle id="spawn" cx="-100" cy="-100" r="16"/><circle id="goal" cx="500" cy="500" r="20"/>{objects}</svg>"#
        )
    }

    fn area(level: &Level) -> f32 {
        level
            .terrain
            .iter()
            .map(|t| {
                t.triangles
                    .as_chunks::<3>()
                    .0
                    .iter()
                    .map(|i| {
                        let [a, b, c] = [
                            t.vertices[i[0] as usize],
                            t.vertices[i[1] as usize],
                            t.vertices[i[2] as usize],
                        ];
                        (b - a).perp_dot(c - a).abs() * 0.5
                    })
                    .sum::<f32>()
            })
            .sum()
    }

    #[test]
    fn overlapping_elements_are_unioned_without_internal_gravity_edges() {
        let level=parse(&fixture(r#"<g id="terrain" fill-rule="evenodd"><rect width="100" height="100"/><rect x="50" width="100" height="100"/></g>"#)).unwrap();
        assert!((area(&level) - 15000.0).abs() < 0.01);
        assert_eq!(level.surfaces.len(), 4);
        assert!(level.terrain[0].contains(Vec2::new(75.0, -50.0)));
    }

    #[test]
    fn compound_paths_preserve_holes_and_resolve_each_fill_rule() {
        for (rule, inner, expected) in [
            ("evenodd", "M 20 20 H 80 V 80 H 20 Z", 6400.0),
            ("nonzero", "M 20 20 V 80 H 80 V 20 Z", 6400.0),
            ("nonzero", "M 20 20 H 80 V 80 H 20 Z", 10000.0),
        ] {
            let level=parse(&fixture(&format!(r#"<path id="terrain-hole" fill-rule="{rule}" d="M 0 0 H 100 V 100 H 0 Z {inner}"/>"#))).unwrap();
            assert!(
                (area(&level) - expected).abs() < 0.01,
                "{rule}: {}",
                area(&level)
            );
            assert_eq!(
                level.terrain[0].contains(Vec2::new(50.0, -50.0)),
                expected == 10000.0
            );
        }
    }

    #[test]
    fn nested_transforms_and_y_conversion_match_svg_order() {
        let level=parse(&fixture(r#"<g id="terrain" transform="translate(20 30)"><g transform="rotate(90)"><rect width="100" height="50"/></g></g>"#)).unwrap();
        assert!(level.terrain[0].contains(Vec2::new(0.0, -80.0)));
        assert!(!level.terrain[0].contains(Vec2::new(40.0, -80.0)));
        assert!((area(&level) - 5000.0).abs() < 0.1);
    }

    #[test]
    fn circles_ellipses_rounded_rectangles_and_curves_flatten_in_world_units() {
        let level=parse(&fixture(r#"<g id="terrain"><rect width="100" height="100" rx="20" ry="10"/><circle cx="200" cy="50" r="40"/><ellipse cx="350" cy="50" rx="50" ry="20"/><path d="M 0 200 h 100 a 50 30 0 0 1 100 0 Q 250 250 200 300 C 100 350 0 350 0 200 Z"/></g>"#)).unwrap();
        assert_eq!(level.terrain.len(), 4);
        assert!(level.surfaces.len() > 45);
        assert!(
            !level
                .terrain
                .iter()
                .any(|s| s.contains(Vec2::new(0.0, 0.0)))
        );
    }

    #[test]
    fn invalid_input_reports_element_and_source_location() {
        for (objects, diagnostic) in [
            (
                r#"<rect id="terrain-bad" width="nan" height="10"/>"#,
                "terrain-bad at",
            ),
            (
                r#"<path id="terrain-open" d="M 0 0 L 20 0 L 20 20"/>"#,
                "closed contours",
            ),
            (
                r#"<rect id="terrain-mask" width="20" height="20" mask="url(#m)"/>"#,
                "mask is unsupported",
            ),
            (
                r##"<use id="terrain-use" href="#example"/>"##,
                "unknown use target",
            ),
            (
                r#"<rect id="terrain" width="20" height="20"/><circle id="spawn" r="10"/>"#,
                "duplicate SVG id",
            ),
            (
                r#"<rect id="terrain" width="20" height="20"/><line id="patrol-orphan" x1="0" y1="0" x2="30" y2="0"/>"#,
                "no matching agent",
            ),
            (
                r#"<rect id="terrain" width="20" height="20"/><rect id="agent-a" x="30" y="30" width="10" height="10"/><line id="patrol-a" x1="0" y1="0" x2="30" y2="0"/>"#,
                "first route point",
            ),
            (
                r#"<rect id="terrain" width="20" height="20"/><line id="patrol-a" x1="0" y1="0" x2="0" y2="0"/>"#,
                "too few distinct",
            ),
            (
                r#"<rect id="terrain" width="20" height="20"/><circle id="light-a" cx="0" cy="-100" r="30" transform="scale(2 1)"/>"#,
                "uniform scale",
            ),
        ] {
            let result = parse(&fixture(objects)).unwrap_err();
            assert!(
                result.contains(diagnostic),
                "expected {diagnostic}, got {result}"
            );
        }
        let missing = fixture(r#"<rect id="terrain" width="20" height="20"/>"#)
            .replace("id=\"spawn\"", "id=\"reference\"");
        assert!(parse(&missing).unwrap_err().contains("needs one circle"));
    }

    #[test]
    fn template_and_shipped_levels_are_valid() {
        for source in [
            include_str!("../assets/authoring/template.svg"),
            include_str!("../assets/levels/tutorial.svg"),
            include_str!("../assets/levels/level-1.svg"),
            include_str!("../assets/levels/level-2.svg"),
            include_str!("../assets/levels/level-3.svg"),
            include_str!("../assets/levels/level-4.svg"),
            include_str!("../assets/levels/level-5.svg"),
            include_str!("../assets/levels/level-6.svg"),
            include_str!("../assets/levels/level-7.svg"),
        ] {
            parse(source).unwrap();
        }
    }

    #[test]
    fn agent_bodies_and_continuous_patrols_must_clear_terrain() {
        let terrain = r#"<rect id="terrain-wall" x="0" y="0" width="32" height="100"/>"#;
        for agent in [
            r#"<circle id="agent-test" cx="16" cy="50" r="3"/>"#,
            r#"<circle id="agent-test" cx="-2" cy="50" r="3"/>"#,
            r#"<circle id="agent-test" cx="-20" cy="50" r="3"/><line id="patrol-test" x1="-20" y1="50" x2="60" y2="50"/>"#,
            r#"<rect id="agent-test" x="-18" y="40" width="16" height="16" data-gs-sweep="90"/>"#,
        ] {
            let error = parse(&fixture(&format!("{terrain}{agent}"))).unwrap_err();
            assert!(
                error.contains("agent-test") && error.contains("overlaps terrain"),
                "{error}"
            );
        }
        let level = parse(&fixture(&format!(r#"{terrain}<circle id="agent-test" cx="-4" cy="10" r="3"/><line id="patrol-test" x1="-4" y1="10" x2="-4" y2="90"/>"#))).unwrap();
        assert!(
            (level.sensors[0].terrain_clearance(&level.terrain, &level.surfaces) - 1.0).abs()
                < 0.01
        );
        let in_hole = parse(&fixture(r#"<path id="terrain-ring" fill-rule="evenodd" d="M0 0 H100 V100 H0 Z M20 20 H80 V80 H20 Z"/><circle id="agent-hole" cx="50" cy="50" r="3"/>"#)).unwrap();
        assert!(in_hole.sensors[0].terrain_clearance(&in_hole.terrain, &in_hole.surfaces) > 26.9);
    }

    #[test]
    fn editor_guides_ignore_paint_visibility_and_preserve_parameters() {
        let level = parse(include_str!("../assets/authoring/template.svg")).unwrap();
        let patrol = level.sensors[0].patrol.as_ref().unwrap();
        assert_eq!(patrol.speed, 60.0);
        assert_eq!(patrol.points[0], Vec2::new(-300.0, 150.0));
        assert_eq!(level.lights[0].color.alpha(), 0.7);
        assert_eq!(
            level.hints[0].text,
            "F5 reloads. F1 shows object IDs and patrol routes."
        );
    }
    #[test]
    fn local_defs_are_instanced_with_svg_transform_order_and_no_unused_bodies() {
        let level=parse(&fixture(r##"<defs><rect id="tile" width="100" height="50"/><rect id="terrain-unused" x="300" y="300" width="20" height="20"/></defs><g id="terrain" transform="translate(20 30)"><use id="terrain-instance" href="#tile" x="10" y="20" transform="rotate(90)"/></g>"##)).unwrap();
        assert_eq!(level.terrain.len(), 1);
        assert!((area(&level) - 5000.0).abs() < 0.1);
        assert!(level.terrain[0].contains(Vec2::new(-25.0, -90.0)));
        assert!(!level.terrain[0].contains(Vec2::new(310.0, -310.0)));
    }

    #[test]
    fn instances_preserve_object_roles_and_inherit_paint() {
        let source = fixture(
            r##"<defs><circle id="marker" r="16"/><circle id="emitter" r="3"/><rect id="box" width="16" height="16"/></defs><rect id="terrain" width="20" height="20"/><use id="agent-reused" href="#emitter" x="100" y="-100" fill="#ffcc00" data-gs-range="850" data-gs-fov="80"/><use id="crate-reused" href="#box" x="50" y="50"/><use id="light-reused" href="#marker" x="300" y="-100" fill="#80aaff"/>"##,
        );
        let level = parse(&source).unwrap();
        assert_eq!(level.sensors[0].origin, Vec2::new(100.0, 100.0));
        assert_eq!(level.sensors[0].range, 850.0);
        assert_eq!(level.sensors[0].color, Color::srgb_u8(255, 204, 0));
        assert_eq!(level.cubes[0].position, Vec2::new(58.0, -58.0));
        assert_eq!(level.lights[0].origin, Vec2::new(300.0, 100.0));
    }

    #[test]
    fn nested_group_instances_support_xlink_without_duplicate_child_ids() {
        let level=parse(&fixture(r##"<defs><g id="pair"><rect id="left" width="20" height="20"/><rect id="right" x="40" width="20" height="20"/></g></defs><g id="terrain" xmlns:xlink="http://www.w3.org/1999/xlink"><use id="terrain-first" xlink:href="#pair"/><use id="terrain-second" href="#pair" y="100"/></g>"##)).unwrap();
        assert_eq!(level.terrain.len(), 4);
        assert!((area(&level) - 1600.0).abs() < 0.01);
    }

    #[test]
    fn reference_cycles_external_files_and_missing_targets_are_rejected() {
        for (objects, message) in [
            (
                r##"<defs><use id="a" href="#b"/><use id="b" href="#a"/></defs><use id="terrain-loop" href="#a"/>"##,
                "cyclic",
            ),
            (
                r##"<use id="terrain-external" href="other.svg#tile"/>"##,
                "only local",
            ),
            (
                r##"<use id="terrain-missing" href="#unknown"/>"##,
                "unknown use target",
            ),
        ] {
            assert!(parse(&fixture(objects)).unwrap_err().contains(message));
        }
    }

    #[test]
    fn metadata_supplies_identity_palette_and_timestamped_references() {
        let source=fixture(r##"<metadata xmlns:gs="urn:gravity-spy:level:1"><gs:level id="metadata-level" order="7"><gs:palette terrain="#804060" crate="#ff80dd" trail="#eeeeee" goal="#ffeeaa" sensor-safe="#dddd44"/><gs:reference source="original-video" start="1254" end="1278">Purple route</gs:reference></gs:level></metadata><rect id="terrain" width="20" height="20"/>"##).replace(r#" data-gs-id="fixture""#,"");
        let level = parse(&source).unwrap();
        assert_eq!(level.id.0, "metadata-level");
        assert_eq!(level.order, 7);
        assert_eq!(level.palette.crate_color, Color::srgb_u8(255, 128, 221));
        assert_eq!(level.references[0].start, 1254.0);
        assert_eq!(level.references[0].end, 1278.0);
        assert_eq!(level.references[0].notes, "Purple route");
        assert!(
            parse(&source.replace("viewBox=", r#"data-gs-id="conflict" viewBox="#))
                .unwrap_err()
                .contains("conflicting")
        );
        assert!(
            parse(&source.replace(r#"end="1278""#, r#"end="1200""#))
                .unwrap_err()
                .contains("start < end")
        );
    }

    #[test]
    fn terrain_colors_preserve_union_collision_and_holes() {
        let level=parse(&fixture(r##"<g id="terrain"><path fill="#3366aa" fill-rule="evenodd" d="M 0 0 H 100 V 100 H 0 Z M 20 20 H 80 V 80 H 20 Z"/><rect x="90" width="50" height="100" fill="#aa6633"/></g>"##)).unwrap();
        assert_eq!(level.terrain.len(), 1);
        assert_eq!(level.terrain_paint.len(), 2);
        assert!(!level.terrain[0].contains(Vec2::new(50.0, -50.0)));
        assert!((area(&level) - 10400.0).abs() < 0.01);
    }

    #[test]
    fn movement_metadata_is_validated_at_the_document_boundary() {
        let source = fixture(
            r#"<metadata xmlns:gs="urn:gravity-spy:level:1"><gs:level><gs:movement gravity="900" acceleration="1500" max-speed="240" jump-speed="720"/></gs:level></metadata><rect id="terrain" width="20" height="20"/>"#,
        );
        let level = parse(&source).unwrap();
        assert_eq!(level.movement.gravity, 900.0);
        assert_eq!(level.movement.acceleration, 1500.0);
        assert_eq!(level.movement.max_speed, 240.0);
        assert_eq!(level.movement.jump_speed, 720.0);
        for invalid in ["0", "-1", "NaN", "inf"] {
            assert!(
                parse(&source.replace("jump-speed=\"720\"", &format!("jump-speed=\"{invalid}\"")))
                    .is_err()
            );
        }
        assert!(
            parse(&source.replace("jump-speed=", "unknown-setting="))
                .unwrap_err()
                .contains("unknown movement")
        );
    }
}
