mod actions;
mod config;
mod engine;
mod menu;
mod notify;
mod output;
mod serial;
mod timer;

use std::path::PathBuf;

use clap::Parser;
use log::info;

use crate::engine::Engine;

/// TourBox NEO CLI arguments
#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    /// Config file location
    #[arg(short, long)]
    config: Option<PathBuf>,

    /// Device serial file
    #[arg(short, long)]
    device: Option<PathBuf>,

    // TODO: unused
    /// PID file
    #[arg(short, long)]
    pid_file: Option<PathBuf>,

    // TODO: unused
    /// daemon mode flag
    #[arg(long)]
    daemon: bool,

    /// verbosity flag
    #[arg(short, long, action = clap::ArgAction::Count)]
    verbose: u8,

    /// quiet flag
    #[arg(short, long)]
    quiet: bool,
}

fn main() {
    let args = Args::parse();

    stderrlog::new().verbosity(args.verbose as usize).quiet(args.quiet).init().unwrap();

    info!("Startup complete at verbosity {}", args.verbose);

    let mut engine = Engine::new(args.device);

    args.config.map(|path| {
        let name = engine.load_config(path);
        engine.set_config(&name)
    });

    engine.run();
}
