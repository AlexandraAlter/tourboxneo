use std::{
    io::{Bytes, Read},
    time::Duration,
};

use log::info;
use mio_serial::{self, SerialPortInfo, SerialPortType, SerialStream};
use num_enum::TryFromPrimitive;

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

pub fn open() -> SerialStream {
    let device = find_device().expect("No device found");
    let config = mio_serial::new(device.port_name, 115_200).timeout(Duration::from_millis(10));
    SerialStream::open(&config).unwrap()
}

#[derive(Debug, Eq, PartialEq, TryFromPrimitive)]
#[repr(u8)]
pub enum Button {
    Tall = 0x00,
    Side = 0x01,
    Top = 0x02,
    Short = 0x03,

    TallDbl = 0x18,
    SideDbl = 0x21,
    ShortDbl = 0x1c,
    TopDbl = 0x1f,

    TallTop = 0x19,
    TallShort = 0x1a,
    TallSide = 0x1b,
    ShortTop = 0x1d,
    ShortSide = 0x1e,
    TopSide = 0x20,

    Tour = 0x2a,

    Up = 0x10,
    Down = 0x11,
    Left = 0x12,
    Right = 0x13,

    UpSide = 0x14,
    DownSide = 0x15,
    LeftSide = 0x16,
    RightSide = 0x17,

    UpTop = 0x2b,
    DownTop = 0x2c,
    LeftTop = 0x2d,
    RightTop = 0x2e,

    C1 = 0x22,
    C2 = 0x23,

    C1Tall = 0x24,
    C2Tall = 0x25,

    C1Short = 0x39,
    C2Short = 0x3a,

    Knob = 0x37,
    Scroll = 0x0a,
    Dial = 0x38,

    KnobS = 0x04,
    KnobSTall = 0x05,
    KnobSShort = 0x06,
    KnobSTop = 0x07,
    KnobSSide = 0x08,

    ScrollS = 0x09,
    ScrollSTall = 0x0b,
    ScrollSShort = 0x0c,
    ScrollSTop = 0x0d,
    ScrollSSide = 0x0e,

    DialS = 0x0f,
}

pub const REVERSE: u8 = 0x40;
pub const RELEASE: u8 = 0x80;

pub struct Event {
    button: Button,
    reverse: bool,
    release: bool,
}

pub struct SerialEventStream {
    serial: Bytes<SerialStream>,
}

impl SerialEventStream {
    pub fn new() -> SerialEventStream {
        let serial_stream = open();
        SerialEventStream {
            serial: serial_stream.bytes(),
        }
    }
}

impl Iterator for SerialEventStream {
    type Item = Event;

    fn next(&mut self) -> Option<Self::Item> {
        let byte = self.serial.next();
        todo!()
    }
}
