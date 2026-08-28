use std::path::{Path, PathBuf};

/// Runtime configuration for a [`crate::Brain`]. Carries only the store
/// directory and open mode — the handle-free facade re-opens the store per
/// operation, so there is no pool size or actor knob to tune anymore
/// (`readers` is kept for source compatibility and ignored).
#[derive(Debug, Clone)]
pub struct BrainConfig {
    pub dir: PathBuf,
    /// Ignored since the handle-free rework; kept so existing struct
    /// literals keep compiling.
    pub readers: usize,
    /// When true, every write path fails fast with a Config error instead
    /// of taking the advisory lock. Set by [`BrainConfig::read_only_at`];
    /// [`Brain::open_ro`](crate::Brain::open_ro) uses it.
    pub read_only: bool,
}

impl BrainConfig {
    pub fn at(path: impl AsRef<Path>) -> Self {
        Self {
            dir: path.as_ref().to_path_buf(),
            readers: 4,
            read_only: false,
        }
    }

    /// Read-only variant of [`BrainConfig::at`]: writes are rejected without
    /// touching the advisory lock.
    pub fn read_only_at(path: impl AsRef<Path>) -> Self {
        Self {
            read_only: true,
            ..Self::at(path)
        }
    }
}

use oxibrain_ports::BrainError;

pub const BUILTIN_DEFAULT_SPACE: &str = "personal";
const CONFIG_RELPATH: [&str; 2] = [".oxi", "config.toml"];

/// `~/.oxi/config.toml` — the §18 user config. First key: `default_space`.
///
/// Strict parse (a malformed file fails every command rather than silently
/// changing where data lands); unknown keys ignored (forward compat);
/// `set_default_space` round-trips through `toml_edit` so user comments and
/// reserved keys survive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserConfig {
    pub default_space: String,
}

#[derive(serde::Deserialize, Default)]
struct RawConfig {
    #[serde(default)]
    default_space: Option<String>,
}

impl UserConfig {
    pub fn config_path(home: &Path) -> PathBuf {
        home.join(CONFIG_RELPATH[0]).join(CONFIG_RELPATH[1])
    }

    /// Load `~/.oxi/config.toml`. Missing file/`HOME` ⇒ built-in default.
    pub fn load(home: Option<&Path>) -> Result<Self, BrainError> {
        let Some(home) = home else {
            return Ok(Self::builtin());
        };
        let path = Self::config_path(home);
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::builtin());
            }
            Err(e) => {
                return Err(BrainError::Config(format!("{}: {e}", path.display())));
            }
        };
        let raw: RawConfig = toml::from_str(&text)
            .map_err(|e| BrainError::Config(format!("{}: parse error: {e}", path.display())))?;
        Ok(Self {
            default_space: raw
                .default_space
                .unwrap_or_else(|| BUILTIN_DEFAULT_SPACE.into()),
        })
    }

    pub fn builtin() -> Self {
        Self {
            default_space: BUILTIN_DEFAULT_SPACE.into(),
        }
    }

    /// Spec §4.1 resolution: `--space` flag > config.toml > built-in.
    pub fn resolve_space(flag: Option<&str>, home: Option<&Path>) -> Result<String, BrainError> {
        if let Some(f) = flag {
            return Ok(f.trim().to_string());
        }
        Ok(Self::load(home)?.default_space)
    }

    /// Write `default_space`, preserving comments and unknown keys.
    pub fn set_default_space(home: &Path, name: &str) -> Result<(), BrainError> {
        let path = Self::config_path(home);
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            // Missing file ⇒ start from an empty document (the same
            // leniency `load` grants a missing config).
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            // Any other read error (EACCES, EISDIR, …) must surface:
            // swallowing it as empty would overwrite the real config.
            Err(e) => {
                return Err(BrainError::Config(format!("{}: {e}", path.display())));
            }
        };
        let mut doc = text
            .parse::<toml_edit::DocumentMut>()
            .map_err(|e| BrainError::Config(format!("{}: {e}", path.display())))?;
        doc["default_space"] = toml_edit::value(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| BrainError::Config(format!("{}: {e}", parent.display())))?;
        }
        std::fs::write(&path, doc.to_string())
            .map_err(|e| BrainError::Config(format!("{}: {e}", path.display())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_and_home_fall_back_to_builtin() {
        assert_eq!(UserConfig::load(None).unwrap().default_space, "personal");
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(
            UserConfig::load(Some(tmp.path())).unwrap().default_space,
            "personal"
        );
    }

    #[test]
    fn loads_default_space_and_ignores_unknown_keys() {
        let tmp = tempfile::tempdir().unwrap();
        let p = UserConfig::config_path(tmp.path());
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(
            p,
            "# comment\ndefault_space = \"dev\"\nprovider = \"local\"\n",
        )
        .unwrap();
        assert_eq!(
            UserConfig::load(Some(tmp.path())).unwrap().default_space,
            "dev"
        );
    }

    #[test]
    fn malformed_file_is_config_error() {
        let tmp = tempfile::tempdir().unwrap();
        let p = UserConfig::config_path(tmp.path());
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, "not [ valid toml").unwrap();
        let err = UserConfig::load(Some(tmp.path())).unwrap_err();
        assert!(matches!(err, BrainError::Config(_)));
        assert!(err.to_string().contains("config.toml"));
    }

    #[test]
    fn resolution_order_flag_beats_file_beats_builtin() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(".oxi")).unwrap();
        std::fs::write(
            UserConfig::config_path(tmp.path()),
            "default_space = \"dev\"\n",
        )
        .unwrap();
        let home = Some(tmp.path());
        assert_eq!(
            UserConfig::resolve_space(Some("work"), home).unwrap(),
            "work"
        );
        assert_eq!(UserConfig::resolve_space(None, home).unwrap(), "dev");
        let empty = tempfile::tempdir().unwrap();
        assert_eq!(
            UserConfig::resolve_space(None, Some(empty.path())).unwrap(),
            "personal"
        );
    }

    #[test]
    fn set_default_space_preserves_comments_and_unknown_keys() {
        let tmp = tempfile::tempdir().unwrap();
        let p = UserConfig::config_path(tmp.path());
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, "# my config\nprovider = \"local\"\n").unwrap();
        UserConfig::set_default_space(tmp.path(), "dev").unwrap();
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(text.contains("# my config"));
        assert!(text.contains("provider = \"local\""));
        assert!(text.contains("default_space = \"dev\""));
        assert_eq!(
            UserConfig::load(Some(tmp.path())).unwrap().default_space,
            "dev"
        );
    }

    #[test]
    fn set_default_space_propagates_read_errors() {
        // A read error other than NotFound (here: the config path is a
        // directory, EISDIR) must surface instead of being treated as an
        // empty file — swallowing it would let the subsequent write
        // clobber whatever is wrong there.
        let tmp = tempfile::tempdir().unwrap();
        let p = UserConfig::config_path(tmp.path());
        std::fs::create_dir_all(&p).unwrap();
        let err = UserConfig::set_default_space(tmp.path(), "dev").unwrap_err();
        assert!(matches!(err, BrainError::Config(_)));
        assert!(err.to_string().contains("config.toml"));
        assert!(p.is_dir(), "the directory must not have been replaced");
    }
}
