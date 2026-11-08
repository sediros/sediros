//! sedir - build SedirOS images and run them as throwaway QEMU VMs.

mod builder;
mod cli;
mod paths;
mod pull;
mod purge;
mod ssh;
mod vm;

use anyhow::Result;
use clap::Parser;

use crate::cli::{Cli, Command};
use crate::paths::Paths;

fn main() {
    let cli = Cli::parse();
    if let Err(err) = run(cli) {
        eprintln!("error: {err:#}");
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> Result<()> {
    let paths = Paths::resolve(cli.project, cli.home)?;
    match cli.command {
        Command::Pull(args) => pull::run(&paths, &args),
        Command::Build(args) => builder::run(&paths, &args),
        Command::Launch(args) => vm::launch(&paths, &args),
        Command::Shell(args) => ssh::shell(&paths, &args),
        Command::Transfer(args) => ssh::transfer(&paths, &args),
        Command::Forward(args) => vm::forward_cmd(&paths, &args),
        Command::Snapshot(args) => vm::snapshot_cmd(&paths, &args),
        Command::Restore(args) => vm::restore_cmd(&paths, &args),
        Command::List => vm::list(&paths),
        Command::Stop(args) => vm::stop_cmd(&paths, &args),
        Command::Purge(args) => purge::run(&paths, &args),
    }
}
