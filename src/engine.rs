use std::io::{self, Bytes, Read};
use std::time::Duration;

use mio::{Events, Interest, Poll, Token};
use mio_serial::SerialStream;

use crate::config::Config;
use crate::serial::{self, SerialEventStream};

const ENGINE: Token = Token(0);

/// Central engine managing peripheral state
pub struct Engine {
    /// MIO Poll
    poll: Poll,
    /// MIO Event queue
    events: Events,
    /// Serial connection to the device
    serial: SerialEventStream,
    /// Currently loaded configuration
    config: Config,
    /// Held buttons
    held_buttons: Vec<u8>,
}

impl Engine {
    pub fn new() -> Engine {
        let mut serial = serial::open();

        let poll = Poll::new().expect("MIO poll failed to start");
        poll.registry()
            .register(&mut serial, ENGINE, Interest::READABLE)
            .expect("MIO register failed");
        let events = Events::with_capacity(128);

        Engine {
            poll: poll,
            events: events,
            serial: serial.bytes(),
            config: Config::new(),
            held_buttons: Vec::new(),
        }
    }

    pub fn begin(&mut self) {
        loop {
            self.poll
                .poll(&mut self.events, Some(Duration::from_millis(100)))
                .unwrap();

            for event in self.events.iter() {
                match event.token() {
                    ENGINE => {
                        // let bytes = self.serial.bytes();
                        for byte in &mut self.serial {
                            match byte {
                                Ok(val) if val < serial::RELEASE => {
                                    log::warn!("Read {0:#04x}", val);
                                }
                                Ok(_val) => {}
                                Err(ref err) if would_block(err) => break,
                                Err(err) => panic!("{}", err),
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}

fn would_block(err: &io::Error) -> bool {
    err.kind() == io::ErrorKind::WouldBlock
}
