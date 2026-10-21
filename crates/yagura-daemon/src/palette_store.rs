//! Persistence port for the default palettes the GUI last sent
//! (the `default_palettes` table), and the one rule a [`DefaultPalettes`] is accepted by, whether it arrives
//! from the wire or is read back from the database.
//!
//! The table holds a single row: the palettes as JSON, with the vendor names
//! as data inside it rather than in the schema.

use std::path::Path;

use rusqlite::types::ValueRef;
use rusqlite::{Connection, OptionalExtension, params};
use thiserror::Error;
use yagura_protocol::{DEFAULT_PALETTES_MAX_VENDORS, DefaultPalettes, validate_window_vendor};

use crate::sql::FixedSql;
use crate::store::{self, StoreError};

/// The most bytes a stored `palettes_json` is read back from; a larger value
/// reads back as nothing saved. Well above what
/// [`DEFAULT_PALETTES_MAX_VENDORS`] palettes take.
pub const MAX_DEFAULT_PALETTES_JSON_BYTES: usize = 256 * 1024;

/// The number of ANSI colors every palette holds: the daemon reads them by
/// index.
const ANSI_COLOR_COUNT: usize = 16;

/// Why [`accept_default_palettes`] refused a [`DefaultPalettes`].
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum DefaultPalettesError {
    /// A palette does not hold exactly 16 ANSI colors.
    #[error("every palette must hold {ANSI_COLOR_COUNT} ANSI colors")]
    AnsiCount,
    /// More vendors than [`DEFAULT_PALETTES_MAX_VENDORS`].
    #[error("at most {DEFAULT_PALETTES_MAX_VENDORS} vendors may be assigned a palette")]
    TooManyVendors,
}

/// Checks `palettes` and drops every vendor key [`validate_window_vendor`]
/// refuses: no window can have such a vendor, so the key could never be
/// looked up. The dropped keys are warned about by count, never echoed.
///
/// # Errors
///
/// [`DefaultPalettesError`] when a palette does not hold 16 ANSI colors or
/// there are more than [`DEFAULT_PALETTES_MAX_VENDORS`] vendors.
pub fn accept_default_palettes(
    mut palettes: DefaultPalettes,
) -> Result<DefaultPalettes, DefaultPalettesError> {
    if palettes.vendors.len() > DEFAULT_PALETTES_MAX_VENDORS {
        return Err(DefaultPalettesError::TooManyVendors);
    }
    if std::iter::once(&palettes.app)
        .chain(palettes.vendors.values())
        .any(|palette| palette.ansi.len() != ANSI_COLOR_COUNT)
    {
        return Err(DefaultPalettesError::AnsiCount);
    }
    let before = palettes.vendors.len();
    palettes
        .vendors
        .retain(|vendor, _| validate_window_vendor(vendor).is_ok());
    let dropped = before - palettes.vendors.len();
    if dropped > 0 {
        tracing::warn!(
            dropped,
            "default palettes: dropped vendor keys no window can have"
        );
    }
    Ok(palettes)
}

/// The `default_palettes` table.
pub struct PaletteStore {
    conn: Connection,
}

impl PaletteStore {
    /// Opens (creating and migrating as needed) the database at `db_path`.
    ///
    /// # Errors
    ///
    /// Any [`StoreError`] from opening or migrating the database.
    pub fn open(db_path: &Path) -> Result<Self, StoreError> {
        let conn = store::open_migrated(db_path)?;
        Ok(Self { conn })
    }

    /// Replaces the saved default palettes with `palettes`, as a whole.
    ///
    /// # Errors
    ///
    /// [`StoreError::Serialize`] or [`StoreError::Sqlite`] when the row
    /// cannot be written.
    pub fn save(&mut self, palettes: &DefaultPalettes) -> Result<(), StoreError> {
        let json = serde_json::to_string(palettes)?;
        self.conn.execute_fixed(
            "INSERT INTO default_palettes (id, palettes_json) VALUES (1, ?1) \
             ON CONFLICT(id) DO UPDATE SET palettes_json = excluded.palettes_json",
            params![json],
        )?;
        Ok(())
    }

    /// The saved default palettes, `None` when nothing was saved. A saved
    /// value that is larger than [`MAX_DEFAULT_PALETTES_JSON_BYTES`], is not
    /// readable as [`DefaultPalettes`] or that [`accept_default_palettes`]
    /// refuses reads back as `None`, with a warning.
    ///
    /// # Errors
    ///
    /// [`StoreError::Sqlite`] when the query itself fails.
    pub fn load(&self) -> Result<Option<DefaultPalettes>, StoreError> {
        let decoded = self
            .conn
            .query_row_fixed(
                "SELECT palettes_json FROM default_palettes WHERE id = 1",
                [],
                |row| Ok(decode_saved(row.get_ref(0)?)),
            )
            .optional()?;
        let Some(decoded) = decoded else {
            return Ok(None);
        };
        match decoded {
            Ok(palettes) => Ok(Some(palettes)),
            Err(reason) => {
                tracing::warn!("ignoring the saved default palettes: {reason}");
                Ok(None)
            }
        }
    }
}

fn decode_saved(cell: ValueRef<'_>) -> Result<DefaultPalettes, String> {
    let ValueRef::Text(bytes) = cell else {
        return Err("not text".to_string());
    };
    if bytes.len() > MAX_DEFAULT_PALETTES_JSON_BYTES {
        return Err(format!(
            "larger than {MAX_DEFAULT_PALETTES_JSON_BYTES} bytes"
        ));
    }
    let palettes: DefaultPalettes = serde_json::from_slice(bytes).map_err(|err| err.to_string())?;
    accept_default_palettes(palettes).map_err(|err| err.to_string())
}
