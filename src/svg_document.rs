//! Normalize local SVG instances and level metadata before geometry enters the game.
use std::collections::{BTreeMap, HashMap};

use roxmltree::{Document, Node, NodeId};
use xmlwriter::{Indent, Options, XmlWriter};

pub const LEVEL_NAMESPACE: &str = "urn:gravity-spy:level:1";
const SVG_NAMESPACE: &str = "http://www.w3.org/2000/svg";
const XLINK_NAMESPACE: &str = "http://www.w3.org/1999/xlink";

pub fn is_svg(node: Node) -> bool {
    matches!(node.tag_name().namespace(), None | Some(SVG_NAMESPACE))
}

fn diagnostic(node: Node, message: &str) -> String {
    let position = node.document().text_pos_at(node.range().start);
    format!(
        "{} at {}:{}: {message}",
        node.attribute("id").unwrap_or(node.tag_name().name()),
        position.row,
        position.col
    )
}

pub fn settings<'a>(root: Node<'a, 'a>) -> Result<BTreeMap<String, String>, String> {
    let mut result = BTreeMap::new();
    let levels: Vec<_> = root
        .children()
        .filter(|n| n.has_tag_name("metadata"))
        .flat_map(|n| n.children())
        .filter(|n| n.has_tag_name((LEVEL_NAMESPACE, "level")))
        .collect();
    if levels.len() > 1 {
        return Err(diagnostic(
            root,
            "only one gs:level metadata block is allowed",
        ));
    }
    if let Some(level) = levels.first().copied() {
        for attribute in level.attributes() {
            let key = match attribute.name() {
                "id" => "data-gs-id",
                "order" => "data-gs-order",
                _ => {
                    return Err(diagnostic(
                        level,
                        "unknown level metadata attribute; supported: id, order",
                    ));
                }
            };
            result.insert(key.to_string(), attribute.value().to_string());
        }
        let mut palette_seen = false;
        let mut movement_seen = false;
        for child in level.children().filter(Node::is_element) {
            if child.tag_name().namespace() != Some(LEVEL_NAMESPACE) {
                continue;
            }
            match child.tag_name().name() {
                "palette" => {
                    if palette_seen {
                        return Err(diagnostic(child, "only one palette is allowed"));
                    }
                    palette_seen = true;
                    for attribute in child.attributes() {
                        if ![
                            "background",
                            "terrain",
                            "outline",
                            "spy-dark",
                            "spy-light",
                            "crate",
                            "trail",
                            "goal",
                            "death",
                            "success",
                            "sensor-safe",
                            "sensor-warning",
                            "sensor-danger",
                        ]
                        .contains(&attribute.name())
                        {
                            return Err(diagnostic(child, "unknown palette color"));
                        }
                        result.insert(
                            format!("data-gs-{}", attribute.name()),
                            attribute.value().to_string(),
                        );
                    }
                }
                "movement" => {
                    if movement_seen {
                        return Err(diagnostic(child, "only one movement block is allowed"));
                    }
                    movement_seen = true;
                    for attribute in child.attributes() {
                        if !["gravity", "acceleration", "max-speed", "jump-speed"]
                            .contains(&attribute.name())
                        {
                            return Err(diagnostic(child, "unknown movement setting"));
                        }
                        result.insert(
                            format!("data-gs-{}", attribute.name()),
                            attribute.value().to_string(),
                        );
                    }
                }
                "reference" => {}
                _ => {
                    return Err(diagnostic(
                        child,
                        "unknown level metadata element; supported: palette, movement, reference",
                    ));
                }
            }
        }
    }
    for (key, value) in &result {
        if root.attribute(key.as_str()).is_some_and(|old| old != value) {
            return Err(diagnostic(
                root,
                &format!("conflicting {key} in root attributes and metadata; keep one setting"),
            ));
        }
    }
    Ok(result)
}

pub fn normalize(source: &str) -> Result<String, String> {
    let document = Document::parse(source).map_err(|e| e.to_string())?;
    let root = document.root_element();
    let settings = settings(root)?;
    let mut ids = HashMap::new();
    for node in root.descendants().filter(|n| {
        n.is_element() && is_svg(*n) && !n.ancestors().any(|a| a.has_tag_name("metadata"))
    }) {
        if let Some(id) = node.attribute("id")
            && ids.insert(id, node).is_some()
        {
            return Err(diagnostic(node, "duplicate SVG id"));
        }
    }
    let mut expansion = Expansion {
        ids,
        stack: Vec::new(),
        serial: 0,
        nodes: 0,
    };
    let mut writer = XmlWriter::new(Options {
        indent: Indent::None,
        ..Options::default()
    });
    expansion.write(&mut writer, root, None, &settings, false)?;
    Ok(writer.end_document())
}

struct Expansion<'a, 'input> {
    ids: HashMap<&'input str, Node<'a, 'input>>,
    stack: Vec<NodeId>,
    serial: usize,
    nodes: usize,
}

impl Expansion<'_, '_> {
    fn write(
        &mut self,
        writer: &mut XmlWriter,
        node: Node,
        prefix: Option<&str>,
        overrides: &BTreeMap<String, String>,
        inside_defs: bool,
    ) -> Result<(), String> {
        if node.is_text() {
            writer.write_text(node.text().unwrap_or_default());
            return Ok(());
        }
        if !node.is_element() {
            return Ok(());
        }
        self.nodes += 1;
        if self.nodes > 100_000 {
            return Err(diagnostic(node, "expanded SVG exceeds 100000 elements"));
        }
        if node.has_tag_name("use") && is_svg(node) && !inside_defs {
            return self.instance(writer, node, prefix, overrides);
        }
        let name = qualified_name(node, node.tag_name().namespace(), node.tag_name().name());
        writer.start_element(&name);
        namespaces(writer, node);
        for attribute in node.attributes() {
            if overrides.contains_key(attribute.name()) {
                continue;
            }
            let name = qualified_name(node, attribute.namespace(), attribute.name());
            if attribute.name() == "id"
                && let Some(prefix) = prefix
            {
                writer.write_attribute(&name, &format!("{prefix}--{}", attribute.value()));
            } else {
                writer.write_attribute(&name, attribute.value());
            }
        }
        for (key, value) in overrides {
            writer.write_attribute(key, value);
        }
        if is_svg(node) {
            let position = node.document().text_pos_at(node.range().start);
            if !overrides.contains_key("data-gs-source-line") {
                writer.write_attribute("data-gs-source-line", &position.row);
            }
            if !overrides.contains_key("data-gs-source-column") {
                writer.write_attribute("data-gs-source-column", &position.col);
            }
        }
        let definition = inside_defs || node.has_tag_name("defs");
        for child in node.children() {
            self.write(writer, child, prefix, &BTreeMap::new(), definition)?;
        }
        writer.end_element();
        Ok(())
    }

    fn instance(
        &mut self,
        writer: &mut XmlWriter,
        node: Node,
        prefix: Option<&str>,
        overrides: &BTreeMap<String, String>,
    ) -> Result<(), String> {
        let href = node
            .attribute("href")
            .or_else(|| node.attribute((XLINK_NAMESPACE, "href")))
            .ok_or_else(|| diagnostic(node, "use needs a local href"))?;
        let id = href
            .strip_prefix('#')
            .filter(|id| !id.is_empty())
            .ok_or_else(|| diagnostic(node, "use supports only local #id references"))?;
        let target = self
            .ids
            .get(id)
            .copied()
            .ok_or_else(|| diagnostic(node, &format!("unknown use target #{id}")))?;
        if !matches!(
            target.tag_name().name(),
            "g" | "rect"
                | "circle"
                | "ellipse"
                | "polygon"
                | "polyline"
                | "path"
                | "line"
                | "text"
                | "use"
        ) || !is_svg(target)
        {
            return Err(diagnostic(
                node,
                "use target must be a shape or group, without a symbol viewport",
            ));
        }
        if self.stack.contains(&target.id()) || self.stack.len() >= 32 {
            return Err(diagnostic(
                node,
                "cyclic or excessively nested use reference",
            ));
        }
        self.serial += 1;
        let instance_id = overrides
            .get("id")
            .cloned()
            .or_else(|| {
                node.attribute("id").map(|id| {
                    prefix.map_or_else(|| id.to_string(), |prefix| format!("{prefix}--{id}"))
                })
            })
            .unwrap_or_else(|| format!("instance-{}", self.serial));
        let x = coordinate(node, "x")?;
        let y = coordinate(node, "y")?;
        writer.start_element("g");
        namespaces(writer, node);
        for attribute in node.attributes() {
            if ["id", "href", "x", "y", "width", "height", "transform"].contains(&attribute.name())
                || attribute.name().starts_with("data-gs-")
            {
                continue;
            }
            writer.write_attribute(
                &qualified_name(node, attribute.namespace(), attribute.name()),
                attribute.value(),
            );
        }
        writer.write_attribute(
            "transform",
            &format!(
                "{} translate({x} {y})",
                node.attribute("transform").unwrap_or("")
            ),
        );
        let mut fields: BTreeMap<_, _> = node
            .attributes()
            .filter(|a| a.name().starts_with("data-gs-"))
            .map(|a| (a.name().to_string(), a.value().to_string()))
            .collect();
        fields.extend(overrides.clone());
        fields.insert("id".into(), instance_id.clone());
        let position = node.document().text_pos_at(node.range().start);
        fields.insert("data-gs-source-line".into(), position.row.to_string());
        fields.insert("data-gs-source-column".into(), position.col.to_string());
        self.stack.push(target.id());
        self.write(writer, target, Some(&instance_id), &fields, false)?;
        self.stack.pop();
        writer.end_element();
        Ok(())
    }
}

fn coordinate(node: Node, key: &str) -> Result<f64, String> {
    let text = node.attribute(key).unwrap_or("0");
    let value = text
        .strip_suffix("px")
        .unwrap_or(text)
        .parse::<f64>()
        .map_err(|_| diagnostic(node, "use x and y need numbers in user units"))?;
    if !value.is_finite() || value.abs() > 1e6 {
        return Err(diagnostic(
            node,
            "use coordinates must be finite and within +/-1000000",
        ));
    }
    Ok(value)
}

fn qualified_name(node: Node, namespace: Option<&str>, name: &str) -> String {
    match namespace {
        Some(SVG_NAMESPACE) | None => name.to_string(),
        Some("http://www.w3.org/XML/1998/namespace") => format!("xml:{name}"),
        Some(namespace) => node
            .lookup_prefix(namespace)
            .filter(|p| !p.is_empty())
            .map_or_else(|| name.to_string(), |prefix| format!("{prefix}:{name}")),
    }
}

fn namespaces(writer: &mut XmlWriter, node: Node) {
    for namespace in node.namespaces() {
        let key = namespace
            .name()
            .map_or_else(|| "xmlns".to_string(), |prefix| format!("xmlns:{prefix}"));
        writer.write_attribute(&key, namespace.uri());
    }
}
