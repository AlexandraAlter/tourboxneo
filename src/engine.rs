use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::iter;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use log::{info, warn};
use mio::{Events, Interest, Poll, Token};

use crate::actions::{Action, Modifiers};
use crate::config::{Bind, Config, ConfigManager, Layer, Rate};
use crate::menu::FuzzelMenu;
use crate::output::OutputDriver;
use crate::serial::{self, Code, Input, SerialEventStream};
use crate::timer::{ClockId, TimerFd};

const SERIAL: Token = Token(0);
const REPEAT: Token = Token(1);
const FUZZEL_SENDER: Token = Token(2);
const FUZZEL_RECEIVER: Token = Token(3);

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

pub enum EngineCmd {
    /// Spawn a menu
    Menu(FuzzelMenu),
}

/// Messages passed back up from the engine to the event loop
pub struct EngineMsg {
    cmds: Vec<EngineCmd>,
}

impl EngineMsg {
    pub fn new() -> EngineMsg {
        EngineMsg { cmds: Vec::new() }
    }

    pub fn add_cmd(&mut self, cmd: EngineCmd) {
        self.cmds.push(cmd);
    }

    pub fn add_menu(&mut self, menu: FuzzelMenu) {
        self.add_cmd(EngineCmd::Menu(menu));
    }

    pub fn append(&mut self, other: &mut EngineMsg) {
        self.cmds.append(&mut other.cmds);
    }

    pub fn append_consume(&mut self, mut other: EngineMsg) {
        self.cmds.append(&mut other.cmds);
    }

    pub fn get_cmds(self) -> Vec<EngineCmd> {
        self.cmds
    }
}

/// Central engine managing peripheral state
pub struct Engine {
    /// Serial connection to the device
    serial: SerialEventStream,
    /// Timer for repeating keys
    timer: TimerFd,
    /// Menu if menu active
    menu: Option<FuzzelMenu>,
    /// Output for Wayland
    output: OutputDriver,
    /// Configuration management
    config_manager: ConfigManager,
    /// Track current config
    config: Rc<Config>,
    /// Track current layer, or None for the base layer
    layer: Option<String>,
    /// Held actions
    held_actions: HashMap<Code, Rc<Action>>,
    /// Held binds, for repeating events
    repeating_codes: Vec<Code>,
    /// Tickers for each dial
    ticks: Tickers,
}

impl Engine {
    pub fn new(device_path: Option<PathBuf>) -> Engine {
        let serial = SerialEventStream::new(serial::open(device_path));
        let timer = TimerFd::new(ClockId::Monotonic).expect("failed to build timer");

        let output = OutputDriver::new();

        let config_manager = ConfigManager::new();
        let config = config_manager.get_default_config();

        Engine {
            serial: serial,
            timer: timer,
            menu: None,
            output: output,
            config_manager: config_manager,
            config: config,
            layer: None,
            held_actions: HashMap::new(),
            repeating_codes: Vec::new(),
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
        if self.held_actions.contains_key(&input_a) {
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
    fn code_to_fallback_bind(&self, layer: &Layer<Rc<Bind>>, input: Code) -> Option<Rc<Bind>> {
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
        self.code_to_bind_inner(layer, code)
    }

    fn code_to_bind_inner(&self, layer: &Layer<Rc<Bind>>, input: Code) -> Option<Rc<Bind>> {
        match input {
            Code::Tall => layer.prime_ref().and_then(|v| v.tall.clone()),
            Code::Side => layer.prime_ref().and_then(|v| v.side.clone()),
            Code::Top => layer.prime_ref().and_then(|v| v.top.clone()),
            Code::Short => layer.prime_ref().and_then(|v| v.short.clone()),
            Code::TallDbl => layer
                .prime_ref()
                .and_then(|v| v.tall_x2.clone())
                .or_else(|| self.code_to_fallback_bind(layer, input)),
            Code::SideDbl => layer
                .prime_ref()
                .and_then(|v| v.side_x2.clone())
                .or_else(|| self.code_to_fallback_bind(layer, input)),
            Code::ShortDbl => layer
                .prime_ref()
                .and_then(|v| v.short_x2.clone())
                .or_else(|| self.code_to_fallback_bind(layer, input)),
            Code::TopDbl => layer
                .prime_ref()
                .and_then(|v| v.top_x2.clone())
                .or_else(|| self.code_to_fallback_bind(layer, input)),

            Code::SideTop => layer
                .prime_ref()
                .and_then(|v| v.side_top.clone())
                .or_else(|| self.code_to_fallback_bind(layer, input)),
            Code::SideTall => layer
                .prime_ref()
                .and_then(|v| v.side_tall.clone())
                .or_else(|| self.code_to_fallback_bind(layer, input)),
            Code::SideShort => layer
                .prime_ref()
                .and_then(|v| v.side_short.clone())
                .or_else(|| self.code_to_fallback_bind(layer, input)),
            Code::TopTall => layer
                .prime_ref()
                .and_then(|v| v.top_tall.clone())
                .or_else(|| self.code_to_fallback_bind(layer, input)),
            Code::TopShort => layer
                .prime_ref()
                .and_then(|v| v.top_short.clone())
                .or_else(|| self.code_to_fallback_bind(layer, input)),
            Code::TallShort => layer
                .prime_ref()
                .and_then(|v| v.tall_short.clone())
                .or_else(|| self.code_to_fallback_bind(layer, input)),

            Code::Tour => layer.kit_ref().and_then(|v| v.tour.clone()),

            Code::Up => layer
                .kit_ref()
                .and_then(|v| v.dpad_ref().and_then(|v| v.up.clone())),
            Code::Down => layer
                .kit_ref()
                .and_then(|v| v.dpad_ref().and_then(|v| v.down.clone())),
            Code::Left => layer
                .kit_ref()
                .and_then(|v| v.dpad_ref().and_then(|v| v.left.clone())),
            Code::Right => layer
                .kit_ref()
                .and_then(|v| v.dpad_ref().and_then(|v| v.right.clone())),

            Code::SideUp => layer
                .kit_ref()
                .and_then(|v| v.side_dpad_ref().and_then(|v| v.up.clone()))
                .or_else(|| self.code_to_fallback_bind(layer, input)),
            Code::SideDown => layer
                .kit_ref()
                .and_then(|v| v.side_dpad_ref().and_then(|v| v.down.clone()))
                .or_else(|| self.code_to_fallback_bind(layer, input)),
            Code::SideLeft => layer
                .kit_ref()
                .and_then(|v| v.side_dpad_ref().and_then(|v| v.left.clone()))
                .or_else(|| self.code_to_fallback_bind(layer, input)),
            Code::SideRight => layer
                .kit_ref()
                .and_then(|v| v.side_dpad_ref().and_then(|v| v.right.clone()))
                .or_else(|| self.code_to_fallback_bind(layer, input)),

            Code::TopUp => layer
                .kit_ref()
                .and_then(|v| v.top_dpad_ref().and_then(|v| v.up.clone()))
                .or_else(|| self.code_to_fallback_bind(layer, input)),
            Code::TopDown => layer
                .kit_ref()
                .and_then(|v| v.top_dpad_ref().and_then(|v| v.down.clone()))
                .or_else(|| self.code_to_fallback_bind(layer, input)),
            Code::TopLeft => layer
                .kit_ref()
                .and_then(|v| v.top_dpad_ref().and_then(|v| v.left.clone()))
                .or_else(|| self.code_to_fallback_bind(layer, input)),
            Code::TopRight => layer
                .kit_ref()
                .and_then(|v| v.top_dpad_ref().and_then(|v| v.right.clone()))
                .or_else(|| self.code_to_fallback_bind(layer, input)),

            Code::C1 => layer.kit_ref().and_then(|v| v.c1.clone()),
            Code::C2 => layer.kit_ref().and_then(|v| v.c2.clone()),

            Code::TallC1 => layer
                .kit_ref()
                .and_then(|v| v.tall_c1.clone())
                .or_else(|| self.code_to_fallback_bind(layer, input)),
            Code::TallC2 => layer
                .kit_ref()
                .and_then(|v| v.tall_c2.clone())
                .or_else(|| self.code_to_fallback_bind(layer, input)),

            Code::ShortC1 => layer
                .kit_ref()
                .and_then(|v| v.short_c1.clone())
                .or_else(|| self.code_to_fallback_bind(layer, input)),
            Code::ShortC2 => layer
                .kit_ref()
                .and_then(|v| v.short_c2.clone())
                .or_else(|| self.code_to_fallback_bind(layer, input)),

            Code::KnobButton => layer.knob_ref().and_then(|v| v.press.clone()),
            Code::Knob => layer.knob_ref().and_then(|v| v.turn.clone()),
            Code::TallKnob => layer
                .knob_ref()
                .and_then(|v| v.tall_turn.clone())
                .or_else(|| self.code_to_fallback_bind(layer, input)),
            Code::ShortKnob => layer
                .knob_ref()
                .and_then(|v| v.short_turn.clone())
                .or_else(|| self.code_to_fallback_bind(layer, input)),
            Code::TopKnob => layer
                .knob_ref()
                .and_then(|v| v.top_turn.clone())
                .or_else(|| self.code_to_fallback_bind(layer, input)),
            Code::SideKnob => layer
                .knob_ref()
                .and_then(|v| v.side_turn.clone())
                .or_else(|| self.code_to_fallback_bind(layer, input)),

            Code::ScrollButton => layer.scroll_ref().and_then(|v| v.press.clone()),
            Code::Scroll => layer.scroll_ref().and_then(|v| v.turn.clone()),
            Code::TallScroll => layer
                .scroll_ref()
                .and_then(|v| v.tall_turn.clone())
                .or_else(|| self.code_to_fallback_bind(layer, input)),
            Code::ShortScroll => layer
                .scroll_ref()
                .and_then(|v| v.short_turn.clone())
                .or_else(|| self.code_to_fallback_bind(layer, input)),
            Code::TopScroll => layer
                .scroll_ref()
                .and_then(|v| v.top_turn.clone())
                .or_else(|| self.code_to_fallback_bind(layer, input)),
            Code::SideScroll => layer
                .scroll_ref()
                .and_then(|v| v.side_turn.clone())
                .or_else(|| self.code_to_fallback_bind(layer, input)),

            Code::DialButton => layer.dial_ref().and_then(|v| v.press.clone()),
            Code::Dial => layer.dial_ref().and_then(|v| v.turn.clone()),

            _ => panic!("Impossible code received"),
        }
    }

    /// There's a series of fallbacks to this functio
    /// First it checks the current layer (if on a layer), then any fallbacks on the current layer
    /// Then it checks the base layer, then any fallbacks on the base layer
    fn code_to_bind(&self, input: Code) -> Option<Rc<Bind>> {
        match self.layer.as_ref() {
            Some(l) => {
                let layer = &self.config.layers.get(l).expect("layer went missing");
                self.code_to_bind_inner(layer, input)
                    .or_else(|| self.code_to_bind_inner(&self.config.base, input))
            }
            None => self.code_to_bind_inner(&self.config.base, input),
        }
    }

    pub fn get_serial(&mut self) -> &mut SerialEventStream {
        &mut self.serial
    }

    pub fn get_timer(&mut self) -> &mut TimerFd {
        &mut self.timer
    }

    fn mods_down(&mut self, mods: &Modifiers) {
        for key in mods.keys() {
            self.output.key_press(*key);
        }
        self.output.mod_append(*mods.flags());
    }

    fn mods_up(&mut self, mods: &Modifiers) {
        for key in mods.keys() {
            self.output.key_release(*key);
        }
        self.output.mod_remove(*mods.flags());
    }

    fn action_down(&mut self, code: Code, action: Rc<Action>) -> EngineMsg {
        let mut msg = EngineMsg::new();
        self.held_actions.insert(code, action.clone());
        match &*action {
            Action::None => {}
            Action::Mod(mods) => {
                self.mods_down(mods);
            }
            Action::Key(key_code, mods) => {
                mods.as_ref().map(|m| self.mods_down(&m));
                self.output.key_press(*key_code);
            }
            Action::PtrMotion(dx, dy, mods) => {
                mods.as_ref().map(|m| self.mods_down(&m));
                self.output.ptr_motion(*dx, *dy);
                self.output.ptr_frame();
            }
            Action::PtrMotionAbs(x, y, x_extent, y_extent, mods) => {
                mods.as_ref().map(|m| self.mods_down(&m));
                self.output
                    .ptr_motion_absolute(*x, *y, *x_extent, *y_extent);
                self.output.ptr_frame();
            }
            Action::PtrButton(button, mods) => {
                mods.as_ref().map(|m| self.mods_down(&m));
                self.output.ptr_button(*button, false);
                self.output.ptr_frame();
            }
            Action::PtrAxis(axis, value, mods) => {
                mods.as_ref().map(|m| self.mods_down(&m));
                self.output.ptr_axis(*axis, *value);
                self.output.ptr_frame();
            }
            Action::PtrAxisDiscrete(axis, value, discrete, mods) => {
                mods.as_ref().map(|m| self.mods_down(&m));
                self.output.ptr_axis_discrete(*axis, *value, *discrete);
                self.output.ptr_axis_stop(*axis);
                self.output.ptr_frame();
            }
            Action::Shortcut(name) => {
                let shortcut = self.config.shortcuts.get(name).expect("macro missing");
                msg.append_consume(self.action_down(code, shortcut.action.clone()));
            }
            Action::Macro(name) => {
                let r#macro = self.config.macros.get(name).expect("macro missing");
                let macro_actions = r#macro
                    .actions
                    .iter()
                    .map(|a| a.clone())
                    .collect::<Vec<_>>();
                for a in macro_actions {
                    self.action_down(Code::Macro, a.clone());
                    self.action_up(Code::Macro, a.clone());
                }
            }
            Action::Menu(name) => {
                let menu = self.config.menus.get(name).expect("menu missing");
                msg.add_menu(FuzzelMenu::new(menu.clone(), self.layer.clone()));
                self.layer = Some("menu".to_string());
            }
            Action::Layer(Some(name)) => {
                info!("moved to layer {}", name);
                if self.config.layers.get(name).is_none() {
                    panic!("assigned an invalid layer {}", name);
                }
                self.layer = Some(name.to_owned());
            }
            Action::Layer(None) => {
                info!("moved to layer base");
                self.layer = None;
            }
        };
        msg
    }

    fn action_up(&mut self, code: Code, action: Rc<Action>) {
        let held_action_opt = self.held_actions.remove(&code);
        if let Some(held_action) = held_action_opt {
            if held_action != action {
                // we've changed layer, release the old action
                self.action_up(code, held_action);
            }
        }
        match &*action {
            Action::None => {}
            Action::Mod(mods) => {
                self.mods_up(mods);
            }
            Action::Key(key_code, mods) => {
                self.output.key_release(*key_code);
                mods.as_ref().map(|m| self.mods_up(&m));
            }
            Action::PtrMotion(_, _, mods) => {
                mods.as_ref().map(|m| self.mods_up(&m));
            }
            Action::PtrMotionAbs(_, _, _, _, mods) => {
                mods.as_ref().map(|m| self.mods_up(&m));
            }
            Action::PtrButton(button, mods) => {
                self.output.ptr_button(*button, true);
                self.output.ptr_frame();
                mods.as_ref().map(|m| self.mods_up(&m));
            }
            Action::PtrAxis(_, _, mods) => {
                mods.as_ref().map(|m| self.mods_up(&m));
            }
            Action::PtrAxisDiscrete(_, _, _, mods) => {
                mods.as_ref().map(|m| self.mods_up(&m));
            }
            Action::Shortcut(name) => {
                let shortcut = self.config.shortcuts.get(name).expect("macro missing");
                self.action_up(code, shortcut.action.clone())
            }
            Action::Macro(_) => {}
            Action::Menu(_) => {}
            Action::Layer(_) => {}
        };
    }

    fn handle_input(&mut self, input: Input) -> EngineMsg {
        let mut msg = EngineMsg::new();
        let bind = self.code_to_bind(input.code);
        if bind.is_none() {
            warn!("no binding for code");
            return msg;
        }
        let bind = bind.unwrap();
        info!("{} -> {} : {}", input, bind, bind.get_action(input.reverse));
        match &*bind {
            Bind::Button(action) => {
                if !input.release {
                    msg.append_consume(self.action_down(input.code, action.clone()));
                } else {
                    self.action_up(input.code, action.clone());
                }
            }
            Bind::ButtonUp(action) => {
                if input.release {
                    msg.append_consume(self.action_down(input.code, action.clone()));
                    self.action_up(input.code, action.clone());
                }
            }
            Bind::ButtonRepeat(action) => {
                if !input.release {
                    self.repeating_codes.push(input.code);
                    msg.append_consume(self.action_down(input.code, action.clone()));
                    self.timer.set_timeout(&Duration::from_millis(100)).unwrap();
                } else {
                    self.repeating_codes.retain(|c| *c != input.code);
                    self.action_up(input.code, action.clone());
                    self.timer.disarm().unwrap();
                }
            }
            Bind::ButtonAB(action_a, action_b) => {
                if !input.release {
                    msg.append_consume(self.action_down(input.code, action_a.clone()));
                    self.action_up(input.code, action_a.clone());
                } else {
                    msg.append_consume(self.action_down(input.code, action_b.clone()));
                    self.action_up(input.code, action_b.clone());
                }
            }
            Bind::Scroll { fwd, bak, rate } => {
                let scroll_code = self.code_to_scroll(input.code);
                let counter = match scroll_code {
                    Some(Code::Knob) => self.ticks.knob.wrapping_add(1),
                    Some(Code::Scroll) => self.ticks.scroll.wrapping_add(1),
                    Some(Code::Dial) => self.ticks.dial.wrapping_add(1),
                    _ => panic!("Scrolled something that should not scroll"),
                };
                let modulo = match rate {
                    Rate::Normal => 1,
                    Rate::Slow => 2,
                    Rate::Slower => 3,
                };
                if counter % modulo == 0 {
                    let action = if input.reverse { bak } else { fwd };
                    if !input.release {
                        msg.append_consume(self.action_down(input.code, action.clone()));
                        self.action_up(input.code, action.clone());
                    }
                }
            }
        }
        msg
    }

    pub fn register_fuzzel_pipe(&mut self, poll: &mut Poll, mut menu: FuzzelMenu) {
        info!("fuzzel cmd");
        if let Some(_) = &self.menu {
            warn!("killing duplicate menu");
            self.deregister_fuzzel_pipe(poll);
        };
        poll.registry()
            .register(menu.receiver(), FUZZEL_RECEIVER, Interest::READABLE)
            .unwrap();
        poll.registry()
            .register(menu.sender(), FUZZEL_SENDER, Interest::WRITABLE)
            .unwrap();
        self.menu = Some(menu);
    }

    pub fn deregister_fuzzel_pipe(&mut self, poll: &mut Poll) {
        poll.registry()
            .deregister(self.menu.as_mut().unwrap().sender())
            .unwrap();
        poll.registry()
            .deregister(self.menu.as_mut().unwrap().receiver())
            .unwrap();
        self.menu = None;
    }

    pub fn handle_serial(&mut self) -> EngineMsg {
        let mut msg = EngineMsg::new();
        loop {
            match self.serial.next() {
                Some(Ok(input)) => {
                    msg.append_consume(self.handle_input(input));
                }
                Some(Err(ref err)) if would_block(err) => break,
                Some(Err(err)) => panic!("{}", err),
                None => break,
            }
        }
        msg
    }

    pub fn handle_repeat(&mut self) {
        info!("Timer tick");
        self.timer.set_timeout(&Duration::from_millis(10)).unwrap();
        let timeout_num = self.timer.read().unwrap();
        assert!(timeout_num == 1);
    }

    pub fn handle_fuzzel(&mut self) {
        // let action = match &mut self.fuzzel {
        //     Some(rec) => read_fuzzel_result(rec),
        //     None => panic!("fuzzel callback but no receiver was present"),
        // };
        let action = "?";
        warn!("fuzzel reports: {:?}", action);
    }

    pub fn run(&mut self) {
        let mut poll = Poll::new().expect("MIO poll failed to start");
        poll.registry()
            .register(self.get_serial(), SERIAL, Interest::READABLE)
            .expect("MIO register failed");
        poll.registry()
            .register(self.get_timer(), REPEAT, Interest::READABLE)
            .unwrap();

        let mut events = Events::with_capacity(128);

        loop {
            poll.poll(&mut events, Some(Duration::from_millis(100)))
                .unwrap();

            for event in events.iter() {
                match event.token() {
                    SERIAL => {
                        let msg = self.handle_serial();
                        for cmd in msg.get_cmds().into_iter() {
                            match cmd {
                                EngineCmd::Menu(menu) => {
                                    self.register_fuzzel_pipe(&mut poll, menu);
                                }
                            }
                        }
                    }
                    REPEAT => self.handle_repeat(),
                    FUZZEL_SENDER if event.is_write_closed() => {
                        info!("fuzzel sender closed");
                        let menu = self.menu.as_mut().expect("menu destroyed before recv");
                        self.layer = menu.last_layer().clone();
                        self.deregister_fuzzel_pipe(&mut poll);
                    }
                    FUZZEL_SENDER => {
                        info!("fuzzel sender hit");
                        let menu = self.menu.as_mut().expect("menu destroyed before send");
                        let stdin = menu.get_stdin(&self.config);
                        menu.sender()
                            .write_all(stdin.as_bytes())
                            .expect("failed to write menu to fuzzel");
                    }
                    FUZZEL_RECEIVER if event.is_read_closed() => {
                        info!("fuzzel receiver closed");
                    }
                    FUZZEL_RECEIVER => {
                        info!("fuzzel receiver event");
                        let menu = self.menu.as_mut().expect("menu destroyed before recv");
                        let mut str = String::new();
                        menu.receiver()
                            .read_to_string(&mut str)
                            .expect("failed to read result from fuzzel");
                        println!("{}", str);
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
