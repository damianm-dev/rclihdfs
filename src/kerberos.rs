use crate::config::AuthConfig;
use crate::error::CliError;
use crate::proc::{run, Env};

fn klist_ok(env: &Env) -> bool {
    run(&["klist", "-s"], Some(env), true).unwrap_or(1) == 0
}

fn kinit_tech(cfg: &AuthConfig, env: &Env) -> Result<(), CliError> {
    if klist_ok(env) {
        return Ok(());
    }
    let rc = run(
        &["kinit", "-kt", &cfg.tech_keytab, &cfg.tech_principal],
        Some(env),
        true,
    )?;
    if rc != 0 {
        return Err(CliError::Runtime("kinit (tech) failed".to_string()));
    }
    Ok(())
}

fn kdestroy_tech(env: &Env) {
    let _ = run(&["kdestroy"], Some(env), true);
}

/// RAII guard that selects the Kerberos context for HDFS commands.
///
/// - `use_tech = true`: sets `KRB5CCNAME` to a dedicated cache, ensures a
///   kinit for the Tech principal, and destroys only that cache on drop.
/// - `use_tech = false`: keeps the user's kerberos context untouched, and
///   HDFS commands inherit the current process environment (`env() == None`).
pub struct KerberosContext {
    env: Option<Env>,
}

impl KerberosContext {
    pub fn enter(use_tech: bool, cfg: &AuthConfig) -> Result<Self, CliError> {
        if !use_tech {
            return Ok(Self { env: None });
        }

        let mut env: Env = std::env::vars().collect();
        env.insert("KRB5CCNAME".to_string(), cfg.tech_ccache.clone());

        kinit_tech(cfg, &env)?;

        Ok(Self { env: Some(env) })
    }

    pub fn env(&self) -> Option<&Env> {
        self.env.as_ref()
    }
}

impl Drop for KerberosContext {
    fn drop(&mut self) {
        if let Some(env) = &self.env {
            kdestroy_tech(env);
        }
    }
}
