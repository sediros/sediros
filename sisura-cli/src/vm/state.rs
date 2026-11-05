use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::process;

const STATE_FILE: &str = "vm.conf";

/// Persistent VM description, stored as key=value in <vm-dir>/vm.conf.
#[derive(Debug, Clone)]
pub struct VmState {
    pub name: String,
    pub dir: PathBuf,
    pub arch: String,
    pub image_key: String,
    pub ssh_port: u16,
    pub cpus: u32,
    pub memory_mib: u32,
    pub created: u64,
}

impl VmState {
    pub fn overlay(&self) -> PathBuf {
        self.dir.join("disk.qcow2")
    }
    pub fn pidfile(&self) -> PathBuf {
        self.dir.join("qemu.pid")
    }
    pub fn qmp_socket(&self) -> PathBuf {
        self.dir.join("qmp.sock")
    }
    pub fn serial_log(&self) -> PathBuf {
        self.dir.join("serial.log")
    }
    pub fn known_hosts(&self) -> PathBuf {
        self.dir.join("known_hosts")
    }

    pub fn save(&self) -> Result<()> {
        let body = format!(
            "name={}\narch={}\nimage_key={}\nssh_port={}\ncpus={}\nmemory_mib={}\ncreated={}\n",
            self.name,
            self.arch,
            self.image_key,
            self.ssh_port,
            self.cpus,
            self.memory_mib,
            self.created
        );
        // Atomic replace: never leave a half-written state file behind.
        let tmp = self.dir.join(".vm.conf.tmp");
        fs::write(&tmp, body).with_context(|| format!("writing {}", tmp.display()))?;
        fs::rename(&tmp, self.dir.join(STATE_FILE))?;
        Ok(())
    }

    pub fn load(dir: &Path) -> Result<Self> {
        let path = dir.join(STATE_FILE);
        let text =
            fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        let map: BTreeMap<&str, &str> = text
            .lines()
            .filter_map(|l| l.split_once('='))
            .map(|(k, v)| (k.trim(), v.trim()))
            .collect();
        let get = |k: &str| {
            map.get(k)
                .copied()
                .with_context(|| format!("{}: missing key '{k}'", path.display()))
        };
        Ok(Self {
            name: get("name")?.to_string(),
            dir: dir.to_path_buf(),
            arch: get("arch")?.to_string(),
            image_key: get("image_key")?.to_string(),
            ssh_port: get("ssh_port")?.parse()?,
            cpus: get("cpus")?.parse()?,
            memory_mib: get("memory_mib")?.parse()?,
            created: get("created")?.parse()?,
        })
    }

    pub fn pid(&self) -> Option<i32> {
        fs::read_to_string(self.pidfile()).ok()?.trim().parse().ok()
    }

    pub fn is_running(&self) -> bool {
        self.pid().is_some_and(process::is_qemu_alive)
    }

    /// Remove runtime leftovers (pidfile, QMP socket) of a stopped VM.
    pub fn clear_runtime(&self) {
        let _ = fs::remove_file(self.pidfile());
        let _ = fs::remove_file(self.qmp_socket());
    }
}

pub fn load_all(vms_dir: &Path) -> Result<Vec<VmState>> {
    let mut out = Vec::new();
    let entries = match fs::read_dir(vms_dir) {
        Ok(e) => e,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(e).with_context(|| format!("reading {}", vms_dir.display())),
    };
    for entry in entries {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        match VmState::load(&entry.path()) {
            Ok(vm) => out.push(vm),
            Err(e) => eprintln!("warning: skipping {}: {e:#}", entry.path().display()),
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}
