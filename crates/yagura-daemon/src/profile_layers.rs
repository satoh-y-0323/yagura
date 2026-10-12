//! Laying a user's profile file over the profile shipped with this build.
//!
//! The shipped profile is the base. The user's file carries only what the
//! user changed: a table is merged key by key, and any other value (a string,
//! an array) replaces the shipped one whole. The result is TOML text for
//! [`crate::profile::parse_profile`], so every rule a profile is checked
//! against applies to what is actually used.

use toml_edit::{Document, Item, Table, Value};

/// `overlay` laid over `base`, both profile documents, as TOML text.
///
/// Keys keep the base's order, and a key only the overlay has comes after
/// them.
///
/// # Errors
///
/// Returns the parse error of whichever document is not valid TOML.
pub(crate) fn overlay_source(base: &str, overlay: &str) -> Result<String, toml_edit::TomlError> {
    Ok(overlay_document(base, overlay)?.to_string())
}

/// [`overlay_source`] as a document, every table in it a standard one.
///
/// # Errors
///
/// Returns the parse error of whichever document is not valid TOML.
pub(crate) fn overlay_document(
    base: &str,
    overlay: &str,
) -> Result<Document, toml_edit::TomlError> {
    let mut base = normalized_document(base)?;
    let overlay = normalized_document(overlay)?;
    merge(base.as_table_mut(), overlay.as_table().clone());
    Ok(base)
}

/// `source` parsed, with every inline table turned into a standard one.
///
/// # Errors
///
/// Returns the parse error when `source` is not valid TOML.
pub(crate) fn normalized_document(source: &str) -> Result<Document, toml_edit::TomlError> {
    let mut document: Document = source.parse()?;
    tables_from_inline(document.as_table_mut());
    Ok(document)
}

/// Turns every inline table into a standard one, so a table written either
/// way in one document merges with the same table written the other way in
/// the other.
fn tables_from_inline(table: &mut Table) {
    for (_, item) in table.iter_mut() {
        if matches!(item, Item::Value(Value::InlineTable(_)))
            && let Item::Value(Value::InlineTable(inline)) = std::mem::take(item)
        {
            *item = Item::Table(inline.into_table());
        }
        if let Item::Table(child) = item {
            tables_from_inline(child);
        }
    }
}

fn merge(base: &mut Table, overlay: Table) {
    for (key, item) in overlay {
        match (base.get_mut(&key), item) {
            (Some(Item::Table(base_child)), Item::Table(overlay_child)) => {
                merge(base_child, overlay_child);
            }
            (_, item) => {
                base.insert(&key, item);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::overlay_source;

    fn merged(base: &str, overlay: &str) -> toml::Value {
        toml::from_str(&overlay_source(base, overlay).unwrap()).unwrap()
    }

    #[test]
    fn a_table_merges_key_by_key_and_other_values_replace_whole() {
        let value = merged(
            "a = 1\nlist = [1, 2]\n[t]\nx = 1\ny = 2\n",
            "list = [3]\n[t]\ny = 9\nz = 3\n",
        );
        assert_eq!(value["a"].as_integer(), Some(1));
        assert_eq!(value["list"].as_array().unwrap().len(), 1);
        assert_eq!(value["t"]["x"].as_integer(), Some(1));
        assert_eq!(value["t"]["y"].as_integer(), Some(9));
        assert_eq!(value["t"]["z"].as_integer(), Some(3));
    }

    #[test]
    fn an_inline_table_and_a_standard_table_merge_either_way() {
        let value = merged("[t]\nf = { a = 1, b = 2 }\n", "[t.f]\nb = 5\n");
        assert_eq!(value["t"]["f"]["a"].as_integer(), Some(1));
        assert_eq!(value["t"]["f"]["b"].as_integer(), Some(5));
        let value = merged("[t.f]\na = 1\nb = 2\n", "t = { f = { b = 5 } }\n");
        assert_eq!(value["t"]["f"]["a"].as_integer(), Some(1));
        assert_eq!(value["t"]["f"]["b"].as_integer(), Some(5));
    }

    #[test]
    fn keys_keep_the_base_order() {
        let text = overlay_source(
            "[t]\nfirst = 1\nsecond = 2\n",
            "[t]\nfirst = 9\nthird = 3\n",
        )
        .unwrap();
        let first = text.find("first").unwrap();
        let second = text.find("second").unwrap();
        let third = text.find("third").unwrap();
        assert!(first < second && second < third, "{text}");
    }
}
