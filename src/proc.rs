use std::collections::HashMap;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::error::CliError;

pub type Env = HashMap<String, String>;

static INTERRUPTED: AtomicBool = AtomicBool::new(false);

/// Catches SIGINT/SIGTERM instead of dying on them, so `Drop` impls (e.g.
/// `KerberosContext`'s `kdestroy`) still run. A handler, unlike `SIG_IGN`,
/// is reset on exec, so child `hdfs` processes still die on Ctrl-C.
pub fn install_interrupt_handler() {
    let _ = ctrlc::set_handler(|| INTERRUPTED.store(true, Ordering::SeqCst));
}

/// Checked after every child exits, so an interrupt aborts the operation
/// via `?` while a child already started (e.g. `kdestroy`) still completes.
fn check_interrupted() -> Result<(), CliError> {
    if INTERRUPTED.load(Ordering::SeqCst) {
        return Err(CliError::Runtime("interrupted".to_string()));
    }
    Ok(())
}

/// Runs a command, optionally overriding the environment, and returns its
/// exit code.
pub fn run(cmd: &[&str], env: Option<&Env>, quiet: bool) -> Result<i32, CliError> {
    let mut command = Command::new(cmd[0]);
    command.args(&cmd[1..]);
    if let Some(env) = env {
        command.envs(env);
    }
    if quiet {
        command.stdout(Stdio::null()).stderr(Stdio::null());
    }
    let status = command
        .status()
        .map_err(|e| CliError::Runtime(format!("failed to run {}: {e}", cmd[0])))?;
    check_interrupted()?;
    Ok(status.code().unwrap_or(-1))
}

/// Runs a command and captures its stdout/stderr as text.
pub fn run_captured(cmd: &[&str], env: Option<&Env>) -> Result<Output, CliError> {
    let mut command = Command::new(cmd[0]);
    command.args(&cmd[1..]);
    if let Some(env) = env {
        command.envs(env);
    }
    let output = command
        .output()
        .map_err(|e| CliError::Runtime(format!("failed to run {}: {e}", cmd[0])))?;
    check_interrupted()?;
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sigint_sets_flag_and_aborts_after_child_runs() {
        install_interrupt_handler();
        assert_eq!(run(&["true"], None, true).unwrap(), 0);

        // The process must survive SIGINT, and the next command must still
        // execute (so `kdestroy` in `Drop` runs) but report the interrupt.
        unsafe { libc::raise(libc::SIGINT) };
        std::thread::sleep(std::time::Duration::from_millis(200));
        let marker = std::env::temp_dir().join(format!("rclihdfs_sigint_{}", std::process::id()));
        let touch = ["touch", marker.to_str().unwrap()];
        assert!(matches!(run(&touch, None, true), Err(CliError::Runtime(_))));
        assert!(marker.exists());
        let _ = std::fs::remove_file(&marker);
        assert!(run_captured(&["true"], None).is_err());

        INTERRUPTED.store(false, Ordering::SeqCst);
    }
}
