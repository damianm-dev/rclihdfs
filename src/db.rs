use chrono::NaiveDateTime;
use regex::Regex;

use crate::error::CliError;

/// Reads and validates `DB.TABLE`. Rejected early because it is interpolated
/// into the SQL, so it cannot be bound as a parameter.
fn audit_table() -> Result<String, CliError> {
    let table = std::env::var("DB.TABLE").unwrap_or_default();
    let table_re = Regex::new(r"^[a-zA-Z0-9_.]+$").unwrap();
    if !table_re.is_match(&table) {
        return Err(CliError::Runtime(format!(
            "Unsafe DB.TABLE value: {table:?}"
        )));
    }
    Ok(table)
}

/// Opens a connection to the audit database from the `DB.*` settings.
fn connect() -> Result<postgres::Client, CliError> {
    let user = std::env::var("DB.USER").unwrap_or_default();
    let password = std::env::var("DB.PASSWORD").unwrap_or_default();
    let host = std::env::var("DB.HOST").unwrap_or_default();
    let port = std::env::var("DB.PORT").unwrap_or_default();
    let dbname = std::env::var("DB.NAME").unwrap_or_default();

    let conn_str =
        format!("user={user} password={password} host={host} port={port} dbname={dbname}");

    postgres::Client::connect(&conn_str, postgres::NoTls)
        .map_err(|e| CliError::Runtime(format!("failed to connect to db: {e}")))
}

/// A single audit-database connection, reused for every record written by one
/// command run.
pub struct AuditLog {
    client: postgres::Client,
    table: String,
}

impl AuditLog {
    /// Validates the config and opens the connection. Called before any HDFS
    /// change is made, so it doubles as the health check: if the database is
    /// misconfigured or unreachable, the command aborts before touching HDFS.
    pub fn open() -> Result<Self, CliError> {
        let table = audit_table()?;
        let client = connect()?;
        Ok(Self { client, table })
    }

    /// Writes one audit record, reusing the connection.
    pub fn insert(
        &mut self,
        username: &str,
        mode: &str,
        source: &str,
        target: &str,
        dt: NaiveDateTime,
    ) -> Result<(), CliError> {
        let query = format!(
            "INSERT INTO {}(username, mode, source, target, date) VALUES ($1,$2,$3,$4,$5)",
            self.table
        );
        self.client
            .execute(&query, &[&username, &mode, &source, &target, &dt])
            .map_err(|e| CliError::Runtime(format!("failed to insert audit record: {e}")))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unsafe_table() {
        std::env::set_var("DB.TABLE", "audit; DROP TABLE x");
        assert!(matches!(audit_table(), Err(CliError::Runtime(_))));
        std::env::set_var("DB.TABLE", "public.hdfs_audit");
        assert_eq!(audit_table().unwrap(), "public.hdfs_audit");
        std::env::remove_var("DB.TABLE");
    }
}
