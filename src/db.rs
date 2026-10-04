use chrono::NaiveDateTime;
use regex::Regex;

use crate::error::CliError;

/// Writes an operation audit record to Postgres.
pub fn log_to_db(
    username: &str,
    mode: &str,
    source: &str,
    target: &str,
    dt: NaiveDateTime,
) -> Result<(), CliError> {
    let table = std::env::var("DB.TABLE").unwrap_or_default();
    let table_re = Regex::new(r"^[a-zA-Z0-9_.]+$").unwrap();
    if !table_re.is_match(&table) {
        return Err(CliError::Policy(format!(
            "Unsafe DB.TABLE value: {table:?}"
        )));
    }

    let user = std::env::var("DB.USER").unwrap_or_default();
    let password = std::env::var("DB.PASSWORD").unwrap_or_default();
    let host = std::env::var("DB.HOST").unwrap_or_default();
    let port = std::env::var("DB.PORT").unwrap_or_default();
    let dbname = std::env::var("DB.NAME").unwrap_or_default();

    let conn_str =
        format!("user={user} password={password} host={host} port={port} dbname={dbname}");

    let mut client = postgres::Client::connect(&conn_str, postgres::NoTls)
        .map_err(|e| CliError::Runtime(format!("failed to connect to db: {e}")))?;

    let query = format!(
        "INSERT INTO {table}(username, mode, source, target, date) VALUES ($1,$2,$3,$4,$5)"
    );
    client
        .execute(&query, &[&username, &mode, &source, &target, &dt])
        .map_err(|e| CliError::Runtime(format!("failed to insert audit record: {e}")))?;

    Ok(())
}
