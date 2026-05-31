mod actions;
mod config;
mod engine;
mod error;
mod output;
mod serial;
mod timer;

use std::{path::PathBuf, time::Duration};

use clap::Parser;
use mio::{Events, Interest, Poll, Token};

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

const SERIAL: Token = Token(0);
const REPEAT: Token = Token(1);

fn main() {
    let args = Args::parse();

    stderrlog::new()
        .verbosity(args.verbose as usize)
        .quiet(args.quiet)
        .init()
        .unwrap();

    println!("Startup complete at verbosity {}", args.verbose);

    let mut engine = Engine::new(args.device);

    args.config.map(|path| {
        let name = engine.load_config(path);
        engine.set_config(&name)
    });

    let mut poll = Poll::new().expect("MIO poll failed to start");
    poll.registry()
        .register(engine.get_serial(), SERIAL, Interest::READABLE)
        .expect("MIO register failed");
    poll.registry()
        .register(engine.get_timer(), REPEAT, Interest::READABLE)
        .unwrap();

    let mut events = Events::with_capacity(128);

    loop {
        poll.poll(&mut events, Some(Duration::from_millis(100)))
            .unwrap();

        for event in events.iter() {
            match event.token() {
                SERIAL => engine.handle_serial(),
                REPEAT => engine.handle_repeat(),
                _ => {}
            }
        }
    }
}
