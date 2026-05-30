use std::{collections::HashMap, fmt, fs, path::PathBuf, rc::Rc};

use log::error;

use lazy_static::lazy_static;
use regex::Regex;
use serde::Deserialize;

use crate::actions::{Action, ActionLibrary};

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
    static ref BUTTON_REGEX: Regex = {
        Regex::new(
            r"(?x)
                ^\s*(?P<action>[[:word:]]+)\s*
                (?:\(\s*(?P<args>[[:word:]]+\s*(?:,\s*[[:word:]]+\s*)*)\s*\))?
                \s*(?P<flags>(?::[[:word:]]+\s*)*)$
            ",
        )
        .expect("Regex failed to compile")
    };
    static ref SCROLL_REGEX: Regex = {
        Regex::new(
            r"(?x)
                ^\s*(?P<fwd_action>[[:word:]]+)\s*
                (?:\(\s*(?P<fwd_args>[[:word:]]+\s*(?:,\s*[[:word:]]+\s*)*)\s*\))?
                (?:\s*/\s*(?P<bak_action>[[:word:]]+)\s*
                  (?:\(\s*(?P<bak_args>[[:word:]]+\s*(?:,\s*[[:word:]]+\s*)*)\s*\))?
                )?
                \s*(?P<flags>(?::[[:word:]]+\s*)*)$
            ",
        )
        .expect("Regex failed to compile")
    };
    static ref SHORTCUT_REGEX: Regex = {
        Regex::new(
            r"(?x)
                ^\s*(?P<action>[[:word:]]+)\s*
                (?:\(\s*(?P<args>[[:word:]]+\s*(?:,\s*[[:word:]]+\s*)*)\s*\))?$
            ",
        )
        .expect("Regex failed to compile")
    };
    static ref ARGUMENT_REGEX: Regex = Regex::new(r"[[:word:]]+").expect("Regex failed to compile");
    static ref FLAG_REGEX: Regex = Regex::new(r":[[:word:]]+").expect("Regex failed to compile");
}

pub struct Lookup<'a>(&'a mut ActionLibrary);

#[derive(Debug)]
struct BindError {
    source: String,
    reason: String,
}

impl BindError {
    fn new(source: &str, reason: &str) -> BindError {
        BindError {
            source: source.to_owned(),
            reason: reason.to_owned(),
        }
    }
}

impl fmt::Display for BindError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "Failed to parse binding '{}': {}",
            self.source, self.reason
        )
    }
}

impl<'a> Lookup<'a> {
    fn action(&self, str: &str, args: Option<Vec<String>>) -> Action {
        self.0
            .get(&str)
            .unwrap_or_else(|| panic!("Invalid action: {}", str))
            .call(args.unwrap_or_else(|| Vec::new()))
    }

    fn button_bind(&self, str: &str) -> Result<Rc<Bind>, BindError> {
        let captures = BUTTON_REGEX.captures(str).expect("Match failed");

        let action = captures
            .name("action")
            .expect("Missing action name")
            .as_str();

        let args = match captures.name("args") {
            Some(args) => Some(
                ARGUMENT_REGEX
                    .find_iter(args.as_str())
                    .map(|m| m.as_str().to_owned())
                    .collect::<Vec<_>>(),
            ),
            None => None,
        };

        let flags = FLAG_REGEX
            .find_iter(captures.name("flags").unwrap().as_str())
            .map(|m| m.as_str())
            .collect::<Vec<_>>();

        let bind = if flags.contains(&":up") {
            Bind::ButtonUp(self.action(action, args))
        } else if flags.contains(&":rep") {
            Bind::ButtonRepeat(self.action(action, args))
        } else if flags.contains(&":ab") {
            Bind::ButtonAB(self.action(action, args.clone()), self.action(action, args)) // TODO make this actually have a B-action
        } else {
            Bind::Button(self.action(action, args))
        };
        Ok(Rc::new(bind))
    }

    fn button_bind_opt(&self, str: &str) -> Option<Rc<Bind>> {
        self.button_bind(str).inspect_err(|e| error!("{}", e)).ok()
    }

    fn scroll_bind(&self, str: &str) -> Result<Rc<Bind>, BindError> {
        let captures = SCROLL_REGEX.captures(str).expect("Match failed");

        let fwd_action = captures.name("fwd_action").unwrap().as_str();
        let fwd_args = match captures.name("fwd_args") {
            Some(args) => ARGUMENT_REGEX
                .find_iter(args.as_str())
                .map(|m| m.as_str().to_owned())
                .collect::<Vec<_>>(),
            None => Vec::new(),
        };
        let fwd = self.action(fwd_action, Some(fwd_args));

        let bak = match captures.name("bak_action") {
            Some(action) => {
                let args = match captures.name("bak_args") {
                    Some(args) => ARGUMENT_REGEX
                        .find_iter(args.as_str())
                        .map(|m| m.as_str().to_owned())
                        .collect::<Vec<_>>(),
                    None => Vec::new(),
                };
                self.action(action.as_str(), Some(args.clone()))
            }
            None => fwd.reverse(),
        };

        let flags = FLAG_REGEX
            .find_iter(captures.name("flags").unwrap().as_str())
            .map(|m| m.as_str());

        let mut rate = Rate::Normal;
        let mut reverse = false;
        for f in flags {
            if f == ":slow" {
                if rate != Rate::Normal {
                    return Err(BindError::new(str, "Duplicate flag: :slow"));
                }
                rate = Rate::Slow;
            } else if f == ":slower" {
                if rate != Rate::Normal {
                    return Err(BindError::new(str, "Duplicate flag: :slower"));
                }
                rate = Rate::Slower;
            } else if f == ":rev" {
                if reverse != false {
                    return Err(BindError::new(str, "Duplicate flag: :rev"));
                }
                reverse = true;
            } else {
                return Err(BindError::new(str, "Unexpected flag"));
            }
        }

        let bind = if !reverse {
            Bind::Scroll {
                fwd: fwd,
                bak: bak,
                rate: rate,
            }
        } else {
            Bind::Scroll {
                fwd: bak,
                bak: fwd,
                rate: rate,
            }
        };

        Ok(Rc::new(bind))
    }

    fn scroll_bind_opt(&self, str: &str) -> Option<Rc<Bind>> {
        self.scroll_bind(str).inspect_err(|e| error!("{}", e)).ok()
    }

    fn shortcut_bind(&self, str: &str) -> Action {
        let captures = SHORTCUT_REGEX.captures(str).expect("Match failed");

        let action = captures
            .name("action")
            .expect("Missing action name")
            .as_str();

        let args = match captures.name("args") {
            Some(args) => ARGUMENT_REGEX
                .find_iter(args.as_str())
                .map(|m| m.as_str().to_owned())
                .collect::<Vec<_>>(),
            None => Vec::new(),
        };

        self.action(action, Some(args))
    }

    fn library(&mut self) -> &mut ActionLibrary {
        self.0
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Rate {
    Normal,
    Slow,
    Slower,
}

#[derive(Debug)]
pub enum Bind {
    Button(Action),
    ButtonUp(Action),
    ButtonRepeat(Action),
    ButtonAB(Action, Action),
    Scroll {
        fwd: Action,
        bak: Action,
        rate: Rate,
    },
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

impl Prime<String> {
    pub fn actualize(self, lookup: &Lookup) -> Prime<Rc<Bind>> {
        Prime {
            side: self.side.and_then(|s| lookup.button_bind_opt(&s)),
            side_x2: self.side_x2.and_then(|s| lookup.button_bind_opt(&s)),
            top: self.top.and_then(|s| lookup.button_bind_opt(&s)),
            top_x2: self.top_x2.and_then(|s| lookup.button_bind_opt(&s)),
            tall: self.tall.and_then(|s| lookup.button_bind_opt(&s)),
            tall_x2: self.tall_x2.and_then(|s| lookup.button_bind_opt(&s)),
            short: self.short.and_then(|s| lookup.button_bind_opt(&s)),
            short_x2: self.short_x2.and_then(|s| lookup.button_bind_opt(&s)),
            side_top: self.side_top.and_then(|s| lookup.button_bind_opt(&s)),
            side_tall: self.side_tall.and_then(|s| lookup.button_bind_opt(&s)),
            side_short: self.side_short.and_then(|s| lookup.button_bind_opt(&s)),
            top_tall: self.top_tall.and_then(|s| lookup.button_bind_opt(&s)),
            top_short: self.top_short.and_then(|s| lookup.button_bind_opt(&s)),
            tall_short: self.tall_short.and_then(|s| lookup.button_bind_opt(&s)),
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

impl DPad<String> {
    pub fn actualize(self, lookup: &Lookup) -> DPad<Rc<Bind>> {
        DPad {
            up: self.up.and_then(|s| lookup.button_bind_opt(&s)),
            down: self.down.and_then(|s| lookup.button_bind_opt(&s)),
            left: self.left.and_then(|s| lookup.button_bind_opt(&s)),
            right: self.right.and_then(|s| lookup.button_bind_opt(&s)),
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

impl Kit<String> {
    pub fn actualize(self, lookup: &Lookup) -> Kit<Rc<Bind>> {
        Kit {
            dpad: self.dpad.map(|dp| dp.actualize(&lookup)),
            side_dpad: self.side_dpad.map(|dp| dp.actualize(&lookup)),
            top_dpad: self.top_dpad.map(|dp| dp.actualize(&lookup)),
            c1: self.c1.and_then(|s| lookup.button_bind_opt(&s)),
            c2: self.c2.and_then(|s| lookup.button_bind_opt(&s)),
            tall_c1: self.tall_c1.and_then(|s| lookup.button_bind_opt(&s)),
            tall_c2: self.tall_c2.and_then(|s| lookup.button_bind_opt(&s)),
            short_c1: self.short_c1.and_then(|s| lookup.button_bind_opt(&s)),
            short_c2: self.short_c2.and_then(|s| lookup.button_bind_opt(&s)),
            tour: self.tour.and_then(|s| lookup.button_bind_opt(&s)),
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

impl Knob<String> {
    pub fn actualize(self, lookup: &Lookup) -> Knob<Rc<Bind>> {
        Knob {
            press: self.press.and_then(|s| lookup.button_bind_opt(&s)),
            turn: self.turn.and_then(|s| lookup.scroll_bind_opt(&s)),
            side_turn: self.side_turn.and_then(|s| lookup.scroll_bind_opt(&s)),
            top_turn: self.top_turn.and_then(|s| lookup.scroll_bind_opt(&s)),
            tall_turn: self.tall_turn.and_then(|s| lookup.scroll_bind_opt(&s)),
            short_turn: self.short_turn.and_then(|s| lookup.scroll_bind_opt(&s)),
        }
    }
}

#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub struct Dial<B> {
    pub press: Option<B>,
    pub turn: Option<B>,
}

impl Dial<String> {
    pub fn actualize(self, lookup: &Lookup) -> Dial<Rc<Bind>> {
        Dial {
            press: self.press.and_then(|s| lookup.button_bind_opt(&s)),
            turn: self.turn.and_then(|s| lookup.scroll_bind_opt(&s)),
        }
    }
}

#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub struct Macro<A> {
    pub name: String,
    pub actions: Vec<A>,
}

#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub struct RawConfig<A, B> {
    pub name: String,
    pub prime: Option<Prime<B>>,
    pub kit: Option<Kit<B>>,
    pub knob: Option<Knob<B>>,
    pub scroll: Option<Knob<B>>,
    pub dial: Option<Dial<B>>,
    #[serde(default)]
    pub shortcuts: HashMap<String, A>,
    #[serde(rename = "macro")]
    #[serde(default)]
    pub macros: Vec<Macro<A>>,
    #[serde(skip)]
    pub library: Option<ActionLibrary>,
}

impl<A, B> RawConfig<A, B> {
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

pub type StringConfig = RawConfig<String, String>;
pub type Config = RawConfig<Action, Rc<Bind>>;

impl StringConfig {
    pub fn actualize(self, mut library: ActionLibrary) -> Config {
        let mut lookup = Lookup(&mut library);

        let mut new_shortcuts = HashMap::new();
        for (key, str) in self.shortcuts.iter() {
            let action = lookup.shortcut_bind(str);
            lookup.library().insert(key.to_string(), action.clone());
            new_shortcuts.insert(key.to_string(), action);
        }

        let mut new_macros = Vec::new();
        for mac in self.macros.iter() {
            let name = mac.name.to_string();
            let actions = Vec::new();
            let mac = Action::Macro(name.clone(), actions.clone());
            lookup.library().insert(name.clone(), mac);
            new_macros.push(Macro {
                name: name.clone(),
                actions: actions,
            });
        }

        Config {
            name: self.name,
            prime: self.prime.map(|v| v.actualize(&lookup)),
            kit: self.kit.map(|v| v.actualize(&lookup)),
            knob: self.knob.map(|v| v.actualize(&lookup)),
            scroll: self.scroll.map(|v| v.actualize(&lookup)),
            dial: self.dial.map(|v| v.actualize(&lookup)),
            shortcuts: new_shortcuts,
            macros: new_macros,
            library: Some(library),
        }
    }
}

impl Config {}

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
        let str = fs::read_to_string(path).expect("Config is not readable");
        let config = toml::from_str::<StringConfig>(&str)
            .unwrap()
            .actualize(library);
        let name = config.name.clone();
        self.configs.push(Rc::new(config));
        name
    }

    pub fn get_default_config(&self) -> Rc<Config> {
        self.default_config.clone()
    }

    pub fn get_config(&self, name: &str) -> Option<Rc<Config>> {
        self.configs
            .iter()
            .find(|c| c.name.eq(name))
            .map(|c| c.clone())
    }

    fn load_default_config(library: Rc<ActionLibrary>) -> Config {
        let library = ActionLibrary::new(Some(library));
        toml::from_str::<StringConfig>(DEFAULT_CONFIG_TOML)
            .unwrap()
            .actualize(library)
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
    fn bind_regexes_match_examples() {
        assert!(BUTTON_REGEX.is_match("a") == true);
        assert!(BUTTON_REGEX.is_match("a:up:rep") == true);
        assert!(BUTTON_REGEX.is_match("ptr_x(val):rep") == true);
        assert!(BUTTON_REGEX.is_match("ptr_x(val):rep") == true);
        assert!(BUTTON_REGEX.is_match(BUTTON_BIND_MAX) == true);
        assert!(SCROLL_REGEX.is_match("a/b") == true);
        assert!(SCROLL_REGEX.is_match("a/b:slow") == true);
        assert!(SCROLL_REGEX.is_match("a/b:slower") == true);
        assert!(SCROLL_REGEX.is_match("ptr_wheel") == true);
        assert!(SCROLL_REGEX.is_match("ptr_wheel:rev") == true);
        assert!(SCROLL_REGEX.is_match(SCROLL_BIND_MAX) == true);
    }

    #[test]
    fn bind_regexes_return_matches() {
        let button_match = BUTTON_REGEX
            .captures(BUTTON_BIND_MAX)
            .expect("Match failed");
        assert!(button_match.name("action").unwrap().as_str() == "a_b_1");

        assert!(button_match.name("args").is_some());
        let button_args_match = ARGUMENT_REGEX
            .find_iter(button_match.name("args").unwrap().as_str())
            .collect::<Vec<_>>();
        assert!(button_args_match.len() == 3);
        assert!(button_args_match.get(0).unwrap().as_str() == "arg_1");
        assert!(button_args_match.get(1).unwrap().as_str() == "arg_2");
        assert!(button_args_match.get(2).unwrap().as_str() == "arg_3");

        assert!(button_match.name("flags").is_some());
        let button_flags_match = FLAG_REGEX
            .find_iter(button_match.name("flags").unwrap().as_str())
            .collect::<Vec<_>>();
        assert!(button_flags_match.len() == 3);
        assert!(button_flags_match.get(0).unwrap().as_str() == ":flag_1");
        assert!(button_flags_match.get(1).unwrap().as_str() == ":flag_2");
        assert!(button_flags_match.get(2).unwrap().as_str() == ":flag_3");

        let scroll_match = SCROLL_REGEX
            .captures(SCROLL_BIND_MAX)
            .expect("Match failed");

        assert!(scroll_match.name("fwd_action").unwrap().as_str() == "a_b_1");

        assert!(scroll_match.name("fwd_args").is_some());
        let scroll_args_match = ARGUMENT_REGEX
            .find_iter(scroll_match.name("fwd_args").unwrap().as_str())
            .collect::<Vec<_>>();
        assert!(scroll_args_match.len() == 3);
        assert!(scroll_args_match.get(0).unwrap().as_str() == "arg_1");
        assert!(scroll_args_match.get(1).unwrap().as_str() == "arg_2");
        assert!(scroll_args_match.get(2).unwrap().as_str() == "1");

        assert!(scroll_match.name("bak_action").unwrap().as_str() == "c_d_2");

        assert!(scroll_match.name("bak_args").is_some());
        let scroll_args_match = ARGUMENT_REGEX
            .find_iter(scroll_match.name("bak_args").unwrap().as_str())
            .collect::<Vec<_>>();
        assert!(scroll_args_match.len() == 3);
        assert!(scroll_args_match.get(0).unwrap().as_str() == "arg_3");
        assert!(scroll_args_match.get(1).unwrap().as_str() == "arg_4");
        assert!(scroll_args_match.get(2).unwrap().as_str() == "2");

        assert!(scroll_match.name("flags").is_some());
        let scroll_flags_match = FLAG_REGEX
            .find_iter(scroll_match.name("flags").unwrap().as_str())
            .collect::<Vec<_>>();
        assert!(scroll_flags_match.len() == 3);
        assert!(scroll_flags_match.get(0).unwrap().as_str() == ":flag_1");
        assert!(scroll_flags_match.get(1).unwrap().as_str() == ":flag_2");
        assert!(scroll_flags_match.get(2).unwrap().as_str() == ":flag_3");
    }

    #[test]
    fn parse_bindings() {
        let mut library = ActionLibrary::new(None);
        library.insert(
            "x".to_string(),
            Action::Key(KeyCode::KEY_X, Some(Modifiers::empty())),
        );
        let lookup = Lookup(&mut library);

        lookup.button_bind("x(1):up:rep").unwrap();
        lookup.scroll_bind("x(1)/x(2):slower:rev").unwrap();
    }

    #[test]
    fn parse_full_config() {
        toml::from_str::<StringConfig>(CONFIG_1).unwrap();
    }

    #[test]
    fn actualize_full_config() {
        let config: StringConfig = toml::from_str(CONFIG_1).unwrap();
        let mut library = ActionLibrary::new(None);
        library.insert(
            "b".to_string(),
            Action::Key(KeyCode::KEY_X, Some(Modifiers::empty())),
        );
        library.insert("t".to_string(), Action::PtrAxis(Axis::VerticalScroll, 20.0));
        library.insert("a".to_string(), Action::None);
        library.insert("b".to_string(), Action::None);
        library.insert("c".to_string(), Action::None);
        config.actualize(library);
    }
}
