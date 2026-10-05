use chrono::Local;
use colored::Colorize;

use crate::config::AuthConfig;
use crate::db::AuditLog;
use crate::error::CliError;
use crate::hdfs;
use crate::kerberos::KerberosContext;
use crate::policy::{has_traversal, has_wildcard, norm_path, Layout};
use crate::proc::Env;

fn log_success() {
    log::info!("{}", "SUCCESS".green().bold());
}

/// Rejects any path with a `.` or `..` component before policy checks run;
/// HDFS would resolve it and move the real target outside the policy.
fn reject_traversal(paths: &[&str]) -> Result<(), CliError> {
    for p in paths {
        if has_traversal(p) {
            return Err(CliError::Policy(format!(
                "path must not contain '.' or '..' segments: {p}"
            )));
        }
    }
    Ok(())
}

/// Prompts the user with `prompt (y/N)` and requires an explicit yes to
/// proceed. Skipped entirely when `yes` is true (the `-y`/`--yes` flag).
fn confirm(prompt: &str, yes: bool) -> Result<(), CliError> {
    if yes {
        return Ok(());
    }

    let confirmed = dialoguer::Confirm::new()
        .with_prompt(prompt)
        .default(false)
        .interact()
        .map_err(|e| CliError::Runtime(format!("failed to read confirmation: {e}")))?;

    if confirmed {
        Ok(())
    } else {
        Err(CliError::Policy("aborted: not confirmed".to_string()))
    }
}

/// A wildcard source needs an existing directory as target. Checks `-d`
/// first, so a valid target costs one `hdfs` call; `-e` only runs to pick
/// the error.
fn ensure_target_dir(target: &str, env: Option<&Env>) -> Result<(), CliError> {
    if hdfs::is_dir(target, env)? {
        return Ok(());
    }
    if !hdfs::exists(target, env)? {
        return Err(CliError::NotFound(format!(
            "target path does not exist: {target}"
        )));
    }
    Err(CliError::Policy(
        "target must be an existing directory when source contains wildcard".to_string(),
    ))
}

pub fn do_cp(
    source: &str,
    target: &str,
    user: &str,
    cfg: &AuthConfig,
    layout: &Layout,
) -> Result<(), CliError> {
    let raw_source = source;
    let raw_target = target;
    reject_traversal(&[raw_source, raw_target])?;
    let target = norm_path(raw_target);

    if has_wildcard(raw_target) {
        return Err(CliError::Policy(
            "wildcard in target is not supported".to_string(),
        ));
    }

    let source_for_policy = if has_wildcard(raw_source) {
        raw_source.to_string()
    } else {
        norm_path(raw_source)
    };

    if !layout.validate_paths_for_cp_mv(&source_for_policy, &target, user) {
        return Err(CliError::Policy(
            "paths are not allowed by policy".to_string(),
        ));
    }

    let use_tech = layout.requires_tech_auth(&[&source_for_policy, &target])
        || layout.is_to_staging(&source_for_policy);

    let mut audit_log = AuditLog::open()?;
    let ctx = KerberosContext::enter(use_tech, cfg)?;
    let env = ctx.env();

    if has_wildcard(raw_source) {
        let matches = hdfs::resolve_glob(raw_source, env)?;
        if matches.is_empty() {
            return Err(CliError::NotFound(format!(
                "source path does not exist or matched nothing: {raw_source}"
            )));
        }

        ensure_target_dir(&target, env)?;

        // On partial failure, only destinations that appeared were copied.
        let dests = hdfs::dest_paths(&matches, &target);
        let before = hdfs::existing(&dests, env)?;
        let result = hdfs::cp_many(&matches, &target, env);
        let copied: Vec<&String> = match result {
            Ok(()) => matches.iter().collect(),
            Err(_) => {
                let after = hdfs::existing(&dests, env)?;
                hdfs::newly_present(&matches, &dests, &before, &after)
            }
        };

        let now = Local::now().naive_local();
        for matched in copied {
            audit_log.insert(user, "cp", matched, &target, now)?;
        }
        result?;

        log_success();
        return Ok(());
    }

    let source = norm_path(raw_source);
    if !hdfs::exists(&source, env)? {
        return Err(CliError::NotFound(format!(
            "source path does not exist: {source}"
        )));
    }

    hdfs::cp(&source, &target, env)?;
    audit_log.insert(user, "cp", &source, &target, Local::now().naive_local())?;
    log_success();
    Ok(())
}

pub fn do_mv(
    source: &str,
    target: &str,
    yes: bool,
    user: &str,
    cfg: &AuthConfig,
    layout: &Layout,
) -> Result<(), CliError> {
    let raw_source = source;
    let raw_target = target;
    reject_traversal(&[raw_source, raw_target])?;
    let target = norm_path(raw_target);

    if has_wildcard(raw_target) {
        return Err(CliError::Policy(
            "wildcard in target is not supported".to_string(),
        ));
    }

    let source_for_policy = if has_wildcard(raw_source) {
        raw_source.to_string()
    } else {
        norm_path(raw_source)
    };

    if !layout.validate_paths_for_mv(&source_for_policy, &target, user) {
        return Err(CliError::Policy(
            "paths are not allowed by policy".to_string(),
        ));
    }

    let use_tech = layout.requires_tech_auth_for_mv(&[&source_for_policy, &target]);

    let mut audit_log = AuditLog::open()?;
    let ctx = KerberosContext::enter(use_tech, cfg)?;
    let env = ctx.env();

    if has_wildcard(raw_source) {
        let matches = hdfs::resolve_glob(raw_source, env)?;
        if matches.is_empty() {
            return Err(CliError::NotFound(format!(
                "source path does not exist or matched nothing: {raw_source}"
            )));
        }

        ensure_target_dir(&target, env)?;

        hdfs::ensure_min_replication_for_staging_to_prod(&matches, &target, env, layout, 3)?;

        confirm(
            &format!(
                "Move {} object(s) matching '{raw_source}' -> '{target}'?",
                matches.len()
            ),
            yes,
        )?;

        // On partial failure, only sources that are gone were moved.
        let result = hdfs::mv_many(&matches, &target, env);
        let moved: Vec<&String> = match result {
            Ok(()) => matches.iter().collect(),
            Err(_) => {
                let left = hdfs::existing(&matches, env)?;
                matches.iter().filter(|m| !left.contains(*m)).collect()
            }
        };

        let now = Local::now().naive_local();
        for matched in moved {
            audit_log.insert(user, "mv", matched, &target, now)?;
        }
        if let Err(e) = result {
            layout.print_user_to_staging_acl_hint(raw_source, &target, user, cfg.tech_user());
            return Err(e);
        }

        log_success();
        return Ok(());
    }

    let source = norm_path(raw_source);
    if !hdfs::exists(&source, env)? {
        return Err(CliError::NotFound(format!(
            "source path does not exist: {source}"
        )));
    }

    hdfs::ensure_min_replication_for_staging_to_prod(
        std::slice::from_ref(&source),
        &target,
        env,
        layout,
        3,
    )?;

    confirm(&format!("Move '{source}' -> '{target}'?"), yes)?;

    if let Err(e) = hdfs::mv(&source, &target, env) {
        layout.print_user_to_staging_acl_hint(&source, &target, user, cfg.tech_user());
        return Err(e);
    }

    audit_log.insert(user, "mv", &source, &target, Local::now().naive_local())?;
    log_success();
    Ok(())
}

pub fn do_mkdir(path: &str, user: &str, cfg: &AuthConfig, layout: &Layout) -> Result<(), CliError> {
    reject_traversal(&[path])?;
    let path = norm_path(path);

    if !layout.is_allowed_root(&path, user) {
        return Err(CliError::Policy(layout.allowed_roots_hint()));
    }

    let use_tech = layout.requires_tech_auth(&[&path]);
    let mut audit_log = AuditLog::open()?;
    let ctx = KerberosContext::enter(use_tech, cfg)?;
    let env = ctx.env();

    hdfs::mkdir_p(&path, env)?;
    audit_log.insert(user, "mkdir", &path, &path, Local::now().naive_local())?;
    log_success();
    Ok(())
}

pub fn do_rm(
    source: &str,
    yes: bool,
    user: &str,
    cfg: &AuthConfig,
    layout: &Layout,
) -> Result<(), CliError> {
    reject_traversal(&[source])?;
    let source = norm_path(source);

    if !layout.is_allowed_root(&source, user) {
        return Err(CliError::Policy(layout.allowed_roots_hint()));
    }

    let target = norm_path(&layout.trash_target_for(&source, user));
    let use_tech = layout.requires_tech_auth(&[&source, &target]);

    log::info!(
        "RM -> TRASH: {source} -> {target} (auth={})",
        if use_tech { "tech" } else { "user" }
    );

    let mut audit_log = AuditLog::open()?;
    let ctx = KerberosContext::enter(use_tech, cfg)?;
    let env = ctx.env();

    if !hdfs::exists(&source, env)? {
        return Err(CliError::NotFound(format!(
            "source path does not exist: {source}"
        )));
    }

    confirm(&format!("Move '{source}' to trash at '{target}'?"), yes)?;

    let parent = std::path::Path::new(&target)
        .parent()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();
    hdfs::mkdir_p(&parent, env)?;

    if hdfs::exists(&target, env)? {
        log::warn!("Trash collision, removing existing: {target}");
        hdfs::rm_rf(&target, env);
    }

    hdfs::mv(&source, &parent, env)?;

    log::info!("{}", format!("MOVED TO TRASH: {target}").cyan());
    audit_log.insert(user, "rm", &source, &target, Local::now().naive_local())?;
    log_success();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reject_traversal_catches_dot_segments() {
        assert!(reject_traversal(&["/data/x", "/data/y"]).is_ok());
        assert!(matches!(
            reject_traversal(&["/data/../user/x"]),
            Err(CliError::Policy(_))
        ));
        assert!(matches!(
            reject_traversal(&["/data/./x"]),
            Err(CliError::Policy(_))
        ));
        // a bad path anywhere in the list is caught
        assert!(matches!(
            reject_traversal(&["/data/x", "/data/.."]),
            Err(CliError::Policy(_))
        ));
        // substrings of '.'/'..' are not components
        assert!(reject_traversal(&["/data/..foo/a.b"]).is_ok());
    }

    #[test]
    fn confirm_with_yes_skips_prompt() {
        // `yes` short-circuits before any stdin interaction.
        assert!(confirm("ignored", true).is_ok());
    }
}
