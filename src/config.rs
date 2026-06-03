use std::{collections::HashMap, fmt, fs, marker::PhantomData, ops::Range, path::PathBuf, rc::Rc};

use log::error;

use lazy_static::lazy_static;
use regex::Regex;
use serde::{
    Deserialize,
    de::{self, Visitor},
};
use toml::Spanned;

use crate::{
    actions::{Action, ActionLibrary, Modifiers},
    error::ConfigError,
};

// Action format:
// - "a"
// - "a:up:rep"
// - "ptr_x(val):rep"
// - "a/b"
// - "a/b:slow"
// - "a/b:slower"
// - "ptr_wheel"
// - "ptr_wheel:rev"

lazy_static! {
    static ref SINGLE_REGEX: Regex = {
        Regex::new(
            r"(?x)
                ^\s*(?P<action>[[:word:]]+)\s*
                (?:\(\s*(?P<args>[[:word:]]+\s*(?:,\s*[[:word:]]+\s*)*)\s*\))?
                \s*(?P<flags>(?::[[:word:]]+\s*)*)$
            ",
        )
        .expect("regex should compile")
    };
    static ref DOUBLE_REGEX: Regex = {
        Regex::new(
            r"(?x)
                ^\s*(?P<action>[[:word:]]+)\s*
                (?:\(\s*(?P<args>[[:word:]]+\s*(?:,\s*[[:word:]]+\s*)*)\s*\))?
                (?P<flags>(?::[[:word:]]+\s*)*)
                (?:\s*/\s*(?P<alt_action>[[:word:]]+)\s*
                  (?:\(\s*(?P<alt_args>[[:word:]]+\s*(?:,\s*[[:word:]]+\s*)*)\s*\))?
                )
                \s*(?P<alt_flags>(?::[[:word:]]+\s*)*)$
            ",
        )
        .expect("regex should compile")
    };
    static ref SHORTCUT_REGEX: Regex = {
        Regex::new(
            r"(?x)
                ^\s*(?P<action>[[:word:]]+)\s*
                (?:\(\s*(?P<args>[[:word:]]+\s*(?:,\s*[[:word:]]+\s*)*)\s*\))?
                \s*(?P<flags>(?::[[:word:]]+\s*)*)$
            ",
        )
        .expect("regex should compile")
    };
    static ref ARGUMENT_REGEX: Regex = Regex::new(r"[[:word:]]+").expect("regex should compile");
    static ref FLAG_REGEX: Regex = Regex::new(r":[[:word:]]+").expect("regex should compile");
}

#[derive(Debug, PartialEq, Eq, Clone)]
enum Flag {
    Up,
    Repeat,
    Rev,
    Rate(Rate),
    Mod(Modifiers),
}

pub struct Lookup<'a>(&'a mut ActionLibrary);

impl<'a> Lookup<'a> {
    fn action(
        &self,
        str: &str,
        args: Option<Vec<String>>,
        range: Range<usize>,
    ) -> Result<Rc<Action>, ConfigError> {
        self.0
            .get(str.as_ref(), &args)
            .ok_or_else(|| ConfigError::new("action not found in library", range))
    }

    fn args(&self, args: Option<&str>) -> Option<Vec<String>> {
        match args {
            Some(args) => Some(
                ARGUMENT_REGEX.find_iter(args).map(|m| m.as_str().to_owned()).collect::<Vec<_>>(),
            ),
            None => None,
        }
    }

    fn flags(&self, flags: &str, range: Range<usize>) -> Result<Vec<Flag>, ConfigError> {
        let flag_vec = FLAG_REGEX
            .find_iter(flags)
            .map(|f| match f.as_str().strip_prefix(":").unwrap() {
                "up" => Some(Flag::Up),
                "rep" => Some(Flag::Repeat),
                "rev" => Some(Flag::Rev),
                "slow" => Some(Flag::Rate(Rate::Slow)),
                "slower" => Some(Flag::Rate(Rate::Slower)),
                m @ _ => Modifiers::lookup_mod(m).map(|m| Flag::Mod(m)),
            })
            .collect::<Vec<_>>();
        if flag_vec.contains(&None) {
            return Err(ConfigError::new("unrecogized flag", range));
        }
        let flag_vec: Vec<_> = flag_vec.iter().flatten().cloned().collect();
        if flag_vec.iter().filter(|f| vec![Flag::Up, Flag::Repeat].contains(*f)).count() > 1 {
            return Err(ConfigError::new("multiple mode-setting flags", range));
        }
        if flag_vec
            .iter()
            .filter(|f| vec![Flag::Rate(Rate::Slow), Flag::Rate(Rate::Slower)].contains(*f))
            .count()
            > 1
        {
            return Err(ConfigError::new("multiple rate flags", range));
        }
        Ok(flag_vec)
    }

    fn action_and_flags(
        &self,
        action_str: &str,
        args_str: Option<&str>,
        flags_str: &str,
        range: Range<usize>,
    ) -> Result<(Rc<Action>, Vec<Flag>), ConfigError> {
        let args = self.args(args_str);
        let mut action = self.action(action_str, args, range.clone())?;
        let flags = self.flags(flags_str, range.clone())?;

        let mut mods = Vec::new();
        let mut non_mods_flags = Vec::new();
        for flag in flags {
            match flag {
                Flag::Mod(modifiers) => mods.push(modifiers),
                Flag::Rev => {
                    action = action
                        .reverse()
                        .ok_or_else(|| ConfigError::new("action is not reversible", range.clone()))?
                        .into()
                }
                _ => non_mods_flags.push(flag),
            }
        }

        if !mods.is_empty() {
            let modifiers = mods.iter().fold(Modifiers::default(), |acc, m| acc.union(m));
            action = action
                .with_modifiers(&modifiers)
                .ok_or_else(|| ConfigError::new("action cannot be modified", range.clone()))?
                .into()
        }

        Ok((action, non_mods_flags))
    }

    fn button_bind(&self, str: Spanned<String>) -> Result<Rc<Bind>, ConfigError> {
        let range = str.span();
        let captures = DOUBLE_REGEX
            .captures(str.as_ref())
            .or_else(|| SINGLE_REGEX.captures(str.as_ref()))
            .ok_or_else(|| ConfigError::new("invalid button", range.clone()))?;

        let action_str = captures.name("action").expect("action name should exist").as_str();
        let args_str = captures.name("args").map(|a| a.as_str());
        let flags_str = captures.name("flags").unwrap().as_str();
        let (action, flags) =
            self.action_and_flags(action_str, args_str, flags_str, range.clone())?;

        // If we've got an alt_action or a :rev flag, we're parsing an AB button
        if captures.name("alt_action").is_some() {
            let alt_action_str =
                captures.name("alt_action").expect("alt action name should exist").as_str();
            let alt_args_str = captures.name("alt_args").map(|a| a.as_str());
            let alt_flags_str = captures.name("alt_flags").unwrap().as_str();
            let (alt_action, alt_flags) =
                self.action_and_flags(alt_action_str, alt_args_str, alt_flags_str, range.clone())?;

            if !alt_flags.is_empty() {
                return Err(ConfigError::new("AB binds accept no other flags", range.clone()));
            }

            return Ok(Bind::ButtonAB(action.into(), alt_action.into()).into());
        }

        let bind = if flags.contains(&Flag::Up) {
            Bind::ButtonUp(action.into())
        } else if flags.contains(&Flag::Repeat) {
            Bind::ButtonRepeat(action.into())
        } else {
            Bind::Button(action.into())
        };

        Ok(bind.into())
    }

    fn button_bind_opt(&self, str: Spanned<String>) -> Option<Rc<Bind>> {
        self.button_bind(str).inspect_err(|e| error!("{}", e)).ok()
    }

    #[cfg(test)]
    fn button_bind_str(&self, str: &str) -> Result<Rc<Bind>, ConfigError> {
        self.button_bind(Spanned::new(0..str.len(), str.to_owned()))
    }

    fn scroll_bind(&self, str: Spanned<String>) -> Result<Rc<Bind>, ConfigError> {
        let range = str.span();
        let captures = DOUBLE_REGEX
            .captures(str.get_ref())
            .or_else(|| SINGLE_REGEX.captures(str.get_ref()))
            .ok_or_else(|| ConfigError::new("invalid scroll", range.clone()))?;

        let action_str = captures.name("action").expect("action name should exist").as_str();
        let args_str = captures.name("args").map(|a| a.as_str());
        let flags_str = captures.name("flags").unwrap().as_str();
        let (action, flags) =
            self.action_and_flags(action_str, args_str, flags_str, range.clone())?;

        let (alt_action, alt_flags) = if captures.name("alt_action").is_some() {
            let alt_action_str =
                captures.name("alt_action").expect("alt action name should exist").as_str();
            let alt_args_str = captures.name("alt_args").map(|a| a.as_str());
            let alt_flags_str = captures.name("alt_flags").unwrap().as_str();
            let (alt_action, alt_flags) =
                self.action_and_flags(alt_action_str, alt_args_str, alt_flags_str, range.clone())?;

            (alt_action, alt_flags)
        } else {
            let reversed = action
                .reverse()
                .ok_or_else(|| ConfigError::new("Action is not reversible", range.clone()))?
                .into();
            (reversed, vec![])
        };

        let mut rate = Rate::Normal;
        for f in flags.iter().chain(alt_flags.iter()) {
            match f {
                Flag::Rate(r) => rate = *r,
                _ => panic!("Unrecognised flag: {:?}", f),
            }
        }

        Ok(Bind::Scroll { fwd: action.into(), bak: alt_action.into(), rate: rate }.into())
    }

    fn scroll_bind_opt(&self, str: Spanned<String>) -> Option<Rc<Bind>> {
        self.scroll_bind(str).inspect_err(|e| error!("{}", e)).ok()
    }

    #[cfg(test)]
    fn scroll_bind_str(&self, str: &str) -> Result<Rc<Bind>, ConfigError> {
        self.scroll_bind(Spanned::new(0..str.len(), str.to_owned()))
    }

    fn shortcut_bind(&self, str: Spanned<String>) -> Result<Rc<Action>, ConfigError> {
        let range = str.span();
        let captures = SHORTCUT_REGEX
            .captures(str.as_ref())
            .ok_or_else(|| ConfigError::new("failed to match shortcut", range.clone()))?;

        let action_str = captures.name("action").expect("action name shuold exist").as_str();
        let args_str = captures.name("args").map(|a| a.as_str());
        let flags_str = captures.name("flags").unwrap().as_str();
        let (action, flags) =
            self.action_and_flags(action_str, args_str, flags_str, range.clone())?;

        if !flags.is_empty() {
            return Err(ConfigError::new("shortcut binds accept no other flags", range.clone()));
        }

        Ok(action.into())
    }

    fn library(&mut self) -> &mut ActionLibrary {
        self.0
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum Rate {
    Normal,
    Slow,
    Slower,
}

#[derive(Debug)]
pub enum Bind {
    Button(Rc<Action>),
    ButtonUp(Rc<Action>),
    ButtonRepeat(Rc<Action>),
    ButtonAB(Rc<Action>, Rc<Action>),
    Scroll { fwd: Rc<Action>, bak: Rc<Action>, rate: Rate },
}

impl Bind {
    pub fn get_action(&self, reverse: bool) -> &Action {
        match self {
            Bind::Button(action) => action,
            Bind::ButtonUp(action) => action,
            Bind::ButtonRepeat(action) => action,
            Bind::ButtonAB(action_a, action_b) => {
                if reverse {
                    action_a
                } else {
                    action_b
                }
            }
            Bind::Scroll { fwd, bak, rate: _ } => {
                if reverse {
                    fwd
                } else {
                    bak
                }
            }
        }
    }
}

impl fmt::Display for Bind {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Bind::Button(_a) => write!(f, "btn"),
            Bind::ButtonUp(_a) => write!(f, "btnUp"),
            Bind::ButtonRepeat(_a) => write!(f, "btnRep"),
            Bind::ButtonAB(_a, _b) => write!(f, "btn A/B"),
            Bind::Scroll { fwd: _, bak: _, rate } => write!(f, "scroll at {:?}", rate),
        }
    }
}

#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub struct Prime<B> {
    pub side: Option<B>,
    pub side_x2: Option<B>,
    pub top: Option<B>,
    pub top_x2: Option<B>,
    pub tall: Option<B>,
    pub tall_x2: Option<B>,
    pub short: Option<B>,
    pub short_x2: Option<B>,
    pub side_top: Option<B>,
    pub side_tall: Option<B>,
    pub side_short: Option<B>,
    pub top_tall: Option<B>,
    pub top_short: Option<B>,
    pub tall_short: Option<B>,
}

impl Prime<Spanned<String>> {
    pub fn actualize(self, lookup: &Lookup) -> Prime<Rc<Bind>> {
        Prime {
            side: self.side.and_then(|s| lookup.button_bind_opt(s)),
            side_x2: self.side_x2.and_then(|s| lookup.button_bind_opt(s)),
            top: self.top.and_then(|s| lookup.button_bind_opt(s)),
            top_x2: self.top_x2.and_then(|s| lookup.button_bind_opt(s)),
            tall: self.tall.and_then(|s| lookup.button_bind_opt(s)),
            tall_x2: self.tall_x2.and_then(|s| lookup.button_bind_opt(s)),
            short: self.short.and_then(|s| lookup.button_bind_opt(s)),
            short_x2: self.short_x2.and_then(|s| lookup.button_bind_opt(s)),
            side_top: self.side_top.and_then(|s| lookup.button_bind_opt(s)),
            side_tall: self.side_tall.and_then(|s| lookup.button_bind_opt(s)),
            side_short: self.side_short.and_then(|s| lookup.button_bind_opt(s)),
            top_tall: self.top_tall.and_then(|s| lookup.button_bind_opt(s)),
            top_short: self.top_short.and_then(|s| lookup.button_bind_opt(s)),
            tall_short: self.tall_short.and_then(|s| lookup.button_bind_opt(s)),
        }
    }
}

#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub struct DPad<B> {
    pub up: Option<B>,
    pub down: Option<B>,
    pub left: Option<B>,
    pub right: Option<B>,
}

impl DPad<Spanned<String>> {
    pub fn actualize(self, lookup: &Lookup) -> DPad<Rc<Bind>> {
        DPad {
            up: self.up.and_then(|s| lookup.button_bind_opt(s)),
            down: self.down.and_then(|s| lookup.button_bind_opt(s)),
            left: self.left.and_then(|s| lookup.button_bind_opt(s)),
            right: self.right.and_then(|s| lookup.button_bind_opt(s)),
        }
    }
}

#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub struct Kit<B> {
    pub dpad: Option<DPad<B>>,
    pub side_dpad: Option<DPad<B>>,
    pub top_dpad: Option<DPad<B>>,
    pub c1: Option<B>,
    pub c2: Option<B>,
    pub tall_c1: Option<B>,
    pub tall_c2: Option<B>,
    pub short_c1: Option<B>,
    pub short_c2: Option<B>,
    pub tour: Option<B>,
}

impl<B> Kit<B> {
    pub fn dpad_ref(&self) -> Option<&DPad<B>> {
        self.dpad.as_ref()
    }

    pub fn side_dpad_ref(&self) -> Option<&DPad<B>> {
        self.side_dpad.as_ref()
    }

    pub fn top_dpad_ref(&self) -> Option<&DPad<B>> {
        self.top_dpad.as_ref()
    }
}

impl Kit<Spanned<String>> {
    pub fn actualize(self, lookup: &Lookup) -> Kit<Rc<Bind>> {
        Kit {
            dpad: self.dpad.map(|dp| dp.actualize(&lookup)),
            side_dpad: self.side_dpad.map(|dp| dp.actualize(&lookup)),
            top_dpad: self.top_dpad.map(|dp| dp.actualize(&lookup)),
            c1: self.c1.and_then(|s| lookup.button_bind_opt(s)),
            c2: self.c2.and_then(|s| lookup.button_bind_opt(s)),
            tall_c1: self.tall_c1.and_then(|s| lookup.button_bind_opt(s)),
            tall_c2: self.tall_c2.and_then(|s| lookup.button_bind_opt(s)),
            short_c1: self.short_c1.and_then(|s| lookup.button_bind_opt(s)),
            short_c2: self.short_c2.and_then(|s| lookup.button_bind_opt(s)),
            tour: self.tour.and_then(|s| lookup.button_bind_opt(s)),
        }
    }
}

#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub struct Knob<B> {
    pub press: Option<B>,
    pub turn: Option<B>,
    pub side_turn: Option<B>,
    pub top_turn: Option<B>,
    pub tall_turn: Option<B>,
    pub short_turn: Option<B>,
}

impl Knob<Spanned<String>> {
    pub fn actualize(self, lookup: &Lookup) -> Knob<Rc<Bind>> {
        Knob {
            press: self.press.and_then(|s| lookup.button_bind_opt(s)),
            turn: self.turn.and_then(|s| lookup.scroll_bind_opt(s)),
            side_turn: self.side_turn.and_then(|s| lookup.scroll_bind_opt(s)),
            top_turn: self.top_turn.and_then(|s| lookup.scroll_bind_opt(s)),
            tall_turn: self.tall_turn.and_then(|s| lookup.scroll_bind_opt(s)),
            short_turn: self.short_turn.and_then(|s| lookup.scroll_bind_opt(s)),
        }
    }
}

#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub struct Dial<B> {
    pub press: Option<B>,
    pub turn: Option<B>,
}

impl Dial<Spanned<String>> {
    pub fn actualize(self, lookup: &Lookup) -> Dial<Rc<Bind>> {
        Dial {
            press: self.press.and_then(|s| lookup.button_bind_opt(s)),
            turn: self.turn.and_then(|s| lookup.scroll_bind_opt(s)),
        }
    }
}

#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub struct Shortcut<A> {
    pub name: String,
    pub action: A,
}

#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub struct Macro<A> {
    pub name: String,
    pub actions: Vec<A>,
}

#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub struct MacroGroup<A> {
    pub name: String,
    #[serde(default)]
    pub reverse: bool,
    pub groups: Vec<Vec<A>>,
}

#[derive(Deserialize, Clone, Debug, Default, PartialEq)]
pub enum MenuAnchor {
    TopLeft,
    Top,
    TopRight,
    Left,
    #[default]
    Center,
    Right,
    BottomLeft,
    Bottom,
    BottomRight,
}

impl fmt::Display for MenuAnchor {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            MenuAnchor::TopLeft => write!(f, "top-left"),
            MenuAnchor::Top => write!(f, "top"),
            MenuAnchor::TopRight => write!(f, "top-right"),
            MenuAnchor::Left => write!(f, "left"),
            MenuAnchor::Center => write!(f, "center"),
            MenuAnchor::Right => write!(f, "right"),
            MenuAnchor::BottomLeft => write!(f, "bottom-left"),
            MenuAnchor::Bottom => write!(f, "bottom"),
            MenuAnchor::BottomRight => write!(f, "bottom-right"),
        }
    }
}

#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub struct Menu<A> {
    pub name: String,
    pub message: Option<String>,
    #[serde(default)]
    pub anchor: MenuAnchor,
    pub entries: Vec<A>,
    pub select: Option<usize>,
}

#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub struct Layer<B> {
    pub prime: Option<Prime<B>>,
    pub kit: Option<Kit<B>>,
    pub knob: Option<Knob<B>>,
    pub scroll: Option<Knob<B>>,
    pub dial: Option<Dial<B>>,
}

impl<B> Layer<B> {
    pub fn prime_ref(&self) -> Option<&Prime<B>> {
        self.prime.as_ref()
    }

    pub fn kit_ref(&self) -> Option<&Kit<B>> {
        self.kit.as_ref()
    }

    pub fn knob_ref(&self) -> Option<&Knob<B>> {
        self.knob.as_ref()
    }

    pub fn scroll_ref(&self) -> Option<&Knob<B>> {
        self.scroll.as_ref()
    }

    pub fn dial_ref(&self) -> Option<&Dial<B>> {
        self.dial.as_ref()
    }
}

impl Layer<Spanned<String>> {
    pub fn actualize(self, lookup: &Lookup) -> Layer<Rc<Bind>> {
        Layer {
            prime: self.prime.map(|v| v.actualize(&lookup)),
            kit: self.kit.map(|v| v.actualize(&lookup)),
            knob: self.knob.map(|v| v.actualize(&lookup)),
            scroll: self.scroll.map(|v| v.actualize(&lookup)),
            dial: self.dial.map(|v| v.actualize(&lookup)),
        }
    }
}

#[derive(Debug)]
pub struct RawConfig<A, B> {
    pub name: String,
    pub base: Layer<B>,
    pub layers: HashMap<String, Layer<B>>,
    pub shortcuts: HashMap<String, Shortcut<A>>,
    pub macros: HashMap<String, Macro<A>>,
    pub macro_groups: HashMap<String, MacroGroup<A>>,
    pub menus: HashMap<String, Rc<Menu<A>>>,
    pub library: Option<ActionLibrary>,
}

pub type StringConfig = RawConfig<Spanned<String>, Spanned<String>>;
pub type Config = RawConfig<Rc<Action>, Rc<Bind>>;

pub static MENU_LAYER_TOML: &'static str = include_str!("menu_layer.toml");

pub fn generate_menu_layer(lookup: &Lookup) -> Layer<Rc<Bind>> {
    toml::from_str::<Layer<Spanned<String>>>(MENU_LAYER_TOML).unwrap().actualize(lookup)
}

impl Config {
    pub fn lookup_name<'a>(&'a self, action: &'a Action) -> &'a str {
        let base = "base";
        match action {
            Action::None => "None",
            Action::Mod(modifiers) => "[mod]",
            Action::Key(key_code, modifiers) => "[key]",
            Action::PtrMotion(_, _, modifiers) => "[ptr_motion]",
            Action::PtrMotionAbs(_, _, _, _, modifiers) => "[ptr_motion_abs]",
            Action::PtrButton(_, modifiers) => "[ptr_button]",
            Action::PtrAxis(axis, _, modifiers) => "[ptr_axis]",
            Action::PtrAxisDiscrete(axis, _, _, modifiers) => "[ptr_axis_discrete]",
            Action::Shortcut(name) => {
                &self.shortcuts.get(name).expect("shortcut should exist").name
            }
            Action::Macro(name) => &self.macros.get(name).expect("macro should exist").name,
            Action::MacroGroup(name) => {
                &self.macro_groups.get(name).expect("macro group should exist").name
            }
            Action::Menu(name) => &self.menus.get(name).expect("menu should exist").name,
            Action::Layer(name) => name.as_ref().map_or(base, |v| v),
        }
    }
}

impl StringConfig {
    pub fn actualize(self, mut library: ActionLibrary) -> Result<Config, ConfigError> {
        let mut lookup = Lookup(&mut library);

        // actions are loaded into the library first, so they can be referenced in mappings

        for (key, _shortcut) in self.shortcuts.iter() {
            let action = Action::Shortcut(key.clone());
            lookup.library().insert(key.clone(), action.into());
        }

        for (key, _macro) in self.macros.iter() {
            let action = Action::Macro(key.clone());
            lookup.library().insert(key.clone(), action.into());
        }

        for (key, _macro_group) in self.macro_groups.iter() {
            let action = Action::MacroGroup(key.clone());
            lookup.library().insert(key.clone(), action.into());
        }

        for (key, _menu) in self.menus.iter() {
            let action = Action::Menu(key.clone());
            lookup.library().insert(key.clone(), action.into());
        }

        for (key, _layer) in self.layers.iter() {
            let action = Action::Layer(Some(key.clone()));
            lookup.library().insert(key.clone(), action.into());
        }

        let shortcuts: HashMap<_, _> = self
            .shortcuts
            .into_iter()
            .map(|(key, c_shortcut)| {
                let action = lookup.shortcut_bind(c_shortcut.action)?;
                Ok((key, Shortcut { name: c_shortcut.name, action }))
            })
            .collect::<Result<_, _>>()?;

        let macros: HashMap<_, _> = self
            .macros
            .into_iter()
            .map(|(key, c_macro)| {
                let actions = c_macro
                    .actions
                    .into_iter()
                    .map(|a| lookup.shortcut_bind(a))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok((key, Macro { name: c_macro.name, actions }))
            })
            .collect::<Result<_, _>>()?;

        let macro_groups: HashMap<_, _> = self
            .macro_groups
            .into_iter()
            .map(|(key, c_macro_group)| {
                let groups = c_macro_group
                    .groups
                    .into_iter()
                    .map(|v| {
                        v.into_iter().map(|a| lookup.shortcut_bind(a)).collect::<Result<_, _>>()
                    })
                    .collect::<Result<_, _>>()?;
                Ok((
                    key,
                    MacroGroup { name: c_macro_group.name, reverse: c_macro_group.reverse, groups },
                ))
            })
            .collect::<Result<_, _>>()?;

        let menus: HashMap<_, _> = self
            .menus
            .into_iter()
            .map(|(key, c_menu)| {
                let entries: Vec<_> = c_menu
                    .entries
                    .clone()
                    .into_iter()
                    .map(|a| lookup.shortcut_bind(a))
                    .collect::<Result<_, _>>()?;
                let menu = Menu {
                    name: c_menu.name.clone(),
                    message: c_menu.message.clone(),
                    anchor: c_menu.anchor.clone(),
                    entries,
                    select: c_menu.select.clone(),
                };
                Ok((key, menu.into()))
            })
            .collect::<Result<_, _>>()?;

        // everything after this point has access to all of the config's actions

        let base = self.base.actualize(&lookup);

        let mut layers = self
            .layers
            .into_iter()
            .map(|(key, layer)| {
                if key == "menu" {
                    panic!("layer cannot be named 'menu'")
                }
                (key.to_string(), layer.actualize(&lookup))
            })
            .collect::<HashMap<_, _>>();

        layers.extend(HashMap::from([("menu".to_string(), generate_menu_layer(&lookup))]));

        Ok(Config {
            name: self.name,
            base: base,
            layers,
            shortcuts,
            macros,
            macro_groups,
            menus,
            library: Some(library),
        })
    }
}

/// Manual implementation until `toml::Spanned` works alongside `#[serde(flatten)]`
/// See: <https://github.com/toml-rs/toml/issues/589>
impl<'de, A, B> Deserialize<'de> for RawConfig<A, B>
where
    A: serde::Deserialize<'de>,
    B: serde::Deserialize<'de>,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(field_identifier, rename_all = "snake_case")]
        enum Field {
            Name,
            Prime,
            Kit,
            Knob,
            Scroll,
            Dial,
            Layer,
            Shortcut,
            Macro,
            MacroGroup,
            Menu,
        }

        struct RawConfigVisitor<A, B> {
            a: PhantomData<A>,
            b: PhantomData<B>,
        }

        impl<'de, A, B> Visitor<'de> for RawConfigVisitor<A, B>
        where
            A: serde::Deserialize<'de>,
            B: serde::Deserialize<'de>,
        {
            type Value = RawConfig<A, B>;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("struct RawConfig")
            }

            fn visit_map<V>(self, mut map: V) -> Result<Self::Value, V::Error>
            where
                V: serde::de::MapAccess<'de>,
            {
                let mut name = None;
                let mut prime = None;
                let mut kit = None;
                let mut knob = None;
                let mut scroll = None;
                let mut dial = None;
                let mut layers = None;
                let mut shortcuts = None;
                let mut macros = None;
                let mut macro_groups = None;
                let mut menus = None;
                while let Some(key) = map.next_key()? {
                    match key {
                        Field::Name => {
                            if name.is_some() {
                                return Err(de::Error::duplicate_field("name"));
                            }
                            name = Some(map.next_value()?);
                        }
                        Field::Prime => {
                            if prime.is_some() {
                                return Err(de::Error::duplicate_field("prime"));
                            }
                            prime = Some(map.next_value()?);
                        }
                        Field::Kit => {
                            if kit.is_some() {
                                return Err(de::Error::duplicate_field("kit"));
                            }
                            kit = Some(map.next_value()?);
                        }
                        Field::Knob => {
                            if knob.is_some() {
                                return Err(de::Error::duplicate_field("knob"));
                            }
                            knob = Some(map.next_value()?);
                        }
                        Field::Scroll => {
                            if scroll.is_some() {
                                return Err(de::Error::duplicate_field("scroll"));
                            }
                            scroll = Some(map.next_value()?);
                        }
                        Field::Dial => {
                            if dial.is_some() {
                                return Err(de::Error::duplicate_field("dial"));
                            }
                            dial = Some(map.next_value()?);
                        }
                        Field::Layer => {
                            if layers.is_some() {
                                return Err(de::Error::duplicate_field("layer"));
                            }
                            layers = Some(map.next_value()?);
                        }
                        Field::Shortcut => {
                            if shortcuts.is_some() {
                                return Err(de::Error::duplicate_field("shortcut"));
                            }
                            shortcuts = Some(map.next_value()?);
                        }
                        Field::Macro => {
                            if macros.is_some() {
                                return Err(de::Error::duplicate_field("macro"));
                            }
                            macros = Some(map.next_value()?);
                        }
                        Field::MacroGroup => {
                            if macro_groups.is_some() {
                                return Err(de::Error::duplicate_field("macro_group"));
                            }
                            macro_groups = Some(map.next_value()?);
                        }
                        Field::Menu => {
                            if menus.is_some() {
                                return Err(de::Error::duplicate_field("menu"));
                            }
                            menus = Some(
                                map.next_value::<HashMap<_, Menu<_>>>()?
                                    .into_iter()
                                    .map(|(k, v)| (k, Rc::new(v)))
                                    .collect(),
                            );
                        }
                    }
                }
                let name = name.ok_or_else(|| de::Error::missing_field("name"))?;
                Ok(RawConfig {
                    name: name,
                    base: Layer { prime, kit, knob, scroll, dial },
                    layers: layers.unwrap_or_else(|| HashMap::new()),
                    shortcuts: shortcuts.unwrap_or_else(|| HashMap::new()),
                    macros: macros.unwrap_or_else(|| HashMap::new()),
                    macro_groups: macro_groups.unwrap_or_else(|| HashMap::new()),
                    menus: menus.unwrap_or_else(|| HashMap::new()),
                    library: None,
                })
            }
        }

        const FIELDS: &[&str] =
            &["name", "prime", "kit", "knob", "scroll", "dial", "layer", "shortcuts", "macro"];
        deserializer.deserialize_struct(
            "RawConfig",
            FIELDS,
            RawConfigVisitor { a: PhantomData, b: PhantomData },
        )
    }
}

pub struct ConfigManager {
    default_library: Rc<ActionLibrary>,
    default_config: Rc<Config>,
    configs: Vec<Rc<Config>>,
}

impl ConfigManager {
    pub fn new() -> ConfigManager {
        let default_library = Rc::new(ActionLibrary::default());
        let default_config = ConfigManager::load_default_config(default_library.clone());

        ConfigManager {
            default_library: default_library,
            default_config: Rc::new(default_config),
            configs: Vec::new(),
        }
    }

    pub fn load_config(&mut self, path: PathBuf) -> String {
        let library = ActionLibrary::new(Some(self.default_library.clone()));
        let str = fs::read_to_string(path).expect("config should be readable");
        let config = toml::from_str::<StringConfig>(&str)
            .expect("config should load")
            .actualize(library)
            .map_err(|e| e.with_exact_loc(&str))
            .unwrap();
        let name = config.name.clone();
        self.configs.push(Rc::new(config));
        name
    }

    pub fn get_default_config(&self) -> Rc<Config> {
        self.default_config.clone()
    }

    pub fn get_config(&self, name: &str) -> Option<Rc<Config>> {
        self.configs.iter().find(|c| c.name.eq(name)).map(|c| c.clone())
    }

    fn load_default_config(library: Rc<ActionLibrary>) -> Config {
        let library = ActionLibrary::new(Some(library));
        toml::from_str::<StringConfig>(DEFAULT_CONFIG_TOML).unwrap().actualize(library).unwrap()
    }
}

pub static DEFAULT_CONFIG_TOML: &'static str = include_str!("default.toml");

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::{Axis, Modifiers};
    use evdev::KeyCode;

    const CONFIG_1: &'static str = include_str!("test_config_1.toml");

    const BUTTON_BIND_MAX: &str = "a_b_1 (arg_1, arg_2, arg_3) :flag_1 :flag_2 :flag_3";
    const SCROLL_BIND_MAX: &str =
        "a_b_1 (arg_1, arg_2, 1) / c_d_2 (arg_3, arg_4, 2) :flag_1 :flag_2 :flag_3";

    #[test]
    fn regexes_match_examples() {
        assert!(SINGLE_REGEX.is_match("a") == true);
        assert!(SINGLE_REGEX.is_match("a:up:rep") == true);
        assert!(SINGLE_REGEX.is_match("ptr_x(val):rep") == true);
        assert!(SINGLE_REGEX.is_match("ptr_x(val):rep") == true);
        assert!(SINGLE_REGEX.is_match("ptr_wheel") == true);
        assert!(SINGLE_REGEX.is_match("ptr_wheel:rev") == true);
        assert!(SINGLE_REGEX.is_match(BUTTON_BIND_MAX) == true);
        assert!(DOUBLE_REGEX.is_match("a/b") == true);
        assert!(DOUBLE_REGEX.is_match("a/b:slow") == true);
        assert!(DOUBLE_REGEX.is_match("a/b:slower") == true);
        assert!(DOUBLE_REGEX.is_match(SCROLL_BIND_MAX) == true);
    }

    #[test]
    fn regexes_return_captures() {
        let button_match = SINGLE_REGEX.captures(BUTTON_BIND_MAX).expect("match should succeed");
        assert_eq!(button_match.name("action").unwrap().as_str(), "a_b_1");

        assert!(button_match.name("args").is_some());
        let button_args_match = ARGUMENT_REGEX
            .find_iter(button_match.name("args").unwrap().as_str())
            .collect::<Vec<_>>();
        assert_eq!(button_args_match.len(), 3);
        assert_eq!(button_args_match.get(0).unwrap().as_str(), "arg_1");
        assert_eq!(button_args_match.get(1).unwrap().as_str(), "arg_2");
        assert_eq!(button_args_match.get(2).unwrap().as_str(), "arg_3");

        assert!(button_match.name("flags").is_some());
        let button_flags_match =
            FLAG_REGEX.find_iter(button_match.name("flags").unwrap().as_str()).collect::<Vec<_>>();
        assert_eq!(button_flags_match.len(), 3);
        assert_eq!(button_flags_match.get(0).unwrap().as_str(), ":flag_1");
        assert_eq!(button_flags_match.get(1).unwrap().as_str(), ":flag_2");
        assert_eq!(button_flags_match.get(2).unwrap().as_str(), ":flag_3");

        let scroll_match = DOUBLE_REGEX.captures(SCROLL_BIND_MAX).expect("match should succeed");

        assert!(scroll_match.name("action").unwrap().as_str() == "a_b_1");

        assert!(scroll_match.name("args").is_some());
        let scroll_args_match = ARGUMENT_REGEX
            .find_iter(scroll_match.name("args").unwrap().as_str())
            .collect::<Vec<_>>();
        assert_eq!(scroll_args_match.len(), 3);
        assert_eq!(scroll_args_match.get(0).unwrap().as_str(), "arg_1");
        assert_eq!(scroll_args_match.get(1).unwrap().as_str(), "arg_2");
        assert_eq!(scroll_args_match.get(2).unwrap().as_str(), "1");

        assert!(scroll_match.name("alt_action").unwrap().as_str() == "c_d_2");

        assert!(scroll_match.name("alt_args").is_some());
        let scroll_args_match = ARGUMENT_REGEX
            .find_iter(scroll_match.name("alt_args").unwrap().as_str())
            .collect::<Vec<_>>();
        assert_eq!(scroll_args_match.len(), 3);
        assert_eq!(scroll_args_match.get(0).unwrap().as_str(), "arg_3");
        assert_eq!(scroll_args_match.get(1).unwrap().as_str(), "arg_4");
        assert_eq!(scroll_args_match.get(2).unwrap().as_str(), "2");

        assert!(scroll_match.name("alt_flags").is_some());
        let scroll_flags_match = FLAG_REGEX
            .find_iter(scroll_match.name("alt_flags").unwrap().as_str())
            .collect::<Vec<_>>();
        assert_eq!(scroll_flags_match.len(), 3);
        assert_eq!(scroll_flags_match.get(0).unwrap().as_str(), ":flag_1");
        assert_eq!(scroll_flags_match.get(1).unwrap().as_str(), ":flag_2");
        assert_eq!(scroll_flags_match.get(2).unwrap().as_str(), ":flag_3");
    }

    #[test]
    fn lookup_returns_bindings() {
        let mut library = ActionLibrary::new(None);
        library.insert(
            "x".to_string(),
            Action::Key(KeyCode::KEY_X, Some(Modifiers::default())).into(),
        );
        let lookup = Lookup(&mut library);

        assert!(lookup.button_bind_str("x(1):S:rightalt:up").is_ok());
        assert!(lookup.button_bind_str("x:D").is_err());
        assert!(lookup.button_bind_str("x(1)/x(2)").is_ok());
        assert!(lookup.scroll_bind_str("x(1):alt/x(2):slower:M").is_ok());
        assert!(lookup.scroll_bind_str("x(1):up").is_err());
    }

    #[test]
    fn parses_full_config() {
        toml::from_str::<StringConfig>(CONFIG_1).unwrap();
    }

    #[test]
    fn actualizes_full_config() {
        let config: StringConfig = toml::from_str(CONFIG_1).unwrap();
        let mut library = ActionLibrary::new(None);
        library.insert(
            "b".to_string(),
            Action::Key(KeyCode::KEY_X, Some(Modifiers::default())).into(),
        );
        library.insert("t".to_string(), Action::PtrAxis(Axis::VerticalScroll, 20.0, None).into());
        library.insert("a".to_string(), Action::None.into());
        library.insert("b".to_string(), Action::None.into());
        library.insert("c".to_string(), Action::None.into());
        config.actualize(library).unwrap();
    }
}
