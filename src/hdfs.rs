use std::io::IsTerminal;

use indicatif::{ProgressBar, ProgressStyle};

use crate::error::CliError;
use crate::policy::Layout;
use crate::proc::{run, run_captured, Env};

/// A progress bar for multi-file operations, shown only when stderr is an
/// actual terminal (never in a log file, CI, or Airflow task output).
fn progress_bar(len: usize) -> Option<ProgressBar> {
    if !std::io::stderr().is_terminal() {
        return None;
    }
    let bar = ProgressBar::new(len as u64);
    bar.set_style(
        ProgressStyle::with_template("{spinner:.cyan} [{bar:30.cyan/blue}] {pos}/{len} {wide_msg}")
            .unwrap()
            .progress_chars("=>-"),
    );
    Some(bar)
}

pub fn exists(path: &str, env: Option<&Env>) -> bool {
    run(&["hdfs", "dfs", "-test", "-e", path], env, true).unwrap_or(1) == 0
}

pub fn is_dir(path: &str, env: Option<&Env>) -> bool {
    run(&["hdfs", "dfs", "-test", "-d", path], env, true).unwrap_or(1) == 0
}

pub fn get_replication(path: &str, env: Option<&Env>) -> Result<u32, CliError> {
    let out = run_captured(&["hdfs", "dfs", "-stat", "%r", path], env)?;
    if !out.status.success() {
        return Err(CliError::Runtime(format!(
            "failed to get replication for: {path}"
        )));
    }
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse::<u32>()
        .map_err(|e| CliError::Runtime(format!("failed to parse replication for {path}: {e}")))
}

/// If `path` is a file, returns `[path]`. If it's a directory, returns all
/// files recursively under it.
pub fn list_all_files(path: &str, env: Option<&Env>) -> Result<Vec<String>, CliError> {
    if !exists(path, env) {
        return Ok(Vec::new());
    }

    if !is_dir(path, env) {
        return Ok(vec![path.to_string()]);
    }

    let out = run_captured(&["hdfs", "dfs", "-ls", "-R", path], env)?;
    if !out.status.success() {
        return Err(CliError::Runtime(format!(
            "failed to list files under: {path}"
        )));
    }

    let mut files = Vec::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with("Found ") {
            continue;
        }
        let parts: Vec<&str> = line.split_whitespace().collect();
        let Some(first) = parts.first() else {
            continue;
        };
        // a file entry in `hdfs ls` output starts with '-'
        if first.starts_with('-') {
            if let Some(last) = parts.last() {
                files.push((*last).to_string());
            }
        }
    }

    Ok(files)
}

/// For mv from staging -> prod (excluding staging itself), requires
/// every file to have replication >= `min_replication`.
pub fn ensure_min_replication_for_staging_to_prod(
    source: &str,
    target: &str,
    env: Option<&Env>,
    layout: &Layout,
    min_replication: u32,
) -> Result<(), CliError> {
    if !layout.is_staging_to_prod_move(source, target) {
        return Ok(());
    }

    let files = list_all_files(source, env)?;
    if files.is_empty() {
        return Err(CliError::Runtime(format!(
            "no files found to validate replication under: {source}"
        )));
    }

    let mut bad = Vec::new();
    for f in &files {
        let repl = get_replication(f, env)?;
        if repl < min_replication {
            bad.push((f.clone(), repl));
        }
    }

    if !bad.is_empty() {
        let details = bad
            .iter()
            .take(20)
            .map(|(path, repl)| format!("{path} (replication={repl})"))
            .collect::<Vec<_>>()
            .join("\n");
        let more = if bad.len() > 20 {
            format!("\n... and {} more file(s)", bad.len() - 20)
        } else {
            String::new()
        };

        return Err(CliError::Runtime(format!(
            "move from {} to {} is forbidden: replication factor is less than {min_replication}\n{details}{more}",
            layout.staging, layout.prod
        )));
    }

    Ok(())
}

pub fn mkdir_p(path: &str, env: Option<&Env>) -> Result<(), CliError> {
    let rc = run(&["hdfs", "dfs", "-mkdir", "-p", path], env, true)?;
    if rc != 0 {
        return Err(CliError::Runtime(format!("hdfs mkdir failed: {path}")));
    }
    Ok(())
}

pub fn cp(source: &str, target: &str, env: Option<&Env>) -> Result<(), CliError> {
    let rc = run(&["hdfs", "dfs", "-cp", source, target], env, false)?;
    if rc != 0 {
        return Err(CliError::Runtime("hdfs cp failed".to_string()));
    }
    Ok(())
}

pub fn mv(source: &str, target: &str, env: Option<&Env>) -> Result<(), CliError> {
    let rc = run(&["hdfs", "dfs", "-mv", source, target], env, false)?;
    if rc != 0 {
        return Err(CliError::Runtime("hdfs mv failed".to_string()));
    }
    Ok(())
}

pub fn rm_rf(path: &str, env: Option<&Env>) {
    let _ = run(&["hdfs", "dfs", "-rm", "-r", "-f", path], env, true);
}

pub fn mv_many(sources: &[String], target: &str, env: Option<&Env>) -> Result<(), CliError> {
    let mut failed = Vec::new();
    let bar = progress_bar(sources.len());
    for source in sources {
        if let Some(bar) = &bar {
            bar.set_message(source.clone());
        }
        let rc = match &bar {
            Some(bar) => {
                bar.suspend(|| run(&["hdfs", "dfs", "-mv", source, target], env, false))?
            }
            None => run(&["hdfs", "dfs", "-mv", source, target], env, false)?,
        };
        if rc != 0 {
            failed.push(source.clone());
        }
        if let Some(bar) = &bar {
            bar.inc(1);
        }
    }
    if let Some(bar) = bar {
        bar.finish_and_clear();
    }
    if !failed.is_empty() {
        return Err(CliError::Runtime(format!(
            "hdfs mv failed for {} object(s): {}",
            failed.len(),
            failed.join(", ")
        )));
    }
    Ok(())
}

pub fn cp_many(sources: &[String], target: &str, env: Option<&Env>) -> Result<(), CliError> {
    let mut failed = Vec::new();
    let bar = progress_bar(sources.len());
    for source in sources {
        if let Some(bar) = &bar {
            bar.set_message(source.clone());
        }
        let rc = match &bar {
            Some(bar) => {
                bar.suspend(|| run(&["hdfs", "dfs", "-cp", source, target], env, false))?
            }
            None => run(&["hdfs", "dfs", "-cp", source, target], env, false)?,
        };
        if rc != 0 {
            failed.push(source.clone());
        }
        if let Some(bar) = &bar {
            bar.inc(1);
        }
    }
    if let Some(bar) = bar {
        bar.finish_and_clear();
    }
    if !failed.is_empty() {
        return Err(CliError::Runtime(format!(
            "hdfs cp failed for {} object(s): {}",
            failed.len(),
            failed.join(", ")
        )));
    }
    Ok(())
}

pub fn resolve_glob(pattern: &str, env: Option<&Env>) -> Result<Vec<String>, CliError> {
    let out = run_captured(&["hdfs", "dfs", "-ls", "-d", pattern], env)?;
    if !out.status.success() {
        return Ok(Vec::new());
    }

    let mut matches = Vec::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with("Found ") {
            continue;
        }
        if let Some(last) = line.split_whitespace().last() {
            matches.push(last.to_string());
        }
    }

    Ok(matches)
}
