use std::{
    fmt,
    io::{Error, Read},
    path::PathBuf,
    time::Duration,
};

use log::info;
use mio::event::Source;
use mio_serial::{self, SerialPortInfo, SerialPortType, SerialStream};
use num_enum::{IntoPrimitive, TryFromPrimitive};

pub fn find_device() -> Option<SerialPortInfo> {
    let ports = mio_serial::available_ports().expect("No ports found!");
    ports.into_iter().find(|p| match &p.port_type {
        SerialPortType::UsbPort(usb_port_info)
            if usb_port_info.vid == 0x2e3c && usb_port_info.pid == 0x5740 =>
        {
            info!("Found valid TourBox serial device: {0}", p.port_name);
            true
        }
        _ => false,
    })
}

pub fn open(path: Option<PathBuf>) -> SerialStream {
    let device = match path {
        Some(p) => p.to_string_lossy().to_string(),
        None => find_device().expect("No device found").port_name,
    };
    let builder = mio_serial::new(device, 115_200).timeout(Duration::from_millis(10));
    SerialStream::open(&builder).unwrap()
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, IntoPrimitive, TryFromPrimitive, Hash)]
#[repr(u8)]
pub enum Code {
    Tall = 0x00,
    Side = 0x01,
    Top = 0x02,
    Short = 0x03,

    TallDbl = 0x18,
    SideDbl = 0x21,
    ShortDbl = 0x1c,
    TopDbl = 0x1f,

    SideTop = 0x20,
    SideTall = 0x1b,
    SideShort = 0x1e,
    TopTall = 0x19,
    TopShort = 0x1d,
    TallShort = 0x1a,

    Tour = 0x2a,

    Up = 0x10,
    Down = 0x11,
    Left = 0x12,
    Right = 0x13,

    SideUp = 0x14,
    SideDown = 0x15,
    SideLeft = 0x16,
    SideRight = 0x17,

    TopUp = 0x2b,
    TopDown = 0x2c,
    TopLeft = 0x2d,
    TopRight = 0x2e,

    C1 = 0x22,
    C2 = 0x23,

    TallC1 = 0x24,
    TallC2 = 0x25,

    ShortC1 = 0x39,
    ShortC2 = 0x3a,

    KnobButton = 0x37,
    Knob = 0x04,
    TallKnob = 0x05,
    ShortKnob = 0x06,
    TopKnob = 0x07,
    SideKnob = 0x08,

    ScrollButton = 0x0a,
    Scroll = 0x09,
    TallScroll = 0x0b,
    ShortScroll = 0x0c,
    TopScroll = 0x0d,
    SideScroll = 0x0e,

    DialButton = 0x38,
    Dial = 0x0f,

    /// Dummy value used in `Engine::held_actions` for ongoing macro invocations
    Macro = 0xff,
}

impl fmt::Display for Code {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{:?}", self)
    }
}

const CODE_MASK: u8 = !0xC0;
const REVERSE: u8 = 0x40;
const RELEASE: u8 = 0x80;

#[derive(Debug)]
pub struct Input {
    pub code: Code,
    pub reverse: bool,
    pub release: bool,
}

impl fmt::Display for Input {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let kind = if self.release { " (rel)" } else { "" };
        let rev = if self.reverse { " (rev)" } else { "" };
        write!(f, "{}{}{}", self.code, kind, rev)
    }
}

impl From<u8> for Input {
    fn from(value: u8) -> Self {
        let release = value & RELEASE != 0;
        let mut reverse = value & REVERSE != 0;
        let code = Code::try_from(value & CODE_MASK).expect("Invalid code");

        // A horrific workaround for the fact that one scroll input comes through reversed
        if code == Code::Knob {
            reverse = !reverse;
        }

        Input {
            code,
            reverse,
            release,
        }
    }
}

pub struct SerialEventStream {
    serial: SerialStream,
}

impl SerialEventStream {
    pub fn new(serial: SerialStream) -> SerialEventStream {
        SerialEventStream { serial: serial }
    }
}

impl Iterator for SerialEventStream {
    type Item = Result<Input, Error>;

    fn next(&mut self) -> Option<Self::Item> {
        let mut buf = [0u8; 1];
        match self.serial.read_exact(&mut buf) {
            Ok(_) => Some(Ok(buf[0].into())),
            Err(err) => Some(Result::Err(err)),
        }
    }
}

impl Source for SerialEventStream {
    fn register(
        &mut self,
        registry: &mio::Registry,
        token: mio::Token,
        interests: mio::Interest,
    ) -> std::io::Result<()> {
        self.serial.register(registry, token, interests)
    }

    fn reregister(
        &mut self,
        registry: &mio::Registry,
        token: mio::Token,
        interests: mio::Interest,
    ) -> std::io::Result<()> {
        self.serial.reregister(registry, token, interests)
    }

    fn deregister(&mut self, registry: &mio::Registry) -> std::io::Result<()> {
        self.serial.deregister(registry)
    }
}
