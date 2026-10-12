//! The one place this crate hands SQL text to SQLite.
//!
//! Every method takes its statement as `&'static str`, so the text is fixed
//! when the binary is built and only bound parameters vary at run time
//! (CWE-89). `clippy.toml` disallows rusqlite's own text-taking methods
//! everywhere else.

use rusqlite::{Connection, Params, Result, Row, Statement, ToSql};

/// [`Connection`]'s text-taking methods, restricted to statements fixed at
/// build time. A [`rusqlite::Transaction`] reaches them through its `Deref`.
pub(crate) trait FixedSql {
    /// [`Connection::execute`].
    fn execute_fixed<P: Params>(&self, sql: &'static str, params: P) -> Result<usize>;

    /// [`Connection::execute_batch`].
    fn execute_batch_fixed(&self, sql: &'static str) -> Result<()>;

    /// [`Connection::prepare`].
    fn prepare_fixed(&self, sql: &'static str) -> Result<Statement<'_>>;

    /// [`Connection::query_row`].
    fn query_row_fixed<T, P, F>(&self, sql: &'static str, params: P, f: F) -> Result<T>
    where
        P: Params,
        F: FnOnce(&Row<'_>) -> Result<T>;

    /// [`Connection::pragma_update`] on the main schema.
    fn pragma_update_fixed<V: ToSql>(&self, name: &'static str, value: V) -> Result<()>;
}

#[allow(clippy::disallowed_methods)] // the one caller of the text-taking methods
impl FixedSql for Connection {
    fn execute_fixed<P: Params>(&self, sql: &'static str, params: P) -> Result<usize> {
        self.execute(sql, params)
    }

    fn execute_batch_fixed(&self, sql: &'static str) -> Result<()> {
        self.execute_batch(sql)
    }

    fn prepare_fixed(&self, sql: &'static str) -> Result<Statement<'_>> {
        self.prepare(sql)
    }

    fn query_row_fixed<T, P, F>(&self, sql: &'static str, params: P, f: F) -> Result<T>
    where
        P: Params,
        F: FnOnce(&Row<'_>) -> Result<T>,
    {
        self.query_row(sql, params, f)
    }

    fn pragma_update_fixed<V: ToSql>(&self, name: &'static str, value: V) -> Result<()> {
        self.pragma_update(None, name, value)
    }
}
