mod config;
mod engine;
mod output;
mod serial;

use std::path::PathBuf;

use clap::Parser;

use crate::{
    engine::Engine,
    output::{Modifiers, OutputDriver},
};

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

    /// PID file
    #[arg(short, long)]
    pid_file: Option<PathBuf>,

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

    stderrlog::new()
        .verbosity(args.verbose as usize)
        .quiet(args.quiet)
        .init()
        .unwrap();

    println!("Startup complete at verbosity {}", args.verbose);

    let mut engine = Engine::new();
    engine.begin();

    // let mut kb = OutputDriver::new();
    // kb.append_mod(Modifiers::SHIFT);
    // kb.key_press(evdev::KeyCode::KEY_X);
    // kb.key_release(evdev::KeyCode::KEY_X);
    // kb.remove_mod(Modifiers::SHIFT);
    // kb.test();
}
