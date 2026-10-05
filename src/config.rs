use crate::error::CliError;
use crate::policy::{norm_path, Layout};

pub const CONF_PATH: &str = "/etc/clihdfs/conf";

/// Kerberos settings for the tech (service) principal.
#[derive(Debug, Clone)]
pub struct AuthConfig {
    pub tech_principal: String,
    pub tech_keytab: String,
    /// Separate cache so we never touch the user's default kerberos session,
    /// e.g. "FILE:/tmp/krb5cc_xxx".
    pub tech_ccache: String,
}

/// Loads `/etc/clihdfs/conf` into the process environment; fails if the file
/// is missing, unreadable or malformed. File values override the caller's
/// environment, so a user cannot redefine the policy roots or principal with
/// e.g. `HDFS_PROD_ROOT=... rclihdfs`.
pub fn load_conf() -> Result<(), CliError> {
    load_conf_from(CONF_PATH)
}

fn load_conf_from(path: &str) -> Result<(), CliError> {
    dotenvy::from_path_override(path)
        .map_err(|e| CliError::Runtime(format!("cannot load config {path}: {e}")))
}

impl AuthConfig {
    /// Short name of the tech principal (`name` of `name[/instance]@REALM`),
    /// i.e. the HDFS user it acts as.
    pub fn tech_user(&self) -> &str {
        self.tech_principal
            .split(['/', '@'])
            .next()
            .unwrap_or_default()
    }
}

fn required_var(key: &str) -> Result<String, CliError> {
    std::env::var(key).map_err(|_| CliError::Runtime(format!("{key} is not set")))
}

pub fn build_auth_config() -> Result<AuthConfig, CliError> {
    let tech_keytab = required_var("TECH_KEYTAB_PATH")?;
    let tech_principal = required_var("TECH_PRINCIPAL")?;

    let uid = unsafe { libc::getuid() };

    Ok(AuthConfig {
        tech_principal,
        tech_keytab,
        tech_ccache: format!("FILE:/tmp/krb5cc_clihdfs_{uid}_tech"),
    })
}

/// Reads an absolute, non-root HDFS path and strips trailing slashes.
fn required_root(key: &str) -> Result<String, CliError> {
    let value = norm_path(&required_var(key)?);
    if !value.starts_with('/') || value == "/" {
        return Err(CliError::Runtime(format!(
            "{key} must be an absolute path other than '/': {value:?}"
        )));
    }
    Ok(value)
}

pub fn build_layout() -> Result<Layout, CliError> {
    Ok(Layout {
        prod: required_root("HDFS_PROD_ROOT")?,
        staging: required_root("HDFS_STAGING_ROOT")?,
        external: required_root("HDFS_EXTERNAL_ROOT")?,
        mv_to_prod: required_root("HDFS_MV_TO_PROD_ROOT")?,
        to_staging: required_root("HDFS_TO_STAGING_ROOT")?,
    })
}

/// Username of the real uid from the passwd database. Deliberately not read
/// from `LOGNAME`/`USER` (as `getpass.getuser()` does), since those can be
/// set by the caller to impersonate another user.
pub fn current_username() -> Result<String, CliError> {
    let pw = unsafe { libc::getpwuid(libc::getuid()) };
    if pw.is_null() {
        return Err(CliError::Runtime(
            "could not determine current username".to_string(),
        ));
    }
    let name = unsafe { std::ffi::CStr::from_ptr((*pw).pw_name) };
    Ok(name.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn username_ignores_env_overrides() {
        let real = std::process::Command::new("id")
            .arg("-un")
            .output()
            .unwrap();
        let real = String::from_utf8(real.stdout).unwrap().trim().to_string();

        for key in ["LOGNAME", "USER", "LNAME", "USERNAME"] {
            std::env::set_var(key, "someone_else");
        }
        assert_eq!(current_username().unwrap(), real);
    }

    #[test]
    fn tech_user_strips_instance_and_realm() {
        let mut cfg = AuthConfig {
            tech_principal: "svc@EXAMPLE.COM".to_string(),
            tech_keytab: String::new(),
            tech_ccache: String::new(),
        };
        assert_eq!(cfg.tech_user(), "svc");
        cfg.tech_principal = "svc/host.example.com@EXAMPLE.COM".to_string();
        assert_eq!(cfg.tech_user(), "svc");
    }

    #[test]
    fn load_conf_requires_a_valid_file() {
        let dir = std::env::temp_dir();
        let pid = std::process::id();

        let missing = dir.join(format!("rclihdfs_conf_missing_{pid}"));
        assert!(load_conf_from(missing.to_str().unwrap()).is_err());

        let bad = dir.join(format!("rclihdfs_conf_bad_{pid}"));
        std::fs::write(&bad, "RCLIHDFS_TEST_BAD='unterminated\n").unwrap();
        assert!(load_conf_from(bad.to_str().unwrap()).is_err());
        let _ = std::fs::remove_file(&bad);

        let good = dir.join(format!("rclihdfs_conf_good_{pid}"));
        std::fs::write(&good, "RCLIHDFS_TEST_KEY=from_file\n").unwrap();
        std::env::set_var("RCLIHDFS_TEST_KEY", "from_env");
        load_conf_from(good.to_str().unwrap()).unwrap();
        assert_eq!(std::env::var("RCLIHDFS_TEST_KEY").unwrap(), "from_file");
        let _ = std::fs::remove_file(&good);
    }

    #[test]
    fn layout_from_env() {
        let keys = [
            ("HDFS_PROD_ROOT", "/data/"),
            ("HDFS_STAGING_ROOT", "/data/staging"),
            ("HDFS_EXTERNAL_ROOT", "/ext"),
            ("HDFS_MV_TO_PROD_ROOT", "/compute"),
            ("HDFS_TO_STAGING_ROOT", "/science"),
        ];
        for (k, v) in keys {
            std::env::set_var(k, v);
        }
        assert_eq!(build_layout().unwrap(), crate::policy::sample_layout());

        std::env::set_var("HDFS_EXTERNAL_ROOT", "/");
        assert!(build_layout().is_err());
        std::env::set_var("HDFS_EXTERNAL_ROOT", "ext");
        assert!(build_layout().is_err());
        std::env::remove_var("HDFS_EXTERNAL_ROOT");
        assert!(build_layout().is_err());
    }
}
