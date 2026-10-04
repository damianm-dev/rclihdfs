use chrono::Local;
use colored::Colorize;

use crate::config::AuthConfig;
use crate::db::log_to_db;
use crate::error::CliError;
use crate::hdfs;
use crate::kerberos::KerberosContext;
use crate::policy::{has_wildcard, norm_path, Layout};

fn log_success() {
    log::info!("{}", "SUCCESS".green().bold());
}

/// Prompts the user with `prompt (y/N)` and requires an explicit yes to
/// proceed. Skipped entirely when `yes` is true (the `-y`/`--yes` flag),
/// which is what non-interactive/automated callers (e.g. Airflow) must pass.
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

pub fn do_cp(
    source: &str,
    target: &str,
    user: &str,
    cfg: &AuthConfig,
    layout: &Layout,
) -> Result<(), CliError> {
    let raw_source = source;
    let raw_target = target;
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

    let ctx = KerberosContext::enter(use_tech, cfg)?;
    let env = ctx.env();

    if has_wildcard(raw_source) {
        let matches = hdfs::resolve_glob(raw_source, env)?;
        if matches.is_empty() {
            return Err(CliError::NotFound(format!(
                "source path does not exist or matched nothing: {raw_source}"
            )));
        }

        if !hdfs::exists(&target, env) {
            return Err(CliError::NotFound(format!(
                "target path does not exist: {target}"
            )));
        }
        if !hdfs::is_dir(&target, env) {
            return Err(CliError::Policy(
                "target must be an existing directory when source contains wildcard".to_string(),
            ));
        }

        hdfs::cp_many(&matches, &target, env)?;

        let now = Local::now().naive_local();
        for matched in &matches {
            log_to_db(user, "cp", matched, &target, now)?;
        }

        log_success();
        return Ok(());
    }

    let source = norm_path(raw_source);
    if !hdfs::exists(&source, env) {
        return Err(CliError::NotFound(format!(
            "source path does not exist: {source}"
        )));
    }

    hdfs::cp(&source, &target, env)?;
    log_to_db(user, "cp", &source, &target, Local::now().naive_local())?;
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

    let ctx = KerberosContext::enter(use_tech, cfg)?;
    let env = ctx.env();

    if has_wildcard(raw_source) {
        let matches = hdfs::resolve_glob(raw_source, env)?;
        if matches.is_empty() {
            return Err(CliError::NotFound(format!(
                "source path does not exist or matched nothing: {raw_source}"
            )));
        }

        if !hdfs::exists(&target, env) {
            return Err(CliError::NotFound(format!(
                "target path does not exist: {target}"
            )));
        }
        if !hdfs::is_dir(&target, env) {
            return Err(CliError::Policy(
                "target must be an existing directory when source contains wildcard".to_string(),
            ));
        }

        for matched in &matches {
            hdfs::ensure_min_replication_for_staging_to_prod(matched, &target, env, layout, 3)?;
        }

        confirm(
            &format!(
                "Move {} object(s) matching '{raw_source}' -> '{target}'?",
                matches.len()
            ),
            yes,
        )?;

        if let Err(e) = hdfs::mv_many(&matches, &target, env) {
            layout.print_user_to_staging_acl_hint(raw_source, &target, user, cfg.tech_user());
            return Err(e);
        }

        let now = Local::now().naive_local();
        for matched in &matches {
            log_to_db(user, "mv", matched, &target, now)?;
        }

        log_success();
        return Ok(());
    }

    let source = norm_path(raw_source);
    if !hdfs::exists(&source, env) {
        return Err(CliError::NotFound(format!(
            "source path does not exist: {source}"
        )));
    }

    hdfs::ensure_min_replication_for_staging_to_prod(&source, &target, env, layout, 3)?;

    confirm(&format!("Move '{source}' -> '{target}'?"), yes)?;

    if let Err(e) = hdfs::mv(&source, &target, env) {
        layout.print_user_to_staging_acl_hint(&source, &target, user, cfg.tech_user());
        return Err(e);
    }

    log_to_db(user, "mv", &source, &target, Local::now().naive_local())?;
    log_success();
    Ok(())
}

pub fn do_mkdir(path: &str, user: &str, cfg: &AuthConfig, layout: &Layout) -> Result<(), CliError> {
    let path = norm_path(path);

    if !layout.is_allowed_root(&path, user) {
        return Err(CliError::Policy(layout.allowed_roots_hint()));
    }

    let use_tech = layout.requires_tech_auth(&[&path]);
    let ctx = KerberosContext::enter(use_tech, cfg)?;
    let env = ctx.env();

    hdfs::mkdir_p(&path, env)?;
    log_to_db(user, "mkdir", &path, &path, Local::now().naive_local())?;
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

    let ctx = KerberosContext::enter(use_tech, cfg)?;
    let env = ctx.env();

    if !hdfs::exists(&source, env) {
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

    if hdfs::exists(&target, env) {
        log::warn!("Trash collision, removing existing: {target}");
        hdfs::rm_rf(&target, env);
    }

    hdfs::mv(&source, &parent, env)?;

    log::info!("{}", format!("MOVED TO TRASH: {target}").cyan());
    log_to_db(user, "rm", &source, &target, Local::now().naive_local())?;
    log_success();
    Ok(())
}
