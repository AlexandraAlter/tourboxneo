use std::{collections::HashMap, fmt, fs, hash::Hash, path::PathBuf, rc::Rc, str::FromStr};

use heck::ToTitleCase;
use log::info;

use lazy_static::lazy_static;
use regex::Regex;
use serde::{
    Deserialize,
    de::{self, Visitor},
};

use crate::{
    actions::{Action, ActionLibrary, Modifiers},
    serial::{Code, CodeCategory},
};

#[derive(Debug, Clone)]
pub struct ConfigError {
    msg: String,
    path: Vec<String>,
}

impl ConfigError {
    pub fn new(msg: String) -> Self {
        Self { msg: msg, path: Vec::new() }
    }

    pub fn with_path(mut self, value: &str) -> Self {
        self.path.insert(0, value.to_string());
        self
    }

    pub fn path_or_dot(&self) -> String {
        if self.path.is_empty() { ".".to_owned() } else { self.path.join(".") }
    }
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "Config error at {}: {}", self.path_or_dot(), self.msg)
    }
}

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
    static ref SERIAL_CODE_REGEX: Regex = {
        Regex::new(
            r"(?x)
                ^\s*(?P<code>[[:word:]]+)\s*x(?P<count>[[:digit:]]+)\s*$
            ",
        )
        .expect("regex should compile")
    };
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
    fn action(&self, str: &str, args: Option<Vec<String>>) -> Result<Rc<Action>, ConfigError> {
        self.0
            .get(str.as_ref(), &args)
            .ok_or_else(|| ConfigError::new(format!("action not found in library: {}", str)))
    }

    fn args(&self, args: Option<&str>) -> Option<Vec<String>> {
        match args {
            Some(args) => Some(
                ARGUMENT_REGEX.find_iter(args).map(|m| m.as_str().to_owned()).collect::<Vec<_>>(),
            ),
            None => None,
        }
    }

    fn flags(&self, flags: &str) -> Result<Vec<Flag>, ConfigError> {
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
            // TODO: pass the exact flag in
            return Err(ConfigError::new(format!("unrecogized flag: {}", flags)));
        }
        let flag_vec: Vec<_> = flag_vec.iter().flatten().cloned().collect();
        if flag_vec.iter().filter(|f| vec![Flag::Up, Flag::Repeat].contains(*f)).count() > 1 {
            return Err(ConfigError::new("multiple mode-setting flags".to_owned()));
        }
        if flag_vec
            .iter()
            .filter(|f| vec![Flag::Rate(Rate::Slow), Flag::Rate(Rate::Slower)].contains(*f))
            .count()
            > 1
        {
            return Err(ConfigError::new("multiple rate flags".to_owned()));
        }
        Ok(flag_vec)
    }

    fn action_and_flags(
        &self,
        action_str: &str,
        args_str: Option<&str>,
        flags_str: &str,
    ) -> Result<(Rc<Action>, Vec<Flag>), ConfigError> {
        let args = self.args(args_str);
        let mut action = self.action(action_str, args)?;
        let flags = self.flags(flags_str)?;

        let mut mods = Vec::new();
        let mut non_mods_flags = Vec::new();
        for flag in flags {
            match flag {
                Flag::Mod(modifiers) => mods.push(modifiers),
                Flag::Rev => {
                    action = action
                        .reverse()
                        .ok_or_else(|| {
                            ConfigError::new(format!("action is not reversible: {}", action_str))
                        })?
                        .into()
                }
                _ => non_mods_flags.push(flag),
            }
        }

        if !mods.is_empty() {
            let modifiers = mods.iter().fold(Modifiers::default(), |acc, m| acc.union(m));
            action = action
                .with_modifiers(&modifiers)
                .ok_or_else(|| {
                    ConfigError::new(format!("action cannot be modified: {}", action_str))
                })?
                .into()
        }

        Ok((action, non_mods_flags))
    }

    fn button_bind(&self, str: &str) -> Result<Rc<Bind>, ConfigError> {
        let captures = DOUBLE_REGEX
            .captures(str)
            .or_else(|| SINGLE_REGEX.captures(str))
            .ok_or_else(|| ConfigError::new(format!("failed to match button: {}", str)))?;

        let action_str = captures.name("action").expect("action name should exist").as_str();
        let args_str = captures.name("args").map(|a| a.as_str());
        let flags_str = captures.name("flags").unwrap().as_str();
        let (action, flags) = self.action_and_flags(action_str, args_str, flags_str)?;

        // If we've got an alt_action or a :rev flag, we're parsing an AB button
        if captures.name("alt_action").is_some() {
            let alt_action_str =
                captures.name("alt_action").expect("alt action name should exist").as_str();
            let alt_args_str = captures.name("alt_args").map(|a| a.as_str());
            let alt_flags_str = captures.name("alt_flags").unwrap().as_str();
            let (alt_action, alt_flags) =
                self.action_and_flags(alt_action_str, alt_args_str, alt_flags_str)?;

            if !alt_flags.is_empty() {
                return Err(ConfigError::new("AB binds accept no other flags".to_owned()));
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

    fn scroll_bind(&self, str: &str) -> Result<Rc<Bind>, ConfigError> {
        let captures = DOUBLE_REGEX
            .captures(str)
            .or_else(|| SINGLE_REGEX.captures(str))
            .ok_or_else(|| ConfigError::new(format!("failed to match scroll: {}", str)))?;

        let action_str = captures.name("action").expect("action name should exist").as_str();
        let args_str = captures.name("args").map(|a| a.as_str());
        let flags_str = captures.name("flags").unwrap().as_str();
        let (action, flags) = self.action_and_flags(action_str, args_str, flags_str)?;

        let (alt_action, alt_flags) = if captures.name("alt_action").is_some() {
            let alt_action_str =
                captures.name("alt_action").expect("alt action name should exist").as_str();
            let alt_args_str = captures.name("alt_args").map(|a| a.as_str());
            let alt_flags_str = captures.name("alt_flags").unwrap().as_str();
            let (alt_action, alt_flags) =
                self.action_and_flags(alt_action_str, alt_args_str, alt_flags_str)?;

            (alt_action, alt_flags)
        } else {
            let reversed = action
                .reverse()
                .ok_or_else(|| {
                    ConfigError::new(format!("action is not reversible: {}", action_str))
                })?
                .into();
            (reversed, vec![])
        };

        let mut rate = Rate::Normal;
        for f in flags.iter().chain(alt_flags.iter()) {
            match f {
                Flag::Rate(r) => rate = *r,
                // TODO make this not panic
                _ => panic!("unrecognised flag: {:?}", f),
            }
        }

        Ok(Bind::Scroll { fwd: action.into(), bak: alt_action.into(), rate: rate }.into())
    }

    fn shortcut_bind(&self, str: &str) -> Result<Rc<Action>, ConfigError> {
        let captures = SHORTCUT_REGEX
            .captures(str.as_ref())
            .ok_or_else(|| ConfigError::new(format!("failed to match shortcut: {}", str)))?;

        let action_str = captures.name("action").expect("action name should exist").as_str();
        let args_str = captures.name("args").map(|a| a.as_str());
        let flags_str = captures.name("flags").unwrap().as_str();
        let (action, flags) = self.action_and_flags(action_str, args_str, flags_str)?;

        if !flags.is_empty() {
            return Err(ConfigError::new("shortcut binds accept no other flags".to_owned()));
        }

        Ok(action.into())
    }

    fn custom_bind(
        &self,
        code_str: &str,
        bind_str: &str,
    ) -> Result<(CustomCode, Rc<Bind>), ConfigError> {
        let code = CustomCode::from_str(code_str)?;
        let bind =
            if code.is_scroll() { self.scroll_bind(bind_str) } else { self.button_bind(bind_str) };
        Ok((code, bind.map_err(|e| e.with_path(code_str))?))
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

#[derive(Deserialize, Default, Debug)]
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

impl Prime<String> {
    pub fn actualize(self, lookup: &Lookup) -> Result<Prime<Rc<Bind>>, ConfigError> {
        let side = self
            .side
            .and_then(|s| Some(lookup.button_bind(&s)))
            .transpose()
            .map_err(|e| e.with_path("side"))?;
        let side_x2 = self
            .side_x2
            .and_then(|s| Some(lookup.button_bind(&s)))
            .transpose()
            .map_err(|e| e.with_path("side_x2"))?;
        let top = self
            .top
            .and_then(|s| Some(lookup.button_bind(&s)))
            .transpose()
            .map_err(|e| e.with_path("top"))?;
        let top_x2 = self
            .top_x2
            .and_then(|s| Some(lookup.button_bind(&s)))
            .transpose()
            .map_err(|e| e.with_path("top_x2"))?;
        let tall = self
            .tall
            .and_then(|s| Some(lookup.button_bind(&s)))
            .transpose()
            .map_err(|e| e.with_path("tall"))?;
        let tall_x2 = self
            .tall_x2
            .and_then(|s| Some(lookup.button_bind(&s)))
            .transpose()
            .map_err(|e| e.with_path("tall_x2"))?;
        let short = self
            .short
            .and_then(|s| Some(lookup.button_bind(&s)))
            .transpose()
            .map_err(|e| e.with_path("short"))?;
        let short_x2 = self
            .short_x2
            .and_then(|s| Some(lookup.button_bind(&s)))
            .transpose()
            .map_err(|e| e.with_path("short_x2"))?;
        let side_top = self
            .side_top
            .and_then(|s| Some(lookup.button_bind(&s)))
            .transpose()
            .map_err(|e| e.with_path("side_top"))?;
        let side_tall = self
            .side_tall
            .and_then(|s| Some(lookup.button_bind(&s)))
            .transpose()
            .map_err(|e| e.with_path("side_tall"))?;
        let side_short = self
            .side_short
            .and_then(|s| Some(lookup.button_bind(&s)))
            .transpose()
            .map_err(|e| e.with_path("side_short"))?;
        let top_tall = self
            .top_tall
            .and_then(|s| Some(lookup.button_bind(&s)))
            .transpose()
            .map_err(|e| e.with_path("top_tall"))?;
        let top_short = self
            .top_short
            .and_then(|s| Some(lookup.button_bind(&s)))
            .transpose()
            .map_err(|e| e.with_path("top_short"))?;
        let tall_short = self
            .tall_short
            .and_then(|s| Some(lookup.button_bind(&s)))
            .transpose()
            .map_err(|e| e.with_path("tall_short"))?;

        Ok(Prime {
            side,
            side_x2,
            top,
            top_x2,
            tall,
            tall_x2,
            short,
            short_x2,
            side_top,
            side_tall,
            side_short,
            top_tall,
            top_short,
            tall_short,
        })
    }
}

#[derive(Deserialize, Default, Debug)]
#[serde(deny_unknown_fields)]
pub struct DPad<B> {
    pub up: Option<B>,
    pub down: Option<B>,
    pub left: Option<B>,
    pub right: Option<B>,
}

impl DPad<String> {
    pub fn actualize(self, lookup: &Lookup) -> Result<DPad<Rc<Bind>>, ConfigError> {
        let up = self
            .up
            .and_then(|s| Some(lookup.button_bind(&s)))
            .transpose()
            .map_err(|e| e.with_path("up"))?;
        let down = self
            .down
            .and_then(|s| Some(lookup.button_bind(&s)))
            .transpose()
            .map_err(|e| e.with_path("down"))?;
        let left = self
            .left
            .and_then(|s| Some(lookup.button_bind(&s)))
            .transpose()
            .map_err(|e| e.with_path("left"))?;
        let right = self
            .right
            .and_then(|s| Some(lookup.button_bind(&s)))
            .transpose()
            .map_err(|e| e.with_path("right"))?;

        Ok(DPad { up, down, left, right })
    }
}

#[derive(Deserialize, Default, Debug)]
#[serde(deny_unknown_fields)]
pub struct Kit<B> {
    #[serde(default)]
    pub dpad: DPad<B>,
    #[serde(default)]
    pub side_dpad: DPad<B>,
    #[serde(default)]
    pub top_dpad: DPad<B>,
    pub c1: Option<B>,
    pub c2: Option<B>,
    pub tall_c1: Option<B>,
    pub tall_c2: Option<B>,
    pub short_c1: Option<B>,
    pub short_c2: Option<B>,
    pub tour: Option<B>,
}

impl Kit<String> {
    pub fn actualize(self, lookup: &Lookup) -> Result<Kit<Rc<Bind>>, ConfigError> {
        let dpad = self.dpad.actualize(&lookup).map_err(|e| e.with_path("dpad"))?;
        let side_dpad = self.side_dpad.actualize(&lookup).map_err(|e| e.with_path("side_dpad"))?;
        let top_dpad = self.top_dpad.actualize(&lookup).map_err(|e| e.with_path("top_dpad"))?;
        let c1 = self
            .c1
            .and_then(|s| Some(lookup.button_bind(&s)))
            .transpose()
            .map_err(|e| e.with_path("c1"))?;
        let c2 = self
            .c2
            .and_then(|s| Some(lookup.button_bind(&s)))
            .transpose()
            .map_err(|e| e.with_path("c2"))?;
        let tall_c1 = self
            .tall_c1
            .and_then(|s| Some(lookup.button_bind(&s)))
            .transpose()
            .map_err(|e| e.with_path("tall_c1"))?;
        let tall_c2 = self
            .tall_c2
            .and_then(|s| Some(lookup.button_bind(&s)))
            .transpose()
            .map_err(|e| e.with_path("tall_c2"))?;
        let short_c1 = self
            .short_c1
            .and_then(|s| Some(lookup.button_bind(&s)))
            .transpose()
            .map_err(|e| e.with_path("short_c1"))?;
        let short_c2 = self
            .short_c2
            .and_then(|s| Some(lookup.button_bind(&s)))
            .transpose()
            .map_err(|e| e.with_path("short_c2"))?;
        let tour = self
            .tour
            .and_then(|s| Some(lookup.button_bind(&s)))
            .transpose()
            .map_err(|e| e.with_path("tour"))?;

        Ok(Kit { dpad, side_dpad, top_dpad, c1, c2, tall_c1, tall_c2, short_c1, short_c2, tour })
    }
}

#[derive(Deserialize, Default, Debug)]
#[serde(deny_unknown_fields)]
pub struct Knob<B> {
    pub press: Option<B>,
    pub turn: Option<B>,
    pub side_turn: Option<B>,
    pub top_turn: Option<B>,
    pub tall_turn: Option<B>,
    pub short_turn: Option<B>,
}

impl Knob<String> {
    pub fn actualize(self, lookup: &Lookup) -> Result<Knob<Rc<Bind>>, ConfigError> {
        let press = self
            .press
            .and_then(|s| Some(lookup.button_bind(&s)))
            .transpose()
            .map_err(|e| e.with_path("press"))?;
        let turn = self
            .turn
            .and_then(|s| Some(lookup.scroll_bind(&s)))
            .transpose()
            .map_err(|e| e.with_path("turn"))?;
        let side_turn = self
            .side_turn
            .and_then(|s| Some(lookup.scroll_bind(&s)))
            .transpose()
            .map_err(|e| e.with_path("side_turn"))?;
        let top_turn = self
            .top_turn
            .and_then(|s| Some(lookup.scroll_bind(&s)))
            .transpose()
            .map_err(|e| e.with_path("top_turn"))?;
        let tall_turn = self
            .tall_turn
            .and_then(|s| Some(lookup.scroll_bind(&s)))
            .transpose()
            .map_err(|e| e.with_path("tall_turn"))?;
        let short_turn = self
            .short_turn
            .and_then(|s| Some(lookup.scroll_bind(&s)))
            .transpose()
            .map_err(|e| e.with_path("short_turn"))?;

        Ok(Knob { press, turn, side_turn, top_turn, tall_turn, short_turn })
    }
}

#[derive(Deserialize, Default, Debug)]
#[serde(deny_unknown_fields)]
pub struct Dial<B> {
    pub press: Option<B>,
    pub turn: Option<B>,
}

impl Dial<String> {
    pub fn actualize(self, lookup: &Lookup) -> Result<Dial<Rc<Bind>>, ConfigError> {
        let press = self
            .press
            .and_then(|s| Some(lookup.button_bind(&s)))
            .transpose()
            .map_err(|e| e.with_path("press"))?;
        let turn = self
            .turn
            .and_then(|s| Some(lookup.scroll_bind(&s)))
            .transpose()
            .map_err(|e| e.with_path("turn"))?;

        Ok(Dial { press, turn })
    }
}

#[derive(Debug)]
pub struct Shortcut<Name, Action> {
    pub name: Name,
    pub action: Action,
}

impl<'de> Deserialize<'de> for Shortcut<Option<String>, String> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(field_identifier, rename_all = "snake_case")]
        enum Field {
            Name,
            Action,
        }

        struct ShortcutVisitor;

        impl<'de> Visitor<'de> for ShortcutVisitor {
            type Value = Shortcut<Option<String>, String>;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("struct Shortcut or string")
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                Ok(Shortcut { name: None, action: value.to_owned() })
            }

            fn visit_map<V>(self, mut map: V) -> Result<Self::Value, V::Error>
            where
                V: serde::de::MapAccess<'de>,
            {
                let mut name = None;
                let mut action = None;
                while let Some(key) = map.next_key()? {
                    match key {
                        Field::Name => {
                            if name.is_some() {
                                return Err(de::Error::duplicate_field("name"));
                            }
                            name = Some(map.next_value()?);
                        }
                        Field::Action => {
                            if action.is_some() {
                                return Err(de::Error::duplicate_field("action"));
                            }
                            action = Some(map.next_value()?);
                        }
                    }
                }
                let name = name.ok_or_else(|| de::Error::missing_field("name"))?;
                let action = action.ok_or_else(|| de::Error::missing_field("action"))?;
                Ok(Shortcut { name: name, action: action })
            }
        }

        const FIELDS: &[&str] = &["name", "action"];
        deserializer.deserialize_struct("Shortcut", FIELDS, ShortcutVisitor)
    }
}

#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub struct Macro<Name, Action> {
    #[serde(default)]
    pub name: Name,
    pub actions: Vec<Action>,
}

#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub struct MacroGroup<Name, Action> {
    #[serde(default)]
    pub name: Name,
    #[serde(default)]
    pub reverse: bool,
    pub groups: Vec<Vec<Action>>,
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
pub struct Menu<Name, Action> {
    #[serde(default)]
    pub name: Name,
    pub message: Option<String>,
    #[serde(default)]
    pub anchor: MenuAnchor,
    pub entries: Vec<Action>,
    pub select: Option<usize>,
}

// only used for custom codes, so we only cover the base keys
impl FromStr for Code {
    type Err = ConfigError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "tall" => Ok(Code::Tall),
            "side" => Ok(Code::Side),
            "top" => Ok(Code::Top),
            "short" => Ok(Code::Short),

            "tour" => Ok(Code::Tour),

            "up" => Ok(Code::Up),
            "down" => Ok(Code::Down),
            "left" => Ok(Code::Left),
            "right" => Ok(Code::Right),

            "c1" => Ok(Code::C1),
            "c2" => Ok(Code::C2),

            "knob_down" => Ok(Code::KnobButton),
            "knob" => Ok(Code::Knob),

            "scroll_down" => Ok(Code::ScrollButton),
            "scroll" => Ok(Code::Scroll),

            "dial_down" => Ok(Code::DialButton),
            "dial" => Ok(Code::Dial),

            _ => Err(ConfigError::new(format!("invalid custom code {}", s))),
        }
    }
}

#[derive(PartialEq, Eq, Hash, Debug)]
pub enum CustomCode {
    /// repeated tap of one input
    Series(Code, usize),
    /// several tapped inputs
    Parallel(Vec<Code>),
}

impl CustomCode {
    pub fn is_scroll(&self) -> bool {
        match self {
            CustomCode::Series(_, _) => false,
            CustomCode::Parallel(codes) => {
                codes.iter().find(|c| c.category() == CodeCategory::Scroll).is_some()
            }
        }
    }
}

impl FromStr for CustomCode {
    type Err = ConfigError;

    fn from_str(str: &str) -> Result<Self, Self::Err> {
        match SERIAL_CODE_REGEX.captures(str.as_ref()) {
            Some(captures) => {
                let code_str = captures.name("code").expect("code should be present").as_str();
                let code = Code::from_str(code_str)?;
                let count_str = captures.name("count").expect("count should be present").as_str();
                let count = usize::from_str(count_str).map_err(|_e| {
                    ConfigError::new(format!("unable to parse custom code's count: {}", count_str))
                })?;
                Ok(CustomCode::Series(code, count))
            }
            None => {
                let mut components =
                    str.split("+")
                        .map(|c| Code::from_str(c))
                        .collect::<Result<Vec<_>, ConfigError>>()?;
                components.sort();
                if Code::is_impossible_combo(&components) {
                    return Err(ConfigError::new(format!(
                        "custom code is impossible to input: {}",
                        str
                    )));
                }
                if Code::is_builtin_combo(&components) {
                    return Err(ConfigError::new(format!(
                        "custom code overlaps a built-in code: {}",
                        str
                    )));
                }
                Ok(CustomCode::Parallel(components))
            }
        }
    }
}

#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub struct Layer<Name, Custom, Bind>
where
    Custom: Eq + Hash,
{
    #[serde(default)]
    pub name: Name,
    #[serde(default)]
    pub prime: Prime<Bind>,
    #[serde(default)]
    pub kit: Kit<Bind>,
    #[serde(default)]
    pub knob: Knob<Bind>,
    #[serde(default)]
    pub scroll: Knob<Bind>,
    #[serde(default)]
    pub dial: Dial<Bind>,
    #[serde(default)]
    pub custom: HashMap<Custom, Bind>,
}

impl Layer<(), String, String> {
    pub fn actualize(
        self,
        lookup: &Lookup,
    ) -> Result<Layer<(), CustomCode, Rc<Bind>>, ConfigError> {
        let prime = self.prime.actualize(&lookup).map_err(|e| e.with_path("prime"))?;
        let kit = self.kit.actualize(&lookup).map_err(|e| e.with_path("kit"))?;
        let knob = self.knob.actualize(&lookup).map_err(|e| e.with_path("knob"))?;
        let scroll = self.scroll.actualize(&lookup).map_err(|e| e.with_path("scroll"))?;
        let dial = self.dial.actualize(&lookup).map_err(|e| e.with_path("dial"))?;
        let custom = self
            .custom
            .into_iter()
            .map(|(k, v)| lookup.custom_bind(&k, &v))
            .collect::<Result<HashMap<_, _>, ConfigError>>()
            .map_err(|e| e.with_path("custom"))?;

        Ok(Layer { name: (), prime, kit, knob, scroll, dial, custom })
    }
}

impl Layer<Option<String>, String, String> {
    pub fn actualize(
        self,
        key: &str,
        lookup: &Lookup,
    ) -> Result<Layer<String, CustomCode, Rc<Bind>>, ConfigError> {
        let name = self.name.unwrap_or_else(|| key.to_title_case());
        let prime = self.prime.actualize(&lookup).map_err(|e| e.with_path("prime"))?;
        let kit = self.kit.actualize(&lookup).map_err(|e| e.with_path("kit"))?;
        let knob = self.knob.actualize(&lookup).map_err(|e| e.with_path("knob"))?;
        let scroll = self.scroll.actualize(&lookup).map_err(|e| e.with_path("scroll"))?;
        let dial = self.dial.actualize(&lookup).map_err(|e| e.with_path("dial"))?;
        let custom = self
            .custom
            .into_iter()
            .map(|(k, v)| lookup.custom_bind(&k, &v))
            .collect::<Result<HashMap<_, _>, ConfigError>>()
            .map_err(|e| e.with_path("custom"))?;

        Ok(Layer { name, prime, kit, knob, scroll, dial, custom })
    }
}

/// As deserialized from the TOML file
#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub struct TomlConfig {
    pub name: String,
    #[serde(flatten)]
    pub base: Layer<(), String, String>,
    #[serde(default = "HashMap::new")]
    pub layers: HashMap<String, Layer<Option<String>, String, String>>,
    #[serde(default = "HashMap::new")]
    pub shortcuts: HashMap<String, Shortcut<Option<String>, String>>,
    #[serde(default = "HashMap::new")]
    pub macros: HashMap<String, Macro<Option<String>, String>>,
    #[serde(default = "HashMap::new")]
    pub macro_groups: HashMap<String, MacroGroup<Option<String>, String>>,
    #[serde(default = "HashMap::new")]
    pub menus: HashMap<String, Menu<Option<String>, String>>,
}

/// As parsed by this module to assign actions, binds, and shortcuts
#[derive(Debug)]
pub struct Config {
    pub name: String,
    pub base: Layer<(), CustomCode, Rc<Bind>>,
    pub layers: HashMap<String, Layer<String, CustomCode, Rc<Bind>>>,
    pub shortcuts: HashMap<String, Shortcut<String, Rc<Action>>>,
    pub macros: HashMap<String, Macro<String, Rc<Action>>>,
    pub macro_groups: HashMap<String, MacroGroup<String, Rc<Action>>>,
    pub menus: HashMap<String, Rc<Menu<String, Rc<Action>>>>,
    pub library: Option<ActionLibrary>,
}

pub static MENU_LAYER_TOML: &'static str = include_str!("menu_layer.toml");

pub fn generate_menu_layer(
    lookup: &Lookup,
) -> Result<Layer<String, CustomCode, Rc<Bind>>, ConfigError> {
    toml::from_str::<Layer<Option<String>, String, String>>(MENU_LAYER_TOML)
        .expect("menu layer toml should parse")
        .actualize("menu", lookup)
        .map_err(|e| e.with_path("menu").with_path("layers"))
}

impl Config {
    pub fn lookup_name<'a>(&'a self, action: &'a Action) -> &'a str {
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
            Action::Layer(name) => name.as_ref().map_or("base", |v| v),
        }
    }
}

impl TomlConfig {
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

        // everything after this point has access to all of the config's actions

        let shortcuts: HashMap<_, _> = self
            .shortcuts
            .into_iter()
            .map(|(key, c_shortcut)| {
                let name = c_shortcut.name.unwrap_or_else(|| key.to_title_case());
                let action =
                    lookup.shortcut_bind(&c_shortcut.action).map_err(|e| e.with_path(&key))?;
                Ok((key, Shortcut { name: name, action }))
            })
            .collect::<Result<_, _>>()
            .map_err(|e: ConfigError| e.with_path("shortcuts"))?;

        let macros: HashMap<_, _> = self
            .macros
            .into_iter()
            .map(|(key, c_macro)| {
                let name = c_macro.name.unwrap_or_else(|| key.to_title_case());
                let actions = c_macro
                    .actions
                    .into_iter()
                    .map(|a| lookup.shortcut_bind(&a).map_err(|e| e.with_path(&key)))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok((key, Macro { name: name, actions }))
            })
            .collect::<Result<_, _>>()
            .map_err(|e: ConfigError| e.with_path("macros"))?;

        let macro_groups: HashMap<_, _> = self
            .macro_groups
            .into_iter()
            .map(|(key, c_macro_group)| {
                let name = c_macro_group.name.unwrap_or_else(|| key.to_title_case());
                let groups = c_macro_group
                    .groups
                    .into_iter()
                    .map(|group| {
                        group
                            .into_iter()
                            .enumerate()
                            .map(|(i, action)| {
                                lookup
                                    .shortcut_bind(&action)
                                    .map_err(|e| e.with_path(&i.to_string()))
                            })
                            .collect::<Result<_, _>>()
                            .map_err(|e| e.with_path(&key))
                    })
                    .collect::<Result<_, _>>()?;
                Ok((key, MacroGroup { name: name, reverse: c_macro_group.reverse, groups }))
            })
            .collect::<Result<_, _>>()
            .map_err(|e: ConfigError| e.with_path("macro_groups"))?;

        let menus: HashMap<_, _> = self
            .menus
            .into_iter()
            .map(|(key, c_menu)| {
                let entries: Vec<_> = c_menu
                    .entries
                    .clone()
                    .into_iter()
                    .map(|a| lookup.shortcut_bind(&a).map_err(|e| e.with_path(&key)))
                    .collect::<Result<_, _>>()?;
                let name = c_menu.name.unwrap_or_else(|| key.to_title_case());
                let menu = Menu {
                    name,
                    message: c_menu.message.clone(),
                    anchor: c_menu.anchor.clone(),
                    entries,
                    select: c_menu.select.clone(),
                };
                Ok((key, menu.into()))
            })
            .collect::<Result<_, _>>()
            .map_err(|e: ConfigError| e.with_path("menus"))?;

        let base = self.base.actualize(&lookup)?;

        let mut layers = self
            .layers
            .into_iter()
            .map(|(key, layer)| {
                if key == "menu" {
                    info!("overloading menu layer");
                }
                layer.actualize(&key, &lookup).map(|l| (key.to_string(), l))
            })
            .collect::<Result<HashMap<_, _>, _>>()
            .map_err(|e: ConfigError| e.with_path("layers"))?;

        if !layers.contains_key("menu") {
            let menu_layer = generate_menu_layer(&lookup)?;
            layers.extend(HashMap::from([("menu".to_string(), menu_layer)]));
        }

        Ok(Config {
            name: self.name,
            base: base,
            layers: layers,
            shortcuts,
            macros,
            macro_groups,
            menus,
            library: Some(library),
        })
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
        let config = toml::from_str::<TomlConfig>(&str)
            .expect("config should load")
            .actualize(library)
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
        toml::from_str::<TomlConfig>(DEFAULT_CONFIG_TOML).unwrap().actualize(library).unwrap()
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

        assert!(lookup.button_bind("x(1):S:rightalt:up").is_ok());
        assert!(lookup.button_bind("x:D").is_err());
        assert!(lookup.button_bind("x(1)/x(2)").is_ok());
        assert!(lookup.scroll_bind(&"x(1):alt/x(2):slower:M").is_ok());
        assert!(lookup.scroll_bind(&"x(1):up").is_err());
    }

    #[test]
    fn parses_full_config() {
        toml::from_str::<TomlConfig>(CONFIG_1).unwrap();
    }

    #[test]
    fn actualizes_full_config() {
        let config: TomlConfig = toml::from_str(CONFIG_1).unwrap();
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

    #[test]
    fn custom_code_from_str() {
        assert!(CustomCode::from_str("c2+tour").is_ok());
    }
}
