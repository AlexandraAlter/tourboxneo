use std::collections::HashMap;
use std::io::{self, Write};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

use log::{debug, info, trace, warn};
use mio::{Events, Interest, Poll, Token};

use crate::actions::{Action, Modifiers};
use crate::config::{Bind, Config, ConfigManager, CustomCode, Layer, Rate};
use crate::menu::FuzzelMenu;
use crate::output::OutputDriver;
use crate::serial::{self, Code, Input, SerialEventStream};
use crate::timer::{ClockId, TimerFd};

const SERIAL: Token = Token(0);
const REPEAT: Token = Token(1);
const MENU_SENDER: Token = Token(2);
const MENU_RECEIVER: Token = Token(3);

const REPEAT_MS: Duration = Duration::from_millis(500);

pub struct Tickers {
    knob: usize,
    scroll: usize,
    dial: usize,
}

impl Tickers {
    pub fn new() -> Tickers {
        Tickers { knob: 0, scroll: 0, dial: 0 }
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

    pub fn get_cmds(self) -> Vec<EngineCmd> {
        self.cmds
    }
}

#[derive(Debug)]
pub struct RepeatTracker {
    pub last_input: Instant,
    pub count: usize,
}

impl RepeatTracker {
    fn new() -> Self {
        Self { last_input: Instant::now(), count: 0 }
    }
}

#[derive(PartialEq, Debug)]
pub struct HeldActionTracker {
    pub action: Rc<Action>,
    pub paused: bool,
}

impl HeldActionTracker {
    fn new(action: Rc<Action>) -> Self {
        Self { action, paused: false }
    }
}

/// Central engine managing peripheral state
pub struct Engine {
    /// Serial source to the device
    serial: SerialEventStream,
    /// Timer source for repeating keys
    timer: TimerFd,
    /// Menu if a menu is active
    menu: Option<FuzzelMenu>,
    /// Output for Wayland
    output: OutputDriver,
    /// Configuration management
    config_manager: ConfigManager,
    /// Currently active config
    config: Rc<Config>,
    /// Currently active layer, or None for the base layer
    layer: Option<String>,
    /// Held codes, for custom actions
    held_codes: Vec<Code>,
    /// Held actions, helps ensure they're released
    held_actions: HashMap<Code, HeldActionTracker>,
    /// Held binds, for repeating events
    // TODO make sure this does anything
    repeating_codes: Vec<Code>,
    /// Tickers for each dial
    dial_ticks: Tickers,
    /// Tickers for each macro group
    macro_group_ticks: HashMap<String, usize>,
    /// Tracks how many times a key has been pressed in a certain window
    repeat_tracker: HashMap<Code, RepeatTracker>,
}

impl Engine {
    pub fn new(device_path: Option<PathBuf>) -> Engine {
        let serial = SerialEventStream::new(serial::open(device_path));
        let timer = TimerFd::new(ClockId::Monotonic).expect("timer should build");

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
            held_codes: Vec::new(),
            held_actions: HashMap::new(),
            repeating_codes: Vec::new(),
            dial_ticks: Tickers::new(),
            macro_group_ticks: HashMap::new(),
            repeat_tracker: HashMap::new(),
        }
    }

    pub fn load_config(&mut self, path: PathBuf) -> String {
        self.config_manager.load_config(path)
    }

    pub fn set_config(&mut self, name: &str) {
        let config = self.config_manager.get_config(name).expect("config name should be valid");
        self.config = config;
    }

    /// Given two codes, returns which one is not currently being held
    /// Used to calculate fallbacks to more complicated keycodes
    fn missing_code(&self, input_a: Code, input_b: Code) -> Code {
        if self.held_codes.contains(&input_a) { input_b } else { input_a }
    }

    /// Check if we're currently executing a custom bind
    fn code_to_custom_bind<N>(
        &self,
        layer: &Layer<N, CustomCode, Rc<Bind>>,
        code: Code,
    ) -> Option<Rc<Bind>> {
        layer
            .custom
            .keys()
            .find(|custom| match custom {
                CustomCode::Series(c_code, c_count) => {
                    c_code == &code
                        && c_count == &self.repeat_tracker.get(c_code).map(|r| r.count).unwrap_or(0)
                }
                CustomCode::Parallel(c_codes) => c_codes == &self.held_codes,
            })
            .map(|k| layer.custom.get(k).expect("should exist"))
            .cloned()
    }

    /// Given a code without a matching bind in the current config, return an appropriate fallback bind
    fn code_to_fallback_bind<N>(
        &self,
        layer: &Layer<N, CustomCode, Rc<Bind>>,
        input: Code,
    ) -> Option<Rc<Bind>> {
        input.to_fallback().and_then(|(fallback_a, fallback_b)| {
            let missing = self.missing_code(fallback_a, fallback_b);
            self.code_to_bind_inner(layer, missing)
        })
    }

    fn code_to_bind_inner<N>(
        &self,
        layer: &Layer<N, CustomCode, Rc<Bind>>,
        input: Code,
    ) -> Option<Rc<Bind>> {
        let fallback = || self.code_to_fallback_bind(layer, input);
        let custom_bind = self.code_to_custom_bind(layer, input);
        if custom_bind.is_some() {
            return custom_bind;
        }
        match input {
            Code::Tall => layer.prime.tall.clone(),
            Code::Side => layer.prime.side.clone(),
            Code::Top => layer.prime.top.clone(),
            Code::Short => layer.prime.short.clone(),
            Code::TallDbl => layer.prime.tall_x2.clone().or_else(fallback),
            Code::SideDbl => layer.prime.side_x2.clone().or_else(fallback),
            Code::ShortDbl => layer.prime.short_x2.clone().or_else(fallback),
            Code::TopDbl => layer.prime.top_x2.clone().or_else(fallback),

            Code::SideTop => layer.prime.side_top.clone().or_else(fallback),
            Code::SideTall => layer.prime.side_tall.clone().or_else(fallback),
            Code::SideShort => layer.prime.side_short.clone().or_else(fallback),
            Code::TopTall => layer.prime.top_tall.clone().or_else(fallback),
            Code::TopShort => layer.prime.top_short.clone().or_else(fallback),
            Code::TallShort => layer.prime.tall_short.clone().or_else(fallback),

            Code::Tour => layer.kit.tour.clone(),

            Code::Up => layer.kit.dpad.up.clone(),
            Code::Down => layer.kit.dpad.down.clone(),
            Code::Left => layer.kit.dpad.left.clone(),
            Code::Right => layer.kit.dpad.right.clone(),

            Code::SideUp => layer.kit.side_dpad.up.clone().or_else(fallback),
            Code::SideDown => layer.kit.side_dpad.down.clone().or_else(fallback),
            Code::SideLeft => layer.kit.side_dpad.left.clone().or_else(fallback),
            Code::SideRight => layer.kit.side_dpad.right.clone().or_else(fallback),

            Code::TopUp => layer.kit.top_dpad.up.clone().or_else(fallback),
            Code::TopDown => layer.kit.top_dpad.down.clone().or_else(fallback),
            Code::TopLeft => layer.kit.top_dpad.left.clone().or_else(fallback),
            Code::TopRight => layer.kit.top_dpad.right.clone().or_else(fallback),

            Code::C1 => layer.kit.c1.clone(),
            Code::C2 => layer.kit.c2.clone(),

            Code::TallC1 => layer.kit.tall_c1.clone().or_else(fallback),
            Code::TallC2 => layer.kit.tall_c2.clone().or_else(fallback),

            Code::ShortC1 => layer.kit.short_c1.clone().or_else(fallback),
            Code::ShortC2 => layer.kit.short_c2.clone().or_else(fallback),

            Code::KnobButton => layer.knob.press.clone(),
            Code::Knob => layer.knob.turn.clone(),
            Code::TallKnob => layer.knob.tall_turn.clone().or_else(fallback),
            Code::ShortKnob => layer.knob.short_turn.clone().or_else(fallback),
            Code::TopKnob => layer.knob.top_turn.clone().or_else(fallback),
            Code::SideKnob => layer.knob.side_turn.clone().or_else(fallback),

            Code::ScrollButton => layer.scroll.press.clone(),
            Code::Scroll => layer.scroll.turn.clone(),
            Code::TallScroll => layer.scroll.tall_turn.clone().or_else(fallback),
            Code::ShortScroll => layer.scroll.short_turn.clone().or_else(fallback),
            Code::TopScroll => layer.scroll.top_turn.clone().or_else(fallback),
            Code::SideScroll => layer.scroll.side_turn.clone().or_else(fallback),

            Code::DialButton => layer.dial.press.clone(),
            Code::Dial => layer.dial.turn.clone(),

            _ => panic!("impossible code received"),
        }
    }

    /// There's a series of fallbacks to this function
    /// First it checks the current layer (if on a layer), then any fallbacks on the current layer
    /// Then it checks the base layer, then any fallbacks on the base layer
    fn code_to_bind(&self, input: Code) -> Option<Rc<Bind>> {
        match self.layer.as_ref() {
            Some(l) => {
                let layer = &self.config.layers.get(l).expect("layer should exist");
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

    fn action_down_inner(&mut self, code: Code, action: Rc<Action>) -> EngineMsg {
        let mut msg = EngineMsg::new();
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
                self.output.ptr_motion_absolute(*x, *y, *x_extent, *y_extent);
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
                let shortcut = self.config.shortcuts.get(name).expect("macro should exist");
                msg.append(&mut self.action_down_inner(code, shortcut.action.clone()));
            }
            Action::Macro(name) => {
                let r#macro = self.config.macros.get(name).expect("macro should exist");
                let macro_actions = r#macro.actions.to_owned();
                for macro_action in macro_actions {
                    debug!("{} (down) -> action {}", Code::Macro, macro_action);
                    self.action_down_inner(Code::Macro, macro_action.clone());
                    debug!("{} (up) -> action {}", Code::Macro, macro_action);
                    self.action_up_inner(Code::Macro, macro_action.clone());
                }
            }
            Action::MacroGroup(name) => {
                let r#macro = self.config.macro_groups.get(name).expect("macro group should exist");
                let ticker_max = r#macro.groups.len();
                let ticker = self
                    .macro_group_ticks
                    .entry(name.clone())
                    .and_modify(|a| *a = (*a + 1) % ticker_max)
                    .or_insert(0);
                let group = if !r#macro.reverse {
                    r#macro.groups.get(*ticker)
                } else {
                    r#macro.groups.get(ticker_max - *ticker - 1)
                };
                let actions = group.expect("macro group index should be in of bounds").to_owned();
                for macro_action in actions {
                    debug!("{} (down) -> action {}", Code::Macro, macro_action);
                    self.action_down_inner(Code::Macro, macro_action.clone());
                    debug!("{} (up) -> action {}", Code::Macro, macro_action);
                    self.action_up_inner(Code::Macro, macro_action.clone());
                }
            }
            Action::Menu(name) => {
                let menu = self.config.menus.get(name).expect("menu should exist");
                msg.add_menu(FuzzelMenu::new(menu.clone(), self.layer.clone()));
                self.layer = Some("menu".to_string());
            }
            Action::Layer(Some(name)) => {
                info!("moved to layer {}", name);
                if self.config.layers.get(name).is_none() {
                    panic!("assigned an invalid layer {}", name);
                }
                // if we're currently in a menu, the menu's exit overwrites our layer change
                // so we change the menu command to return us here
                self.layer = Some(name.to_owned());
                self.menu.as_mut().map(|m| m.set_last_layer(self.layer.clone()));
            }
            Action::Layer(None) => {
                info!("moved to layer base");
                self.layer = None;
            }
        };
        msg
    }

    fn action_up_inner(&mut self, code: Code, action: Rc<Action>) {
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
                let shortcut = self.config.shortcuts.get(name).expect("macro should exist");
                self.action_up_inner(code, shortcut.action.clone())
            }
            Action::Macro(_) => {}
            Action::MacroGroup(_) => {}
            Action::Menu(_) => {}
            Action::Layer(_) => {}
        }
    }

    fn pause_held_actions(&mut self) {
        let actions = self
            .held_actions
            .iter_mut()
            .filter(|(_code, tracker)| !tracker.paused)
            .map(|(code, tracker)| {
                tracker.paused = true;
                (*code, tracker.action.clone())
            })
            .collect::<Vec<_>>();
        for (code, action) in actions.into_iter() {
            debug!("{} (pause mods) -> {}", code, action);
            action.mods().map(|m| self.mods_up(m));
        }
    }

    fn resume_held_actions(&mut self) {
        let actions = self
            .held_actions
            .iter_mut()
            .filter(|(_code, tracker)| tracker.paused)
            .map(|(code, tracker)| {
                tracker.paused = false;
                (*code, tracker.action.clone())
            })
            .collect::<Vec<_>>();
        for (code, action) in actions.into_iter() {
            debug!("{} (resume mods) -> {}", code, action);
            action.mods().map(|m| self.mods_down(m));
        }
    }

    fn action_down(&mut self, code: Code, action: Rc<Action>) -> EngineMsg {
        self.pause_held_actions();
        debug!("{} (down) -> action {}", code, action);
        self.held_actions.insert(code, HeldActionTracker::new(action.clone()));
        let msg = self.action_down_inner(code, action);
        msg
    }

    fn action_up(&mut self, code: Code, action: Rc<Action>) {
        if let Some(prev_action_tracker) = self.held_actions.remove(&code) {
            let prev_action = prev_action_tracker.action;
            if prev_action != action {
                // we've changed layer, release the old action
                debug!("{} (up) (cleanup) -> action {}", code, prev_action);
                self.action_up_inner(code, prev_action);
            }
        }
        debug!("{} (up) -> action {}", code, action);
        self.action_up_inner(code, action);
        self.resume_held_actions();
    }

    fn execute_bind(&mut self, input: &Input, bind: &Bind) -> EngineMsg {
        let mut msg = EngineMsg::new();
        match bind {
            Bind::Button(action) => {
                if !input.release {
                    msg.append(&mut self.action_down(input.code, action.clone()));
                } else {
                    self.action_up(input.code, action.clone());
                }
            }
            Bind::ButtonUp(action) => {
                if input.release {
                    msg.append(&mut self.action_down(input.code, action.clone()));
                    self.action_up(input.code, action.clone());
                }
            }
            Bind::ButtonRepeat(action) => {
                if !input.release {
                    self.repeating_codes.push(input.code);
                    msg.append(&mut self.action_down(input.code, action.clone()));
                    self.timer.set_timeout(&Duration::from_millis(100)).unwrap();
                } else {
                    self.repeating_codes.retain(|c| *c != input.code);
                    self.action_up(input.code, action.clone());
                    self.timer.disarm().unwrap();
                }
            }
            Bind::ButtonAB(action_a, action_b) => {
                if !input.release {
                    msg.append(&mut self.action_down(input.code, action_a.clone()));
                    self.action_up(input.code, action_a.clone());
                } else {
                    msg.append(&mut self.action_down(input.code, action_b.clone()));
                    self.action_up(input.code, action_b.clone());
                }
            }
            Bind::Scroll { fwd, bak, rate } => {
                let scroll_code = input.code.to_scroll_trio();
                let counter = match scroll_code {
                    Some(Code::Knob) => self.dial_ticks.knob.wrapping_add(1),
                    Some(Code::Scroll) => self.dial_ticks.scroll.wrapping_add(1),
                    Some(Code::Dial) => self.dial_ticks.dial.wrapping_add(1),
                    _ => panic!("scrolled something that should not scroll"),
                };
                let modulo = match rate {
                    Rate::Normal => 1,
                    Rate::Slow => 2,
                    Rate::Slower => 3,
                };
                if counter % modulo == 0 {
                    let action = if input.reverse { bak } else { fwd };
                    if !input.release {
                        msg.append(&mut self.action_down(input.code, action.clone()));
                        self.action_up(input.code, action.clone());
                    }
                }
            }
        }
        msg
    }

    pub fn register_menu_pipe(&mut self, poll: &mut Poll, mut menu: FuzzelMenu) {
        if let Some(_) = &self.menu {
            warn!("menu duplication, killing old menu");
            self.deregister_menu_pipe(poll);
        };
        poll.registry()
            .register(menu.receiver(), MENU_RECEIVER, Interest::READABLE)
            .expect("MIO register should succeed for menu stdin");
        poll.registry()
            .register(menu.sender(), MENU_SENDER, Interest::WRITABLE)
            .expect("MIO register should succeed for menu stdin");
        self.menu = Some(menu);
    }

    pub fn deregister_menu_pipe(&mut self, poll: &mut Poll) {
        let menu = self.menu.as_mut().expect("menu should exist");
        poll.registry()
            .deregister(menu.sender())
            .expect("MIO deregister should succeed for menu stdin");
        poll.registry()
            .deregister(menu.receiver())
            .expect("MIO deregister should succeed for menu stdout");
        self.menu = None;
    }

    /// set any new code as being held
    pub fn handle_held_code_early(&mut self, input: &Input) {
        if !input.release {
            self.held_codes.push(input.code);
            self.held_codes.sort();
        }
    }

    /// release any code that's no longer held
    pub fn handle_held_code_late(&mut self, input: &Input) {
        if input.release {
            self.held_codes.retain(|c| *c != input.code);
        }
    }

    fn handle_repeat_tracker(&mut self, input: &Input) {
        if !input.release {
            let code = input.code.dedup();
            let entry = self.repeat_tracker.entry(code).or_insert_with(|| RepeatTracker::new());
            let now = Instant::now();
            if now.duration_since(entry.last_input) > REPEAT_MS {
                entry.count = 0;
            }
            entry.count += 1;
            entry.last_input = now;
        }
    }

    // handle a serial input from the device
    pub fn handle_serial(&mut self) -> EngineMsg {
        let mut msg = EngineMsg::new();
        loop {
            match self.serial.next() {
                Some(Ok(input)) => {
                    self.handle_held_code_early(&input);
                    self.handle_repeat_tracker(&input);
                    if let Some(bind) = self.code_to_bind(input.code) {
                        info!("{} -> bind {} -> {}", input, bind, bind.get_action(input.reverse));
                        msg.append(&mut self.execute_bind(&input, bind.as_ref()));
                    } else {
                        info!("no binding for code");
                    }
                    self.handle_held_code_late(&input);
                }
                Some(Err(ref err)) if would_block(err) => break,
                Some(Err(err)) => panic!("{}", err),
                None => break,
            }
        }
        msg
    }

    // handle a timer-based repetition
    pub fn handle_repeat(&mut self) {
        info!("timer tick");
        self.timer.set_timeout(&Duration::from_millis(10)).unwrap();
        let timeout_num = self.timer.read().unwrap();
        assert!(timeout_num == 1);
    }

    // handle the return of an external menu
    pub fn handle_menu(&mut self) -> EngineMsg {
        let mut msg = EngineMsg::new();
        let menu = self.menu.as_mut().expect("menu should exist");
        match menu.read_action().expect("menu should provide an action") {
            Some(action) => {
                info!("{} -> bind {}", Code::Menu, action);
                msg.append(&mut self.action_down(Code::Menu, action.clone()));
                self.action_up(Code::Menu, action.clone());
            }
            None => info!("menu aborted"),
        }
        msg
    }

    // handle a message passed from deeper in the event loop
    pub fn handle_engine_msg(&mut self, msg: EngineMsg, poll: &mut Poll) {
        for cmd in msg.get_cmds().into_iter() {
            match cmd {
                EngineCmd::Menu(menu) => {
                    self.register_menu_pipe(poll, menu);
                }
            }
        }
    }

    pub fn run(&mut self) {
        let mut poll = Poll::new().expect("MIO poll should start");
        poll.registry()
            .register(self.get_serial(), SERIAL, Interest::READABLE)
            .expect("MIO serial should register");
        poll.registry()
            .register(self.get_timer(), REPEAT, Interest::READABLE)
            .expect("MIO timer should register");

        let mut events = Events::with_capacity(128);

        loop {
            poll.poll(&mut events, Some(Duration::from_millis(100)))
                .expect("MIO poll should succeed");

            for event in events.iter() {
                match event.token() {
                    // serial message, run action
                    SERIAL => {
                        let msg = self.handle_serial();
                        self.handle_engine_msg(msg, &mut poll);
                    }
                    // timer message, re-run action
                    REPEAT => self.handle_repeat(),
                    // fuzzel sender closed, clean up
                    MENU_SENDER if event.is_write_closed() => {
                        let menu = self.menu.as_mut().expect("menu should exist");
                        self.layer = menu.last_layer().clone();
                        self.deregister_menu_pipe(&mut poll);
                    }
                    // fuzzel sender established, send stdin
                    MENU_SENDER => {
                        let menu = self.menu.as_mut().expect("menu should exist");
                        let stdin = menu.get_stdin(&self.config);
                        menu.sender()
                            .write_all(stdin.as_bytes())
                            .expect("menu sender should write options");
                    }
                    // fuzzel receiver closed, but we clean up elsewhere
                    MENU_RECEIVER if event.is_read_closed() => {}
                    // fuzzel receiver message, run action
                    MENU_RECEIVER => {
                        let msg = self.handle_menu();
                        self.handle_engine_msg(msg, &mut poll);
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
