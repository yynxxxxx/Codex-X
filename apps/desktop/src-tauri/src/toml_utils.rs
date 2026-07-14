use crate::error::{CodexxError, Result};
use toml_edit::{DocumentMut, Item, Table};

pub(crate) fn string_value(doc: &DocumentMut, key: &str) -> Option<String> {
    doc.get(key)
        .and_then(|item| item.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string)
}

pub(super) fn ensure_table<'a>(parent: &'a mut Table, key: &str) -> Result<&'a mut Table> {
    if !parent.contains_key(key) {
        parent[key] = Item::Table(Table::new());
    }
    parent
        .get_mut(key)
        .and_then(|item| item.as_table_mut())
        .ok_or_else(|| CodexxError::Config(format!("{key} 不是 TOML table")))
}
