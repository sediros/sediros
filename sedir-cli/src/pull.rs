//! `sedir pull`: download a released base image from GitHub and verify it
//! against the release's SHA256SUMS. Uses the system's curl (HTTPS, proxies,
//! certificates) instead of linking an HTTP client into the binary.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};

use crate::builder::host_debian_arch;
use crate::cli::PullArgs;
use crate::paths::{create_private_dir, Paths};

/// Files of one release that a VM needs (see build_rootfs.sh artifacts).
const IMAGE_FILES: &[&str] = &["qcow2", "vmlinuz", "initrd.img", "cmdline", "build-info"];

pub fn run(paths: &Paths, args: &PullArgs) -> Result<()> {
    let repo = args.repo.as_str();
    if repo.split('/').count() != 2
        || !repo
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./".contains(c))
    {
        bail!("invalid repository '{repo}' (expected owner/name)");
    }

    let tag = match &args.release {
        Some(r) => format!("v{}", r.trim_start_matches('v')),
        None if args.url.is_some() => bail!("--url needs --release"),
        None => latest_tag(repo)?,
    };
    // A pre-release such as v13.1-rc1 has the same file names as 13.1.
    let label = tag.trim_start_matches('v').to_string();
    let release = label
        .split_once("-rc")
        .map_or(label.as_str(), |(r, _)| r)
        .to_string();
    let valid_release = release.split('.').count() == 2
        && release
            .split('.')
            .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()));
    if !valid_release {
        bail!("invalid release '{release}' (expected e.g. 13.1)");
    }
    let arch = host_debian_arch()?;
    let stem = format!("sedir-{release}-{arch}");
    let base = match &args.url {
        Some(u) => u.trim_end_matches('/').to_string(),
        None => format!("https://github.com/{repo}/releases/download/{tag}"),
    };

    let dest = paths.downloads_dir().join(&release);
    // .complete holds the tag that was downloaded: a final release replaces
    // an earlier pre-release of the same version.
    let have = fs::read_to_string(dest.join(".complete")).unwrap_or_default();
    if have.trim() == tag && !args.force {
        println!(
            "SedirOS {label} ({arch}) is already downloaded: {}",
            dest.display()
        );
        return Ok(());
    }

    create_private_dir(&paths.downloads_dir())?;
    let staging = paths
        .downloads_dir()
        .join(format!(".{release}.tmp-{}", std::process::id()));
    if staging.exists() {
        fs::remove_dir_all(&staging)?;
    }
    create_private_dir(&staging)?;

    println!("Downloading SedirOS {label} ({arch}) from {base}");
    download(
        &format!("{base}/SHA256SUMS"),
        &staging.join("SHA256SUMS"),
        false,
    )?;
    let sums = fs::read_to_string(staging.join("SHA256SUMS"))?;

    for ext in IMAGE_FILES {
        let name = format!("{stem}.{ext}");
        let want = expected_sum(&sums, &name)
            .with_context(|| format!("{name} is not listed in the release's SHA256SUMS"))?;
        let file = staging.join(&name);
        download(&format!("{base}/{name}"), &file, *ext == "qcow2")?;
        let got = sha256(&file)?;
        if got != want {
            let _ = fs::remove_dir_all(&staging);
            bail!("checksum mismatch for {name}: expected {want}, got {got}");
        }
        if *ext == "qcow2" {
            // Same key as a local build: the image cache is named after it.
            fs::write(
                staging.join(format!("{name}.sha256")),
                format!("{got}  {name}\n"),
            )?;
        }
        println!("  ✓ {name}");
    }
    fs::write(staging.join(".complete"), format!("{tag}\n"))?;

    if dest.exists() {
        fs::remove_dir_all(&dest)?;
    }
    fs::rename(&staging, &dest)?;

    let build = fs::read_to_string(dest.join(format!("{stem}.build-info")))
        .ok()
        .and_then(|t| {
            t.lines()
                .find_map(|l| l.strip_prefix("BUILD_ID=").map(str::to_string))
        })
        .unwrap_or_else(|| "?".into());
    println!("\nSedirOS {label} build {build} is ready. Start a VM: sedir launch <name>");
    Ok(())
}

/// Newest pulled base image (by release number), if any.
pub fn newest_pulled(paths: &Paths) -> Option<PathBuf> {
    let arch = host_debian_arch().ok()?;
    let mut best: Option<((u32, u32), PathBuf)> = None;
    for entry in fs::read_dir(paths.downloads_dir()).ok()?.flatten() {
        let release = entry.file_name().to_string_lossy().into_owned();
        let (major, point) = release.split_once('.')?;
        let key = (major.parse().ok()?, point.parse().ok()?);
        let qcow = entry.path().join(format!("sedir-{release}-{arch}.qcow2"));
        if entry.path().join(".complete").is_file()
            && qcow.is_file()
            && best.as_ref().map_or(true, |(k, _)| key > *k)
        {
            best = Some((key, qcow));
        }
    }
    best.map(|(_, p)| p)
}

fn curl() -> Command {
    let mut c = Command::new("curl");
    // -f: fail on HTTP errors; --proto: never downgrade from HTTPS on redirect.
    c.args(["-fL", "--proto", "=https,http", "--retry", "3"]);
    c
}

fn download(url: &str, to: &Path, progress: bool) -> Result<()> {
    let mut cmd = curl();
    if progress {
        cmd.arg("--progress-bar");
    } else {
        cmd.arg("-sS");
    }
    let status = cmd
        .arg("-o")
        .arg(to)
        .arg(url)
        .stdin(Stdio::null())
        .status()
        .context("running curl (is it installed?)")?;
    if !status.success() {
        bail!("download failed: {url}");
    }
    Ok(())
}

/// Tag of the latest published release (drafts and pre-releases excluded).
fn latest_tag(repo: &str) -> Result<String> {
    let api = |path: &str| {
        curl()
            .args(["-sS", "-H", "Accept: application/vnd.github+json"])
            .arg(format!("https://api.github.com/repos/{repo}/{path}"))
            .stdin(Stdio::null())
            .output()
            .context("running curl (is it installed?)")
    };
    let out = api("releases/latest")?;
    if out.status.success() {
        return tag_name(&String::from_utf8_lossy(&out.stdout));
    }
    // No final release yet: the newest pre-release (drafts are never listed).
    let out = api("releases?per_page=1")?;
    let body = String::from_utf8_lossy(&out.stdout);
    if !out.status.success() || !body.contains("\"tag_name\"") {
        bail!("no published release found for {repo} (use --release)");
    }
    let tag = tag_name(&body)?;
    println!("No final release yet, using pre-release {tag}");
    Ok(tag)
}

fn tag_name(body: &str) -> Result<String> {
    // "tag_name": "v13.1" - a full JSON parser is not needed for one field.
    let after = body
        .split_once("\"tag_name\"")
        .map(|(_, rest)| rest)
        .context("unexpected GitHub API response")?;
    let value = after
        .split('"')
        .nth(1)
        .context("unexpected GitHub API response")?;
    Ok(value.to_string())
}

fn expected_sum<'a>(sums: &'a str, name: &str) -> Option<&'a str> {
    sums.lines().find_map(|l| {
        let mut parts = l.split_whitespace();
        let sum = parts.next()?;
        let file = parts.next()?.trim_start_matches('*');
        (file == name && sum.len() == 64).then_some(sum)
    })
}

fn sha256(file: &Path) -> Result<String> {
    let out = if cfg!(target_os = "macos") {
        Command::new("shasum")
            .args(["-a", "256"])
            .arg(file)
            .output()
    } else {
        Command::new("sha256sum").arg(file).output()
    }
    .context("computing sha256")?;
    if !out.status.success() {
        bail!("sha256 of {} failed", file.display());
    }
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .next()
        .map(str::to_lowercase)
        .context("sha256 output")
}

#[cfg(test)]
mod tests {
    use super::expected_sum;

    #[test]
    fn finds_sums_in_both_formats() {
        let sums = format!(
            "{a}  sedir-13.1-arm64.qcow2\n{b} *sedir-13.1-arm64.vmlinuz\n",
            a = "a".repeat(64),
            b = "b".repeat(64)
        );
        assert_eq!(
            expected_sum(&sums, "sedir-13.1-arm64.qcow2"),
            Some("a".repeat(64).as_str())
        );
        assert_eq!(
            expected_sum(&sums, "sedir-13.1-arm64.vmlinuz"),
            Some("b".repeat(64).as_str())
        );
        assert_eq!(expected_sum(&sums, "sedir-13.1-arm64.iso"), None);
    }
}
