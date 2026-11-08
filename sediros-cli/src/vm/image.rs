//! Base image cache.
//!
//! VM overlays reference their base image by absolute path. Rebuilding
//! build/sediros-*.qcow2 in place would silently corrupt every existing
//! overlay, so each distinct base image is copied once into
//! <home>/images/<key>/ (key = sha256 prefix from build_image.sh) together
//! with the matching kernel/initrd. On APFS/btrfs/XFS the copy is a reflink.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use anyhow::{bail, Context, Result};

use crate::paths::{create_private_dir, Paths};

/// BUILD_ID of a cached image (from <stem>.build-info, written by
/// build_rootfs.sh), or "-" for images built before build ids existed.
pub fn cached_build_id(paths: &Paths, key: &str) -> String {
    fs::read_to_string(paths.images_dir().join(key).join("build-info"))
        .ok()
        .and_then(|t| {
            t.lines()
                .find_map(|l| l.strip_prefix("BUILD_ID="))
                .map(str::to_string)
        })
        .unwrap_or_else(|| "-".to_string())
}

pub struct BaseImage {
    pub key: String,
    pub arch: String,
    pub dir: PathBuf,
}

impl BaseImage {
    pub fn disk(&self) -> PathBuf {
        self.dir.join("base.qcow2")
    }
    pub fn kernel(&self) -> PathBuf {
        self.dir.join("vmlinuz")
    }
    pub fn initrd(&self) -> PathBuf {
        self.dir.join("initrd.img")
    }
    pub fn cmdline(&self) -> String {
        fs::read_to_string(self.dir.join("kernel-cmdline.txt"))
            .map(|s| s.trim().to_string())
            .unwrap_or_default()
    }

    /// Open an already cached image (used when restarting an existing VM).
    pub fn cached(paths: &Paths, key: &str, arch: &str) -> Result<Self> {
        let img = Self {
            key: key.to_string(),
            arch: arch.to_string(),
            dir: paths.images_dir().join(key),
        };
        if !img.dir.join(".complete").is_file() {
            bail!("cached base image {key} is missing; purge this VM and launch it again");
        }
        Ok(img)
    }

    /// Resolve the base image (explicit or newest in build/) and make sure it is cached.
    pub fn prepare(paths: &Paths, explicit: Option<&Path>) -> Result<Self> {
        let qcow = match explicit {
            Some(p) => p
                .canonicalize()
                .with_context(|| format!("image {}", p.display()))?,
            // A source checkout's own build wins; otherwise (no checkout, or
            // nothing built in it yet) the newest pull.
            None => match paths
                .build_dir()
                .ok()
                .filter(|d| d.is_dir())
                .and_then(|d| newest_image(&d).ok())
            {
                Some(built) => built,
                None => crate::pull::newest_pulled(paths).context(
                    "no base image: run `sediros pull` (or `sediros build` in a source checkout)",
                )?,
            },
        };
        let src_dir = qcow.parent().context("image has no parent directory")?;
        let arch = arch_from_name(&qcow)?;
        let key = image_key(&qcow)?;
        let dest = paths.images_dir().join(&key);

        let img = Self {
            key: key.clone(),
            arch,
            dir: dest.clone(),
        };
        if dest.join(".complete").is_file() {
            return Ok(img);
        }

        // Kernel artifacts share the image's name: sediros-trixie-arm64.{vmlinuz,initrd.img,cmdline}
        let stem = qcow
            .file_stem()
            .and_then(|s| s.to_str())
            .context("image file name is not UTF-8")?;
        let kernel = src_dir.join(format!("{stem}.vmlinuz"));
        let initrd = src_dir.join(format!("{stem}.initrd.img"));
        if !kernel.is_file() || !initrd.is_file() {
            bail!(
                "{stem}.vmlinuz / {stem}.initrd.img not found in {} (rebuild with `make build`)",
                src_dir.display()
            );
        }

        println!("Caching base image {} as {key}", qcow.display());
        create_private_dir(&paths.images_dir())?;
        let staging = paths
            .images_dir()
            .join(format!(".{key}.tmp-{}", std::process::id()));
        if staging.exists() {
            fs::remove_dir_all(&staging)?;
        }
        create_private_dir(&staging)?;

        copy_readonly(&qcow, &staging.join("base.qcow2"))?;
        copy_readonly(&kernel, &staging.join("vmlinuz"))?;
        copy_readonly(&initrd, &staging.join("initrd.img"))?;
        let cmdline = src_dir.join(format!("{stem}.cmdline"));
        if cmdline.is_file() {
            copy_readonly(&cmdline, &staging.join("kernel-cmdline.txt"))?;
        }
        let info = src_dir.join(format!("{stem}.build-info"));
        if info.is_file() {
            copy_readonly(&info, &staging.join("build-info"))?;
        }
        fs::write(staging.join(".complete"), b"")?;

        if dest.exists() {
            fs::remove_dir_all(&dest)?; // stale, incomplete entry
        }
        fs::rename(&staging, &dest)?;
        Ok(img)
    }
}

fn copy_readonly(src: &Path, dst: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::copy(src, dst)
        .with_context(|| format!("copying {} -> {}", src.display(), dst.display()))?;
    fs::set_permissions(dst, fs::Permissions::from_mode(0o444))?;
    Ok(())
}

fn newest_image(build_dir: &Path) -> Result<PathBuf> {
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    let entries = fs::read_dir(build_dir).with_context(|| {
        format!(
            "reading {} (run `sediros build` first)",
            build_dir.display()
        )
    })?;
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !(name.starts_with("sediros-") && name.ends_with(".qcow2")) {
            continue;
        }
        let mtime = entry.metadata()?.modified()?;
        if best.as_ref().map_or(true, |(t, _)| mtime > *t) {
            best = Some((mtime, entry.path()));
        }
    }
    best.map(|(_, p)| p).with_context(|| {
        format!(
            "no sediros-*.qcow2 in {}; run `sediros build`",
            build_dir.display()
        )
    })
}

/// sediros-13.1-arm64.qcow2 -> "arm64"
fn arch_from_name(path: &Path) -> Result<String> {
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default();
    match stem.rsplit('-').next() {
        Some(a @ ("amd64" | "arm64")) => Ok(a.to_string()),
        _ => bail!("cannot infer architecture from image name '{stem}' (expected *-amd64/*-arm64)"),
    }
}

/// First 16 hex chars of the sha256 written by build_image.sh;
/// falls back to size+mtime if the checksum file is missing.
fn image_key(qcow: &Path) -> Result<String> {
    let mut sha_path = qcow.as_os_str().to_owned();
    sha_path.push(".sha256");
    if let Ok(text) = fs::read_to_string(&sha_path) {
        if let Some(hash) = text.split_whitespace().next() {
            if hash.len() >= 16 && hash.chars().all(|c| c.is_ascii_hexdigit()) {
                return Ok(hash[..16].to_ascii_lowercase());
            }
        }
    }
    let meta = fs::metadata(qcow)?;
    let mtime = meta.modified()?.duration_since(UNIX_EPOCH)?.as_secs();
    Ok(format!("{:x}-{:x}", meta.len(), mtime))
}
