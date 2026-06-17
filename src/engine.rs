use std::collections::hash_map::Entry;
use std::collections::{HashMap, LinkedList};
use std::fmt;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use log::{debug, error, info, warn};
use mio::{Events, Interest, Poll, Token};
use notify_debouncer_full::notify::{self, INotifyWatcher, RecursiveMode};
use notify_debouncer_full::{
    DebounceEventResult, DebouncedEvent, Debouncer, NoCache, new_debouncer,
};

use crate::actions::{Action, Bind, Combi, Modifiers};
use crate::config::{Config, ConfigManager, CustomCode, Layer};
use crate::menu::FuzzelMenu;
use crate::notify::notify;
use crate::output::OutputDriver;
use crate::serial::{self, Code, Input, SerialEventStream};
use crate::timer::{ClockId, TimerFd};

const SERIAL: Token = Token(0);
const REPEAT: Token = Token(1);
const MENU_SENDER: Token = Token(2);
const MENU_RECEIVER: Token = Token(3);

const REPEAT_MS: Duration = Duration::from_millis(500);

/// Something that can be attached to a binding. A code or a custom code.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Command {
    Simple(Code),
    Custom(Rc<CustomCode>),
    Shortcut,
    Macro,
    Menu,
}

impl Command {
    pub fn into_single_code(&self) -> Option<Code> {
        match self {
            Command::Simple(code) if code.is_basic() => Some(*code),
            Command::Simple(code) if code.dedup().is_some() => code.dedup(),
            Command::Simple(code) if code.to_scroll_trio().is_some() => code.to_scroll_trio(),
            Command::Simple(_) => None,
            Command::Custom(custom_code) => match &**custom_code {
                CustomCode::Series(code, _) => Some(*code),
                CustomCode::Parallel(_) => None,
            },
            Command::Shortcut => None,
            Command::Macro => None,
            Command::Menu => None,
        }
    }

    pub fn into_multiple_codes(&self) -> Option<&[Code]> {
        match self {
            Command::Simple(code) => {
                let fallbacks = code.to_fallbacks();
                if fallbacks.len() > 1 { Some(fallbacks) } else { None }
            }
            Command::Custom(custom_code) => match &**custom_code {
                CustomCode::Series(_code, _) => None,
                CustomCode::Parallel(codes) => Some(codes),
            },
            Command::Shortcut => None,
            Command::Macro => None,
            Command::Menu => None,
        }
    }

    pub fn can_ignore(&self) -> bool {
        match self {
            Command::Shortcut => true,
            Command::Macro => true,
            Command::Menu => true,
            _ => false,
        }
    }

    pub fn is_subset(&self, other: &Command) -> bool {
        if let Some(c_self) = self.into_single_code() {
            if let Some(c_other) = other.into_single_code() {
                c_self == c_other
            } else if let Some(cs_other) = other.into_multiple_codes() {
                cs_other.contains(&c_self)
            } else {
                unreachable!()
            }
        } else if let Some(cs_self) = self.into_multiple_codes() {
            if let Some(_) = other.into_single_code() {
                false
            } else if let Some(cs_other) = other.into_multiple_codes() {
                cs_self.iter().all(|c_self| cs_other.contains(c_self))
            } else {
                unreachable!()
            }
        } else {
            self == other
        }
    }
}

impl fmt::Display for Command {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Command::Simple(code) => write!(f, "{code}"),
            Command::Custom(custom) => write!(f, "{custom}"),
            Command::Shortcut => write!(f, "shortcut"),
            Command::Macro => write!(f, "macro"),
            Command::Menu => write!(f, "menu"),
        }
    }
}

// TODO make this reset on reversal
pub struct Tickers {
    knob: usize,
    scroll: usize,
    dial: usize,
}

impl Tickers {
    pub fn new() -> Tickers {
        Tickers { knob: 0, scroll: 0, dial: 0 }
    }

    fn clear(&mut self) {
        self.knob = 0;
        self.scroll = 0;
        self.dial = 0;
    }
}

pub enum EngineCmd {
    /// Spawn a menu
    Menu(FuzzelMenu),
    Log(Rc<Action>),
}

/// Messages passed back up from the engine to the event loop
pub struct EngineCmds(LinkedList<EngineCmd>);

impl EngineCmds {
    pub fn new() -> EngineCmds {
        EngineCmds(LinkedList::new())
    }

    pub fn push(&mut self, cmd: EngineCmd) {
        self.0.push_back(cmd);
    }

    pub fn push_menu(&mut self, menu: FuzzelMenu) {
        self.push(EngineCmd::Menu(menu));
    }

    pub fn push_log(&mut self, log: Rc<Action>) {
        self.push(EngineCmd::Log(log));
    }

    pub fn append(&mut self, other: &mut EngineCmds) {
        self.0.append(&mut other.0);
    }

    pub fn get(self) -> LinkedList<EngineCmd> {
        self.0
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

/// Central engine managing peripheral state
pub struct Engine {
    /// Serial source to the device
    serial: SerialEventStream,
    /// Timer source for repeating keys
    timer: TimerFd,
    /// Menu if a menu is active
    menu: Option<FuzzelMenu>,
    /// Watcher for file changes
    watcher: Debouncer<INotifyWatcher, NoCache>,
    /// Watcher for file changes
    watcher_rx: Receiver<Result<Vec<DebouncedEvent>, Vec<notify::Error>>>,
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
    /// Tracks how many times a code has been pressed in a certain window
    repeat_tracker: HashMap<Code, RepeatTracker>,
    /// Held commands, used to track invalidation and ensure actions are released
    held_commands: HashMap<Command, Option<Rc<Action>>>,
    /// Invalidated codes, subsets of this will not fire when released if :up is set
    invalidated_commands: Vec<Command>,
    /// Tickers for each dial
    dial_ticks: Tickers,
    /// Tickers for each macro group
    macro_group_ticks: HashMap<String, usize>,
}

impl Engine {
    pub fn new(device_path: Option<PathBuf>) -> Engine {
        let serial = SerialEventStream::new(serial::open(device_path));
        let timer = TimerFd::new(ClockId::Monotonic).expect("timer should build");
        let (notify_tx, notify_rx) = mpsc::channel::<DebounceEventResult>();
        let watcher = new_debouncer(Duration::from_millis(500), None, notify_tx).unwrap();

        let output = OutputDriver::new();

        let config_manager = ConfigManager::new();
        let config = config_manager.get_default_config();

        Engine {
            serial: serial,
            timer: timer,
            menu: None,
            watcher: watcher,
            watcher_rx: notify_rx,
            output: output,
            config_manager: config_manager,
            config: config,
            layer: None,
            held_codes: Vec::new(),
            repeat_tracker: HashMap::new(),
            held_commands: HashMap::new(),
            invalidated_commands: Vec::new(),
            dial_ticks: Tickers::new(),
            macro_group_ticks: HashMap::new(),
        }
    }

    pub fn load_config(&mut self, path: &PathBuf) -> Option<String> {
        let path = std::path::absolute(path).expect("path should become absolute");
        self.watcher.unwatch(&path).ok();
        let name = self
            .config_manager
            .load_config(&path)
            .inspect_err(|e| {
                error!(target: "engine", "failed to load config at {}", path.to_str().unwrap_or("unkwown"));
                println!("{}", e);
            })
            .ok();
        self.watcher.watch(&path, RecursiveMode::NonRecursive).expect("watcher should watch");
        name
    }

    fn reload_cur_config(&mut self) {
        self.output.cleanse();
        self.set_config(Some(self.config.name.clone()))
    }

    pub fn set_config(&mut self, config: Option<String>) {
        self.output.cleanse();
        match config {
            Some(name) => match self.config_manager.get_config(&name) {
                Some(c) => {
                    self.config = c;
                }
                None => {
                    warn!(target: "engine", "defaulting to the default config");
                    self.config = self.config_manager.get_default_config();
                }
            },
            None => {
                self.config = self.config_manager.get_default_config();
            }
        }
        self.layer = self.config.default_layer.clone();
    }

    fn reset_ticks(&mut self) {
        self.dial_ticks.clear();
        self.macro_group_ticks.clear();
    }

    /// Given a code, returns up to two fallback codes that are not currently being held
    /// Used to calculate fallbacks to combo keycodes
    fn missing_fallback_codes(&self, input: Code) -> Vec<Code> {
        let fallbacks = input.to_fallbacks();
        match fallbacks.len() {
            a if a < 2 => Vec::from(fallbacks),
            // TODO make sure this handles okay on release, and test thoroughly
            _ => fallbacks
                .iter()
                .filter(|c| self.held_commands.get(&Command::Simple(**c)).is_none())
                .cloned()
                .collect(),
        }
    }

    /// A command is invalid if it's a subset of any command in `self.invalidated_commands`
    fn is_command_invalidated(&self, cmd: &Command) -> bool {
        self.invalidated_commands.iter().find(|invalid_cmd| cmd.is_subset(invalid_cmd)).is_some()
    }

    /// Lookup a simple code
    fn lookup_simple<N>(
        &self,
        layer: &Layer<N, Rc<CustomCode>, Rc<Bind>>,
        input: Code,
    ) -> Option<(Command, Rc<Bind>)> {
        let bind = match input {
            Code::Tall => layer.prime.tall.clone(),
            Code::Side => layer.prime.side.clone(),
            Code::Top => layer.prime.top.clone(),
            Code::Short => layer.prime.short.clone(),
            Code::TallDbl => layer.prime.tall_x2.clone(),
            Code::SideDbl => layer.prime.side_x2.clone(),
            Code::ShortDbl => layer.prime.short_x2.clone(),
            Code::TopDbl => layer.prime.top_x2.clone(),

            Code::SideTop => layer.prime.side_top.clone(),
            Code::SideTall => layer.prime.side_tall.clone(),
            Code::SideShort => layer.prime.side_short.clone(),
            Code::TopTall => layer.prime.top_tall.clone(),
            Code::TopShort => layer.prime.top_short.clone(),
            Code::TallShort => layer.prime.tall_short.clone(),

            Code::Tour => layer.kit.tour.clone(),

            Code::Up => layer.kit.up.clone(),
            Code::Down => layer.kit.down.clone(),
            Code::Left => layer.kit.left.clone(),
            Code::Right => layer.kit.right.clone(),

            Code::SideUp => layer.kit.side_up.clone(),
            Code::SideDown => layer.kit.side_down.clone(),
            Code::SideLeft => layer.kit.side_left.clone(),
            Code::SideRight => layer.kit.side_right.clone(),

            Code::TopUp => layer.kit.top_up.clone(),
            Code::TopDown => layer.kit.top_down.clone(),
            Code::TopLeft => layer.kit.top_left.clone(),
            Code::TopRight => layer.kit.top_right.clone(),

            Code::C1 => layer.kit.c1.clone(),
            Code::C2 => layer.kit.c2.clone(),

            Code::TallC1 => layer.kit.tall_c1.clone(),
            Code::TallC2 => layer.kit.tall_c2.clone(),

            Code::ShortC1 => layer.kit.short_c1.clone(),
            Code::ShortC2 => layer.kit.short_c2.clone(),

            Code::KnobButton => layer.knob.press.clone(),
            Code::Knob => layer.knob.turn.clone(),
            Code::TallKnob => layer.knob.tall_turn.clone(),
            Code::ShortKnob => layer.knob.short_turn.clone(),
            Code::TopKnob => layer.knob.top_turn.clone(),
            Code::SideKnob => layer.knob.side_turn.clone(),

            Code::ScrollButton => layer.scroll.press.clone(),
            Code::Scroll => layer.scroll.turn.clone(),
            Code::TallScroll => layer.scroll.tall_turn.clone(),
            Code::ShortScroll => layer.scroll.short_turn.clone(),
            Code::TopScroll => layer.scroll.top_turn.clone(),
            Code::SideScroll => layer.scroll.side_turn.clone(),

            Code::DialButton => layer.dial.press.clone(),
            Code::Dial => layer.dial.turn.clone(),
        };
        bind.map(|b| (Command::Simple(input), b))
    }

    /// Lookup the fallback code for a given layer and code
    /// If a non-fallback code is present, this method won't return it.
    /// Call `lookup_simple` first.
    fn lookup_fallback<N>(
        &self,
        layer: &Layer<N, Rc<CustomCode>, Rc<Bind>>,
        input: Code,
    ) -> Option<(Command, Rc<Bind>)> {
        let fallbacks = self.missing_fallback_codes(input);
        if fallbacks.len() > 1 {
            error!(target: "engine", "more than one fallback bind matched, returning just one");
        }
        fallbacks.first().and_then(|c| self.lookup_simple(layer, *c))
    }

    /// Lookup whether currently executing a custom bind
    /// If a non-custom code is present, this method won't return it.
    /// Call `lookup_simple` or `lookup_fallback` first.
    fn lookup_custom<N>(
        &self,
        layer: &Layer<N, Rc<CustomCode>, Rc<Bind>>,
        code: Code,
    ) -> Option<(Command, Rc<Bind>)> {
        layer
            .custom
            .keys()
            .find(|custom| match &***custom {
                CustomCode::Series(c_code, c_count) => {
                    c_code == &code
                        && c_count == &self.repeat_tracker.get(c_code).map(|r| r.count).unwrap_or(0)
                }
                CustomCode::Parallel(c_codes) => c_codes == &self.held_codes,
            })
            .map(|k| {
                let bind = layer.custom.get(k).expect("custom bind should exist");
                (Command::Custom(k.clone()), bind.clone())
            })
    }

    /// Lookup a code using sensible fallbacks. Handles custom commands, simple commands, and fallbacks.
    fn lookup(&self, code: Code) -> Option<(Command, Rc<Bind>)> {
        match self.layer.as_ref() {
            Some(l) => {
                let layer = &self.config.layers.get(l).expect("layer should exist");
                self.lookup_custom(layer, code)
                    .or_else(|| self.lookup_custom(&self.config.base, code))
                    .or_else(|| self.lookup_simple(layer, code))
                    .or_else(|| self.lookup_simple(&self.config.base, code))
                    .or_else(|| self.lookup_fallback(layer, code))
                    .or_else(|| self.lookup_fallback(&self.config.base, code))
            }
            None => self
                .lookup_custom(&self.config.base, code)
                .or_else(|| self.lookup_simple(&self.config.base, code))
                .or_else(|| self.lookup_fallback(&self.config.base, code)),
        }
    }

    fn mods_down(&mut self, mods: &Modifiers, combi: Combi) {
        for key in mods.keys() {
            self.output.key_press(*key);
        }
        match combi {
            Combi::Off => self.output.mod_append(*mods.flags()),
            Combi::On => self.output.mod_set(*mods.flags()),
        }
    }

    fn mods_up(&mut self, mods: &Modifiers) {
        for key in mods.keys() {
            self.output.key_release(*key);
        }
        self.output.mod_remove(*mods.flags());
    }

    /// Re-instates the modifiers of all held actions, which get clobbered by any non-:combi bind
    fn resume_held_commands(&mut self) {
        let actions = self
            .held_commands
            .iter_mut()
            .map(|(cmd, action)| (cmd.clone(), action.clone()))
            .collect::<Vec<_>>();
        for (cmd, action_opt) in actions.into_iter() {
            action_opt.map(|action| {
                action.mods().map(|m| {
                    debug!(target: "engine", "{cmd} (resume mods) -> {action}");
                    self.mods_down(m, Combi::On)
                })
            });
        }
    }

    fn execute_action_down(
        &mut self,
        msgs: &mut EngineCmds,
        cmd: &Command,
        action: Rc<Action>,
        combi: Combi,
    ) {
        if !cmd.can_ignore() {
            if let Some(Some(prev_action)) = self.held_commands.get(cmd) {
                // we've changed layer, release the old action
                info!(target: "engine", "{cmd} (down) -> cleanup({prev_action})");
                self.execute_action_up(msgs, cmd, prev_action.clone());
            }
            self.held_commands.insert(cmd.clone(), Some(action.clone()));
        }

        match &*action {
            Action::None => {}
            Action::Mod(mods) => {
                self.mods_down(mods, combi);
            }
            Action::Key(key_code, mods) => {
                mods.as_ref().map(|m| self.mods_down(&m, combi));
                self.output.key_press(*key_code);
            }
            Action::PtrMotion(dx, dy, mods) => {
                mods.as_ref().map(|m| self.mods_down(&m, combi));
                self.output.ptr_motion(*dx, *dy);
                self.output.ptr_frame();
            }
            Action::PtrMotionAbs(x, y, x_extent, y_extent, mods) => {
                mods.as_ref().map(|m| self.mods_down(&m, combi));
                self.output.ptr_motion_absolute(*x, *y, *x_extent, *y_extent);
                self.output.ptr_frame();
            }
            Action::PtrButton(button, mods) => {
                mods.as_ref().map(|m| self.mods_down(&m, combi));
                self.output.ptr_button(*button, false);
                self.output.ptr_frame();
            }
            Action::PtrAxis(axis, value, mods) => {
                mods.as_ref().map(|m| self.mods_down(&m, combi));
                self.output.ptr_axis(*axis, *value);
                self.output.ptr_frame();
            }
            Action::PtrAxisDiscrete(axis, value, discrete, mods) => {
                mods.as_ref().map(|m| self.mods_down(&m, combi));
                self.output.ptr_axis_discrete(*axis, *value, *discrete);
                self.output.ptr_axis_stop(*axis);
                self.output.ptr_frame();
            }
            Action::Shortcut(name, rev) => {
                let shortcut = self.config.shortcuts.get(name).expect("shortcut should exist");
                let action = if let Some(true) = rev {
                    shortcut.alt_action.as_ref().expect("shortcut alt action should exist").clone()
                } else {
                    shortcut.action.clone()
                };
                self.execute_action_down(msgs, &Command::Shortcut, action, combi);
            }
            Action::Macro(name) => {
                let r#macro = self.config.macros.get(name).expect("macro should exist");
                let macro_actions = r#macro.actions.to_owned();
                for macro_action in macro_actions {
                    self.execute_action_down(msgs, &Command::Macro, macro_action.clone(), combi);
                    self.execute_action_up(msgs, &Command::Macro, macro_action.clone());
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
                    self.execute_action_down(msgs, &Command::Macro, macro_action.clone(), combi);
                    self.execute_action_up(msgs, &Command::Macro, macro_action.clone());
                }
            }
            Action::Menu(name) => {
                let menu = self.config.menus.get(name).expect("menu should exist");
                msgs.push_menu(FuzzelMenu::new(menu.clone(), self.layer.clone()));
                self.layer = Some("menu".to_string());
                // reset all the tickers
                self.reset_ticks();
            }
            Action::Layer(Some(name)) => {
                let layer = self
                    .config
                    .layers
                    .get(name)
                    .unwrap_or_else(|| panic!("assigned an invalid layer {}", name));
                info!(target: "engine", "moved to layer {}", layer.name);
                if let Err(err) = notify("Layer Change", &layer.name) {
                    warn!(target: "engine", "dbus notification failed to send: {}", err);
                }
                self.layer = Some(name.to_owned());
                // reset all the tickers
                self.reset_ticks();
                // if we're currently in a menu, the menu's exit overwrites our layer change
                // so we change the menu command to return us here
                self.menu.as_mut().map(|m| m.set_last_layer(self.layer.clone()));
            }
            Action::Layer(None) => {
                let layer = self.config.default_layer.clone();
                info!(target: "engine", "moved to default layer");
                if let Err(err) = notify("Layer Change", layer.as_ref().map_or("Default", |v| v)) {
                    warn!(target: "engine", "dbus notification failed to send: {}", err);
                }
                self.layer = layer;
                // reset all the tickers
                self.reset_ticks();
            }
        };
    }

    fn execute_action_up(&mut self, msgs: &mut EngineCmds, cmd: &Command, action: Rc<Action>) {
        if !cmd.can_ignore() {
            if let Some(Some(prev_action)) = self.held_commands.insert(cmd.clone(), None) {
                if *prev_action != *action {
                    // we've changed layer, release the old action instead
                    info!(target: "engine", "{cmd} (up) -> cleanup({prev_action})");
                    self.execute_action_up(msgs, cmd, prev_action.clone());
                    return;
                }
            } else {
                info!(target: "engine", "{cmd} (up) -> ignored");
                return;
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
            Action::Shortcut(name, rev) => {
                let shortcut = self.config.shortcuts.get(name).expect("shortcut should exist");
                let action = if let Some(true) = rev {
                    shortcut.alt_action.as_ref().expect("shortcut alt action should exist").clone()
                } else {
                    shortcut.action.clone()
                };
                self.execute_action_up(msgs, &Command::Shortcut, action);
            }
            Action::Macro(_) => {}
            Action::MacroGroup(_) => {}
            Action::Menu(_) => {}
            Action::Layer(_) => {}
        }

        self.resume_held_commands();
    }

    fn execute_bind(&mut self, msgs: &mut EngineCmds, input: &Input, cmd: &Command, bind: &Bind) {
        match bind {
            Bind::Button(action, combi) => {
                if !input.release {
                    info!(target: "engine", "{cmd} -> {bind}:down({action})");
                    self.execute_action_down(msgs, cmd, action.clone(), *combi);
                } else {
                    info!(target: "engine", "{cmd} -> {bind}:up({action})");
                    self.execute_action_up(msgs, cmd, action.clone());
                }
            }
            Bind::ButtonUp(action) => {
                if input.release && !self.is_command_invalidated(&cmd) {
                    info!(target: "engine", "{cmd} -> {bind}({action})");
                    self.execute_action_down(msgs, cmd, action.clone(), Combi::Off);
                    self.execute_action_up(msgs, cmd, action.clone());
                }
            }
            Bind::ButtonRepeat(action, combi) => {
                if !input.release {
                    info!(target: "engine", "{cmd} -> {bind}:down({action})");
                    self.execute_action_down(msgs, cmd, action.clone(), *combi);
                    self.timer.set_timeout(&Duration::from_millis(100)).unwrap();
                } else {
                    info!(target: "engine", "{cmd} -> {bind}:up({action})");
                    self.execute_action_up(msgs, cmd, action.clone());
                    self.timer.disarm().unwrap();
                }
            }
            Bind::ButtonAB(action_a, action_b) => {
                if !input.release {
                    info!(target: "engine", "{cmd} -> {bind}:A({action_a})");
                    self.execute_action_down(msgs, cmd, action_a.clone(), Combi::Off);
                    self.execute_action_up(msgs, cmd, action_a.clone());
                } else {
                    info!(target: "engine", "{cmd} -> {bind}:B({action_b})");
                    self.execute_action_down(msgs, cmd, action_b.clone(), Combi::Off);
                    self.execute_action_up(msgs, cmd, action_b.clone());
                }
            }
            Bind::Scroll { fwd, bak, rate } => {
                if !input.release {
                    let counter = match cmd {
                        Command::Simple(code) => match code.to_scroll_trio() {
                            Some(Code::Knob) => &mut self.dial_ticks.knob,
                            Some(Code::Scroll) => &mut self.dial_ticks.scroll,
                            Some(Code::Dial) => &mut self.dial_ticks.dial,
                            _ => panic!("scrolled something that should not scroll"),
                        },
                        Command::Custom(custom_cmd) => match &**custom_cmd {
                            // TODO test both of these
                            CustomCode::Series(_, _) => {
                                todo!("custom serial binds cannot be scrolled yet")
                            }
                            CustomCode::Parallel(codes) => {
                                let code = codes.iter().filter_map(|c| c.to_scroll_trio()).nth(0);
                                match code {
                                    Some(Code::Knob) => &mut self.dial_ticks.knob,
                                    Some(Code::Scroll) => &mut self.dial_ticks.scroll,
                                    Some(Code::Dial) => &mut self.dial_ticks.dial,
                                    _ => panic!("scrolled something that should not scroll"),
                                }
                            }
                        },
                        _ => panic!("scrolled something that should not scroll"),
                    };
                    *counter = if input.reverse {
                        counter.wrapping_sub(1)
                    } else {
                        counter.wrapping_add(1)
                    };
                    if *counter % rate.speed() == 0 {
                        let action = if input.reverse { bak } else { fwd };
                        if !input.release {
                            info!(target: "engine", "{cmd} -> {bind}({action})");
                            self.execute_action_down(msgs, cmd, action.clone(), Combi::Off);
                            self.execute_action_up(msgs, cmd, action.clone());
                        }
                    }
                }
            }
        }
    }

    /// set any new code as being held, and return them
    fn set_held_codes(&mut self, input: &Input) {
        if !input.release {
            if input.code.is_basic() && !self.held_codes.contains(&input.code) {
                self.held_codes.push(input.code);
            } else {
                for code in self.missing_fallback_codes(input.code) {
                    self.held_codes.push(code);
                }
            }
            self.held_codes.sort();
        }
    }

    /// set or invalidate held commands
    fn set_invalid_cmds(&mut self, input: &Input, cmd: &Command) {
        // An invalidated command is set when:
        // - a multi-code command covers already-held commands
        if !input.release {
            if cmd.into_multiple_codes().is_some() {
                let mut invalid: Vec<_> = self
                    .held_commands
                    .keys()
                    .filter(|held_cmd| {
                        held_cmd.is_subset(cmd) && !self.invalidated_commands.contains(cmd)
                    })
                    .cloned()
                    .collect();
                self.invalidated_commands.append(&mut invalid);
            }
        }
    }

    /// release any code that's no longer held, release invalidated commands that no longer apply
    fn release_held_codes_or_cmds(&mut self, input: &Input) {
        if input.release {
            self.held_codes.retain(|c| {
                *c != input.code
                    && Some(*c) != input.code.dedup()
                    && Some(*c) != input.code.to_scroll_trio()
            });

            // An invalidated command can be released if all of its component inputs are released
            self.invalidated_commands.retain(|invalid_cmd| {
                if let Some(code) = invalid_cmd.into_single_code() {
                    self.held_codes.contains(&code)
                } else if let Some(codes) = invalid_cmd.into_multiple_codes() {
                    codes.iter().any(|c| self.held_codes.contains(c))
                } else {
                    true
                }
            });
        }
    }

    fn update_repeat_tracker(&mut self, input: &Input) {
        if !input.release {
            let code = input.code.dedup().unwrap_or(input.code);
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
    fn handle_serial(&mut self, msgs: &mut EngineCmds) {
        loop {
            match self.serial.next() {
                Some(Ok(input)) => {
                    self.update_repeat_tracker(&input);
                    self.set_held_codes(&input);
                    if let Some((cmd, bind)) = self.lookup(input.code) {
                        self.set_invalid_cmds(&input, &cmd);
                        if !input.release {
                            let entry = self.held_commands.entry(cmd.clone());
                            if let Entry::Occupied(_) = entry {
                                debug!(target: "engine", "{cmd} -> ignored duplicate");
                                break;
                            }
                            entry.or_insert(None);
                        }
                        if self.held_commands.contains_key(&cmd) {
                            self.execute_bind(msgs, &input, &cmd, &bind);
                        } else {
                            debug!(target: "engine", "{cmd} -> ignored bare release");
                        }
                        if input.release {
                            if let Some(Some(c)) = self.held_commands.remove(&cmd) {
                                panic!("held command {c:?} should have been emptied by this point");
                            };
                        }
                    } else {
                        debug!(target: "engine", "{input} -> no binding for code");
                    }
                    self.release_held_codes_or_cmds(&input);
                }
                Some(Err(ref err)) if would_block(err) => break,
                Some(Err(err)) => panic!("{}", err),
                None => break,
            }
        }
    }

    // handle a timer-based repetition
    fn handle_repeat(&mut self, msgs: &mut EngineCmds) {
        info!(target: "engine", "timer tick");
        self.timer.set_timeout(&Duration::from_millis(10)).unwrap();
        let timeout_num = self.timer.read().unwrap();
        assert!(timeout_num == 1);
    }

    // handle the return of an external menu
    fn handle_menu(&mut self, msgs: &mut EngineCmds) {
        let menu = self.menu.as_mut().expect("menu should exist");
        match menu.read_action().expect("menu should provide an action") {
            Some(action) => {
                info!(target: "engine", "{} -> bind {}", Command::Menu, action);
                self.execute_action_down(msgs, &Command::Menu, action.clone(), Combi::Off);
                self.execute_action_up(msgs, &Command::Menu, action.clone());
            }
            None => info!(target: "engine", "menu aborted"),
        }
    }

    // handle a message passed from deeper in the event loop
    fn handle_engine_msgs(&mut self, msgs: EngineCmds, poll: &mut Poll) {
        for cmd in msgs.get().into_iter() {
            match cmd {
                EngineCmd::Menu(menu) => {
                    self.register_menu_pipe(poll, menu);
                }
                EngineCmd::Log(action) => todo!(),
            }
        }
    }

    fn register_menu_pipe(&mut self, poll: &mut Poll, mut menu: FuzzelMenu) {
        if let Some(_) = &self.menu {
            warn!(target: "engine", "menu duplication, killing old menu");
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

    fn deregister_menu_pipe(&mut self, poll: &mut Poll) {
        let menu = self.menu.as_mut().expect("menu should exist");
        poll.registry()
            .deregister(menu.sender())
            .expect("MIO deregister should succeed for menu stdin");
        poll.registry()
            .deregister(menu.receiver())
            .expect("MIO deregister should succeed for menu stdout");
        self.menu = None;
    }

    fn sanity_check(&self) {
        if self.held_codes.is_empty() {
            if !self.invalidated_commands.is_empty() {
                error!(target: "engine", "no codes are held, but an invalidated code is still held");
            }
            if !self.held_commands.is_empty() {
                error!(target: "engine", "no codes are held, but a command is still held");
                dbg!(&self.held_commands);
            }
            if self.output.held_keys_count() > 0 {
                error!(target: "engine", "no codes are held, but the output still has keys");
            }
        }
        if let None = self.layer
            && let Some(_) = self.config.default_layer
        {
            error!(target: "engine", "fell to base layer instead of default layer");
        }
    }

    pub fn run(&mut self) {
        let mut poll = Poll::new().expect("MIO poll should start");
        poll.registry()
            .register(&mut self.serial, SERIAL, Interest::READABLE)
            .expect("MIO serial should register");
        poll.registry()
            .register(&mut self.timer, REPEAT, Interest::READABLE)
            .expect("MIO timer should register");

        self.watcher.watch(Path::new("darktable.toml"), RecursiveMode::NonRecursive).unwrap();

        let mut events = Events::with_capacity(128);

        loop {
            poll.poll(&mut events, Some(Duration::from_millis(100)))
                .expect("MIO poll should succeed");

            // handle config changes
            match self.watcher_rx.try_recv() {
                Ok(Ok(events)) => {
                    for event in events {
                        for path in &event.paths {
                            // we don't need to do anything if this fails
                            self.load_config(path);
                        }
                    }
                    self.reload_cur_config();
                }
                _ => {}
            }

            for event in events.iter() {
                self.sanity_check();
                match event.token() {
                    // serial message, run action
                    SERIAL => {
                        let mut msgs = EngineCmds::new();
                        self.handle_serial(&mut msgs);
                        self.handle_engine_msgs(msgs, &mut poll);
                    }
                    // timer message, re-run action
                    REPEAT => {
                        let mut msgs = EngineCmds::new();
                        self.handle_repeat(&mut msgs);
                        self.handle_engine_msgs(msgs, &mut poll);
                    }
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
                        let mut msgs = EngineCmds::new();
                        self.handle_menu(&mut msgs);
                        self.handle_engine_msgs(msgs, &mut poll);
                    }
                    _ => {}
                }
                self.sanity_check();
            }
        }
    }
}

fn would_block(err: &io::Error) -> bool {
    err.kind() == io::ErrorKind::WouldBlock
}
