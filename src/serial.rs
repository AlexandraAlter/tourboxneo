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
    let ports = mio_serial::available_ports().expect("should find ports");
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
        None => find_device().expect("should find device").port_name,
    };
    let builder = mio_serial::new(device, 115_200).timeout(Duration::from_millis(10));
    SerialStream::open(&builder).unwrap()
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, IntoPrimitive, TryFromPrimitive, Hash,
)]
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

    /// Dummy value used in `Engine` for ongoing macro invocations
    Macro = 0xfd,
    /// Dummy value used in `Engine` for ongoing menu invocations
    Menu = 0xfe,
    /// Dummy value used in `Engine` for invalid values
    None = 0xff,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum CodeCategory {
    Prime,
    C,
    Arrow,
    Scroll,
    ScrollPress,
    Combo,
    Other,
    Dummy,
}

impl Code {
    /// what category is this code in?
    pub fn category(self: Code) -> CodeCategory {
        match self {
            Code::Tall | Code::Side | Code::Top | Code::Short => CodeCategory::Prime,
            Code::Tour => CodeCategory::Other,
            Code::Up | Code::Down | Code::Left | Code::Right => CodeCategory::Arrow,
            Code::C1 | Code::C2 => CodeCategory::C,
            Code::Knob | Code::Scroll | Code::Dial => CodeCategory::Scroll,
            Code::KnobButton | Code::ScrollButton | Code::DialButton => CodeCategory::ScrollPress,
            Code::Macro | Code::Menu | Code::None => CodeCategory::Dummy,
            _ => CodeCategory::Combo,
        }
    }

    /// is this a basic one-key code?
    pub fn is_basic(self: Code) -> bool {
        match self {
            Code::Tall | Code::Side | Code::Top | Code::Short => true,
            Code::Tour => true,
            Code::Up | Code::Down | Code::Left | Code::Right => true,
            Code::C1 | Code::C2 => true,
            Code::KnobButton | Code::Knob => true,
            Code::ScrollButton | Code::Scroll => true,
            Code::DialButton | Code::Dial => true,
            _ => false,
        }
    }

    /// if this is a double-press, return the single-press version
    pub fn dedup(self: Code) -> Code {
        match self {
            Code::TallDbl => Code::Tall,
            Code::SideDbl => Code::Side,
            Code::TopDbl => Code::Top,
            Code::ShortDbl => Code::Short,
            _ => self,
        }
    }

    /// for any of the scrolling codes, report one of the three base scroll codes
    pub fn to_scroll_trio(self: Code) -> Option<Code> {
        match self {
            Code::Knob => Some(Code::Knob),
            Code::TallKnob => Some(Code::Knob),
            Code::ShortKnob => Some(Code::Knob),
            Code::TopKnob => Some(Code::Knob),
            Code::SideKnob => Some(Code::Knob),

            Code::Scroll => Some(Code::Scroll),
            Code::TallScroll => Some(Code::Scroll),
            Code::ShortScroll => Some(Code::Scroll),
            Code::TopScroll => Some(Code::Scroll),
            Code::SideScroll => Some(Code::Scroll),

            Code::Dial => Some(Code::Dial),

            _ => None,
        }
    }

    pub fn to_fallback(self: Code) -> Option<(Code, Code)> {
        match self {
            Code::TallDbl => Some((Code::Tall, Code::None)),
            Code::SideDbl => Some((Code::Side, Code::None)),
            Code::TopDbl => Some((Code::Top, Code::None)),
            Code::ShortDbl => Some((Code::Short, Code::None)),

            Code::SideTop => Some((Code::Side, Code::Top)),
            Code::SideTall => Some((Code::Side, Code::Tall)),
            Code::SideShort => Some((Code::Side, Code::Short)),
            Code::TopTall => Some((Code::Top, Code::Tall)),
            Code::TopShort => Some((Code::Top, Code::Short)),
            Code::TallShort => Some((Code::Tall, Code::Short)),

            Code::SideUp => Some((Code::Side, Code::Up)),
            Code::SideDown => Some((Code::Side, Code::Down)),
            Code::SideLeft => Some((Code::Side, Code::Left)),
            Code::SideRight => Some((Code::Side, Code::Right)),

            Code::TopUp => Some((Code::Top, Code::Up)),
            Code::TopDown => Some((Code::Top, Code::Down)),
            Code::TopLeft => Some((Code::Top, Code::Left)),
            Code::TopRight => Some((Code::Top, Code::Right)),

            Code::TallC1 => Some((Code::Tall, Code::C1)),
            Code::TallC2 => Some((Code::Tall, Code::C2)),

            Code::ShortC1 => Some((Code::Short, Code::C1)),
            Code::ShortC2 => Some((Code::Short, Code::C2)),

            Code::TallKnob => Some((Code::Tall, Code::Knob)),
            Code::ShortKnob => Some((Code::Short, Code::Knob)),
            Code::TopKnob => Some((Code::Top, Code::Knob)),
            Code::SideKnob => Some((Code::Side, Code::Knob)),

            Code::TallScroll => Some((Code::Tall, Code::Scroll)),
            Code::ShortScroll => Some((Code::Short, Code::Scroll)),
            Code::TopScroll => Some((Code::Top, Code::Scroll)),
            Code::SideScroll => Some((Code::Side, Code::Scroll)),

            _ => return None,
        }
    }

    /// is this a combination of codes that matches a builtin combo?
    /// assumes `codes` is sorted
    pub fn is_builtin_combo(codes: &Vec<Code>) -> bool {
        if !codes.is_sorted() {
            panic!("unsorted list of codes")
        }
        match codes.len() {
            0 => false,
            1 => true,
            2 => {
                let k1 = *codes.get(0).unwrap();
                let k2 = *codes.get(1).unwrap();

                let two_primes =
                    k1.category() == CodeCategory::Prime && k2.category() == CodeCategory::Prime;
                let prime_scroll = k1.category() == CodeCategory::Prime
                    && (k2 == Code::Knob || k2 == Code::Scroll);
                let mod_c =
                    (k1 == Code::Tall || k1 == Code::Short) && k2.category() == CodeCategory::C;
                let mod_arrow =
                    (k1 == Code::Top || k1 == Code::Side) && k2.category() == CodeCategory::Arrow;
                two_primes || prime_scroll || mod_c || mod_arrow
            }
            _ => false,
        }
    }

    /// is this a combination of codes that cannot be produced?
    /// assumes `codes` is sorted
    pub fn is_impossible_combo(codes: &Vec<Code>) -> bool {
        if !codes.is_sorted() {
            panic!("unsorted list of codes")
        }
        match codes.len() {
            0 => true,
            1 => false,
            2 => {
                let k1 = *codes.get(0).unwrap();
                let k2 = *codes.get(1).unwrap();

                let eq = k1 == k2;
                let two_scrolls =
                    k1.category() == CodeCategory::Scroll && k2.category() == CodeCategory::Scroll;
                let scroll_and_press = k1.to_scroll_trio() == Some(Code::Knob)
                    && k2 == Code::KnobButton
                    || k1.to_scroll_trio() == Some(Code::Scroll) && k2 == Code::ScrollButton
                    || k2.to_scroll_trio() == Some(Code::Scroll) && k1 == Code::ScrollButton // this one can be backwards
                    || k1.to_scroll_trio() == Some(Code::Dial) && k2 == Code::DialButton;

                eq || two_scrolls || scroll_and_press
            }
            _ => false,
        }
    }
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
        let code = Code::try_from(value & CODE_MASK).expect("code should be valid");

        // A horrific workaround for the fact that one scroll input comes through reversed
        if code == Code::Knob {
            reverse = !reverse;
        }

        Input { code, reverse, release }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_builtins_correct() {
        assert!(Code::is_builtin_combo(&vec![Code::Tall, Code::Short]));
        assert!(Code::is_builtin_combo(&vec![Code::Side, Code::Top]));
        assert!(Code::is_builtin_combo(&vec![Code::Top, Code::Knob]));
        assert!(Code::is_builtin_combo(&vec![Code::Top, Code::Scroll]));
        assert!(!Code::is_builtin_combo(&vec![Code::Top, Code::Dial])); // prime+dial isn't builtin
        assert!(Code::is_builtin_combo(&vec![Code::Tall, Code::C2]));
        assert!(!Code::is_builtin_combo(&vec![Code::Top, Code::C1])); // top/side+c isn't builtin
        assert!(Code::is_builtin_combo(&vec![Code::Side, Code::Up]));
        assert!(!Code::is_builtin_combo(&vec![Code::Short, Code::Down])); // tall/short+arrow isn't builtin
    }

    #[test]
    fn code_impossibilities_correct() {
        assert!(Code::is_impossible_combo(&vec![Code::Tall, Code::Tall])); // duplicate code
        assert!(Code::is_impossible_combo(&vec![Code::Knob, Code::Dial])); // two scrolls
        assert!(Code::is_impossible_combo(&vec![Code::Knob, Code::Scroll])); // two scrolls
        assert!(Code::is_impossible_combo(&vec![Code::Knob, Code::KnobButton])); // matching scroll and press
        assert!(Code::is_impossible_combo(&vec![Code::TopKnob, Code::KnobButton])); // matching scroll and press
        assert!(Code::is_impossible_combo(&vec![Code::Scroll, Code::ScrollButton])); // matching scroll and press
        assert!(Code::is_impossible_combo(&vec![Code::ScrollButton, Code::TallScroll,])); // matching scroll and press
        assert!(Code::is_impossible_combo(&vec![Code::Dial, Code::DialButton])); // matching scroll and press
        assert!(!Code::is_impossible_combo(&vec![Code::Knob, Code::ScrollButton])); // non-matching scroll and press is fine
    }

    #[test]
    fn input_from_u8() {
        // TODO
    }
}
