//! Resolve a complete view before changing the tree. A missing pointer or
//! incompatible update leaves the last good view visible instead of partly
//! rebuilding a screen with a mixture of old and new data.

use std::collections::BTreeSet;

use noa_gpui_core::style::{Size as LayoutSize, auto, length};
use noa_gpui_core::widgets::{Button, Container, Label, Tag, Tone};
use noa_gpui_core::{Paint, PaintCx, Role, Style, UiTree, Widget, WidgetId};
use noa_gpui_scene::{Border, Color, Rect};
use serde_json::{Value, json};

use crate::images::{self, Picture};
use crate::style;

#[derive(Debug, Clone, PartialEq)]
pub struct Action {
    pub id: String,
    pub payload: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Node {
    Column {
        gap: f32,
        children: Vec<Node>,
    },
    Row {
        gap: f32,
        children: Vec<Node>,
    },
    Section {
        title: String,
        gap: f32,
        children: Vec<Node>,
    },
    Text {
        text: String,
        tone: String,
    },
    Metric {
        label: String,
        value: String,
    },
    Badge {
        text: String,
        tone: String,
    },
    Progress {
        label: Option<String>,
        fraction: f64,
    },
    Table {
        columns: Vec<String>,
        rows: Vec<Vec<String>>,
    },
    List(Vec<String>),
    Button {
        label: String,
        action: Action,
    },
    Image {
        asset: String,
        alt: String,
        width: f32,
        height: f32,
        shape: Shape,
        /// Filled in by `Images::attach`; `None` draws the alt text.
        picture: Option<Picture>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Shape {
    Square,
    Circle,
}

fn image(view: &Value, data: &Value) -> Result<Node, String> {
    let (width, height) = image_box(view)?;
    Ok(Node::Image {
        asset: bound(view, "src", data)?
            .as_str()
            .ok_or("image src must resolve to an asset id")?
            .to_owned(),
        alt: display(&bound(view, "alt", data)?)?,
        width,
        height,
        shape: match view.get("shape").and_then(Value::as_str) {
            Some("circle") => Shape::Circle,
            _ => Shape::Square,
        },
        picture: None,
    })
}

/// The logical box of an image node: `width`/`height`, else `size` (md).
fn image_box(view: &Value) -> Result<(f32, f32), String> {
    if let (Some(width), Some(height)) = (view.get("width"), view.get("height")) {
        let side = |value: &Value| {
            value
                .as_u64()
                .filter(|n| (1..=noa_widget::manifest::MAX_IMAGE_BOX).contains(n))
                .map(|n| n as f32)
                .ok_or("image width and height must be integers 1..=1024")
        };
        return Ok((side(width)?, side(height)?));
    }
    let size = view.get("size").and_then(Value::as_str).unwrap_or("md");
    noa_widget::manifest::IMAGE_SIZES
        .iter()
        .find(|(name, _)| *name == size)
        .map(|(_, side)| (*side as f32, *side as f32))
        .ok_or_else(|| format!("unsupported image size {size:?}"))
}

fn field<'a>(view: &'a Value, key: &str) -> Result<&'a Value, String> {
    view.get(key)
        .ok_or_else(|| format!("view field {key:?} is missing"))
}

fn string(view: &Value, key: &str) -> Result<String, String> {
    field(view, key)?
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| format!("view field {key:?} must be a string"))
}

fn bound(view: &Value, key: &str, data: &Value) -> Result<Value, String> {
    noa_widget::resolve(field(view, key)?, data).map_err(|error| error.to_string())
}

fn display(value: &Value) -> Result<String, String> {
    match value {
        Value::String(value) => Ok(value.clone()),
        Value::Bool(_) | Value::Number(_) => Ok(value.to_string()),
        Value::Null => Ok("—".into()),
        _ => Err("display bindings must resolve to a string, number, boolean, or null".into()),
    }
}

fn children(view: &Value, data: &Value) -> Result<Vec<Node>, String> {
    field(view, "children")?
        .as_array()
        .ok_or("children must be an array")?
        .iter()
        .map(|child| resolve(child, data))
        .collect()
}

fn gap(view: &Value) -> f32 {
    match view.get("gap").and_then(Value::as_str).unwrap_or("md") {
        "none" => 0.0,
        "xs" => 4.0,
        "sm" => 8.0,
        "lg" => 24.0,
        _ => 16.0,
    }
}

fn rows(view: &Value, key: &str, data: &Value) -> Result<Vec<Value>, String> {
    bound(view, key, data)?
        .as_array()
        .cloned()
        .ok_or_else(|| format!("{key} must resolve to an array"))
}

fn row_value<'a>(row: &'a Value, pointer: &str) -> Result<&'a Value, String> {
    row.pointer(pointer)
        .ok_or_else(|| format!("row pointer {pointer:?} does not exist"))
}

fn unique_keys(rows: &[Value], pointer: &str) -> Result<(), String> {
    let mut keys = BTreeSet::new();
    for row in rows {
        let value = row_value(row, pointer)?;
        if !value.is_string() && !value.is_number() {
            return Err("row keys must be strings or numbers".into());
        }
        if !keys.insert(value.to_string()) {
            return Err(format!("duplicate row key at {pointer:?}"));
        }
    }
    Ok(())
}

pub fn resolve(view: &Value, data: &Value) -> Result<Node, String> {
    match field(view, "type")?
        .as_str()
        .ok_or("view type must be a string")?
    {
        "column" => Ok(Node::Column {
            gap: gap(view),
            children: children(view, data)?,
        }),
        "row" => Ok(Node::Row {
            gap: gap(view),
            children: children(view, data)?,
        }),
        "section" => Ok(Node::Section {
            title: display(&bound(view, "title", data)?)?,
            gap: gap(view),
            children: children(view, data)?,
        }),
        "text" => Ok(Node::Text {
            text: display(&bound(view, "text", data)?)?,
            tone: view
                .get("tone")
                .and_then(Value::as_str)
                .unwrap_or("default")
                .into(),
        }),
        "metric" => Ok(Node::Metric {
            label: display(&bound(view, "label", data)?)?,
            value: display(&bound(view, "value", data)?)?,
        }),
        "badge" => Ok(Node::Badge {
            text: display(&bound(view, "text", data)?)?,
            tone: view
                .get("tone")
                .and_then(Value::as_str)
                .unwrap_or("default")
                .into(),
        }),
        "progress" => {
            let value = bound(view, "value", data)?
                .as_f64()
                .ok_or("progress value must be numeric")?;
            let maximum = match view.get("max") {
                Some(maximum) => noa_widget::resolve(maximum, data)
                    .map_err(|error| error.to_string())?
                    .as_f64()
                    .ok_or("progress max must be numeric")?,
                None => 1.0,
            };
            if maximum <= 0.0 || !maximum.is_finite() || !value.is_finite() {
                return Err("progress needs a finite value and a positive finite maximum".into());
            }
            let label = view
                .get("label")
                .map(|_| bound(view, "label", data).and_then(|value| display(&value)))
                .transpose()?;
            Ok(Node::Progress {
                label,
                fraction: (value / maximum).clamp(0.0, 1.0),
            })
        }
        "table" => {
            let rows = rows(view, "rows", data)?;
            unique_keys(&rows, &string(view, "rowKey")?)?;
            let columns = field(view, "columns")?
                .as_array()
                .ok_or("columns must be an array")?;
            let titles = columns
                .iter()
                .map(|column| string(column, "title"))
                .collect::<Result<Vec<_>, _>>()?;
            let pointers = columns
                .iter()
                .map(|column| string(column, "pointer"))
                .collect::<Result<Vec<_>, _>>()?;
            let rows = rows
                .iter()
                .map(|row| {
                    pointers
                        .iter()
                        .map(|pointer| display(row_value(row, pointer)?))
                        .collect()
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Node::Table {
                columns: titles,
                rows,
            })
        }
        "list" => {
            let rows = rows(view, "items", data)?;
            unique_keys(&rows, &string(view, "itemKey")?)?;
            let pointer = string(view, "textPointer")?;
            Ok(Node::List(
                rows.iter()
                    .map(|row| display(row_value(row, &pointer)?))
                    .collect::<Result<_, _>>()?,
            ))
        }
        "button" => Ok(Node::Button {
            label: display(&bound(view, "label", data)?)?,
            action: Action {
                id: string(view, "action")?,
                payload: match view.get("payload") {
                    Some(payload) => {
                        noa_widget::resolve(payload, data).map_err(|error| error.to_string())?
                    }
                    None => json!({}),
                },
            },
        }),
        "image" => image(view, data),
        other => Err(format!("unsupported component {other:?}")),
    }
}

fn tone(name: &str) -> Tone {
    match name {
        "success" => Tone::Success,
        "warning" => Tone::Warning,
        "error" => Tone::Critical,
        _ => Tone::Neutral,
    }
}

fn text_color(tone: &str) -> Paint {
    match tone {
        "muted" => Role::Muted.into(),
        "success" => Color::hex(0x30d158).into(),
        "warning" => Color::hex(0xffb840).into(),
        "error" => Color::hex(0xff453a).into(),
        _ => Role::Text.into(),
    }
}

pub fn insert(
    tree: &mut UiTree,
    parent: WidgetId,
    node: &Node,
    actions: &mut Vec<(WidgetId, Action)>,
) {
    match node {
        Node::Column { gap, children } | Node::Row { gap, children } => {
            let row = matches!(node, Node::Row { .. });
            let container = tree.insert(
                Some(parent),
                Container::new(style::wide(if row {
                    style::row(*gap)
                } else {
                    style::column(*gap)
                })),
            );
            for child in children {
                if row {
                    let slot = tree.insert(Some(container), Container::new(style::grow()));
                    insert(tree, slot, child, actions);
                } else {
                    insert(tree, container, child, actions);
                }
            }
        }
        Node::Section {
            title,
            gap,
            children,
        } => {
            let container = tree.insert(
                Some(parent),
                Container::new(style::wide(style::column(*gap)))
                    .background(Role::Surface)
                    .border(1.0, Role::Separator)
                    .radius(8.0)
                    .padding(16.0),
            );
            tree.insert(
                Some(container),
                Label::new(title).font_size(15.0).weight(700),
            );
            for child in children {
                insert(tree, container, child, actions);
            }
        }
        Node::Text { text, tone } => {
            tree.insert(
                Some(parent),
                Label::new(text)
                    .font_size(14.0)
                    .max_lines(24)
                    .color(text_color(tone)),
            );
        }
        Node::Metric { label, value } => {
            let container = tree.insert(
                Some(parent),
                Container::new(style::wide(style::column(6.0)))
                    .background(Role::Surface)
                    .border(1.0, Role::Separator)
                    .radius(8.0)
                    .padding(16.0),
            );
            tree.insert(
                Some(container),
                Label::new(label).font_size(12.0).color(Role::Muted),
            );
            tree.insert(
                Some(container),
                Label::new(value).font_size(30.0).weight(700),
            );
        }
        Node::Badge { text, tone: name } => {
            tree.insert(Some(parent), Tag::new(text).tone(tone(name)));
        }
        Node::Progress { label, fraction } => {
            let container = tree.insert(
                Some(parent),
                Container::new(style::wide(style::column(6.0))),
            );
            if let Some(label) = label {
                tree.insert(Some(container), Label::new(label).font_size(12.0));
            }
            tree.insert(Some(container), Progress(*fraction));
            tree.insert(
                Some(container),
                Label::new(format!("{:.0}%", fraction * 100.0))
                    .font_size(11.0)
                    .color(Role::Muted),
            );
        }
        Node::Table { columns, rows } => {
            let table = tree.insert(
                Some(parent),
                Container::new(style::wide(style::column(0.0)))
                    .border(1.0, Role::Separator)
                    .radius(6.0),
            );
            table_row(tree, table, columns, true);
            if rows.is_empty() {
                tree.insert(
                    Some(table),
                    Label::new("項目はありません")
                        .font_size(13.0)
                        .color(Role::Muted),
                );
            }
            for cells in rows {
                table_row(tree, table, cells, false);
            }
        }
        Node::List(items) => {
            let list = tree.insert(
                Some(parent),
                Container::new(style::wide(style::column(8.0))),
            );
            if items.is_empty() {
                tree.insert(
                    Some(list),
                    Label::new("項目はありません").color(Role::Muted),
                );
            }
            for item in items {
                tree.insert(
                    Some(list),
                    Label::new(format!("• {item}")).font_size(14.0).max_lines(8),
                );
            }
        }
        Node::Button { label, action } => {
            let button = tree.insert(Some(parent), Button::new(label));
            actions.push((button, action.clone()));
        }
        Node::Image {
            alt,
            width,
            height,
            shape,
            picture,
            ..
        } => {
            images::insert(
                tree,
                parent,
                alt,
                (*width, *height),
                *shape,
                picture.as_ref(),
            );
        }
    }
}

fn table_row(tree: &mut UiTree, parent: WidgetId, cells: &[String], header: bool) {
    let row = tree.insert(
        Some(parent),
        Container::new(style::wide(style::row(12.0)))
            .padding(10.0)
            .background(if header {
                Role::SurfaceAlt
            } else {
                Role::Surface
            }),
    );
    for value in cells {
        let cell = tree.insert(Some(row), Container::new(style::grow()));
        tree.insert(
            Some(cell),
            Label::new(value)
                .font_size(13.0)
                .weight(if header { 700 } else { 400 })
                .max_lines(8),
        );
    }
}

struct Progress(f64);

impl Widget for Progress {
    fn style(&self) -> Style {
        Style {
            size: LayoutSize {
                width: auto(),
                height: length(10.0),
            },
            ..style::wide(Style::default())
        }
    }

    fn paint(&mut self, cx: &mut PaintCx) {
        let rect = cx.bounds();
        let track = cx.color(Role::SurfaceAlt);
        cx.rounded_rect(rect, 5.0, track, Border::default());
        let width = rect.width * self.0 as f32;
        if width > 0.0 {
            let color = cx.color(Role::Accent);
            cx.rounded_rect(
                Rect::new(rect.x, rect.y, width, rect.height),
                5.0_f32.min(width / 2.0),
                color,
                Border::default(),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_bindings_are_values_and_never_evaluated() {
        let view = json!({"type":"text","id":"text","text":{"source":"data","pointer":"/text"}});
        let literal = "$(touch /tmp/unwanted) <script>bad()</script>";
        assert_eq!(
            resolve(&view, &json!({"text":literal})).unwrap(),
            Node::Text {
                text: literal.into(),
                tone: "default".into()
            }
        );
        assert!(resolve(&view, &json!({})).is_err());
    }

    #[test]
    fn table_pointers_and_keys_are_checked_before_rendering() {
        let view = json!({"type":"table","id":"table","rows":{"source":"data","pointer":"/rows"},"rowKey":"/id","columns":[{"title":"Name","pointer":"/name"}]});
        let data = json!({"rows":[{"id":"a","name":"first"},{"id":"b","name":"second"}]});
        assert_eq!(
            resolve(&view, &data).unwrap(),
            Node::Table {
                columns: vec!["Name".into()],
                rows: vec![vec!["first".into()], vec!["second".into()]]
            }
        );
        assert!(
            resolve(
                &view,
                &json!({"rows":[{"id":"a","name":"first"},{"id":"a","name":"again"}]})
            )
            .is_err()
        );
        assert!(resolve(&view, &json!({"rows":[{"id":"a"}]})).is_err());
    }

    #[test]
    fn action_payload_is_resolved_without_mutating_data() {
        let data = json!({"request":{"test":"one"}});
        let view = json!({"type":"button","id":"button","label":"Retry","action":"retry","payload":{"source":"data","pointer":"/request"}});
        assert_eq!(
            resolve(&view, &data).unwrap(),
            Node::Button {
                label: "Retry".into(),
                action: Action {
                    id: "retry".into(),
                    payload: json!({"test":"one"})
                }
            }
        );
        assert_eq!(data, json!({"request":{"test":"one"}}));
    }
}
