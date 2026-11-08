use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

/// Resolved locations: the source project (scripts, build output) and the
/// per-user state directory (VM overlays, cached base images).
pub struct Paths {
    project: Option<PathBuf>,
    pub home: PathBuf,
}

impl Paths {
    pub fn resolve(project: Option<PathBuf>, home: Option<PathBuf>) -> Result<Self> {
        let project = match project {
            Some(p) => {
                let p = p
                    .canonicalize()
                    .with_context(|| format!("project directory {}", p.display()))?;
                if !is_project(&p) {
                    bail!(
                        "{} is not a SedirOS project (scripts/build_rootfs.sh missing)",
                        p.display()
                    );
                }
                Some(p)
            }
            None => find_project(&env::current_dir()?),
        };
        let home = match home {
            Some(h) => h,
            None => default_home()?,
        };
        Ok(Self { project, home })
    }

    pub fn project(&self) -> Result<&Path> {
        self.project.as_deref().context(
            "SedirOS project not found: run inside the repo or set --project / SEDIR_PROJECT_DIR",
        )
    }

    pub fn build_dir(&self) -> Result<PathBuf> {
        Ok(self.project()?.join("build"))
    }

    pub fn vms_dir(&self) -> PathBuf {
        self.home.join("vms")
    }

    pub fn images_dir(&self) -> PathBuf {
        self.home.join("images")
    }

    /// Releases fetched by `sedir pull`: <home>/downloads/<release>/.
    pub fn downloads_dir(&self) -> PathBuf {
        self.home.join("downloads")
    }

    pub fn vm_dir(&self, name: &str) -> PathBuf {
        self.vms_dir().join(name)
    }
}

fn is_project(p: &Path) -> bool {
    p.join("scripts/build_rootfs.sh").is_file()
}

fn find_project(start: &Path) -> Option<PathBuf> {
    start
        .ancestors()
        .find(|p| is_project(p))
        .map(Path::to_path_buf)
}

fn default_home() -> Result<PathBuf> {
    if let Some(x) = env::var_os("XDG_DATA_HOME").filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(x).join("sedir"));
    }
    let home = env::var_os("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home).join(".local/share/sedir"))
}

pub fn validate_vm_name(name: &str) -> Result<()> {
    let valid = !name.is_empty()
        && name.len() <= 32
        && name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if !valid {
        bail!(
            "invalid VM name '{name}': use [a-z0-9-], max 32 chars, start with a letter or digit"
        );
    }
    Ok(())
}

/// Create a directory readable only by the owner (overlays may hold secrets).
pub fn create_private_dir(path: &Path) -> Result<()> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
        .with_context(|| format!("creating {}", path.display()))?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

/// Recursively delete `target`, but only if it really lives inside `base`
/// and is not a symlink. Guards every destructive operation in the CLI.
pub fn remove_dir_within(base: &Path, target: &Path) -> Result<()> {
    let meta =
        fs::symlink_metadata(target).with_context(|| format!("stat {}", target.display()))?;
    if meta.file_type().is_symlink() {
        bail!("refusing to delete symlink {}", target.display());
    }
    let base_c = base.canonicalize()?;
    let target_c = target.canonicalize()?;
    if target_c == base_c || !target_c.starts_with(&base_c) {
        bail!(
            "refusing to delete {} (outside {})",
            target_c.display(),
            base_c.display()
        );
    }
    fs::remove_dir_all(&target_c).with_context(|| format!("deleting {}", target_c.display()))
}

/// The CLI's own SSH key pair (<home>/id_ed25519), created on first use.
/// Its public key is handed to every VM at boot; images contain no key.
pub fn ensure_ssh_key(home: &Path) -> Result<PathBuf> {
    let key = home.join("id_ed25519");
    if key.is_file() && key.with_extension("pub").is_file() {
        return Ok(key);
    }
    create_private_dir(home)?;
    let status = std::process::Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-C", "sedir-cli", "-f"])
        .arg(&key)
        .stdin(std::process::Stdio::null())
        .status()
        .context("running ssh-keygen")?;
    if !status.success() {
        bail!("ssh-keygen failed creating {}", key.display());
    }
    Ok(key)
}

/// Locate an executable on PATH.
pub fn which(binary: &str) -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    env::split_paths(&path)
        .map(|dir| dir.join(binary))
        .find(|candidate| candidate.is_file())
}
