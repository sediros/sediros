//! Installed-system settings collected before the disk is touched: admin
//! account (name, sudo password, SSH keys), keyboard layout, GRUB password.
//! Everything is validated up front so that a bad answer never leaves a
//! half-installed disk behind.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::cmd::{self, chroot};
use crate::secret::Secret;

pub const MIN_PASSWORD: usize = 12;

pub struct Keyboard {
    pub layout: String,
    pub variant: String,
}

impl Keyboard {
    /// "tr" or "tr:f" (layout[:variant]).
    pub fn parse(spec: &str) -> Result<Self> {
        let (layout, variant) = spec.split_once(':').unwrap_or((spec, ""));
        let kb = Self {
            layout: layout.trim().to_string(),
            variant: variant.trim().to_string(),
        };
        kb.validate()?;
        Ok(kb)
    }

    fn validate(&self) -> Result<()> {
        let ok_chars = |s: &str| {
            s.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        };
        if self.layout.is_empty() || !ok_chars(&self.layout) || !ok_chars(&self.variant) {
            bail!("invalid keyboard layout '{}'", self.describe());
        }
        let symbols = Path::new("/usr/share/X11/xkb/symbols").join(&self.layout);
        let text = fs::read_to_string(&symbols)
            .with_context(|| format!("unknown keyboard layout '{}'", self.layout))?;
        if !self.variant.is_empty() && !text.contains(&format!("xkb_symbols \"{}\"", self.variant))
        {
            bail!("layout '{}' has no variant '{}'", self.layout, self.variant);
        }
        Ok(())
    }

    pub fn describe(&self) -> String {
        if self.variant.is_empty() {
            self.layout.clone()
        } else {
            format!("{}:{}", self.layout, self.variant)
        }
    }

    fn file(&self) -> String {
        format!(
            "# Written by sisura-install. Also used by the initramfs (LUKS prompt).\n\
             XKBMODEL=\"pc105\"\n\
             XKBLAYOUT=\"{}\"\n\
             XKBVARIANT=\"{}\"\n\
             XKBOPTIONS=\"\"\n\
             BACKSPACE=\"guess\"\n",
            self.layout, self.variant
        )
    }
}

pub struct Account {
    pub user: String,
    /// None: keep the image's empty, expired password (set at first login).
    pub password: Option<Secret>,
    /// authorized_keys lines.
    pub ssh_keys: Vec<String>,
}

pub struct Setup {
    pub account: Account,
    pub keyboard: Keyboard,
    pub grub_password: Option<Secret>,
    /// Port of the initramfs SSH server for unlocking LUKS (needs SSH keys).
    pub ssh_unlock_port: Option<u16>,
}

pub fn valid_username(u: &str) -> bool {
    let mut chars = u.chars();
    matches!(chars.next(), Some('a'..='z' | '_'))
        && u.len() <= 32
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
        && !matches!(u, "root" | "daemon" | "bin" | "sys" | "nobody" | "admin")
}

/// Same rules as the installed system (pam_pwquality + Sisura's conf.d),
/// checked with pwscore when available.
pub fn check_password(user: &str, password: &Secret) -> Result<()> {
    if password.bytes().len() < MIN_PASSWORD {
        bail!("at least {MIN_PASSWORD} characters");
    }
    if cmd::output("sh", &["-c", "command -v pwscore"]).is_err() {
        return Ok(());
    }
    let mut input = password.bytes().to_vec();
    input.push(b'\n');
    let verdict = cmd::check_with_stdin("pwscore", &[user], &input);
    input.iter_mut().for_each(|b| *b = 0);
    match verdict? {
        Ok(_) => Ok(()),
        Err(reason) => bail!(
            "{}",
            reason
                .trim()
                .trim_start_matches("Password quality check failed:")
                .trim()
        ),
    }
}

/// Validate authorized_keys text: one public key per line, no private keys.
pub fn parse_keys(text: &str) -> Result<Vec<String>> {
    if text.contains("PRIVATE KEY") {
        bail!("that is a PRIVATE key - paste the public key (the .pub file)");
    }
    let mut keys = Vec::new();
    for line in text.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut input = line.as_bytes().to_vec();
        input.push(b'\n');
        if cmd::check_with_stdin("ssh-keygen", &["-l", "-f", "-"], &input)?.is_err() {
            bail!("not a valid SSH public key: {}", shorten(line));
        }
        keys.push(line.to_string());
    }
    Ok(keys)
}

fn shorten(s: &str) -> String {
    if s.len() > 40 {
        format!("{}...", &s[..40])
    } else {
        s.to_string()
    }
}

/// Public keys published on GitHub (https://github.com/<user>.keys).
pub fn github_keys(user: &str) -> Result<Vec<String>> {
    if user.is_empty()
        || user.len() > 39
        || !user.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
    {
        bail!("invalid GitHub user name '{user}'");
    }
    let url = format!("https://github.com/{user}.keys");
    let text = cmd::output(
        "curl",
        &["-fsSL", "--proto", "=https", "--max-time", "20", &url],
    )
    .with_context(|| format!("fetching {url}"))?;
    let keys = parse_keys(&text)?;
    if keys.is_empty() {
        bail!("GitHub user '{user}' has no public SSH keys");
    }
    Ok(keys)
}

/// Use the chosen layout on the live console too, so the passphrases typed
/// now match what the boot prompt will expect. Best effort, VT only.
pub fn apply_live_keyboard(kb: &Keyboard) {
    let on_vt = unsafe {
        let p = libc::ttyname(0);
        !p.is_null()
            && std::ffi::CStr::from_ptr(p)
                .to_string_lossy()
                .trim_start_matches("/dev/tty")
                .parse::<u32>()
                .is_ok()
    };
    if !on_vt || fs::write("/etc/default/keyboard", kb.file()).is_err() {
        return;
    }
    let _ = std::process::Command::new("setupcon")
        .args(["-k", "--force"])
        .status();
}

// --- applied to the target -------------------------------------------------

/// Keyboard for the installed console and the initramfs (KEYMAP=y makes
/// initramfs-tools embed it, so the LUKS prompt uses the same layout).
pub fn write_keyboard(target: &Path, kb: &Keyboard) -> Result<()> {
    fs::write(target.join("etc/default/keyboard"), kb.file())?;
    let conf = target.join("etc/initramfs-tools/conf.d");
    fs::create_dir_all(&conf)?;
    fs::write(
        conf.join("sisura-keymap"),
        "# Written by sisura-install: keyboard layout for the LUKS prompt.\nKEYMAP=y\n",
    )?;
    Ok(())
}

fn image_user(target: &Path) -> Result<String> {
    let profile = fs::read_to_string(target.join("etc/sisura/build-profile"))
        .context("reading /etc/sisura/build-profile of the copied system")?;
    profile
        .lines()
        .find_map(|l| l.strip_prefix("USER="))
        .map(str::to_string)
        .context("no USER= in /etc/sisura/build-profile")
}

fn user_exists(target: &Path, user: &str) -> Result<bool> {
    let passwd = fs::read_to_string(target.join("etc/passwd"))?;
    Ok(passwd.lines().any(|l| l.split(':').next() == Some(user)))
}

/// Admin account, inside the chroot: rename, sudo password, SSH keys.
pub fn configure_account(target: &Path, account: &Account) -> Result<()> {
    let old = image_user(target)?;
    let user = account.user.as_str();
    if user != old {
        if user_exists(target, user)? {
            bail!("user '{user}' already exists in the system image");
        }
        chroot(
            target,
            "usermod",
            &["-l", user, "-d", &format!("/home/{user}"), "-m", &old],
        )?;
        chroot(target, "groupmod", &["-n", user, &old])?;
        let profile = target.join("etc/sisura/build-profile");
        let text = fs::read_to_string(&profile)?;
        let updated: Vec<String> = text
            .lines()
            .map(|l| {
                if l.starts_with("USER=") {
                    format!("USER={user}")
                } else {
                    l.to_string()
                }
            })
            .collect();
        fs::write(&profile, updated.join("\n") + "\n")?;
    }

    if let Some(pw) = &account.password {
        // chpasswd hashes with ENCRYPT_METHOD (yescrypt) through PAM, which
        // applies the same pwquality rules check_password() used.
        let mut line = format!("{user}:").into_bytes();
        line.extend_from_slice(pw.bytes());
        line.push(b'\n');
        let result = cmd::run_with_stdin(
            "chroot",
            &[target.to_str().context("target path")?, "chpasswd"],
            &line,
        );
        line.iter_mut().for_each(|b| *b = 0);
        result?;
    }

    if !account.ssh_keys.is_empty() {
        let ssh: PathBuf = target.join("home").join(user).join(".ssh");
        fs::create_dir_all(&ssh)?;
        fs::write(
            ssh.join("authorized_keys"),
            account.ssh_keys.join("\n") + "\n",
        )?;
        let inside = format!("/home/{user}/.ssh");
        chroot(target, "chown", &["-R", &format!("{user}:{user}"), &inside])?;
        chroot(target, "chmod", &["0700", &inside])?;
        chroot(
            target,
            "chmod",
            &["0600", &format!("{inside}/authorized_keys")],
        )?;
    }
    Ok(())
}

/// GRUB password via sisura-boot-hardening (runs update-grub itself).
pub fn set_grub_password(target: &Path, password: &Secret) -> Result<()> {
    let mut input = password.bytes().to_vec();
    input.push(b'\n');
    let result = cmd::run_with_stdin(
        "chroot",
        &[
            target.to_str().context("target path")?,
            "sisura-grub-password",
            "set",
            "--stdin",
        ],
        &input,
    );
    input.iter_mut().for_each(|b| *b = 0);
    result
}

// --- SSH unlock in the initramfs (dropbear-initramfs) ------------------------

/// Diverted away in the image by build_rootfs.sh (disable_initramfs_ssh).
const DROPBEAR_INITRAMFS_FILES: &[&str] = &[
    "hooks/dropbear",
    "scripts/init-premount/dropbear",
    "scripts/init-bottom/dropbear",
];
const DROPBEAR_DIVERT_DIR: &str = "/usr/share/sisura/initramfs-dropbear";
const DROPBEAR_CONF_DIR: &str = "etc/dropbear/initramfs";
/// Path of cryptroot-unlock inside the initramfs.
const CRYPTROOT_UNLOCK: &str = "/usr/bin/cryptroot-unlock";

fn has_package(target: &Path, pkg: &str) -> bool {
    cmd::output(
        "chroot",
        &[
            target.to_str().unwrap_or("/"),
            "dpkg-query",
            "-W",
            "-f=${Status}",
            pkg,
        ],
    )
    .map(|s| s.ends_with(" installed"))
    .unwrap_or(false)
}

fn diverted(target: &Path, file: &str) -> bool {
    target
        .join(DROPBEAR_DIVERT_DIR.trim_start_matches('/'))
        .join(file)
        .exists()
        || !target.join("usr/share/initramfs-tools").join(file).exists()
}

/// Enable SSH unlock: restore dropbear's initramfs files, create this
/// machine's host keys, and allow only cryptroot-unlock for the given keys.
/// Returns the host key fingerprint to show the user.
pub fn enable_ssh_unlock(target: &Path, keys: &[String], port: u16) -> Result<String> {
    if !has_package(target, "dropbear-initramfs") {
        bail!("dropbear-initramfs is not in the system image");
    }
    for f in DROPBEAR_INITRAMFS_FILES {
        if diverted(target, f) {
            chroot(
                target,
                "dpkg-divert",
                &[
                    "--local",
                    "--rename",
                    "--remove",
                    &format!("/usr/share/initramfs-tools/{f}"),
                ],
            )?;
        }
    }

    let conf = target.join(DROPBEAR_CONF_DIR);
    fs::create_dir_all(&conf)?;
    for entry in fs::read_dir(&conf)? {
        let path = entry?.path();
        if path.to_string_lossy().contains("_host_key") {
            fs::remove_file(path)?;
        }
    }
    for kind in ["ed25519", "ecdsa"] {
        chroot(
            target,
            "dropbearkey",
            &[
                "-t",
                kind,
                "-f",
                &format!("/{DROPBEAR_CONF_DIR}/dropbear_{kind}_host_key"),
            ],
        )?;
    }

    // Every key may only run cryptroot-unlock: no shell, no forwarding. A pty
    // is kept on purpose: without one the passphrase would echo on the
    // client terminal.
    let restrict = format!(
        "no-port-forwarding,no-agent-forwarding,no-X11-forwarding,command=\"{CRYPTROOT_UNLOCK}\""
    );
    let authorized: Vec<String> = keys.iter().map(|k| format!("{restrict} {k}")).collect();
    let ak = conf.join("authorized_keys");
    fs::write(&ak, authorized.join("\n") + "\n")?;
    cmd::run("chmod", &["0600", ak.to_str().context("path")?])?;

    fs::write(
        conf.join("dropbear.conf"),
        format!(
            "# Written by sisura-install: SSH in the initramfs only unlocks the disk.\n\
             #  -p port  -s no passwords  -j/-k no port forwarding\n\
             #  -I idle timeout  -c forced command\n\
             DROPBEAR_OPTIONS=\"-p {port} -s -j -k -I 300 -c {CRYPTROOT_UNLOCK}\"\n"
        ),
    )?;

    let public = cmd::output(
        "chroot",
        &[
            target.to_str().context("path")?,
            "dropbearkey",
            "-y",
            "-f",
            &format!("/{DROPBEAR_CONF_DIR}/dropbear_ed25519_host_key"),
        ],
    )?;
    let line = public
        .lines()
        .find(|l| l.starts_with("ssh-ed25519 "))
        .context("dropbearkey printed no public key")?;
    let mut input = line.as_bytes().to_vec();
    input.push(b'\n');
    match cmd::check_with_stdin("ssh-keygen", &["-l", "-f", "-"], &input)? {
        Ok(fp) => Ok(fp.split_whitespace().nth(1).unwrap_or_default().to_string()),
        Err(e) => bail!("ssh-keygen: {e}"),
    }
}

/// No SSH unlock: remove dropbear from the installed system entirely.
pub fn remove_ssh_unlock(target: &Path) -> Result<()> {
    for pkg in ["dropbear-initramfs", "dropbear-bin"] {
        if has_package(target, pkg) {
            chroot(target, "dpkg", &["--purge", pkg])?;
        }
    }
    for f in DROPBEAR_INITRAMFS_FILES {
        let path = format!("/usr/share/initramfs-tools/{f}");
        let listed = cmd::output(
            "chroot",
            &[
                target.to_str().context("path")?,
                "dpkg-divert",
                "--list",
                &path,
            ],
        )
        .unwrap_or_default();
        if !listed.is_empty() {
            chroot(target, "dpkg-divert", &["--local", "--remove", &path])?;
        }
    }
    Ok(())
}
