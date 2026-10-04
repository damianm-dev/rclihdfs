/// Site-specific HDFS roots, read from the config file. All paths are
/// absolute and stored without a trailing slash.
#[derive(Debug, Clone, PartialEq)]
pub struct Layout {
    /// Production data root; writes require the tech principal.
    pub prod: String,
    /// Staging area inside `prod`; moving from it into `prod` requires
    /// sufficient replication.
    pub staging: String,
    /// External data root; may be copied/moved into `prod` or within itself.
    pub external: String,
    /// Root whose data may only be moved into `prod`.
    pub mv_to_prod: String,
    /// Root whose data may only be copied/moved into `staging`.
    pub to_staging: String,
}

/// Normalizes an HDFS path: removes trailing slashes (except root).
pub fn norm_path(p: &str) -> String {
    if p == "/" {
        return p.to_string();
    }
    p.trim_end_matches('/').to_string()
}

/// `path` is strictly inside `root` (prefix match on `root/`).
fn starts_under(path: &str, root: &str) -> bool {
    path.starts_with(&format!("{root}/"))
}

/// `path` is `root` itself or inside it.
fn is_under(path: &str, root: &str) -> bool {
    let path = norm_path(path);
    path == root || starts_under(&path, root)
}

impl Layout {
    fn priv_roots(&self) -> [&str; 2] {
        [&self.prod, &self.external]
    }

    pub fn is_prod(&self, path: &str) -> bool {
        is_under(path, &self.prod)
    }

    pub fn is_staging(&self, path: &str) -> bool {
        is_under(path, &self.staging)
    }

    pub fn is_to_staging(&self, path: &str) -> bool {
        is_under(path, &self.to_staging)
    }

    pub fn is_staging_to_prod_move(&self, source: &str, target: &str) -> bool {
        self.is_staging(source) && self.is_prod(target) && !self.is_staging(target)
    }

    pub fn is_allowed_root(&self, path: &str, user: &str) -> bool {
        self.priv_roots().iter().any(|r| starts_under(path, r)) || is_user_space(path, user)
    }

    pub fn allowed_roots_hint(&self) -> String {
        format!(
            "paths should start with '{}/' or '{}/' or '/user/<you>/'",
            self.prod, self.external
        )
    }

    pub fn requires_tech_auth(&self, paths: &[&str]) -> bool {
        paths
            .iter()
            .any(|p| !p.is_empty() && self.priv_roots().iter().any(|r| starts_under(p, r)))
    }

    pub fn requires_tech_auth_for_mv(&self, paths: &[&str]) -> bool {
        let roots = [
            &self.prod,
            &self.external,
            &self.mv_to_prod,
            &self.to_staging,
        ];
        paths
            .iter()
            .any(|p| !p.is_empty() && roots.iter().any(|r| starts_under(p, r)))
    }

    /// Policy for `cp`:
    ///   - prod/            -> only prod
    ///   - external/        -> prod OR external/
    ///   - /user/<user>/    -> only /user/<user>/ OR staging
    ///   - to_staging       -> only staging
    pub fn validate_paths_for_cp_mv(&self, source: &str, target: &str, user: &str) -> bool {
        if starts_under(source, &self.prod) {
            return self.is_prod(target);
        }

        if starts_under(source, &self.external) {
            return self.is_prod(target) || starts_under(target, &self.external);
        }

        if source.starts_with(&user_prefix(user)) {
            return target.starts_with(&user_prefix(user)) || self.is_staging(target);
        }

        if self.is_to_staging(source) {
            return self.is_staging(target);
        }

        false
    }

    /// Policy for `mv`:
    ///   - prod/            -> only prod
    ///   - external/        -> prod OR external/
    ///   - mv_to_prod/      -> only prod
    ///   - to_staging       -> only staging
    ///   - /user/<user>/    -> only /user/<user>/ OR staging
    pub fn validate_paths_for_mv(&self, source: &str, target: &str, user: &str) -> bool {
        if starts_under(source, &self.prod) {
            return self.is_prod(target);
        }

        if starts_under(source, &self.external) {
            return self.is_prod(target) || starts_under(target, &self.external);
        }

        if starts_under(source, &self.mv_to_prod) {
            return self.is_prod(target);
        }

        if self.is_to_staging(source) {
            return self.is_staging(target);
        }

        if source.starts_with(&user_prefix(user)) {
            return target.starts_with(&user_prefix(user)) || self.is_staging(target);
        }

        false
    }

    pub fn trash_target_for(&self, source: &str, user: &str) -> String {
        if source.starts_with(&user_prefix(user)) {
            format!("/user/{user}/.trash{source}")
        } else {
            format!("{}/trash{source}", self.staging)
        }
    }

    pub fn print_user_to_staging_acl_hint(
        &self,
        source: &str,
        target: &str,
        user: &str,
        tech_user: &str,
    ) {
        if source.starts_with(&user_prefix(user)) && self.is_staging(target) {
            println!(
                "Hint: to mv from your home directory to {}, grant the service user \
rwx on it:\n\
hdfs dfs -setfacl -m user:{tech_user}:rwx {}",
                self.staging,
                user_prefix(user).trim_end_matches('/')
            );
        }
    }
}

pub fn user_prefix(user: &str) -> String {
    format!("/user/{user}/")
}

/// Only the user's own `/user/<user>/`.
pub fn is_user_space(path: &str, user: &str) -> bool {
    path.starts_with(&user_prefix(user))
}

pub fn has_wildcard(path: &str) -> bool {
    path.contains(['*', '?', '['])
}

#[cfg(test)]
pub(crate) fn sample_layout() -> Layout {
    Layout {
        prod: "/data".to_string(),
        staging: "/data/staging".to_string(),
        external: "/ext".to_string(),
        mv_to_prod: "/compute".to_string(),
        to_staging: "/science".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cp_policy() {
        let l = sample_layout();
        assert!(l.validate_paths_for_cp_mv("/data/a", "/data/b", "u"));
        assert!(!l.validate_paths_for_cp_mv("/data/a", "/ext/b", "u"));
        assert!(l.validate_paths_for_cp_mv("/ext/a", "/data", "u"));
        assert!(l.validate_paths_for_cp_mv("/ext/a", "/ext/b", "u"));
        assert!(l.validate_paths_for_cp_mv("/user/u/a", "/user/u/b", "u"));
        assert!(l.validate_paths_for_cp_mv("/user/u/a", "/data/staging/x", "u"));
        assert!(!l.validate_paths_for_cp_mv("/user/u/a", "/user/v/b", "u"));
        assert!(l.validate_paths_for_cp_mv("/science/a", "/data/staging", "u"));
        assert!(!l.validate_paths_for_cp_mv("/science/a", "/data/x", "u"));
        assert!(!l.validate_paths_for_cp_mv("/compute/a", "/data/x", "u"));
        assert!(!l.validate_paths_for_cp_mv("/datax/a", "/data/x", "u"));
    }

    #[test]
    fn mv_policy() {
        let l = sample_layout();
        assert!(l.validate_paths_for_mv("/compute/a", "/data/x", "u"));
        assert!(!l.validate_paths_for_mv("/compute/a", "/ext/x", "u"));
        assert!(l.validate_paths_for_mv("/science/a", "/data/staging/x", "u"));
        assert!(!l.validate_paths_for_mv("/science/a", "/data/x", "u"));
        assert!(l.validate_paths_for_mv("/user/u/a", "/data/staging", "u"));
    }

    #[test]
    fn tech_auth() {
        let l = sample_layout();
        assert!(l.requires_tech_auth(&["/user/u/a", "/data/x"]));
        assert!(l.requires_tech_auth(&["/ext/x"]));
        assert!(!l.requires_tech_auth(&["/user/u/a", "/compute/x"]));
        assert!(l.requires_tech_auth_for_mv(&["/compute/x"]));
        assert!(l.requires_tech_auth_for_mv(&["/science/x"]));
        assert!(!l.requires_tech_auth_for_mv(&["/user/u/a"]));
    }

    #[test]
    fn staging_to_prod_and_trash() {
        let l = sample_layout();
        assert!(l.is_staging_to_prod_move("/data/staging/a", "/data/b"));
        assert!(!l.is_staging_to_prod_move("/data/staging/a", "/data/staging/b"));
        assert!(!l.is_staging_to_prod_move("/data/a", "/data/b"));
        assert!(l.is_allowed_root("/data/x", "u"));
        assert!(!l.is_allowed_root("/data", "u"));
        assert!(!l.is_allowed_root("/compute/x", "u"));
        assert_eq!(
            l.trash_target_for("/data/x", "u"),
            "/data/staging/trash/data/x"
        );
        assert_eq!(
            l.trash_target_for("/user/u/x", "u"),
            "/user/u/.trash/user/u/x"
        );
    }
}
