use std::collections::HashSet;

use crate::error::CliError;
use crate::policy::{is_under, Layout};
use crate::proc::{run, run_captured, Env};

/// Maps the exit code of `hdfs dfs -test`: 0 is true, 1 is false. A missing
/// path gives 1 silently; other failures (e.g. Kerberos) also give 1, but
/// `hdfs` prints their cause to stderr, which is left visible.
fn test_result(rc: i32) -> Result<bool, CliError> {
    match rc {
        0 => Ok(true),
        1 => Ok(false),
        rc => Err(CliError::Runtime(format!("hdfs -test failed (exit {rc})"))),
    }
}

pub fn exists(path: &str, env: Option<&Env>) -> Result<bool, CliError> {
    test_result(run(&["hdfs", "dfs", "-test", "-e", path], env, false)?)
}

pub fn is_dir(path: &str, env: Option<&Env>) -> Result<bool, CliError> {
    test_result(run(&["hdfs", "dfs", "-test", "-d", path], env, false)?)
}

/// Parses `hdfs dfs -ls -R` output into `(path, replication)` for every file
/// entry. Lines look like `-rw-r--r--  3 owner group size date time path`;
/// directories (first char `d`, replication `-`) are skipped.
fn parse_ls_files(stdout: &str) -> Result<Vec<(String, u32)>, CliError> {
    let mut files = Vec::new();
    for line in stdout.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        // a file entry in `hdfs ls` output starts with '-'
        if parts.len() < 8 || !parts[0].starts_with('-') {
            continue;
        }
        let path = parts[parts.len() - 1];
        let repl = parts[1].parse::<u32>().map_err(|e| {
            CliError::Runtime(format!("failed to parse replication for {path}: {e}"))
        })?;
        files.push((path.to_string(), repl));
    }
    Ok(files)
}

/// All files under `paths` (each a file or a directory) with their
/// replication factor, from a single `hdfs dfs -ls -R` call.
fn files_with_replication(
    paths: &[&str],
    env: Option<&Env>,
) -> Result<Vec<(String, u32)>, CliError> {
    let mut cmd = vec!["hdfs", "dfs", "-ls", "-R"];
    cmd.extend_from_slice(paths);
    let out = run_captured(&cmd, env)?;
    if !out.status.success() {
        eprint!("{}", String::from_utf8_lossy(&out.stderr));
        return Err(CliError::Runtime(format!(
            "failed to list files under: {}",
            paths.join(", ")
        )));
    }
    parse_ls_files(&String::from_utf8_lossy(&out.stdout))
}

/// For mv from staging -> prod (excluding staging itself), requires
/// every file under every such source to have replication >= `min_replication`.
pub fn ensure_min_replication_for_staging_to_prod(
    sources: &[String],
    target: &str,
    env: Option<&Env>,
    layout: &Layout,
    min_replication: u32,
) -> Result<(), CliError> {
    let sources: Vec<&str> = sources
        .iter()
        .map(String::as_str)
        .filter(|s| layout.is_staging_to_prod_move(s, target))
        .collect();
    if sources.is_empty() {
        return Ok(());
    }

    let files = files_with_replication(&sources, env)?;
    check_replication(&sources, &files, layout, min_replication)
}

fn check_replication(
    sources: &[&str],
    files: &[(String, u32)],
    layout: &Layout,
    min_replication: u32,
) -> Result<(), CliError> {
    if let Some(source) = sources
        .iter()
        .find(|s| !files.iter().any(|(f, _)| is_under(f, s)))
    {
        return Err(CliError::Runtime(format!(
            "no files found to validate replication under: {source}"
        )));
    }

    let bad: Vec<_> = files
        .iter()
        .filter(|(_, repl)| *repl < min_replication)
        .collect();

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
    let rc = run(&["hdfs", "dfs", "-mkdir", "-p", path], env, false)?;
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

/// `hdfs dfs -<op> src1 ... srcN target`: one JVM start for all sources.
fn many_cmd<'a>(op: &'a str, sources: &'a [String], target: &'a str) -> Vec<&'a str> {
    let mut cmd = vec!["hdfs", "dfs", op];
    cmd.extend(sources.iter().map(String::as_str));
    cmd.push(target);
    cmd
}

/// Runs `op` on all `sources` into the directory `target` with a single
/// `hdfs` call. On failure, `hdfs` reports the failing paths on stderr.
fn run_many(op: &str, sources: &[String], target: &str, env: Option<&Env>) -> Result<(), CliError> {
    let rc = run(&many_cmd(op, sources, target), env, false)?;
    if rc != 0 {
        return Err(CliError::Runtime(format!(
            "hdfs {} failed for one or more of {} object(s)",
            op.trim_start_matches('-'),
            sources.len()
        )));
    }
    Ok(())
}

pub fn mv_many(sources: &[String], target: &str, env: Option<&Env>) -> Result<(), CliError> {
    run_many("-mv", sources, target, env)
}

pub fn cp_many(sources: &[String], target: &str, env: Option<&Env>) -> Result<(), CliError> {
    run_many("-cp", sources, target, env)
}

/// Paths listed in `hdfs dfs -ls -d` output (the last token of each entry).
fn parse_ls_paths(stdout: &str) -> Vec<String> {
    let mut paths = Vec::new();
    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with("Found ") {
            continue;
        }
        if let Some(last) = line.split_whitespace().last() {
            paths.push(last.to_string());
        }
    }
    paths
}

pub fn resolve_glob(pattern: &str, env: Option<&Env>) -> Result<Vec<String>, CliError> {
    let out = run_captured(&["hdfs", "dfs", "-ls", "-d", pattern], env)?;
    if !out.status.success() {
        // show the cause: "No such file or directory" or e.g. an auth error
        eprint!("{}", String::from_utf8_lossy(&out.stderr));
        return Ok(Vec::new());
    }

    Ok(parse_ls_paths(&String::from_utf8_lossy(&out.stdout)))
}

/// Which of `paths` exist, from a single `hdfs dfs -ls -d` call. The exit
/// code is ignored: it is non-zero whenever some of the paths are missing.
pub fn existing(paths: &[String], env: Option<&Env>) -> Result<HashSet<String>, CliError> {
    let mut cmd = vec!["hdfs", "dfs", "-ls", "-d"];
    cmd.extend(paths.iter().map(String::as_str));
    let out = run_captured(&cmd, env)?;
    Ok(parse_ls_paths(&String::from_utf8_lossy(&out.stdout))
        .into_iter()
        .collect())
}

/// `target/<name>` for each source: where `-cp`/`-mv` into the directory
/// `target` places it.
pub fn dest_paths(sources: &[String], target: &str) -> Vec<String> {
    let dir = target.trim_end_matches('/');
    sources
        .iter()
        .map(|s| format!("{dir}/{}", s.rsplit('/').next().unwrap_or_default()))
        .collect()
}

/// Sources whose destination is in `after` but was not in `before`, i.e.
/// the ones a partly failed `-cp` actually copied.
pub fn newly_present<'a>(
    sources: &'a [String],
    dests: &[String],
    before: &HashSet<String>,
    after: &HashSet<String>,
) -> Vec<&'a String> {
    sources
        .iter()
        .zip(dests)
        .filter(|(_, d)| after.contains(*d) && !before.contains(*d))
        .map(|(s, _)| s)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::sample_layout;

    #[test]
    fn parse_ls_files_keeps_files_with_replication() {
        let out = "Found 2 items\n\
            drwxr-xr-x   - u g          0 2024-01-01 10:00 /data/staging/d\n\
            -rw-r--r--   3 u g        100 2024-01-01 10:00 /data/staging/d/a\n\
            -rw-r--r--+  1 u g        100 2024-01-01 10:00 /data/staging/b\n";
        assert_eq!(
            parse_ls_files(out).unwrap(),
            [
                ("/data/staging/d/a".to_string(), 3),
                ("/data/staging/b".to_string(), 1)
            ]
        );
    }

    #[test]
    fn check_replication_rules() {
        let l = sample_layout();
        let files = [
            ("/data/staging/d/a".to_string(), 3),
            ("/data/staging/b".to_string(), 3),
        ];
        assert!(check_replication(&["/data/staging/d", "/data/staging/b"], &files, &l, 3).is_ok());
        // a source with no files under it (empty dir) is rejected
        assert!(check_replication(&["/data/staging/d", "/data/staging/e"], &files, &l, 3).is_err());
        // a prefix match is not "under": /data/staging/d does not cover /data/staging/dd
        assert!(check_replication(&["/data/staging/dd"], &files, &l, 3).is_err());
        let low = [("/data/staging/d/a".to_string(), 2)];
        assert!(check_replication(&["/data/staging/d"], &low, &l, 3).is_err());
    }

    #[test]
    fn test_result_maps_exit_codes() {
        assert!(test_result(0).unwrap());
        assert!(!test_result(1).unwrap());
        assert!(matches!(test_result(255), Err(CliError::Runtime(_))));
        assert!(matches!(test_result(-1), Err(CliError::Runtime(_))));
    }

    #[test]
    fn parse_ls_paths_takes_last_token() {
        let out = "drwxr-xr-x   - u g          0 2024-01-01 10:00 /b/x\n\
            -rw-r--r--   3 u g        100 2024-01-01 10:00 /b/y\n";
        assert_eq!(parse_ls_paths(out), ["/b/x", "/b/y"]);
    }

    #[test]
    fn dest_paths_join_basename() {
        let sources = vec!["/a/x".to_string(), "/a/d/y".to_string()];
        assert_eq!(dest_paths(&sources, "/b"), ["/b/x", "/b/y"]);
        assert_eq!(dest_paths(&sources, "/"), ["/x", "/y"]);
    }

    #[test]
    fn newly_present_skips_preexisting_and_missing() {
        let sources = vec!["/a/x".to_string(), "/a/y".to_string(), "/a/z".to_string()];
        let dests = dest_paths(&sources, "/b");
        // /b/y existed before the copy, /b/z was never created
        let before = HashSet::from(["/b/y".to_string()]);
        let after = HashSet::from(["/b/x".to_string(), "/b/y".to_string()]);
        assert_eq!(
            newly_present(&sources, &dests, &before, &after),
            [&sources[0]]
        );
    }

    #[test]
    fn many_cmd_uses_single_call_with_all_sources() {
        let sources = vec!["/a/x".to_string(), "/a/y".to_string()];
        for op in ["-mv", "-cp"] {
            assert_eq!(
                many_cmd(op, &sources, "/b"),
                ["hdfs", "dfs", op, "/a/x", "/a/y", "/b"]
            );
        }
    }
}
