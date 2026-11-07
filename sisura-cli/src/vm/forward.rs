//! Port forwards: 127.0.0.1:<host> on this machine -> <guest> in the VM.
//!
//! QEMU's user network forwards the port (at launch, or live through QMP),
//! and the VM's firewall gets a rule that admits the forwarded connections:
//! they arrive from 10.0.2.2, the host's address in the user network. Host
//! ports are bound to loopback only, so nothing is exposed to the network.

use anyhow::{bail, Context, Result};

use super::network::port_free;
use super::qemu;
use super::state::{load_all, Forward, VmState};
use crate::cli::ForwardArgs;
use crate::paths::{validate_vm_name, Paths};
use crate::ssh::run_in_vm;

/// The host as seen from the guest (QEMU user network).
const HOST_IN_GUEST: &str = "10.0.2.2";

pub fn forward_cmd(paths: &Paths, args: &ForwardArgs) -> Result<()> {
    validate_vm_name(&args.name)?;
    let mut vm = VmState::load(&paths.vm_dir(&args.name))
        .with_context(|| format!("VM '{}' not found", args.name))?;
    let running = vm.is_running();

    if args.add.is_empty() && args.remove.is_empty() {
        print_forwards(&vm);
        return Ok(());
    }

    for host in &args.remove {
        let Some(pos) = vm.forwards.iter().position(|f| f.host == *host) else {
            bail!("VM '{}' has no forward from port {host}", vm.name);
        };
        let f = vm.forwards.remove(pos);
        if running {
            qemu::qmp_hostfwd(&vm.qmp_socket(), false, f.host, f.guest)?;
        }
        vm.save()?;
        println!("Removed 127.0.0.1:{} -> {}:{}", f.host, vm.name, f.guest);
        // Another forward may still use the same guest port.
        if !vm.forwards.iter().any(|o| o.guest == f.guest) {
            if running {
                firewall(paths, &vm, f.guest, false);
            } else {
                println!(
                    "  The VM's firewall still allows port {} (VM stopped); remove it there with:\n  \
                     sudo ufw delete allow from {HOST_IN_GUEST} to any port {} proto tcp",
                    f.guest, f.guest
                );
            }
        }
    }

    let new = parse_new(&vm, &args.add)?;
    check_host_ports(paths, &vm, &new, running)?;
    for f in new {
        if running {
            qemu::qmp_hostfwd(&vm.qmp_socket(), true, f.host, f.guest)?;
        }
        vm.forwards.push(f);
        vm.forwards.sort();
        vm.save()?;
        println!("Forwarding 127.0.0.1:{} -> {}:{}", f.host, vm.name, f.guest);
        if running {
            firewall(paths, &vm, f.guest, true);
        } else {
            println!(
                "  (VM stopped: active from the next `sisura launch {}`)",
                vm.name
            );
        }
    }
    Ok(())
}

/// `launch --forward`: new forwards for this VM, checked against all others.
pub fn add_at_launch(paths: &Paths, vm: &mut VmState, specs: &[String]) -> Result<()> {
    let new = parse_new(vm, specs)?;
    vm.forwards.extend(new);
    vm.forwards.sort();
    // At launch every forward of the VM must be free, not only new ones.
    let all = vm.forwards.clone();
    check_host_ports(paths, vm, &all, false)
}

/// After the VM is reachable: open its firewall for every forwarded port
/// (idempotent, ufw skips rules it already has).
pub fn open_firewall(paths: &Paths, vm: &VmState) {
    let mut ports: Vec<u16> = vm.forwards.iter().map(|f| f.guest).collect();
    ports.dedup();
    for port in ports {
        firewall(paths, vm, port, true);
    }
}

pub fn print_forwards(vm: &VmState) {
    if vm.forwards.is_empty() {
        println!(
            "VM '{}' has no port forwards. Add one: sisura forward {} 8080:80",
            vm.name, vm.name
        );
        return;
    }
    for f in &vm.forwards {
        println!(
            "127.0.0.1:{:<5} -> {}:{}   http://localhost:{}",
            f.host, vm.name, f.guest, f.host
        );
    }
}

fn parse_new(vm: &VmState, specs: &[String]) -> Result<Vec<Forward>> {
    let mut new: Vec<Forward> = Vec::new();
    for spec in specs {
        let f = Forward::parse(spec)?;
        if f.guest == 22 {
            bail!(
                "port 22 is the VM's SSH, already on 127.0.0.1:{} (sisura shell)",
                vm.ssh_port
            );
        }
        if f.host == vm.ssh_port {
            bail!("port {} is this VM's SSH port", f.host);
        }
        if let Some(old) = vm.forwards.iter().chain(&new).find(|o| o.host == f.host) {
            if *old == f {
                println!(
                    "Already forwarding 127.0.0.1:{} -> {}:{}",
                    f.host, vm.name, f.guest
                );
                continue;
            }
            bail!(
                "port {} already forwards to {}:{}",
                f.host,
                vm.name,
                old.guest
            );
        }
        new.push(f);
    }
    Ok(new)
}

/// Host ports must not belong to another Sisura VM (SSH or forward, even
/// while it is stopped) and must be free on 127.0.0.1.
fn check_host_ports(paths: &Paths, vm: &VmState, ports: &[Forward], running: bool) -> Result<()> {
    let others = load_all(&paths.vms_dir())?;
    for f in ports {
        for o in others.iter().filter(|o| o.name != vm.name) {
            if o.ssh_port == f.host || o.forwards.iter().any(|of| of.host == f.host) {
                bail!("port {} is already used by VM '{}'", f.host, o.name);
            }
        }
        // A running VM's own existing forwards hold their ports already.
        let own_live = running && vm.forwards.contains(f);
        if !own_live && !port_free(f.host) {
            bail!(
                "port {} is already in use by another program on this machine; pick another host port",
                f.host
            );
        }
    }
    Ok(())
}

fn firewall(paths: &Paths, vm: &VmState, port: u16, allow: bool) {
    let rule = format!("from {HOST_IN_GUEST} to any port {port} proto tcp");
    let cmd = if allow {
        format!("sudo -n ufw allow {rule} comment 'sisura forward'")
    } else {
        format!("sudo -n ufw delete allow {rule}")
    };
    match run_in_vm(paths, vm, &cmd) {
        Ok(true) if allow => {
            println!("  firewall: port {port} opened for this machine ({HOST_IN_GUEST}) only")
        }
        Ok(true) => println!("  firewall: port {port} closed again"),
        _ => println!(
            "  warning: could not update the VM's firewall; run in the VM:\n  sudo ufw {} {rule}",
            if allow { "allow" } else { "delete allow" }
        ),
    }
}
