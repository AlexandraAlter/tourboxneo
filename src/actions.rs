use std::{collections::HashMap, fmt, rc::Rc};

use bitflags::bitflags;

use evdev::KeyCode;
use lazy_static::lazy_static;

bitflags! {
    #[derive(Default, Copy, Clone, PartialEq, Eq, Debug)]
    pub struct ModifierFlags: u32 {
        const SHIFT = 0x0001;
        const CAPSLOCK = 0x0002;
        const CTRL = 0x0004;
        const MOD1 = 0x0008;
        const MOD2 = 0x0010;
        const MOD3 = 0x0020;
        const MOD4 = 0x0040;
        const MOD5 = 0x0080;
        const NUMLOCK = 0x0100;
        const ALT = 0x0200;
        const LEVELTHREE = 0x0400;
        const SUPER = 0x0800;
        const LEVELFIVE = 0x1000;
        const META = 0x2000;
        const HYPER = 0x4000;
        const SCROLLLOCK = 0x8000;
    }
}

#[derive(Default, Clone, PartialEq, Eq, Debug)]
pub struct Modifiers(ModifierFlags, Vec<KeyCode>);

impl Modifiers {
    pub fn flags(&self) -> &ModifierFlags {
        &self.0
    }

    pub fn keys(&self) -> &Vec<KeyCode> {
        &self.1
    }

    pub fn union(&self, other: &Modifiers) -> Modifiers {
        let mut keys = Vec::new();
        keys.extend(self.1.iter());
        keys.extend(other.1.iter());
        Modifiers(self.0.union(other.0), keys)
    }

    fn lookup_short_mod(str: &str) -> Option<Modifiers> {
        SHORT_MODIFIERS.get(str).map(|m| {
            MODIFIERS
                .get(m)
                .expect("short modifier should correspond to a long modifier")
                .to_owned()
        })
    }

    pub fn lookup_mod(str: &str) -> Option<Modifiers> {
        MODIFIERS.get(str).map(|t| t.to_owned()).or_else(|| Self::lookup_short_mod(str))
    }
}

impl fmt::Display for Modifiers {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let mods = self
            .flags()
            .iter()
            .map(|mf| {
                let mod_short = match mf {
                    ModifierFlags::SHIFT => "S",
                    ModifierFlags::CAPSLOCK => "Cl",
                    ModifierFlags::CTRL => "C",
                    ModifierFlags::MOD1 => "M1",
                    ModifierFlags::MOD2 => "M2",
                    ModifierFlags::MOD3 => "M3",
                    ModifierFlags::MOD4 => "M4",
                    ModifierFlags::MOD5 => "M5",
                    ModifierFlags::NUMLOCK => "Nl",
                    ModifierFlags::ALT => "A",
                    ModifierFlags::LEVELTHREE => "L3",
                    ModifierFlags::SUPER => "S",
                    ModifierFlags::LEVELFIVE => "L5",
                    ModifierFlags::META => "M",
                    ModifierFlags::HYPER => "H",
                    ModifierFlags::SCROLLLOCK => "Sl",
                    _ => "?",
                };
                ":".to_string() + mod_short
            })
            .collect::<Vec<String>>()
            .join("");
        write!(f, "{}", mods)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Axis {
    VerticalScroll,
    HorizontalScroll,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    None,
    Mod(Modifiers),
    Key(KeyCode, Option<Modifiers>),
    PtrMotion(f64, f64, Option<Modifiers>),
    PtrMotionAbs(u32, u32, u32, u32, Option<Modifiers>),
    PtrButton(u32, Option<Modifiers>),
    PtrAxis(Axis, f64, Option<Modifiers>),
    PtrAxisDiscrete(Axis, f64, i32, Option<Modifiers>),
    // second arg represents whether the shortcut is reversible
    Shortcut(String, Option<bool>),
    Macro(String),
    MacroGroup(String),
    Menu(String),
    Layer(Option<String>),
}

impl Action {
    pub fn reverse(&self) -> Option<Action> {
        match self {
            Action::None => Some(Action::None),
            Action::PtrMotion(dx, dy, mods) => Some(Action::PtrMotion(-dx, -dy, mods.clone())),
            Action::PtrAxis(axis, value, mods) => {
                Some(Action::PtrAxis(*axis, -value, mods.clone()))
            }
            Action::PtrAxisDiscrete(axis, v, d, mods) => {
                Some(Action::PtrAxisDiscrete(*axis, -v, -d, mods.clone()))
            }
            Action::Shortcut(name, Some(rev)) => Some(Action::Shortcut(name.clone(), Some(!rev))),
            _ => None,
        }
    }

    pub fn mods(&self) -> Option<&Modifiers> {
        match self {
            Action::Mod(mods) => Some(mods),
            Action::Key(_keycode, mods) => mods.as_ref(),
            Action::PtrMotion(_dx, _dy, mods) => mods.as_ref(),
            Action::PtrMotionAbs(_x, _y, _x_xt, _y_xt, mods) => mods.as_ref(),
            Action::PtrButton(_btn, mods) => mods.as_ref(),
            Action::PtrAxis(_axis, _value, mods) => mods.as_ref(),
            Action::PtrAxisDiscrete(_axis, _v, _d, mods) => mods.as_ref(),
            _ => None,
        }
    }

    pub fn with_modifiers(&self, modifiers: &Modifiers) -> Option<Action> {
        let modmap = move |o: &Option<Modifiers>| {
            o.clone().map(|m| m.union(modifiers)).or(Some(modifiers.clone()))
        };
        match self {
            Action::Mod(mods) => Some(Action::Mod(modmap(&Some(mods.clone())).unwrap())),
            Action::Key(keycode, mods) => Some(Action::Key(*keycode, modmap(mods))),
            Action::PtrMotion(dx, dy, mods) => Some(Action::PtrMotion(*dx, *dy, modmap(mods))),
            Action::PtrMotionAbs(x, y, x_xt, y_xt, mods) => {
                Some(Action::PtrMotionAbs(*x, *y, *x_xt, *y_xt, modmap(mods)))
            }
            Action::PtrButton(btn, mods) => Some(Action::PtrButton(*btn, modmap(mods))),
            Action::PtrAxis(axis, value, mods) => {
                Some(Action::PtrAxis(*axis, *value, modmap(mods)))
            }
            Action::PtrAxisDiscrete(axis, v, d, mods) => {
                Some(Action::PtrAxisDiscrete(*axis, *v, *d, modmap(mods)))
            }
            _ => None,
        }
    }
}

impl fmt::Display for Action {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let no_mods = Modifiers::default();
        match self {
            Action::None => write!(f, "none"),
            Action::Mod(mods) => write!(f, "mods {}", mods),
            Action::Key(key_code, mods) => {
                let m = mods.as_ref().unwrap_or(&no_mods);
                write!(f, "key {:?}{}", key_code, m)
            }
            Action::PtrMotion(x, y, mods) => {
                let m = mods.as_ref().unwrap_or(&no_mods);
                write!(f, "ptr_motion {} {} {}", x, y, m)
            }
            Action::PtrMotionAbs(x, y, x_ex, y_ex, mods) => write!(f, "unimpl"),
            Action::PtrButton(btn, mods) => write!(f, "unimpl"),
            Action::PtrAxis(axis, v, mods) => write!(f, "unimpl"),
            Action::PtrAxisDiscrete(axis, v, d, mods) => write!(f, "unimpl"),
            Action::Shortcut(name, rev) => write!(f, "shortcut {}", name),
            Action::Macro(name) => write!(f, "macro {}", name),
            Action::MacroGroup(name) => write!(f, "macro group {}", name),
            Action::Menu(name) => write!(f, "menu {}", name),
            Action::Layer(name) => {
                let name_or_base = name.as_ref().map(|s| s.as_ref()).unwrap_or_else(|| "base");
                write!(f, "layer {}", name_or_base)
            }
        }
    }
}

#[derive(Clone)]
pub struct ActionFn(pub Rc<dyn Fn(&Vec<String>) -> Action>);

impl ActionFn {
    pub fn call(&self, args: &Vec<String>) -> Action {
        self.0(args)
    }
}

impl fmt::Debug for ActionFn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("ActionFn").finish()
    }
}

#[derive(Debug)]
pub struct ActionLibrary {
    parent: Option<Rc<ActionLibrary>>,
    actions: HashMap<String, Rc<Action>>,
    action_fns: HashMap<String, ActionFn>,
}

impl ActionLibrary {
    pub fn new(parent: Option<Rc<ActionLibrary>>) -> ActionLibrary {
        ActionLibrary { parent, actions: HashMap::new(), action_fns: HashMap::new() }
    }

    // TODO: make this return result
    pub fn get(&self, key: &str, args: &Option<Vec<String>>) -> Option<Rc<Action>> {
        self.actions
            .get(key)
            .map(|a| a.clone())
            .or_else(|| {
                self.action_fns.get(key).map(|func| {
                    let args = args.as_ref().unwrap_or_else(|| {
                        panic!("action '{}' requires arguments to be passed", key)
                    });
                    func.call(args).into()
                })
            })
            .or_else(|| self.parent.as_ref().and_then(|p| p.get(key, args)))
    }

    pub fn insert(&mut self, key: String, action: Rc<Action>) -> Option<Rc<Action>> {
        self.actions.insert(key, action)
    }

    pub fn insert_fn(&mut self, key: String, action: ActionFn) -> Option<ActionFn> {
        self.action_fns.insert(key, action)
    }
}

impl Default for ActionLibrary {
    fn default() -> Self {
        let mut library =
            Self { parent: None, actions: HashMap::new(), action_fns: HashMap::new() };

        library.insert("none".to_string(), Action::None.into());

        library.insert("base".to_string(), Action::Layer(None).into());

        for (key, value) in MODIFIERS.iter() {
            library.insert(key.to_string(), Action::Mod(value.clone()).into());
        }

        for (key, value) in KEYCODES.iter() {
            library.insert(key.to_string(), Action::Key(*value, None).into());
        }

        for (key, value) in BUTTONS.iter() {
            library.insert(key.to_string(), Action::PtrButton(value.0 as u32, None).into());
        }

        library.insert_fn(
            "ptr_motion".to_string(),
            ActionFn(Rc::new(|args| {
                Action::PtrMotion(
                    args.get(0).unwrap().parse().unwrap(),
                    args.get(1).unwrap().parse().unwrap(),
                    None,
                )
            })),
        );

        library.insert_fn(
            "ptr_motion_abs".to_string(),
            ActionFn(Rc::new(|args| {
                Action::PtrMotionAbs(
                    args.get(0).unwrap().parse().unwrap(),
                    args.get(1).unwrap().parse().unwrap(),
                    args.get(2).unwrap().parse().unwrap(),
                    args.get(3).unwrap().parse().unwrap(),
                    None,
                )
            })),
        );

        library.insert_fn(
            "ptr_wheel".to_string(),
            ActionFn(Rc::new(|args| {
                Action::PtrAxis(Axis::VerticalScroll, args.get(0).unwrap().parse().unwrap(), None)
            })),
        );

        library.insert_fn(
            "ptr_hwheel".to_string(),
            ActionFn(Rc::new(|args| {
                Action::PtrAxis(Axis::HorizontalScroll, args.get(0).unwrap().parse().unwrap(), None)
            })),
        );

        library.insert_fn(
            "ptr_wheel_discrete".to_string(),
            ActionFn(Rc::new(|args| {
                Action::PtrAxisDiscrete(
                    Axis::VerticalScroll,
                    args.get(0).unwrap().parse().unwrap(),
                    args.get(1).unwrap().parse().unwrap(),
                    None,
                )
            })),
        );

        library.insert_fn(
            "ptr_hwheel_discrete".to_string(),
            ActionFn(Rc::new(|args| {
                Action::PtrAxisDiscrete(
                    Axis::HorizontalScroll,
                    args.get(0).unwrap().parse().unwrap(),
                    args.get(1).unwrap().parse().unwrap(),
                    None,
                )
            })),
        );

        library
    }
}

lazy_static! {
    static ref MODIFIERS: HashMap<&'static str, Modifiers> = {
        let mut m = HashMap::new();
        m.insert("shift", Modifiers(ModifierFlags::SHIFT, vec![KeyCode::KEY_LEFTSHIFT]));
        m.insert("leftshift", Modifiers(ModifierFlags::SHIFT, vec![KeyCode::KEY_LEFTSHIFT]));
        m.insert("rightshift", Modifiers(ModifierFlags::SHIFT, vec![KeyCode::KEY_RIGHTSHIFT]));
        m.insert("capslock", Modifiers(ModifierFlags::CAPSLOCK, vec![KeyCode::KEY_CAPSLOCK]));
        m.insert("ctrl", Modifiers(ModifierFlags::CTRL, vec![KeyCode::KEY_LEFTCTRL]));
        m.insert("leftctrl", Modifiers(ModifierFlags::CTRL, vec![KeyCode::KEY_LEFTCTRL]));
        m.insert("rightctrl", Modifiers(ModifierFlags::CTRL, vec![KeyCode::KEY_RIGHTCTRL]));
        m.insert("mod1", Modifiers(ModifierFlags::MOD1, Vec::new()));
        m.insert("mod2", Modifiers(ModifierFlags::MOD2, Vec::new()));
        m.insert("mod3", Modifiers(ModifierFlags::MOD3, Vec::new()));
        m.insert("mod4", Modifiers(ModifierFlags::MOD4, Vec::new()));
        m.insert("mod5", Modifiers(ModifierFlags::MOD5, Vec::new()));
        m.insert("numlock", Modifiers(ModifierFlags::NUMLOCK, vec![KeyCode::KEY_NUMLOCK]));
        m.insert("alt", Modifiers(ModifierFlags::ALT, vec![KeyCode::KEY_LEFTALT]));
        m.insert("leftalt", Modifiers(ModifierFlags::ALT, vec![KeyCode::KEY_LEFTALT]));
        m.insert("rightalt", Modifiers(ModifierFlags::ALT, vec![KeyCode::KEY_RIGHTALT]));
        m.insert("levelthree", Modifiers(ModifierFlags::LEVELTHREE, Vec::new()));
        m.insert("super", Modifiers(ModifierFlags::SUPER, Vec::new()));
        m.insert("levelfive", Modifiers(ModifierFlags::LEVELFIVE, Vec::new()));
        m.insert("meta", Modifiers(ModifierFlags::META, vec![KeyCode::KEY_LEFTMETA]));
        m.insert("leftmeta", Modifiers(ModifierFlags::META, vec![KeyCode::KEY_LEFTMETA]));
        m.insert("rightmeta", Modifiers(ModifierFlags::META, vec![KeyCode::KEY_RIGHTMETA]));
        m.insert("hyper", Modifiers(ModifierFlags::HYPER, Vec::new()));
        m.insert("scrolllock", Modifiers(ModifierFlags::SCROLLLOCK, vec![KeyCode::KEY_SCROLLLOCK]));
        m
    };

    static ref SHORT_MODIFIERS: HashMap<&'static str, &'static str> = {
        let mut m = HashMap::new();
        m.insert("S", "shift");
        m.insert("C", "ctrl");
        m.insert("M1", "mod1");
        m.insert("M2", "mod2");
        m.insert("M3", "mod3");
        m.insert("M4", "mod4");
        m.insert("M5", "mod5");
        m.insert("A", "alt");
        m.insert("L3", "levelthree");
        m.insert("SP", "super");
        m.insert("L5", "levelfive");
        m.insert("M", "meta");
        m.insert("H", "hyper");
        m
    };

    static ref KEYCODES: HashMap<&'static str, KeyCode> = {
        let mut m = HashMap::new();
        m.insert("esc", KeyCode::KEY_ESC);
        m.insert("1", KeyCode::KEY_1);
        m.insert("2", KeyCode::KEY_2);
        m.insert("3", KeyCode::KEY_3);
        m.insert("4", KeyCode::KEY_4);
        m.insert("5", KeyCode::KEY_5);
        m.insert("6", KeyCode::KEY_6);
        m.insert("7", KeyCode::KEY_7);
        m.insert("8", KeyCode::KEY_8);
        m.insert("9", KeyCode::KEY_9);
        m.insert("0", KeyCode::KEY_0);
        m.insert("minus", KeyCode::KEY_MINUS);
        m.insert("equal", KeyCode::KEY_EQUAL);
        m.insert("backspace", KeyCode::KEY_BACKSPACE);
        m.insert("tab", KeyCode::KEY_TAB);
        m.insert("q", KeyCode::KEY_Q);
        m.insert("w", KeyCode::KEY_W);
        m.insert("e", KeyCode::KEY_E);
        m.insert("r", KeyCode::KEY_R);
        m.insert("t", KeyCode::KEY_T);
        m.insert("y", KeyCode::KEY_Y);
        m.insert("u", KeyCode::KEY_U);
        m.insert("i", KeyCode::KEY_I);
        m.insert("o", KeyCode::KEY_O);
        m.insert("p", KeyCode::KEY_P);
        m.insert("leftbrace", KeyCode::KEY_LEFTBRACE);
        m.insert("rightbrace", KeyCode::KEY_RIGHTBRACE);
        m.insert("enter", KeyCode::KEY_ENTER);
        m.insert("leftctrl", KeyCode::KEY_LEFTCTRL);
        m.insert("a", KeyCode::KEY_A);
        m.insert("s", KeyCode::KEY_S);
        m.insert("d", KeyCode::KEY_D);
        m.insert("f", KeyCode::KEY_F);
        m.insert("g", KeyCode::KEY_G);
        m.insert("h", KeyCode::KEY_H);
        m.insert("j", KeyCode::KEY_J);
        m.insert("k", KeyCode::KEY_K);
        m.insert("l", KeyCode::KEY_L);
        m.insert("semicolon", KeyCode::KEY_SEMICOLON);
        m.insert("apostrophe", KeyCode::KEY_APOSTROPHE);
        m.insert("grave", KeyCode::KEY_GRAVE);
        m.insert("leftshift", KeyCode::KEY_LEFTSHIFT);
        m.insert("backslash", KeyCode::KEY_BACKSLASH);
        m.insert("z", KeyCode::KEY_Z);
        m.insert("x", KeyCode::KEY_X);
        m.insert("c", KeyCode::KEY_C);
        m.insert("v", KeyCode::KEY_V);
        m.insert("b", KeyCode::KEY_B);
        m.insert("n", KeyCode::KEY_N);
        m.insert("m", KeyCode::KEY_M);
        m.insert("comma", KeyCode::KEY_COMMA);
        m.insert("dot", KeyCode::KEY_DOT);
        m.insert("slash", KeyCode::KEY_SLASH);
        m.insert("rightshift", KeyCode::KEY_RIGHTSHIFT);
        m.insert("kpasterisk", KeyCode::KEY_KPASTERISK);
        m.insert("leftalt", KeyCode::KEY_LEFTALT);
        m.insert("space", KeyCode::KEY_SPACE);
        m.insert("capslock", KeyCode::KEY_CAPSLOCK);
        m.insert("f1", KeyCode::KEY_F1);
        m.insert("f2", KeyCode::KEY_F2);
        m.insert("f3", KeyCode::KEY_F3);
        m.insert("f4", KeyCode::KEY_F4);
        m.insert("f5", KeyCode::KEY_F5);
        m.insert("f6", KeyCode::KEY_F6);
        m.insert("f7", KeyCode::KEY_F7);
        m.insert("f8", KeyCode::KEY_F8);
        m.insert("f9", KeyCode::KEY_F9);
        m.insert("f10", KeyCode::KEY_F10);
        m.insert("numlock", KeyCode::KEY_NUMLOCK);
        m.insert("scrolllock", KeyCode::KEY_SCROLLLOCK);
        m.insert("kp7", KeyCode::KEY_KP7);
        m.insert("kp8", KeyCode::KEY_KP8);
        m.insert("kp9", KeyCode::KEY_KP9);
        m.insert("kpminus", KeyCode::KEY_KPMINUS);
        m.insert("kp4", KeyCode::KEY_KP4);
        m.insert("kp5", KeyCode::KEY_KP5);
        m.insert("kp6", KeyCode::KEY_KP6);
        m.insert("kpplus", KeyCode::KEY_KPPLUS);
        m.insert("kp1", KeyCode::KEY_KP1);
        m.insert("kp2", KeyCode::KEY_KP2);
        m.insert("kp3", KeyCode::KEY_KP3);
        m.insert("kp0", KeyCode::KEY_KP0);
        m.insert("kpdot", KeyCode::KEY_KPDOT);
        m.insert("zenkakuhankaku", KeyCode::KEY_ZENKAKUHANKAKU);
        m.insert("102nd", KeyCode::KEY_102ND);
        m.insert("f11", KeyCode::KEY_F11);
        m.insert("f12", KeyCode::KEY_F12);
        m.insert("ro", KeyCode::KEY_RO);
        m.insert("katakana", KeyCode::KEY_KATAKANA);
        m.insert("hiragana", KeyCode::KEY_HIRAGANA);
        m.insert("henkan", KeyCode::KEY_HENKAN);
        m.insert("katakanahiragana", KeyCode::KEY_KATAKANAHIRAGANA);
        m.insert("muhenkan", KeyCode::KEY_MUHENKAN);
        m.insert("kpjpcomma", KeyCode::KEY_KPJPCOMMA);
        m.insert("kpenter", KeyCode::KEY_KPENTER);
        m.insert("rightctrl", KeyCode::KEY_RIGHTCTRL);
        m.insert("kpslash", KeyCode::KEY_KPSLASH);
        m.insert("sysrq", KeyCode::KEY_SYSRQ);
        m.insert("rightalt", KeyCode::KEY_RIGHTALT);
        m.insert("linefeed", KeyCode::KEY_LINEFEED);
        m.insert("home", KeyCode::KEY_HOME);
        m.insert("up", KeyCode::KEY_UP);
        m.insert("pageup", KeyCode::KEY_PAGEUP);
        m.insert("left", KeyCode::KEY_LEFT);
        m.insert("right", KeyCode::KEY_RIGHT);
        m.insert("end", KeyCode::KEY_END);
        m.insert("down", KeyCode::KEY_DOWN);
        m.insert("pagedown", KeyCode::KEY_PAGEDOWN);
        m.insert("insert", KeyCode::KEY_INSERT);
        m.insert("delete", KeyCode::KEY_DELETE);
        m.insert("macro", KeyCode::KEY_MACRO);
        m.insert("mute", KeyCode::KEY_MUTE);
        m.insert("volumedown", KeyCode::KEY_VOLUMEDOWN);
        m.insert("volumeup", KeyCode::KEY_VOLUMEUP);
        m.insert("power", KeyCode::KEY_POWER); /* SC System Power Down */
        m.insert("kpequal", KeyCode::KEY_KPEQUAL);
        m.insert("kpplusminus", KeyCode::KEY_KPPLUSMINUS);
        m.insert("pause", KeyCode::KEY_PAUSE);
        m.insert("scale", KeyCode::KEY_SCALE); /* AL Compiz Scale (Expose) */
        m.insert("kpcomma", KeyCode::KEY_KPCOMMA);
        m.insert("hangeul", KeyCode::KEY_HANGEUL);
        m.insert("hanja", KeyCode::KEY_HANJA);
        m.insert("yen", KeyCode::KEY_YEN);
        m.insert("leftmeta", KeyCode::KEY_LEFTMETA);
        m.insert("rightmeta", KeyCode::KEY_RIGHTMETA);
        m.insert("compose", KeyCode::KEY_COMPOSE);
        m.insert("stop", KeyCode::KEY_STOP);     /* AC Stop */
        m.insert("again", KeyCode::KEY_AGAIN);
        m.insert("props", KeyCode::KEY_PROPS);   /* AC Properties */
        m.insert("undo", KeyCode::KEY_UNDO);     /* AC Undo */
        m.insert("front", KeyCode::KEY_FRONT);
        m.insert("copy", KeyCode::KEY_COPY);     /* AC Copy */
        m.insert("open", KeyCode::KEY_OPEN);     /* AC Open */
        m.insert("paste", KeyCode::KEY_PASTE);   /* AC Paste */
        m.insert("find", KeyCode::KEY_FIND);     /* AC Search */
        m.insert("cut", KeyCode::KEY_CUT);       /* AC Cut */
        m.insert("help", KeyCode::KEY_HELP);     /* AL Integrated Help Center */
        m.insert("menu", KeyCode::KEY_MENU);     /* Menu (show menu) */
        m.insert("calc", KeyCode::KEY_CALC);     /* AL Calculator */
        m.insert("setup", KeyCode::KEY_SETUP);
        m.insert("sleep", KeyCode::KEY_SLEEP);   /* SC System Sleep */
        m.insert("wakeup", KeyCode::KEY_WAKEUP); /* System Wake Up */
        m.insert("file", KeyCode::KEY_FILE);     /* AL Local Machine Browser */
        m.insert("sendfile", KeyCode::KEY_SENDFILE);
        m.insert("deletefile", KeyCode::KEY_DELETEFILE);
        m.insert("xfer", KeyCode::KEY_XFER);
        m.insert("prog1", KeyCode::KEY_PROG1);
        m.insert("prog2", KeyCode::KEY_PROG2);
        m.insert("www", KeyCode::KEY_WWW); /* AL Internet Browser */
        m.insert("msdos", KeyCode::KEY_MSDOS);
        m.insert("coffee", KeyCode::KEY_COFFEE); /* AL Terminal Lock/Screensaver */
        m.insert("direction", KeyCode::KEY_DIRECTION);
        m.insert("rotate_display", KeyCode::KEY_ROTATE_DISPLAY);
        m.insert("cyclewindows", KeyCode::KEY_CYCLEWINDOWS);
        m.insert("mail", KeyCode::KEY_MAIL);
        m.insert("bookmarks", KeyCode::KEY_BOOKMARKS); /* AC Bookmarks */
        m.insert("computer", KeyCode::KEY_COMPUTER);
        m.insert("back", KeyCode::KEY_BACK);           /* AC Back */
        m.insert("forward", KeyCode::KEY_FORWARD);     /* AC Forward */
        m.insert("closecd", KeyCode::KEY_CLOSECD);
        m.insert("ejectcd", KeyCode::KEY_EJECTCD);
        m.insert("ejectclosecd", KeyCode::KEY_EJECTCLOSECD);
        m.insert("nextsong", KeyCode::KEY_NEXTSONG);
        m.insert("playpause", KeyCode::KEY_PLAYPAUSE);
        m.insert("previoussong", KeyCode::KEY_PREVIOUSSONG);
        m.insert("stopcd", KeyCode::KEY_STOPCD);
        m.insert("record", KeyCode::KEY_RECORD);
        m.insert("rewind", KeyCode::KEY_REWIND);
        m.insert("phone", KeyCode::KEY_PHONE); /* Media Select Telephone */
        m.insert("iso", KeyCode::KEY_ISO);
        m.insert("config", KeyCode::KEY_CONFIG);     /* AL Consumer Control Configuration */
        m.insert("homepage", KeyCode::KEY_HOMEPAGE); /* AC Home */
        m.insert("refresh", KeyCode::KEY_REFRESH);   /* AC Refresh */
        m.insert("exit", KeyCode::KEY_EXIT);         /* AC Exit */
        m.insert("move", KeyCode::KEY_MOVE);
        m.insert("edit", KeyCode::KEY_EDIT);
        m.insert("scrollup", KeyCode::KEY_SCROLLUP);
        m.insert("scrolldown", KeyCode::KEY_SCROLLDOWN);
        m.insert("kpleftparen", KeyCode::KEY_KPLEFTPAREN);
        m.insert("kprightparen", KeyCode::KEY_KPRIGHTPAREN);
        m.insert("new", KeyCode::KEY_NEW);   /* AC New */
        m.insert("redo", KeyCode::KEY_REDO); /* AC Redo/Repeat */
        m.insert("f13", KeyCode::KEY_F13);
        m.insert("f14", KeyCode::KEY_F14);
        m.insert("f15", KeyCode::KEY_F15);
        m.insert("f16", KeyCode::KEY_F16);
        m.insert("f17", KeyCode::KEY_F17);
        m.insert("f18", KeyCode::KEY_F18);
        m.insert("f19", KeyCode::KEY_F19);
        m.insert("f20", KeyCode::KEY_F20);
        m.insert("f21", KeyCode::KEY_F21);
        m.insert("f22", KeyCode::KEY_F22);
        m.insert("f23", KeyCode::KEY_F23);
        m.insert("f24", KeyCode::KEY_F24);
        m.insert("playcd", KeyCode::KEY_PLAYCD);
        m.insert("pausecd", KeyCode::KEY_PAUSECD);
        m.insert("prog3", KeyCode::KEY_PROG3);
        m.insert("prog4", KeyCode::KEY_PROG4);
        m.insert("dashboard", KeyCode::KEY_DASHBOARD); /* AL Dashboard */
        m.insert("suspend", KeyCode::KEY_SUSPEND);
        m.insert("close", KeyCode::KEY_CLOSE); /* AC Close */
        m.insert("play", KeyCode::KEY_PLAY);
        m.insert("fastforward", KeyCode::KEY_FASTFORWARD);
        m.insert("bassboost", KeyCode::KEY_BASSBOOST);
        m.insert("print", KeyCode::KEY_PRINT); /* AC Print */
        m.insert("hp", KeyCode::KEY_HP);
        m.insert("camera", KeyCode::KEY_CAMERA);
        m.insert("sound", KeyCode::KEY_SOUND);
        m.insert("question", KeyCode::KEY_QUESTION);
        m.insert("email", KeyCode::KEY_EMAIL);
        m.insert("chat", KeyCode::KEY_CHAT);
        m.insert("search", KeyCode::KEY_SEARCH);
        m.insert("connect", KeyCode::KEY_CONNECT);
        m.insert("finance", KeyCode::KEY_FINANCE);
        m.insert("sport", KeyCode::KEY_SPORT);
        m.insert("shop", KeyCode::KEY_SHOP);
        m.insert("alterase", KeyCode::KEY_ALTERASE);
        m.insert("cancel", KeyCode::KEY_CANCEL);
        m.insert("brightnessdown", KeyCode::KEY_BRIGHTNESSDOWN);
        m.insert("brightnessup", KeyCode::KEY_BRIGHTNESSUP);
        m.insert("media", KeyCode::KEY_MEDIA);
        m.insert("switchvideomode", KeyCode::KEY_SWITCHVIDEOMODE);
        m.insert("kbdillumtoggle", KeyCode::KEY_KBDILLUMTOGGLE);
        m.insert("kbdillumdown", KeyCode::KEY_KBDILLUMDOWN);
        m.insert("kbdillumup", KeyCode::KEY_KBDILLUMUP);
        m.insert("send", KeyCode::KEY_SEND);
        m.insert("reply", KeyCode::KEY_REPLY);
        m.insert("forwardmail", KeyCode::KEY_FORWARDMAIL);
        m.insert("save", KeyCode::KEY_SAVE);
        m.insert("documents", KeyCode::KEY_DOCUMENTS);
        m.insert("battery", KeyCode::KEY_BATTERY);
        m.insert("bluetooth", KeyCode::KEY_BLUETOOTH);
        m.insert("wlan", KeyCode::KEY_WLAN);
        m.insert("uwb", KeyCode::KEY_UWB);
        m.insert("unknown", KeyCode::KEY_UNKNOWN);
        m.insert("video_next", KeyCode::KEY_VIDEO_NEXT);
        m.insert("video_prev", KeyCode::KEY_VIDEO_PREV);
        m.insert("brightness_cycle", KeyCode::KEY_BRIGHTNESS_CYCLE);
        m.insert("brightness_auto", KeyCode::KEY_BRIGHTNESS_AUTO);
        m.insert("display_off", KeyCode::KEY_DISPLAY_OFF);
        m.insert("wwan", KeyCode::KEY_WWAN);
        m.insert("rfkill", KeyCode::KEY_RFKILL);
        m.insert("micmute", KeyCode::KEY_MICMUTE);
        m.insert("ok", KeyCode::KEY_OK);
        m.insert("select", KeyCode::KEY_SELECT);
        m.insert("goto", KeyCode::KEY_GOTO);
        m.insert("clear", KeyCode::KEY_CLEAR);
        m.insert("power2", KeyCode::KEY_POWER2);
        m.insert("option", KeyCode::KEY_OPTION);
        m.insert("info", KeyCode::KEY_INFO); /* AL OEM Features/Tips/Tutorial */
        m.insert("time", KeyCode::KEY_TIME);
        m.insert("vendor", KeyCode::KEY_VENDOR);
        m.insert("archive", KeyCode::KEY_ARCHIVE);
        m.insert("program", KeyCode::KEY_PROGRAM); /* Media Select Program Guide */
        m.insert("channel", KeyCode::KEY_CHANNEL);
        m.insert("favorites", KeyCode::KEY_FAVORITES);
        m.insert("epg", KeyCode::KEY_EPG);
        m.insert("pvr", KeyCode::KEY_PVR); /* Media Select Home */
        m.insert("mhp", KeyCode::KEY_MHP);
        m.insert("language", KeyCode::KEY_LANGUAGE);
        m.insert("title", KeyCode::KEY_TITLE);
        m.insert("subtitle", KeyCode::KEY_SUBTITLE);
        m.insert("angle", KeyCode::KEY_ANGLE);
        m.insert("zoom", KeyCode::KEY_ZOOM);
        m.insert("full_screen", KeyCode::KEY_FULL_SCREEN);
        m.insert("mode", KeyCode::KEY_MODE);
        m.insert("keyboard", KeyCode::KEY_KEYBOARD);
        m.insert("screen", KeyCode::KEY_SCREEN);
        m.insert("pc", KeyCode::KEY_PC);     /* Media Select Computer */
        m.insert("tv", KeyCode::KEY_TV);     /* Media Select TV */
        m.insert("tv2", KeyCode::KEY_TV2);   /* Media Select Cable */
        m.insert("vcr", KeyCode::KEY_VCR);   /* Media Select VCR */
        m.insert("vcr2", KeyCode::KEY_VCR2); /* VCR Plus */
        m.insert("sat", KeyCode::KEY_SAT);   /* Media Select Satellite */
        m.insert("sat2", KeyCode::KEY_SAT2);
        m.insert("cd", KeyCode::KEY_CD);     /* Media Select CD */
        m.insert("tape", KeyCode::KEY_TAPE); /* Media Select Tape */
        m.insert("radio", KeyCode::KEY_RADIO);
        m.insert("tuner", KeyCode::KEY_TUNER); /* Media Select Tuner */
        m.insert("player", KeyCode::KEY_PLAYER);
        m.insert("text", KeyCode::KEY_TEXT);
        m.insert("dvd", KeyCode::KEY_DVD); /* Media Select DVD */
        m.insert("aux", KeyCode::KEY_AUX);
        m.insert("mp3", KeyCode::KEY_MP3);
        m.insert("audio", KeyCode::KEY_AUDIO); /* AL Audio Browser */
        m.insert("video", KeyCode::KEY_VIDEO); /* AL Movie Browser */
        m.insert("directory", KeyCode::KEY_DIRECTORY);
        m.insert("list", KeyCode::KEY_LIST);
        m.insert("memo", KeyCode::KEY_MEMO); /* Media Select Messages */
        m.insert("calendar", KeyCode::KEY_CALENDAR);
        m.insert("red", KeyCode::KEY_RED);
        m.insert("green", KeyCode::KEY_GREEN);
        m.insert("yellow", KeyCode::KEY_YELLOW);
        m.insert("blue", KeyCode::KEY_BLUE);
        m.insert("channelup", KeyCode::KEY_CHANNELUP);     /* Channel Increment */
        m.insert("channeldown", KeyCode::KEY_CHANNELDOWN); /* Channel Decrement */
        m.insert("first", KeyCode::KEY_FIRST);
        m.insert("last", KeyCode::KEY_LAST); /* Recall Last */
        m.insert("ab", KeyCode::KEY_AB);
        m.insert("next", KeyCode::KEY_NEXT);
        m.insert("restart", KeyCode::KEY_RESTART);
        m.insert("slow", KeyCode::KEY_SLOW);
        m.insert("shuffle", KeyCode::KEY_SHUFFLE);
        m.insert("break", KeyCode::KEY_BREAK);
        m.insert("previous", KeyCode::KEY_PREVIOUS);
        m.insert("digits", KeyCode::KEY_DIGITS);
        m.insert("teen", KeyCode::KEY_TEEN);
        m.insert("twen", KeyCode::KEY_TWEN);
        m.insert("videophone", KeyCode::KEY_VIDEOPHONE);         /* Media Select Video Phone */
        m.insert("games", KeyCode::KEY_GAMES);                   /* Media Select Games */
        m.insert("zoomin", KeyCode::KEY_ZOOMIN);                 /* AC Zoom In */
        m.insert("zoomout", KeyCode::KEY_ZOOMOUT);               /* AC Zoom Out */
        m.insert("zoomreset", KeyCode::KEY_ZOOMRESET);           /* AC Zoom */
        m.insert("wordprocessor", KeyCode::KEY_WORDPROCESSOR);   /* AL Word Processor */
        m.insert("editor", KeyCode::KEY_EDITOR);                 /* AL Text Editor */
        m.insert("spreadsheet", KeyCode::KEY_SPREADSHEET);       /* AL Spreadsheet */
        m.insert("graphicseditor", KeyCode::KEY_GRAPHICSEDITOR); /* AL Graphics Editor */
        m.insert("presentation", KeyCode::KEY_PRESENTATION);     /* AL Presentation App */
        m.insert("database", KeyCode::KEY_DATABASE);             /* AL Database App */
        m.insert("news", KeyCode::KEY_NEWS);                     /* AL Newsreader */
        m.insert("voicemail", KeyCode::KEY_VOICEMAIL);           /* AL Voicemail */
        m.insert("addressbook", KeyCode::KEY_ADDRESSBOOK);       /* AL Contacts/Address Book */
        m.insert("messenger", KeyCode::KEY_MESSENGER);           /* AL Instant Messaging */
        m.insert("displaytoggle", KeyCode::KEY_DISPLAYTOGGLE);   /* Turn display (LCD) on and off */
        m.insert("spellcheck", KeyCode::KEY_SPELLCHECK);         /* AL Spell Check */
        m.insert("logoff", KeyCode::KEY_LOGOFF);                 /* AL Logoff */
        m.insert("dollar", KeyCode::KEY_DOLLAR);
        m.insert("euro", KeyCode::KEY_EURO);
        m.insert("frameback", KeyCode::KEY_FRAMEBACK);           /* Consumer - transport controls */
        m.insert("frameforward", KeyCode::KEY_FRAMEFORWARD);
        m.insert("context_menu", KeyCode::KEY_CONTEXT_MENU);     /* GenDesc - system context menu */
        m.insert("media_repeat", KeyCode::KEY_MEDIA_REPEAT);     /* Consumer - transport control */
        m.insert("10channelsup", KeyCode::KEY_10CHANNELSUP);     /* 10 channels up (10+) */
        m.insert("10channelsdown", KeyCode::KEY_10CHANNELSDOWN); /* 10 channels down (10-) */
        m.insert("images", KeyCode::KEY_IMAGES);                 /* AL Image Browser */
        m.insert("pickup_phone", KeyCode::KEY_PICKUP_PHONE);
        m.insert("hangup_phone", KeyCode::KEY_HANGUP_PHONE);
        m.insert("del_eol", KeyCode::KEY_DEL_EOL);
        m.insert("del_eos", KeyCode::KEY_DEL_EOS);
        m.insert("ins_line", KeyCode::KEY_INS_LINE);
        m.insert("del_line", KeyCode::KEY_DEL_LINE);
        m.insert("fn", KeyCode::KEY_FN);
        m.insert("fn_esc", KeyCode::KEY_FN_ESC);
        m.insert("fn_f1", KeyCode::KEY_FN_F1);
        m.insert("fn_f2", KeyCode::KEY_FN_F2);
        m.insert("fn_f3", KeyCode::KEY_FN_F3);
        m.insert("fn_f4", KeyCode::KEY_FN_F4);
        m.insert("fn_f5", KeyCode::KEY_FN_F5);
        m.insert("fn_f6", KeyCode::KEY_FN_F6);
        m.insert("fn_f7", KeyCode::KEY_FN_F7);
        m.insert("fn_f8", KeyCode::KEY_FN_F8);
        m.insert("fn_f9", KeyCode::KEY_FN_F9);
        m.insert("fn_f10", KeyCode::KEY_FN_F10);
        m.insert("fn_f11", KeyCode::KEY_FN_F11);
        m.insert("fn_f12", KeyCode::KEY_FN_F12);
        m.insert("fn_1", KeyCode::KEY_FN_1);
        m.insert("fn_2", KeyCode::KEY_FN_2);
        m.insert("fn_d", KeyCode::KEY_FN_D);
        m.insert("fn_e", KeyCode::KEY_FN_E);
        m.insert("fn_f", KeyCode::KEY_FN_F);
        m.insert("fn_s", KeyCode::KEY_FN_S);
        m.insert("fn_b", KeyCode::KEY_FN_B);
        m.insert("brl_dot1", KeyCode::KEY_BRL_DOT1);
        m.insert("brl_dot2", KeyCode::KEY_BRL_DOT2);
        m.insert("brl_dot3", KeyCode::KEY_BRL_DOT3);
        m.insert("brl_dot4", KeyCode::KEY_BRL_DOT4);
        m.insert("brl_dot5", KeyCode::KEY_BRL_DOT5);
        m.insert("brl_dot6", KeyCode::KEY_BRL_DOT6);
        m.insert("brl_dot7", KeyCode::KEY_BRL_DOT7);
        m.insert("brl_dot8", KeyCode::KEY_BRL_DOT8);
        m.insert("brl_dot9", KeyCode::KEY_BRL_DOT9);
        m.insert("brl_dot10", KeyCode::KEY_BRL_DOT10);
        m.insert("numeric_0", KeyCode::KEY_NUMERIC_0); /* used by phones, remote controls, */
        m.insert("numeric_1", KeyCode::KEY_NUMERIC_1); /* and other keypads */
        m.insert("numeric_2", KeyCode::KEY_NUMERIC_2);
        m.insert("numeric_3", KeyCode::KEY_NUMERIC_3);
        m.insert("numeric_4", KeyCode::KEY_NUMERIC_4);
        m.insert("numeric_5", KeyCode::KEY_NUMERIC_5);
        m.insert("numeric_6", KeyCode::KEY_NUMERIC_6);
        m.insert("numeric_7", KeyCode::KEY_NUMERIC_7);
        m.insert("numeric_8", KeyCode::KEY_NUMERIC_8);
        m.insert("numeric_9", KeyCode::KEY_NUMERIC_9);
        m.insert("numeric_star", KeyCode::KEY_NUMERIC_STAR);
        m.insert("numeric_pound", KeyCode::KEY_NUMERIC_POUND);
        m.insert("numeric_a", KeyCode::KEY_NUMERIC_A); /* Phone key A - HUT Telephony 0xb9 */
        m.insert("numeric_b", KeyCode::KEY_NUMERIC_B);
        m.insert("numeric_c", KeyCode::KEY_NUMERIC_C);
        m.insert("numeric_d", KeyCode::KEY_NUMERIC_D);
        m.insert("camera_focus", KeyCode::KEY_CAMERA_FOCUS);
        m.insert("wps_button", KeyCode::KEY_WPS_BUTTON);      /* WiFi Protected Setup key */
        m.insert("touchpad_toggle", KeyCode::KEY_TOUCHPAD_TOGGLE); /* Request switch touchpad on or off */
        m.insert("touchpad_on", KeyCode::KEY_TOUCHPAD_ON);
        m.insert("touchpad_off", KeyCode::KEY_TOUCHPAD_OFF);
        m.insert("camera_zoomin", KeyCode::KEY_CAMERA_ZOOMIN);
        m.insert("camera_zoomout", KeyCode::KEY_CAMERA_ZOOMOUT);
        m.insert("camera_up", KeyCode::KEY_CAMERA_UP);
        m.insert("camera_down", KeyCode::KEY_CAMERA_DOWN);
        m.insert("camera_left", KeyCode::KEY_CAMERA_LEFT);
        m.insert("camera_right", KeyCode::KEY_CAMERA_RIGHT);
        m.insert("attendant_on", KeyCode::KEY_ATTENDANT_ON);
        m.insert("attendant_off", KeyCode::KEY_ATTENDANT_OFF);
        m.insert("attendant_toggle", KeyCode::KEY_ATTENDANT_TOGGLE); /* Attendant call on or off */
        m.insert("lights_toggle", KeyCode::KEY_LIGHTS_TOGGLE);    /* Reading light on or off */
        m.insert("als_toggle", KeyCode::KEY_ALS_TOGGLE);   /* Ambient light sensor */
        m.insert("buttonconfig", KeyCode::KEY_BUTTONCONFIG); /* AL Button Configuration */
        m.insert("taskmanager", KeyCode::KEY_TASKMANAGER);  /* AL Task/Project Manager */
        m.insert("journal", KeyCode::KEY_JOURNAL);      /* AL Log/Journal/Timecard */
        m.insert("controlpanel", KeyCode::KEY_CONTROLPANEL); /* AL Control Panel */
        m.insert("appselect", KeyCode::KEY_APPSELECT);    /* AL Select Task/Application */
        m.insert("screensaver", KeyCode::KEY_SCREENSAVER);  /* AL Screen Saver */
        m.insert("voicecommand", KeyCode::KEY_VOICECOMMAND); /* Listening Voice Command */
        m.insert("assistant", KeyCode::KEY_ASSISTANT);
        m.insert("kbd_layout_next", KeyCode::KEY_KBD_LAYOUT_NEXT);
        m.insert("brightness_min", KeyCode::KEY_BRIGHTNESS_MIN); /* Set Brightness to Minimum */
        m.insert("brightness_max", KeyCode::KEY_BRIGHTNESS_MAX); /* Set Brightness to Maximum */
        m.insert("kbdinputassist_prev", KeyCode::KEY_KBDINPUTASSIST_PREV);
        m.insert("kbdinputassist_next", KeyCode::KEY_KBDINPUTASSIST_NEXT);
        m.insert("kbdinputassist_prevgroup", KeyCode::KEY_KBDINPUTASSIST_PREVGROUP);
        m.insert("kbdinputassist_nextgroup", KeyCode::KEY_KBDINPUTASSIST_NEXTGROUP);
        m.insert("kbdinputassist_accept", KeyCode::KEY_KBDINPUTASSIST_ACCEPT);
        m.insert("kbdinputassist_cancel", KeyCode::KEY_KBDINPUTASSIST_CANCEL);
        m.insert("right_up", KeyCode::KEY_RIGHT_UP);
        m.insert("right_down", KeyCode::KEY_RIGHT_DOWN);
        m.insert("left_up", KeyCode::KEY_LEFT_UP);
        m.insert("left_down", KeyCode::KEY_LEFT_DOWN);
        m.insert("root_menu", KeyCode::KEY_ROOT_MENU);
        m.insert("media_top_menu", KeyCode::KEY_MEDIA_TOP_MENU);
        m.insert("numeric_11", KeyCode::KEY_NUMERIC_11);
        m.insert("numeric_12", KeyCode::KEY_NUMERIC_12);
        m.insert("audio_desc", KeyCode::KEY_AUDIO_DESC);
        m.insert("3d_mode", KeyCode::KEY_3D_MODE);
        m.insert("next_favorite", KeyCode::KEY_NEXT_FAVORITE);
        m.insert("stop_record", KeyCode::KEY_STOP_RECORD);
        m.insert("pause_record", KeyCode::KEY_PAUSE_RECORD);
        m.insert("vod", KeyCode::KEY_VOD); /* Video on Demand */
        m.insert("unmute", KeyCode::KEY_UNMUTE);
        m.insert("fastreverse", KeyCode::KEY_FASTREVERSE);
        m.insert("slowreverse", KeyCode::KEY_SLOWREVERSE);
        m.insert("data", KeyCode::KEY_DATA);
        m.insert("onscreen_keyboard", KeyCode::KEY_ONSCREEN_KEYBOARD);
        m.insert("privacy_screen_toggle", KeyCode::KEY_PRIVACY_SCREEN_TOGGLE);
        m.insert("selective_screenshot", KeyCode::KEY_SELECTIVE_SCREENSHOT);
        m
    };

    static ref BUTTONS: HashMap<&'static str, KeyCode> = {
        let mut m = HashMap::new();
        m.insert("btn_0", KeyCode::BTN_0);
        m.insert("btn_1", KeyCode::BTN_1);
        m.insert("btn_2", KeyCode::BTN_2);
        m.insert("btn_3", KeyCode::BTN_3);
        m.insert("btn_4", KeyCode::BTN_4);
        m.insert("btn_5", KeyCode::BTN_5);
        m.insert("btn_6", KeyCode::BTN_6);
        m.insert("btn_7", KeyCode::BTN_7);
        m.insert("btn_8", KeyCode::BTN_8);
        m.insert("btn_9", KeyCode::BTN_9);
        m.insert("btn_left", KeyCode::BTN_LEFT);
        m.insert("btn_right", KeyCode::BTN_RIGHT);
        m.insert("btn_middle", KeyCode::BTN_MIDDLE);
        m.insert("btn_side", KeyCode::BTN_SIDE);
        m.insert("btn_extra", KeyCode::BTN_EXTRA);
        m.insert("btn_forward", KeyCode::BTN_FORWARD);
        m.insert("btn_back", KeyCode::BTN_BACK);
        m.insert("btn_task", KeyCode::BTN_TASK);
        m.insert("btn_trigger", KeyCode::BTN_TRIGGER);
        m.insert("btn_thumb", KeyCode::BTN_THUMB);
        m.insert("btn_thumb2", KeyCode::BTN_THUMB2);
        m.insert("btn_top", KeyCode::BTN_TOP);
        m.insert("btn_top2", KeyCode::BTN_TOP2);
        m.insert("btn_pinkie", KeyCode::BTN_PINKIE);
        m.insert("btn_base", KeyCode::BTN_BASE);
        m.insert("btn_base2", KeyCode::BTN_BASE2);
        m.insert("btn_base3", KeyCode::BTN_BASE3);
        m.insert("btn_base4", KeyCode::BTN_BASE4);
        m.insert("btn_base5", KeyCode::BTN_BASE5);
        m.insert("btn_base6", KeyCode::BTN_BASE6);
        m.insert("btn_dead", KeyCode::BTN_DEAD);
        m.insert("btn_south", KeyCode::BTN_SOUTH);
        m.insert("btn_east", KeyCode::BTN_EAST);
        m.insert("btn_c", KeyCode::BTN_C);
        m.insert("btn_north", KeyCode::BTN_NORTH);
        m.insert("btn_west", KeyCode::BTN_WEST);
        m.insert("btn_z", KeyCode::BTN_Z);
        m.insert("btn_tl", KeyCode::BTN_TL);
        m.insert("btn_tr", KeyCode::BTN_TR);
        m.insert("btn_tl2", KeyCode::BTN_TL2);
        m.insert("btn_tr2", KeyCode::BTN_TR2);
        m.insert("btn_select", KeyCode::BTN_SELECT);
        m.insert("btn_start", KeyCode::BTN_START);
        m.insert("btn_mode", KeyCode::BTN_MODE);
        m.insert("btn_thumbl", KeyCode::BTN_THUMBL);
        m.insert("btn_thumbr", KeyCode::BTN_THUMBR);
        m.insert("btn_tool_pen", KeyCode::BTN_TOOL_PEN);
        m.insert("btn_tool_rubber", KeyCode::BTN_TOOL_RUBBER);
        m.insert("btn_tool_brush", KeyCode::BTN_TOOL_BRUSH);
        m.insert("btn_tool_pencil", KeyCode::BTN_TOOL_PENCIL);
        m.insert("btn_tool_airbrush", KeyCode::BTN_TOOL_AIRBRUSH);
        m.insert("btn_tool_finger", KeyCode::BTN_TOOL_FINGER);
        m.insert("btn_tool_mouse", KeyCode::BTN_TOOL_MOUSE);
        m.insert("btn_tool_lens", KeyCode::BTN_TOOL_LENS);
        m.insert("btn_tool_quinttap", KeyCode::BTN_TOOL_QUINTTAP); /* Five fingers on trackpad */
        m.insert("btn_touch", KeyCode::BTN_TOUCH);
        m.insert("btn_stylus", KeyCode::BTN_STYLUS);
        m.insert("btn_stylus2", KeyCode::BTN_STYLUS2);
        m.insert("btn_tool_doubletap", KeyCode::BTN_TOOL_DOUBLETAP);
        m.insert("btn_tool_tripletap", KeyCode::BTN_TOOL_TRIPLETAP);
        m.insert("btn_tool_quadtap", KeyCode::BTN_TOOL_QUADTAP); /* Four fingers on trackpad */
        m.insert("btn_gear_down", KeyCode::BTN_GEAR_DOWN);
        m.insert("btn_gear_up", KeyCode::BTN_GEAR_UP);
        m.insert("btn_dpad_up", KeyCode::BTN_DPAD_UP);
        m.insert("btn_dpad_down", KeyCode::BTN_DPAD_DOWN);
        m.insert("btn_dpad_left", KeyCode::BTN_DPAD_LEFT);
        m.insert("btn_dpad_right", KeyCode::BTN_DPAD_RIGHT);
        m.insert("btn_trigger_happy1", KeyCode::BTN_TRIGGER_HAPPY1);
        m.insert("btn_trigger_happy2", KeyCode::BTN_TRIGGER_HAPPY2);
        m.insert("btn_trigger_happy3", KeyCode::BTN_TRIGGER_HAPPY3);
        m.insert("btn_trigger_happy4", KeyCode::BTN_TRIGGER_HAPPY4);
        m.insert("btn_trigger_happy5", KeyCode::BTN_TRIGGER_HAPPY5);
        m.insert("btn_trigger_happy6", KeyCode::BTN_TRIGGER_HAPPY6);
        m.insert("btn_trigger_happy7", KeyCode::BTN_TRIGGER_HAPPY7);
        m.insert("btn_trigger_happy8", KeyCode::BTN_TRIGGER_HAPPY8);
        m.insert("btn_trigger_happy9", KeyCode::BTN_TRIGGER_HAPPY9);
        m.insert("btn_trigger_happy10", KeyCode::BTN_TRIGGER_HAPPY10);
        m.insert("btn_trigger_happy11", KeyCode::BTN_TRIGGER_HAPPY11);
        m.insert("btn_trigger_happy12", KeyCode::BTN_TRIGGER_HAPPY12);
        m.insert("btn_trigger_happy13", KeyCode::BTN_TRIGGER_HAPPY13);
        m.insert("btn_trigger_happy14", KeyCode::BTN_TRIGGER_HAPPY14);
        m.insert("btn_trigger_happy15", KeyCode::BTN_TRIGGER_HAPPY15);
        m.insert("btn_trigger_happy16", KeyCode::BTN_TRIGGER_HAPPY16);
        m.insert("btn_trigger_happy17", KeyCode::BTN_TRIGGER_HAPPY17);
        m.insert("btn_trigger_happy18", KeyCode::BTN_TRIGGER_HAPPY18);
        m.insert("btn_trigger_happy19", KeyCode::BTN_TRIGGER_HAPPY19);
        m.insert("btn_trigger_happy20", KeyCode::BTN_TRIGGER_HAPPY20);
        m.insert("btn_trigger_happy21", KeyCode::BTN_TRIGGER_HAPPY21);
        m.insert("btn_trigger_happy22", KeyCode::BTN_TRIGGER_HAPPY22);
        m.insert("btn_trigger_happy23", KeyCode::BTN_TRIGGER_HAPPY23);
        m.insert("btn_trigger_happy24", KeyCode::BTN_TRIGGER_HAPPY24);
        m.insert("btn_trigger_happy25", KeyCode::BTN_TRIGGER_HAPPY25);
        m.insert("btn_trigger_happy26", KeyCode::BTN_TRIGGER_HAPPY26);
        m.insert("btn_trigger_happy27", KeyCode::BTN_TRIGGER_HAPPY27);
        m.insert("btn_trigger_happy28", KeyCode::BTN_TRIGGER_HAPPY28);
        m.insert("btn_trigger_happy29", KeyCode::BTN_TRIGGER_HAPPY29);
        m.insert("btn_trigger_happy30", KeyCode::BTN_TRIGGER_HAPPY30);
        m.insert("btn_trigger_happy31", KeyCode::BTN_TRIGGER_HAPPY31);
        m.insert("btn_trigger_happy32", KeyCode::BTN_TRIGGER_HAPPY32);
        m.insert("btn_trigger_happy33", KeyCode::BTN_TRIGGER_HAPPY33);
        m.insert("btn_trigger_happy34", KeyCode::BTN_TRIGGER_HAPPY34);
        m.insert("btn_trigger_happy35", KeyCode::BTN_TRIGGER_HAPPY35);
        m.insert("btn_trigger_happy36", KeyCode::BTN_TRIGGER_HAPPY36);
        m.insert("btn_trigger_happy37", KeyCode::BTN_TRIGGER_HAPPY37);
        m.insert("btn_trigger_happy38", KeyCode::BTN_TRIGGER_HAPPY38);
        m.insert("btn_trigger_happy39", KeyCode::BTN_TRIGGER_HAPPY39);
        m.insert("btn_trigger_happy40", KeyCode::BTN_TRIGGER_HAPPY40);
        m
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modifiers_can_lookup() {
        assert_eq!(
            Modifiers::lookup_mod("S"),
            Some(Modifiers(ModifierFlags::SHIFT, vec![KeyCode::KEY_LEFTSHIFT]))
        );
        assert_eq!(Modifiers::lookup_mod("Q"), None);
        assert_eq!(
            Modifiers::lookup_mod("shift"),
            Some(Modifiers(ModifierFlags::SHIFT, vec![KeyCode::KEY_LEFTSHIFT]))
        );
        assert_eq!(
            Modifiers::lookup_mod("rightmeta"),
            Some(Modifiers(ModifierFlags::META, vec![KeyCode::KEY_RIGHTMETA]))
        );
    }

    #[test]
    fn action_can_reverse() {
        assert_eq!(Action::None.reverse(), Some(Action::None));
        assert_eq!(
            Action::PtrAxis(Axis::HorizontalScroll, 20.0, None).reverse(),
            Some(Action::PtrAxis(Axis::HorizontalScroll, -20.0, None))
        );
    }
}
