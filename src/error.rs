use std::fmt;

/// CLI error; `Policy`/`NotFound` map to exit code 1, `Runtime` to exit code 2.
#[derive(Debug)]
pub enum CliError {
    /// Policy violation, bad argument, declined confirmation, etc.
    Policy(String),
    /// Missing HDFS path.
    NotFound(String),
    /// Anything else.
    Runtime(String),
}

impl CliError {
    pub fn exit_code(&self) -> i32 {
        match self {
            CliError::Policy(_) | CliError::NotFound(_) => 1,
            CliError::Runtime(_) => 2,
        }
    }
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CliError::Policy(msg) => write!(f, "{msg}"),
            CliError::NotFound(msg) => write!(f, "{msg}"),
            CliError::Runtime(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for CliError {}

impl From<std::io::Error> for CliError {
    fn from(e: std::io::Error) -> Self {
        CliError::Runtime(e.to_string())
    }
}
