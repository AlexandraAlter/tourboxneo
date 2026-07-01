use std::collections::{HashMap, LinkedList};
use std::fmt;
use std::io::{self, Write};
use std::path::PathBuf;
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
    /// If this command decomposes into one `is_basic` codes, return it
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

    /// If this command decomposes into several `is_basic` codes, return them
    pub fn into_multiple_codes(&self) -> Option<&[Code]> {
        match self {
            Command::Simple(code) => {
                let constituants = code.to_constituants();
                if constituants.len() > 1 { Some(constituants) } else { None }
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
        // run multiple_codes functions first, in case both functions return usable data
        if let Some(cs_self) = self.into_multiple_codes() {
            if let Some(cs_other) = other.into_multiple_codes() {
                cs_self.iter().all(|c_self| cs_other.contains(c_self))
            } else if let Some(_) = other.into_single_code() {
                false
            } else {
                unreachable!()
            }
        } else if let Some(c_self) = self.into_single_code() {
            if let Some(cs_other) = other.into_multiple_codes() {
                cs_other.contains(&c_self)
            } else if let Some(c_other) = other.into_single_code() {
                c_self == c_other
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
    knob_fwd: usize,
    knob_bak: usize,
    scroll_fwd: usize,
    scroll_bak: usize,
    dial_fwd: usize,
    dial_bak: usize,
}

impl Tickers {
    pub fn new() -> Tickers {
        Tickers { knob_fwd: 0, knob_bak: 0, scroll_fwd: 0, scroll_bak: 0, dial_fwd: 0, dial_bak: 0 }
    }

    fn clear(&mut self) {
        self.knob_fwd = 0;
        self.knob_bak = 0;
        self.scroll_fwd = 0;
        self.scroll_bak = 0;
        self.dial_fwd = 0;
        self.dial_bak = 0;
    }

    fn get(&mut self, code: Code, reverse: bool) -> (&mut usize, &mut usize) {
        match (code.to_scroll_trio(), reverse) {
            (Some(Code::Knob), false) => (&mut self.knob_fwd, &mut self.knob_bak),
            (Some(Code::Knob), true) => (&mut self.knob_bak, &mut self.knob_fwd),
            (Some(Code::Scroll), false) => (&mut self.scroll_fwd, &mut self.scroll_bak),
            (Some(Code::Scroll), true) => (&mut self.scroll_bak, &mut self.scroll_fwd),
            (Some(Code::Dial), false) => (&mut self.dial_fwd, &mut self.dial_bak),
            (Some(Code::Dial), true) => (&mut self.dial_bak, &mut self.dial_fwd),
            _ => panic!("scrolled something that should not scroll"),
        }
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
    /// Serial source from the device
    serial: SerialEventStream,
    /// Timer source for repeating keys
    timer: TimerFd,
    /// Currently active menu, or none
    menu: Option<FuzzelMenu>,
    /// Watcher for file changes
    watcher: Debouncer<INotifyWatcher, NoCache>,
    /// Watcher receiving pipe for file changes
    watcher_rx: Receiver<Result<Vec<DebouncedEvent>, Vec<notify::Error>>>,
    /// Output for Wayland
    output: OutputDriver,
    /// Configuration management
    config_manager: ConfigManager,
    /// Currently active config
    config: Rc<Config>,
    /// Currently active layer, or None for the base layer
    layer: Option<String>,
    /// Held basic codes only, used for custom actions
    held_codes: Vec<Code>,
    /// Tracks how many times a code has been pressed in a certain window
    repeat_tracker: HashMap<Code, RepeatTracker>,
    /// Held commands, used to track invalidation and ensure actions are released
    held_actions: HashMap<Command, Rc<Action>>,
    /// Held binds, used for released codes
    held_binds: HashMap<Command, Rc<Bind>>,
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
            held_actions: HashMap::new(),
            held_binds: HashMap::new(),
            invalidated_commands: Vec::new(),
            dial_ticks: Tickers::new(),
            macro_group_ticks: HashMap::new(),
        }
    }

    /// Load a config from the disk, adding it to the watch list
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

    /// Try to reload the current config from the manager
    fn reload_cur_config(&mut self) {
        self.output.cleanse();
        self.set_config(Some(self.config.name.clone()))
    }

    /// Switch to a loaded config in the manager
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

    /// Reset the dial tickers and macro tickers
    /// Call when changing layer or config
    fn reset_ticks(&mut self) {
        self.dial_ticks.clear();
        self.macro_group_ticks.clear();
    }

    /// Given a code, returns up to two fallback codes that are not currently being held
    /// Used to calculate fallbacks to combo keycodes
    fn find_missing_fallback_codes(&self, input: Code) -> Vec<Code> {
        input.to_fallbacks().into_iter().filter(|c| !self.held_codes.contains(c)).cloned().collect()
    }

    /// Given a code, returns up to two fallback commands that are not currently being held
    /// Used to calculate fallbacks to combo keycodes
    fn find_missing_fallback_commands(&self, input: Code) -> Vec<Code> {
        let fallbacks = input.to_fallbacks();
        match fallbacks.len() {
            0 => Vec::from(fallbacks),
            // We only get one fallback if it's a double or a scroll
            1 => Vec::from(fallbacks),
            // TODO make sure this handles okay on release, and test thoroughly
            _ => fallbacks
                .iter()
                .filter(|c| self.held_binds.get(&Command::Simple(**c)).is_none())
                .cloned()
                .collect(),
        }
    }

    /// A command is invalid if it's a subset of any command in `self.invalidated_commands`
    fn is_command_invalidated(&self, cmd: &Command) -> bool {
        self.invalidated_commands.iter().find(|invalid_cmd| cmd.is_subset(invalid_cmd)).is_some()
    }

    /// Lookup a simple code
    // TODO make this an iterator?
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
    // TODO make this an iterator
    fn lookup_fallback<N>(
        &self,
        layer: &Layer<N, Rc<CustomCode>, Rc<Bind>>,
        input: Code,
    ) -> Option<(Command, Rc<Bind>)> {
        let fallbacks = self.find_missing_fallback_commands(input);
        if fallbacks.len() > 1 {
            error!(target: "engine", "more than one fallback bind matched, returning just one");
        }
        fallbacks.first().and_then(|c| self.lookup_simple(layer, *c))
    }

    /// Lookup whether currently executing a custom bind
    /// If a non-custom code is present, this method won't return it.
    /// Call `lookup_simple` or `lookup_fallback` first.
    // TODO make this an iterator
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
    // TODO make this an iterator
    fn lookup_pressed(&self, code: Code) -> Option<(Command, Rc<Bind>)> {
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

    /// Lookup whether we're releasing something already held
    fn lookup_released(&self) -> impl Iterator<Item = (Command, Rc<Bind>)> {
        dbg!(&self.held_codes);
        self.held_binds
            .iter()
            .filter(move |(cmd, _b)| {
                if let Some(code) = cmd.into_single_code() {
                    !self.held_codes.contains(&code)
                } else if let Some(codes) = cmd.into_multiple_codes() {
                    !codes.iter().any(|c| !self.held_codes.contains(c))
                } else {
                    true
                }
            })
            .map(|(c, b)| (c.clone(), b.clone()))
    }

    /// Append or set modifiers (and related keys)
    fn mods_down(&mut self, mods: &Modifiers, combi: Combi) {
        for key in mods.keys() {
            self.output.key_press(*key);
        }
        match combi {
            Combi::On => self.output.mod_append(*mods.flags()),
            Combi::Off => self.output.mod_set(*mods.flags()),
        }
    }

    /// Release modifiers (and related keys)
    fn mods_up(&mut self, mods: &Modifiers) {
        for key in mods.keys() {
            self.output.key_release(*key);
        }
        self.output.mod_remove(*mods.flags());
    }

    /// Re-instates the modifiers of all held actions, which get clobbered by any non-:combi bind
    fn resume_held_commands(&mut self) {
        let actions = self
            .held_actions
            .iter_mut()
            .map(|(cmd, action)| (cmd.clone(), action.clone()))
            .collect::<Vec<_>>();
        for (cmd, action) in actions.into_iter() {
            action.mods().map(|m| {
                debug!(target: "engine", "{cmd} (resume mods) -> {action}");
                self.mods_down(m, Combi::On)
            });
        }
    }

    /// Given a command and action, add it to the held list and execute the action's press
    fn execute_action_down(
        &mut self,
        msgs: &mut EngineCmds,
        cmd: &Command,
        action: Rc<Action>,
        combi: Combi,
    ) {
        if !cmd.can_ignore() {
            if let Some(prev_action) = self.held_actions.get(cmd) {
                // we've changed layer, release the old action
                // this shouldn't really happen
                warn!(target: "engine", "{cmd} (down) -> clobbering cleanup({prev_action})");
                self.execute_action_up(msgs, cmd, None);
            }
            self.held_actions.insert(cmd.clone(), action.clone());
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
                    self.execute_action_up(msgs, &Command::Macro, Some(macro_action.clone()));
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
                    self.execute_action_up(msgs, &Command::Macro, Some(macro_action.clone()));
                }
            }
            Action::Menu(name) => {
                let menu = self.config.menus.get(name).expect("menu should exist");
                msgs.push_menu(FuzzelMenu::new(menu.clone(), self.layer.clone()));
                self.layer = Some("menu".to_string());
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

    /// Given a command, remove its action from the held list, execute the action's release, restore held modifiers
    /// Only supply an `action` if `cmd` is a dummy command (shortcut, macro, menu)
    fn execute_action_up(
        &mut self,
        msgs: &mut EngineCmds,
        cmd: &Command,
        action: Option<Rc<Action>>,
    ) {
        let action = if cmd.can_ignore() {
            action.expect("action should be provided by the caller")
        } else {
            if action.is_some() {
                panic!("action should not be provided by the caller");
            }
            self.held_actions.remove(cmd).expect("action should be present in held_actions")
        };

        match &*action {
            Action::None => {}
            Action::Mod(mods) => {
                self.mods_up(&mods);
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
                self.execute_action_up(msgs, &Command::Shortcut, Some(action));
            }
            Action::Macro(_) => {}
            Action::MacroGroup(_) => {}
            Action::Menu(_) => {}
            Action::Layer(_) => {}
        }

        self.resume_held_commands();
    }

    /// Given a command and binding, optionally run some actions
    fn execute_bind_down(
        &mut self,
        msgs: &mut EngineCmds,
        cmd: &Command,
        bind: Rc<Bind>,
        reverse: bool,
    ) {
        if self.held_binds.contains_key(&cmd) {
            debug!(target: "engine", "{cmd} -> {bind} ignored duplicate");
            return;
        }

        self.held_binds.insert(cmd.clone(), bind.clone());

        match &*bind {
            Bind::Button(action, combi) => {
                info!(target: "engine", "{cmd} -> {bind}:down({action})");
                self.execute_action_down(msgs, cmd, action.clone(), *combi);
            }
            Bind::ButtonUp(_action) => {}
            Bind::ButtonRepeat(action, combi) => {
                info!(target: "engine", "{cmd} -> {bind}:down({action})");
                self.execute_action_down(msgs, cmd, action.clone(), *combi);
                self.timer.set_timeout(&Duration::from_millis(100)).unwrap();
            }
            Bind::ButtonAB(action_a, _action_b) => {
                info!(target: "engine", "{cmd} -> {bind}:A({action_a})");
                self.execute_action_down(msgs, cmd, action_a.clone(), Combi::Off);
                self.execute_action_up(msgs, cmd, None);
            }
            Bind::Scroll { fwd, bak, rate } => {
                let (counter, counter_rev) = match cmd {
                    Command::Simple(code) => self.dial_ticks.get(*code, reverse),
                    Command::Custom(custom_cmd) => match &**custom_cmd {
                        // TODO test both of these
                        CustomCode::Series(_, _) => {
                            todo!("custom binds cannot be scrolled yet")
                        }
                        CustomCode::Parallel(codes) => {
                            let code = codes
                                .iter()
                                .filter_map(|c| c.to_scroll_trio())
                                .nth(0)
                                .expect("parallel command should have one scrollable code");
                            self.dial_ticks.get(code, reverse)
                        }
                    },
                    _ => panic!("scrolled something that should not scroll"),
                };
                *counter = counter.wrapping_add(1);
                *counter_rev = 0;
                if *counter % rate.speed() == 0 {
                    self.dial_ticks.clear();
                    let action = if reverse { bak } else { fwd };
                    info!(target: "engine", "{cmd} -> {bind}({action})");
                    self.execute_action_down(msgs, cmd, action.clone(), Combi::Off);
                    self.execute_action_up(msgs, cmd, None);
                }
            }
        }
    }

    /// Given a command and binding, optionally run some actions
    fn execute_bind_up(&mut self, msgs: &mut EngineCmds, cmd: &Command, bind: &Bind) {
        if let None = self.held_binds.remove(&cmd) {
            error!(target: "engine", "{cmd} -> {bind} went missing from held_binds");
        }

        match bind {
            Bind::Button(action, _combi) => {
                info!(target: "engine", "{cmd} -> {bind}:up({action})");
                self.execute_action_up(msgs, cmd, None);
            }
            Bind::ButtonUp(action) => {
                if !self.is_command_invalidated(&cmd) {
                    info!(target: "engine", "{cmd} -> {bind}({action})");
                    self.execute_action_down(msgs, cmd, action.clone(), Combi::Off);
                    self.execute_action_up(msgs, cmd, None);
                }
            }
            Bind::ButtonRepeat(action, _combi) => {
                info!(target: "engine", "{cmd} -> {bind}:up({action})");
                self.execute_action_up(msgs, cmd, None);
                self.timer.disarm().unwrap();
            }
            Bind::ButtonAB(_action_a, action_b) => {
                info!(target: "engine", "{cmd} -> {bind}:B({action_b})");
                self.execute_action_down(msgs, cmd, action_b.clone(), Combi::Off);
                self.execute_action_up(msgs, cmd, None);
            }
            Bind::Scroll { fwd: _fwd, bak: _bak, rate: _rate } => {}
        }
    }

    /// set any new code as being held
    fn set_held_codes(&mut self, input: &Input) {
        if !input.release {
            if input.code.is_basic() && !self.held_codes.contains(&input.code) {
                self.held_codes.push(input.code);
            } else {
                for code in self.find_missing_fallback_codes(input.code) {
                    self.held_codes.push(code);
                }
            }
            self.held_codes.sort();
        } else {
            self.held_codes.retain(|c| {
                *c != input.code
                    && Some(*c) != input.code.dedup()
                    && Some(*c) != input.code.to_scroll_trio()
            });
        }
    }

    /// set invalidated commands
    fn set_invalid_commands(&mut self, input: &Input, cmd: &Command) {
        // An invalidated command is set when:
        // - a multi-code command covers already-held commands
        if !input.release {
            if cmd.into_multiple_codes().is_some() {
                let mut invalid_cmds: Vec<_> = self
                    .held_binds
                    .keys()
                    .filter(|held_cmd| {
                        held_cmd.is_subset(cmd) && !self.invalidated_commands.contains(held_cmd)
                    })
                    .cloned()
                    .collect();
                self.invalidated_commands.append(&mut invalid_cmds);
            }
        }
    }

    /// release invalidated commands that no longer apply
    fn release_invalid_commands(&mut self, input: &Input) {
        if input.release {
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

    /// Update `repeat_tracker`, resetting the count if it's past `REPEAT_MS`
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

    /// handle a serial input from the device
    fn handle_serial(&mut self, msgs: &mut EngineCmds) {
        loop {
            match self.serial.next() {
                Some(Ok(input)) => {
                    self.update_repeat_tracker(&input);
                    self.set_held_codes(&input);
                    if !input.release {
                        if let Some((cmd, bind)) = self.lookup_pressed(input.code) {
                            self.set_invalid_commands(&input, &cmd);
                            self.execute_bind_down(msgs, &cmd, bind, input.reverse);
                        } else {
                            debug!(target: "engine", "{input} -> no binding for code");
                        }
                    } else {
                        let released: Vec<_> = self.lookup_released().collect();
                        for (cmd, bind) in released.iter() {
                            self.execute_bind_up(msgs, &cmd, &bind);
                            if let Some(c) = self.held_actions.remove(&cmd) {
                                panic!("held command {c:?} should have been emptied by this point");
                            };
                        }
                        if released.is_empty() {
                            debug!(target: "engine", "{input} -> no binding for code");
                        }
                    }
                    self.release_invalid_commands(&input);
                }
                Some(Err(ref err)) if would_block(err) => break,
                Some(Err(err)) => panic!("{}", err),
                None => break,
            }
        }
    }

    /// handle a timer-based repetition
    // TODO implement this
    fn handle_repeat(&mut self, msgs: &mut EngineCmds) {
        info!(target: "engine", "timer tick");
        self.timer.set_timeout(&Duration::from_millis(10)).unwrap();
        let timeout_num = self.timer.read().unwrap();
        assert!(timeout_num == 1);
    }

    /// handle the return of an external menu
    fn handle_menu(&mut self, msgs: &mut EngineCmds) {
        let menu = self.menu.as_mut().expect("menu should exist");
        match menu.read_action().expect("menu should provide an action") {
            Some(action) => {
                info!(target: "engine", "{} -> bind {}", Command::Menu, action);
                self.execute_action_down(msgs, &Command::Menu, action.clone(), Combi::Off);
                self.execute_action_up(msgs, &Command::Menu, Some(action.clone()));
            }
            None => info!(target: "engine", "menu aborted"),
        }
    }

    /// handle a message passed from deeper in the event loop
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

    /// register a new menu pipe
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

    /// deregister a new menu pipe
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

    /// A few common-sense checks to be run at the start and end of every cycle
    fn sanity_check(&self) {
        if self.held_codes.is_empty() {
            if !self.invalidated_commands.is_empty() {
                error!(target: "engine", "no codes are held, but an invalidated code is still held:\n{:?}", &self.invalidated_commands);
            }
            if !self.held_binds.is_empty() {
                error!(target: "engine", "no codes are held, but a bind is still held:\n{:?}\n{:?}\n{:?}", &self.held_binds, &self.held_actions, &self.output.held_keys());
            } else if !self.held_actions.is_empty() {
                error!(target: "engine", "no codes are held, but a command is still held:\n{:?}\n{:?}", &self.held_actions, &self.output.held_keys());
            } else if self.output.held_keys_count() > 0 {
                error!(target: "engine", "no codes are held, but the output still has keys:\n{:?}", &self.output.held_keys());
            }
        }
        if let None = self.layer
            && let Some(_) = self.config.default_layer
        {
            error!(target: "engine", "fell to base layer instead of default layer");
        }
    }

    /// Enter the event loop with MIO
    pub fn run(&mut self) {
        let mut poll = Poll::new().expect("MIO poll should start");
        poll.registry()
            .register(&mut self.serial, SERIAL, Interest::READABLE)
            .expect("MIO serial should register");
        poll.registry()
            .register(&mut self.timer, REPEAT, Interest::READABLE)
            .expect("MIO timer should register");

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
