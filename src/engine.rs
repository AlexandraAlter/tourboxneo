use std::io;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use log::info;

use crate::actions::Action;
use crate::config::{Bind, Config, ConfigManager, Rate};
use crate::output::OutputDriver;
use crate::serial::{self, Code, Input, SerialEventStream};
use crate::timer::{ClockId, TimerFd};

pub struct Tickers {
    knob: u8,
    scroll: u8,
    dial: u8,
}

impl Tickers {
    pub fn new() -> Tickers {
        Tickers {
            knob: 0,
            scroll: 0,
            dial: 0,
        }
    }
}

/// Central engine managing peripheral state
pub struct Engine {
    /// Serial connection to the device
    serial: SerialEventStream,
    /// Timer for repeating keys
    timer: TimerFd,
    /// Output for Wayland
    output: OutputDriver,
    /// Configuration management
    config_manager: ConfigManager,
    /// Track current config
    config: Rc<Config>,
    /// Held buttons
    held_buttons: Vec<Code>,
    /// Held binds, for repeating events
    held_binds: Vec<Bind>,
    /// Tickers for each dial
    ticks: Tickers,
}

impl Engine {
    pub fn new(device_path: Option<PathBuf>) -> Engine {
        let timer = TimerFd::new(ClockId::Monotonic).expect("Failed to build timer");
        let serial = SerialEventStream::new(serial::open(device_path));

        let output = OutputDriver::new();

        let config_manager = ConfigManager::new();
        let config = config_manager.get_default_config();

        Engine {
            serial: serial,
            timer: timer,
            output: output,
            config_manager: config_manager,
            config: config,
            held_buttons: Vec::new(),
            held_binds: Vec::new(),
            ticks: Tickers::new(),
        }
    }

    pub fn load_config(&mut self, path: PathBuf) -> String {
        self.config_manager.load_config(path)
    }

    pub fn set_config(&mut self, name: &str) {
        let config = self
            .config_manager
            .get_config(name)
            .expect("Config name should be valid");
        self.config = config;
    }

    /// Given two codes, returns which one is not currently being held
    /// Used to calculate fallbacks to more complicated keycodes
    fn missing_code(&self, input_a: Code, input_b: Code) -> Code {
        if self.held_buttons.contains(&input_a) {
            input_b
        } else {
            input_a
        }
    }

    fn code_to_scroll(&self, input: Code) -> Option<Code> {
        match input {
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

    /// Given a code without a matching bind in the current config, return an appropriate fallback bind
    fn fallback_code_to_bind(&self, input: Code) -> Option<Rc<Bind>> {
        let code = match input {
            Code::TallDbl => Code::Tall,
            Code::SideDbl => Code::Side,
            Code::TopDbl => Code::Top,
            Code::ShortDbl => Code::Short,

            Code::SideTop => self.missing_code(Code::Side, Code::Top),
            Code::SideTall => self.missing_code(Code::Side, Code::Tall),
            Code::SideShort => self.missing_code(Code::Side, Code::Short),
            Code::TopTall => self.missing_code(Code::Top, Code::Tall),
            Code::TopShort => self.missing_code(Code::Top, Code::Short),
            Code::TallShort => self.missing_code(Code::Tall, Code::Short),

            Code::SideUp => self.missing_code(Code::Side, Code::Up),
            Code::SideDown => self.missing_code(Code::Side, Code::Down),
            Code::SideLeft => self.missing_code(Code::Side, Code::Left),
            Code::SideRight => self.missing_code(Code::Side, Code::Right),

            Code::TopUp => self.missing_code(Code::Top, Code::Up),
            Code::TopDown => self.missing_code(Code::Top, Code::Down),
            Code::TopLeft => self.missing_code(Code::Top, Code::Left),
            Code::TopRight => self.missing_code(Code::Top, Code::Right),

            Code::TallC1 => self.missing_code(Code::Tall, Code::C1),
            Code::TallC2 => self.missing_code(Code::Tall, Code::C2),

            Code::ShortC1 => self.missing_code(Code::Short, Code::C1),
            Code::ShortC2 => self.missing_code(Code::Short, Code::C2),

            Code::TallKnob => self.missing_code(Code::Tall, Code::Knob),
            Code::ShortKnob => self.missing_code(Code::Short, Code::Knob),
            Code::TopKnob => self.missing_code(Code::Top, Code::Knob),
            Code::SideKnob => self.missing_code(Code::Side, Code::Knob),

            Code::TallScroll => self.missing_code(Code::Tall, Code::Scroll),
            Code::ShortScroll => self.missing_code(Code::Short, Code::Scroll),
            Code::TopScroll => self.missing_code(Code::Top, Code::Scroll),
            Code::SideScroll => self.missing_code(Code::Side, Code::Scroll),

            _ => return None,
        };
        self.code_to_bind(code)
    }

    fn code_to_bind(&self, input: Code) -> Option<Rc<Bind>> {
        let config = &self.config;
        match input {
            Code::Tall => config.prime_ref().and_then(|v| v.tall.clone()),
            Code::Side => config.prime_ref().and_then(|v| v.side.clone()),
            Code::Top => config.prime_ref().and_then(|v| v.top.clone()),
            Code::Short => config.prime_ref().and_then(|v| v.short.clone()),
            Code::TallDbl => config
                .prime_ref()
                .and_then(|v| v.tall_x2.clone())
                .or_else(|| self.fallback_code_to_bind(input)),
            Code::SideDbl => config
                .prime_ref()
                .and_then(|v| v.side_x2.clone())
                .or_else(|| self.fallback_code_to_bind(input)),
            Code::ShortDbl => config
                .prime_ref()
                .and_then(|v| v.short_x2.clone())
                .or_else(|| self.fallback_code_to_bind(input)),
            Code::TopDbl => config
                .prime_ref()
                .and_then(|v| v.top_x2.clone())
                .or_else(|| self.fallback_code_to_bind(input)),

            Code::SideTop => config
                .prime_ref()
                .and_then(|v| v.side_top.clone())
                .or_else(|| self.fallback_code_to_bind(input)),
            Code::SideTall => config
                .prime_ref()
                .and_then(|v| v.side_tall.clone())
                .or_else(|| self.fallback_code_to_bind(input)),
            Code::SideShort => config
                .prime_ref()
                .and_then(|v| v.side_short.clone())
                .or_else(|| self.fallback_code_to_bind(input)),
            Code::TopTall => config
                .prime_ref()
                .and_then(|v| v.top_tall.clone())
                .or_else(|| self.fallback_code_to_bind(input)),
            Code::TopShort => config
                .prime_ref()
                .and_then(|v| v.top_short.clone())
                .or_else(|| self.fallback_code_to_bind(input)),
            Code::TallShort => config
                .prime_ref()
                .and_then(|v| v.tall_short.clone())
                .or_else(|| self.fallback_code_to_bind(input)),

            Code::Tour => config.kit_ref().and_then(|v| v.tour.clone()),

            Code::Up => config
                .kit_ref()
                .and_then(|v| v.dpad_ref().and_then(|v| v.up.clone())),
            Code::Down => config
                .kit_ref()
                .and_then(|v| v.dpad_ref().and_then(|v| v.down.clone())),
            Code::Left => config
                .kit_ref()
                .and_then(|v| v.dpad_ref().and_then(|v| v.left.clone())),
            Code::Right => config
                .kit_ref()
                .and_then(|v| v.dpad_ref().and_then(|v| v.right.clone())),

            Code::SideUp => config
                .kit_ref()
                .and_then(|v| v.side_dpad_ref().and_then(|v| v.up.clone()))
                .or_else(|| self.fallback_code_to_bind(input)),
            Code::SideDown => config
                .kit_ref()
                .and_then(|v| v.side_dpad_ref().and_then(|v| v.down.clone()))
                .or_else(|| self.fallback_code_to_bind(input)),
            Code::SideLeft => config
                .kit_ref()
                .and_then(|v| v.side_dpad_ref().and_then(|v| v.left.clone()))
                .or_else(|| self.fallback_code_to_bind(input)),
            Code::SideRight => config
                .kit_ref()
                .and_then(|v| v.side_dpad_ref().and_then(|v| v.right.clone()))
                .or_else(|| self.fallback_code_to_bind(input)),

            Code::TopUp => config
                .kit_ref()
                .and_then(|v| v.top_dpad_ref().and_then(|v| v.up.clone()))
                .or_else(|| self.fallback_code_to_bind(input)),
            Code::TopDown => config
                .kit_ref()
                .and_then(|v| v.top_dpad_ref().and_then(|v| v.down.clone()))
                .or_else(|| self.fallback_code_to_bind(input)),
            Code::TopLeft => config
                .kit_ref()
                .and_then(|v| v.top_dpad_ref().and_then(|v| v.left.clone()))
                .or_else(|| self.fallback_code_to_bind(input)),
            Code::TopRight => config
                .kit_ref()
                .and_then(|v| v.top_dpad_ref().and_then(|v| v.right.clone()))
                .or_else(|| self.fallback_code_to_bind(input)),

            Code::C1 => config.kit_ref().and_then(|v| v.c1.clone()),
            Code::C2 => config.kit_ref().and_then(|v| v.c2.clone()),

            Code::TallC1 => config
                .kit_ref()
                .and_then(|v| v.tall_c1.clone())
                .or_else(|| self.fallback_code_to_bind(input)),
            Code::TallC2 => config
                .kit_ref()
                .and_then(|v| v.tall_c2.clone())
                .or_else(|| self.fallback_code_to_bind(input)),

            Code::ShortC1 => config
                .kit_ref()
                .and_then(|v| v.short_c1.clone())
                .or_else(|| self.fallback_code_to_bind(input)),
            Code::ShortC2 => config
                .kit_ref()
                .and_then(|v| v.short_c2.clone())
                .or_else(|| self.fallback_code_to_bind(input)),

            Code::KnobButton => config.knob_ref().and_then(|v| v.press.clone()),
            Code::Knob => config.knob_ref().and_then(|v| v.turn.clone()),
            Code::TallKnob => config
                .knob_ref()
                .and_then(|v| v.tall_turn.clone())
                .or_else(|| self.fallback_code_to_bind(input)),
            Code::ShortKnob => config
                .knob_ref()
                .and_then(|v| v.short_turn.clone())
                .or_else(|| self.fallback_code_to_bind(input)),
            Code::TopKnob => config
                .knob_ref()
                .and_then(|v| v.top_turn.clone())
                .or_else(|| self.fallback_code_to_bind(input)),
            Code::SideKnob => config
                .knob_ref()
                .and_then(|v| v.side_turn.clone())
                .or_else(|| self.fallback_code_to_bind(input)),

            Code::ScrollButton => config.scroll_ref().and_then(|v| v.press.clone()),
            Code::Scroll => config.scroll_ref().and_then(|v| v.turn.clone()),
            Code::TallScroll => config
                .scroll_ref()
                .and_then(|v| v.tall_turn.clone())
                .or_else(|| self.fallback_code_to_bind(input)),
            Code::ShortScroll => config
                .scroll_ref()
                .and_then(|v| v.short_turn.clone())
                .or_else(|| self.fallback_code_to_bind(input)),
            Code::TopScroll => config
                .scroll_ref()
                .and_then(|v| v.top_turn.clone())
                .or_else(|| self.fallback_code_to_bind(input)),
            Code::SideScroll => config
                .scroll_ref()
                .and_then(|v| v.side_turn.clone())
                .or_else(|| self.fallback_code_to_bind(input)),

            Code::DialButton => config.dial_ref().and_then(|v| v.press.clone()),
            Code::Dial => config.dial_ref().and_then(|v| v.turn.clone()),
        }
    }

    pub fn get_serial(&mut self) -> &mut SerialEventStream {
        &mut self.serial
    }

    pub fn get_timer(&mut self) -> &mut TimerFd {
        &mut self.timer
    }

    fn action_down(&mut self, action: &Action) {
        match action {
            Action::None => {}
            Action::Mod(modifiers, keycode) => {
                keycode.map(|k| self.output.key_press(k));
                self.output.mod_append(*modifiers)
            }
            Action::Key(key_code, modifiers) => {
                modifiers.map(|m| self.output.mod_append(m));
                self.output.key_press(*key_code);
            }
            Action::PtrMotion(dx, dy) => {
                self.output.ptr_motion(*dx, *dy);
                self.output.ptr_frame();
            }
            Action::PtrMotionAbs(x, y, x_extent, y_extent) => {
                self.output
                    .ptr_motion_absolute(*x, *y, *x_extent, *y_extent);
                self.output.ptr_frame();
            }
            Action::PtrButton(button) => {
                self.output.ptr_button(*button, false);
                self.output.ptr_frame();
            }
            Action::PtrAxis(axis, value) => {
                self.output.ptr_axis(*axis, *value);
                self.output.ptr_frame();
            }
            Action::PtrAxisDiscrete(axis, value, discrete) => {
                self.output.ptr_axis_discrete(*axis, *value, *discrete);
                self.output.ptr_axis_stop(*axis);
                self.output.ptr_frame();
            }
            Action::Macro(_, actions) => {
                for a in actions {
                    self.action_down(a);
                    self.action_up(a);
                }
            }
            Action::Menu(_, _items) => todo!(),
        };
    }

    fn action_up(&mut self, action: &Action) {
        match action {
            Action::None => {}
            Action::Mod(modifiers, keycode) => {
                keycode.map(|k| self.output.key_press(k));
                self.output.mod_remove(*modifiers)
            }
            Action::Key(key_code, modifiers) => {
                self.output.key_release(*key_code);
                modifiers.map(|m| self.output.mod_remove(m));
            }
            Action::PtrMotion(_, _) => {}
            Action::PtrMotionAbs(_, _, _, _) => {}
            Action::PtrButton(button) => {
                self.output.ptr_button(*button, true);
                self.output.ptr_frame();
            }
            Action::PtrAxis(_, _) => {}
            Action::PtrAxisDiscrete(_, _, _) => {}
            Action::Macro(_, _actions) => {}
            Action::Menu(_, _items) => todo!(),
        };
    }

    fn handle_input(&mut self, input: Input) {
        info!("Input {:?}", input);
        let bind = self.code_to_bind(input.code);
        if bind.is_none() {
            return;
        }
        match bind.unwrap().as_ref() {
            Bind::Button { cmd, up, rep } => {
                if *up && input.release {
                    self.action_down(cmd);
                    self.action_up(cmd);
                } else if input.release {
                    self.action_up(cmd);
                    if *rep {
                        self.timer.disarm().unwrap();
                    }
                } else {
                    self.action_down(cmd);
                    if *rep {
                        self.timer.set_timeout(&Duration::from_millis(100)).unwrap();
                    }
                }
            }
            Bind::Scroll { fwd, bak, rate } => {
                let scroll_code = self.code_to_scroll(input.code);
                let counter = match scroll_code {
                    Some(Code::Knob) => self.ticks.knob.wrapping_add(1),
                    Some(Code::Scroll) => self.ticks.scroll.wrapping_add(1),
                    Some(Code::Dial) => self.ticks.dial.wrapping_add(1),
                    _ => panic!(),
                };
                let modulo = match rate {
                    Rate::Normal => 1,
                    Rate::Slow => 2,
                    Rate::Slower => 3,
                };
                if counter % modulo == 0 {
                    let act = if input.reverse { bak } else { fwd };
                    if !input.release {
                        self.action_down(act);
                        self.action_up(act);
                    }
                }
            }
        }
    }

    pub fn handle_serial(&mut self) {
        loop {
            match self.serial.next() {
                Some(Ok(input)) => self.handle_input(input),
                Some(Err(ref err)) if would_block(err) => break,
                Some(Err(err)) => panic!("{}", err),
                None => break,
            }
        }
    }

    pub fn handle_repeat(&mut self) {
        info!("Timer tick");
        self.timer.set_timeout(&Duration::from_millis(10)).unwrap();
        let timeout_num = self.timer.read().unwrap();
        assert!(timeout_num == 1);
    }
}

fn would_block(err: &io::Error) -> bool {
    err.kind() == io::ErrorKind::WouldBlock
}
