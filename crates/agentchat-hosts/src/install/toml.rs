//! TOML edits preserve foreign comments/decor and compare only the owned value.
use super::{conflict, value_digest};
use agentchat_core::Result;
use serde_json::Value;
use toml_edit::{DocumentMut, Item, Table, TableLike};
pub(super) fn parse(text: &str) -> Result<DocumentMut> {
    text.parse()
        .map_err(|_| conflict("Existing TOML is invalid."))
}
pub(super) fn replace(
    doc: &mut DocumentMut,
    path: &[String],
    value: &Value,
    old: Option<&str>,
) -> Result<String> {
    let mut table: &mut dyn TableLike = doc.as_table_mut();
    for key in &path[..path.len() - 1] {
        if !table.contains_key(key) {
            table.insert(key, Item::Table(Table::new()));
        }
        table = table
            .get_mut(key)
            .and_then(Item::as_table_like_mut)
            .ok_or_else(|| conflict("TOML ownership path crosses a non-table value."))?;
    }
    let key = &path[path.len() - 1];
    let actual = table.get(key).filter(|i| !i.is_none());
    match (actual, old) {
        (None, None) => {}
        (Some(item), Some(expected)) => {
            if item_digest(item)? != expected {
                return Err(conflict("Owned TOML entry was modified externally."));
            }
        }
        _ => {
            return Err(conflict(
                "TOML entry is missing or has an unowned name collision.",
            ));
        }
    }
    let new = item(value)?;
    // Identical requests retain the exact existing bytes and comments.
    let hash = item_digest(&new)?;
    if old != Some(hash.as_str()) {
        table.insert(key, new);
    }
    Ok(hash)
}

fn item_digest(item: &Item) -> Result<String> {
    value_digest(&json(item)?)
}
pub(super) fn json(item: &Item) -> Result<Value> {
    Ok(match item {
        Item::Table(table) => Value::Object(
            table
                .iter()
                .map(|(k, v)| Ok((k.into(), json(v)?)))
                .collect::<Result<_>>()?,
        ),
        Item::ArrayOfTables(tables) => Value::Array(
            tables
                .iter()
                .map(|t| json(&Item::Table(t.clone())))
                .collect::<Result<_>>()?,
        ),
        Item::None => return Err(conflict("Missing owned TOML value.")),
        Item::Value(value) => match value {
            toml_edit::Value::String(v) => Value::String(v.value().clone()),
            toml_edit::Value::Integer(v) => Value::from(*v.value()),
            toml_edit::Value::Float(v) => Value::Number(
                serde_json::Number::from_f64(*v.value())
                    .ok_or_else(|| conflict("Nonfinite owned TOML value."))?,
            ),
            toml_edit::Value::Boolean(v) => Value::Bool(*v.value()),
            toml_edit::Value::Array(values) => Value::Array(
                values
                    .iter()
                    .map(|v| json(&Item::Value(v.clone())))
                    .collect::<Result<_>>()?,
            ),
            toml_edit::Value::InlineTable(table) => Value::Object(
                table
                    .iter()
                    .map(|(k, v)| Ok((k.into(), json(&Item::Value(v.clone()))?)))
                    .collect::<Result<_>>()?,
            ),
            toml_edit::Value::Datetime(_) => {
                return Err(conflict(
                    "Owned TOML datetime is not a JSON-compatible value.",
                ));
            }
        },
    })
}
fn item(value: &Value) -> Result<Item> {
    // Convert explicitly: serde_json's arbitrary-precision Number serializes
    // through a private struct understood only by JSON serializers, not TOML.
    Ok(match value {
        Value::Object(map) => {
            let mut table = Table::new();
            for (key, value) in map {
                table.insert(key, item(value)?);
            }
            Item::Table(table)
        }
        Value::Array(values) => {
            let mut array = toml_edit::Array::new();
            for value in values {
                array.push(
                    item(value)?
                        .into_value()
                        .map_err(|_| conflict("Invalid TOML array member."))?,
                );
            }
            Item::Value(toml_edit::Value::Array(array))
        }
        Value::String(value) => toml_edit::value(value),
        Value::Bool(value) => toml_edit::value(*value),
        Value::Number(value) => {
            if let Some(integer) = value.as_i64() {
                toml_edit::value(integer)
            } else if value.is_f64()
                && let Some(float) = value.as_f64().filter(|f| f.is_finite())
            {
                toml_edit::value(float)
            } else {
                return Err(conflict(
                    "Owned number exceeds TOML's signed integer/finite float range.",
                ));
            }
        }
        Value::Null => return Err(conflict("Null is not representable in TOML.")),
    })
}
