//! VM lifecycle: launch, list, stop.

mod forward;
mod image;
mod network;
mod process;
mod qemu;
mod snapshot;
mod state;

use std::fs;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};

pub use forward::forward_cmd;
pub use snapshot::{restore_cmd, snapshot_cmd};
pub use state::{load_all, VmState};

use crate::cli::{LaunchArgs, StopArgs};
use crate::paths::{create_private_dir, remove_dir_within, validate_vm_name, Paths};
use image::BaseImage;
use qemu::{Accel, Platform};

/// vCPUs and memory (MiB) of a new VM when --cpus / --memory are not given.
const DEFAULT_CPUS: u32 = 2;
const DEFAULT_MEMORY_MIB: u32 = 1024;
/// Virtual disk of a new persistent VM. Sparse: the host only stores what
/// the VM writes. Ephemeral VMs keep the image's own size.
const DEFAULT_DISK_BYTES: u64 = 20 << 30;

pub fn launch(paths: &Paths, args: &LaunchArgs) -> Result<()> {
    validate_vm_name(&args.name)?;
    if args.cpus == Some(0) || args.memory.is_some_and(|m| m < 256) {
        bail!("need at least 1 vCPU and 256 MiB of memory");
    }

    let dir = paths.vm_dir(&args.name);
    let existing = if dir.join("vm.conf").is_file() {
        Some(VmState::load(&dir)?)
    } else {
        None
    };
    if let Some(vm) = &existing {
        if vm.is_running() {
            bail!(
                "VM '{}' is already running (ssh port {})",
                vm.name,
                vm.ssh_port
            );
        }
        if args.image.is_some() || args.data_disk.is_some() || args.ephemeral {
            bail!(
                "VM '{}' already exists; --image/--data-disk/--ephemeral only apply to new VMs (purge it first)",
                vm.name
            );
        }
    }
    let disk_size = args.disk.as_deref().map(qemu::parse_size).transpose()?;

    // Ports already assigned to *other* VMs stay reserved even while stopped.
    let taken: Vec<u16> = load_all(&paths.vms_dir())?
        .into_iter()
        .filter(|vm| vm.name != args.name)
        .flat_map(|vm| {
            let mut ports: Vec<u16> = vm.forwards.iter().map(|f| f.host).collect();
            ports.push(vm.ssh_port);
            ports
        })
        .collect();

    let (mut vm, image, fresh) = match existing {
        Some(vm) => {
            let image = BaseImage::cached(paths, &vm.image_key, &vm.arch)?;
            (vm, image, false)
        }
        None => {
            let image = BaseImage::prepare(paths, args.image.as_deref())?;
            create_private_dir(&dir)?;
            let vm = VmState {
                name: args.name.clone(),
                dir: dir.clone(),
                arch: image.arch.clone(),
                image_key: image.key.clone(),
                ssh_port: 0,
                cpus: args.cpus.unwrap_or(DEFAULT_CPUS),
                memory_mib: args.memory.unwrap_or(DEFAULT_MEMORY_MIB),
                created: SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs(),
                forwards: Vec::new(),
                ephemeral: args.ephemeral,
                grow_root: false,
            };
            (vm, image, true)
        }
    };

    // Keep the VM's previous port if still free, so ssh configs stay valid.
    vm.ssh_port = match args.port {
        Some(p) => network::pick_port(Some(p), &taken)?,
        None if !fresh && network::port_free(vm.ssh_port) => vm.ssh_port,
        None => network::pick_port(None, &taken)?,
    };
    // An existing VM keeps its CPUs and memory; --cpus/--memory change them
    // and are saved for the next launch.
    if let Some(cpus) = args.cpus {
        vm.cpus = cpus;
    }
    if let Some(memory) = args.memory {
        vm.memory_mib = memory;
    }
    // The VM's own SSH port must not collide with one of its forwards.
    if vm.forwards.iter().any(|f| f.host == vm.ssh_port) {
        bail!("SSH port {} is also a port forward of this VM", vm.ssh_port);
    }
    forward::add_at_launch(paths, &mut vm, &args.forwards)?;

    let platform = Platform::detect(&vm.arch)?;
    if platform.accel == Accel::Tcg {
        eprintln!(
            "warning: no hardware acceleration for {} ({}), using TCG (slow)",
            vm.arch,
            platform.tcg_reason.as_deref().unwrap_or("unknown reason")
        );
    }

    if fresh {
        let base = qemu::virtual_size(&platform, &image.disk())?;
        let want = disk_size.or((!vm.ephemeral).then_some(DEFAULT_DISK_BYTES));
        let size = match want {
            Some(w) if w < base => {
                // Nothing of the new VM exists yet but its directory.
                remove_dir_within(&paths.vms_dir(), &vm.dir)?;
                bail!(
                    "--disk {} is smaller than the image ({})",
                    qemu::human_size(w),
                    qemu::human_size(base)
                )
            }
            Some(w) if w > base => Some(w),
            _ => None,
        };
        qemu::create_overlay(&platform, &image, &vm.overlay(), size)?;
        vm.grow_root = size.is_some();
        if let Some(size) = &args.data_disk {
            qemu::create_blank(&platform, &vm.data_disk(), size)?;
        }
    } else if let Some(want) = disk_size {
        let have = qemu::virtual_size(&platform, &vm.overlay())?;
        if want < have {
            bail!(
                "disks never shrink: '{}' already has {}",
                vm.name,
                qemu::human_size(have)
            );
        }
        if want > have {
            qemu::grow_disk(&platform, &vm.overlay(), want)?;
            vm.grow_root = true;
            println!(
                "Disk of '{}' grown from {} to {}",
                vm.name,
                qemu::human_size(have),
                qemu::human_size(want)
            );
        }
    }
    vm.save()?;
    vm.clear_runtime();
    let _ = fs::remove_file(vm.serial_log());

    let started = Instant::now();
    let key = crate::paths::ensure_ssh_key(&paths.home)?;
    let pubkey = fs::read_to_string(key.with_extension("pub"))
        .with_context(|| format!("reading {}.pub", key.display()))?;
    qemu::spawn(&platform, &vm, &image, &pubkey)?;
    let accel = format!("{:?}", platform.accel).to_lowercase();
    println!(
        "Launched '{}' ({}, {accel}, {} vCPU, {} MiB) - ssh on 127.0.0.1:{}",
        vm.name, vm.arch, vm.cpus, vm.memory_mib, vm.ssh_port
    );

    if args.no_wait {
        if !vm.forwards.is_empty() {
            println!("Port forwards (the VM's firewall is not updated with --no-wait):");
            forward::print_forwards(&vm);
        }
        return Ok(());
    }

    print!("Waiting for SSH...");
    let _ = std::io::Write::flush(&mut std::io::stdout());
    let result = network::wait_for_ssh(vm.ssh_port, Duration::from_secs(args.timeout), || {
        vm.is_running()
    });
    match result {
        Ok(()) => {
            println!(" ready in {:.1}s", started.elapsed().as_secs_f64());
            if vm.grow_root {
                grow_root_fs(paths, &mut vm)?;
            }
            if !vm.forwards.is_empty() {
                forward::open_firewall(paths, &vm);
                forward::print_forwards(&vm);
            }
            println!("Next: sediros shell {}", vm.name);
            Ok(())
        }
        Err(err) => {
            println!();
            print_serial_tail(&vm, 25);
            Err(err).context(format!("VM '{}' did not become reachable", vm.name))
        }
    }
}

/// Grow the root filesystem to the disk after the disk grew. Images since
/// 1.2 do it themselves at boot (x-systemd.growfs), then this is a no-op;
/// older images need it. Kept pending until it worked (e.g. --no-wait).
fn grow_root_fs(paths: &Paths, vm: &mut VmState) -> Result<()> {
    let cmd = r#"sudo -n resize2fs "$(findmnt -nvo SOURCE /)" >/dev/null 2>&1"#;
    match crate::ssh::run_in_vm(paths, vm, cmd) {
        Ok(true) => {
            vm.grow_root = false;
            vm.save()?;
        }
        _ => eprintln!(
            "warning: could not grow the root filesystem; run in the VM:\n  sudo resize2fs \"$(findmnt -nvo SOURCE /)\""
        ),
    }
    Ok(())
}

fn print_serial_tail(vm: &VmState, lines: usize) {
    if let Ok(log) = fs::read_to_string(vm.serial_log()) {
        let all: Vec<&str> = log.lines().collect();
        eprintln!(
            "--- last {lines} lines of {} ---",
            vm.serial_log().display()
        );
        for line in &all[all.len().saturating_sub(lines)..] {
            eprintln!("{line}");
        }
        eprintln!("---");
    }
}

pub fn list(paths: &Paths) -> Result<()> {
    let vms = load_all(&paths.vms_dir())?;
    if vms.is_empty() {
        println!("No VMs. Create one with: sediros launch <name>");
        return Ok(());
    }
    println!(
        "{:<20} {:<8} {:<10} {:<6} {:<5} {:<8} {:<6} {:<6} {:<18} {:<28} FORWARDS",
        "NAME", "STATE", "MODE", "ARCH", "CPUS", "MEMORY", "DISK", "SSH", "IMAGE", "BUILD"
    );
    for vm in vms {
        let state = if vm.is_running() {
            "running"
        } else {
            "stopped"
        };
        let forwards = vm
            .forwards
            .iter()
            .map(|f| f.to_string())
            .collect::<Vec<_>>()
            .join(",");
        println!(
            "{:<20} {:<8} {:<10} {:<6} {:<5} {:<8} {:<6} {:<6} {:<18} {:<28} {}",
            vm.name,
            state,
            if vm.ephemeral {
                "ephemeral"
            } else {
                "persistent"
            },
            vm.arch,
            vm.cpus,
            format!("{}M", vm.memory_mib),
            disk_used(&vm),
            vm.ssh_port,
            vm.image_key,
            image::cached_build_id(paths, &vm.image_key),
            if forwards.is_empty() { "-" } else { &forwards }
        );
    }
    Ok(())
}

pub fn stop_cmd(paths: &Paths, args: &StopArgs) -> Result<()> {
    validate_vm_name(&args.name)?;
    let vm = VmState::load(&paths.vm_dir(&args.name))
        .with_context(|| format!("VM '{}' not found", args.name))?;
    if vm.is_running() {
        stop(&vm, Duration::from_secs(args.timeout), args.force)?;
        println!("Stopped '{}'.", vm.name);
    } else {
        vm.clear_runtime();
        println!("VM '{}' is not running.", vm.name);
    }
    if vm.ephemeral {
        remove_dir_within(&paths.vms_dir(), &vm.dir)?;
        println!("Deleted '{}' (ephemeral).", vm.name);
    }
    Ok(())
}

/// Space the VM's disks take on this machine (not their virtual size).
fn disk_used(vm: &VmState) -> String {
    use std::os::unix::fs::MetadataExt;
    let bytes: u64 = [vm.overlay(), vm.data_disk()]
        .iter()
        .filter_map(|p| fs::metadata(p).ok())
        .map(|m| m.blocks() * 512)
        .sum();
    qemu::human_size(bytes)
}

/// Graceful power-down via QMP, escalating to SIGTERM/SIGKILL.
pub fn stop(vm: &VmState, timeout: Duration, force: bool) -> Result<()> {
    let Some(pid) = vm.pid() else {
        vm.clear_runtime();
        return Ok(());
    };

    if !force {
        match qemu::qmp_powerdown(&vm.qmp_socket()) {
            Ok(()) => {
                if process::wait_exit(pid, timeout) {
                    vm.clear_runtime();
                    return Ok(());
                }
                eprintln!(
                    "warning: '{}' did not power off within {}s, killing",
                    vm.name,
                    timeout.as_secs()
                );
            }
            Err(e) => eprintln!("warning: QMP power-down failed ({e:#}), killing"),
        }
    }

    process::signal(pid, libc::SIGTERM)?;
    if !process::wait_exit(pid, Duration::from_secs(5)) {
        process::signal(pid, libc::SIGKILL)?;
        if !process::wait_exit(pid, Duration::from_secs(5)) {
            bail!("could not kill QEMU (pid {pid})");
        }
    }
    vm.clear_runtime();
    Ok(())
}
