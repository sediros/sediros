//! VM lifecycle: launch, list, stop.

mod image;
mod network;
mod process;
mod qemu;
mod state;

use std::fs;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};

pub use state::{load_all, VmState};

use crate::cli::{LaunchArgs, StopArgs};
use crate::paths::{create_private_dir, validate_vm_name, Paths};
use image::BaseImage;
use qemu::{Accel, Platform};

pub fn launch(paths: &Paths, args: &LaunchArgs) -> Result<()> {
    validate_vm_name(&args.name)?;
    if args.cpus == 0 || args.memory < 256 {
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
        if args.image.is_some() || args.data_disk.is_some() {
            bail!(
                "VM '{}' already exists; --image/--data-disk only apply to new VMs (purge it first)",
                vm.name
            );
        }
    }

    // Ports already assigned to *other* VMs stay reserved even while stopped.
    let taken: Vec<u16> = load_all(&paths.vms_dir())?
        .into_iter()
        .filter(|vm| vm.name != args.name)
        .map(|vm| vm.ssh_port)
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
                cpus: args.cpus,
                memory_mib: args.memory,
                created: SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs(),
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
    vm.cpus = args.cpus;
    vm.memory_mib = args.memory;

    let platform = Platform::detect(&vm.arch)?;
    if platform.accel == Accel::Tcg {
        eprintln!(
            "warning: no hardware acceleration for {} on this host, using TCG (slow)",
            vm.arch
        );
    }

    if fresh {
        qemu::create_overlay(&platform, &image, &vm.overlay())?;
        if let Some(size) = &args.data_disk {
            qemu::create_blank(&platform, &vm.data_disk(), size)?;
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
            println!("Next: sisura shell {}", vm.name);
            Ok(())
        }
        Err(err) => {
            println!();
            print_serial_tail(&vm, 25);
            Err(err).context(format!("VM '{}' did not become reachable", vm.name))
        }
    }
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
        println!("No VMs. Create one with: sisura launch <name>");
        return Ok(());
    }
    println!(
        "{:<20} {:<8} {:<6} {:<5} {:<8} {:<6} {:<18} BUILD",
        "NAME", "STATE", "ARCH", "CPUS", "MEMORY", "SSH", "IMAGE"
    );
    for vm in vms {
        let state = if vm.is_running() {
            "running"
        } else {
            "stopped"
        };
        println!(
            "{:<20} {:<8} {:<6} {:<5} {:<8} {:<6} {:<18} {}",
            vm.name,
            state,
            vm.arch,
            vm.cpus,
            format!("{}M", vm.memory_mib),
            vm.ssh_port,
            vm.image_key,
            image::cached_build_id(paths, &vm.image_key)
        );
    }
    Ok(())
}

pub fn stop_cmd(paths: &Paths, args: &StopArgs) -> Result<()> {
    validate_vm_name(&args.name)?;
    let vm = VmState::load(&paths.vm_dir(&args.name))
        .with_context(|| format!("VM '{}' not found", args.name))?;
    if !vm.is_running() {
        vm.clear_runtime();
        println!("VM '{}' is not running.", vm.name);
        return Ok(());
    }
    stop(&vm, Duration::from_secs(args.timeout), args.force)?;
    println!("Stopped '{}'.", vm.name);
    Ok(())
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
