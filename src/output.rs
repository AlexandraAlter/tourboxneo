/// Substantially copied from https://github.com/ptazithos/wkeys/tree/main/wkeys/src/native
use bitflags::bitflags;
use log::{info, warn};

use std::{fs::File, io::Write, os::fd::AsFd, path::PathBuf};
use wayland_client::{
    Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum,
    protocol::{
        wl_keyboard::{self, KeyState},
        wl_registry::{self, WlRegistry},
        wl_seat::{self, WlSeat},
    },
};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::{
    zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1,
    zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1,
};
use wayland_protocols_wlr::virtual_pointer::v1::client::{
    zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1,
    zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1,
};
use xkbcommon::xkb;

struct SessionState {
    pub keyboard_manager: Option<ZwpVirtualKeyboardManagerV1>,
    pub keyboard: Option<ZwpVirtualKeyboardV1>,
    pub pointer_manager: Option<ZwlrVirtualPointerManagerV1>,
    pub pointer: Option<ZwlrVirtualPointerV1>,
    pub seat: Option<WlSeat>,
}

impl Dispatch<WlRegistry, ()> for SessionState {
    fn event(
        state: &mut Self,
        registry: &WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qhandle: &QueueHandle<SessionState>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            match interface.as_str() {
                "zwp_virtual_keyboard_manager_v1" => {
                    state.keyboard_manager = Some(registry.bind(name, version, &qhandle, ()))
                }
                "zwlr_virtual_pointer_manager_v1" => {
                    state.pointer_manager = Some(registry.bind(name, version, &qhandle, ()))
                }
                "wl_seat" => {
                    state.seat = Some(registry.bind(name, 1, qhandle, ()));
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<ZwpVirtualKeyboardManagerV1, ()> for SessionState {
    fn event(
        _state: &mut Self,
        _proxy: &ZwpVirtualKeyboardManagerV1,
        _event: <ZwpVirtualKeyboardManagerV1 as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qhandle: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwlrVirtualPointerManagerV1, ()> for SessionState {
    fn event(
        _state: &mut Self,
        _proxy: &ZwlrVirtualPointerManagerV1,
        _event: <ZwlrVirtualPointerManagerV1 as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qhandle: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlSeat, ()> for SessionState {
    fn event(
        state: &mut Self,
        seat: &wl_seat::WlSeat,
        event: wl_seat::Event,
        _data: &(),
        _conn: &Connection,
        qhandle: &QueueHandle<SessionState>,
    ) {
        if let wl_seat::Event::Capabilities {
            capabilities: WEnum::Value(capabilities),
        } = event
            && capabilities.contains(wl_seat::Capability::Keyboard)
            && let Some(keyboard_manager) = &state.keyboard_manager
            && let Some(pointer_manager) = &state.pointer_manager
        {
            let keyboard = keyboard_manager.create_virtual_keyboard(seat, qhandle, ());
            let pointer = pointer_manager.create_virtual_pointer(Some(seat), qhandle, ());

            let (keymap, keymap_len) = get_keymap_as_file();
            keyboard.keymap(
                wl_keyboard::KeymapFormat::XkbV1.into(),
                keymap.as_fd(),
                keymap_len,
            );

            state.keyboard = Some(keyboard);
            state.pointer = Some(pointer);
        }
    }
}

impl Dispatch<ZwpVirtualKeyboardV1, ()> for SessionState {
    fn event(
        _state: &mut Self,
        _proxy: &ZwpVirtualKeyboardV1,
        _event: <ZwpVirtualKeyboardV1 as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qhandle: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwlrVirtualPointerV1, ()> for SessionState {
    fn event(
        _state: &mut Self,
        _proxy: &ZwlrVirtualPointerV1,
        _event: <ZwlrVirtualPointerV1 as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qhandle: &QueueHandle<Self>,
    ) {
    }
}

fn get_keymap_as_file() -> (File, u32) {
    let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);

    let keymap = xkb::Keymap::new_from_names(
        &context,
        "",
        "",
        "us",
        "",
        None,
        xkb::KEYMAP_COMPILE_NO_FLAGS,
    )
    .expect("xkbcommon keymap panicked!");
    let xkb_state = xkb::State::new(&keymap);
    let keymap = xkb_state
        .get_keymap()
        .get_as_string(xkb::KEYMAP_FORMAT_TEXT_V1);
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let mut file = tempfile::tempfile_in(dir).expect("File could not be created!");
    file.write_all(keymap.as_bytes()).unwrap();
    file.flush().unwrap();
    (file, keymap.len() as u32)
}

bitflags! {
    #[derive(Debug)]
    pub struct Modifiers: u32 {
        const SHIFT = 0x01;
        const CTRL = 0x04;
        const ALT = 0x08;
        const META = 0x40;
    }

    #[derive(Debug)]
    pub struct Locks: u32 {
        const CAPSLOCK = 0x0002;
        const NUMLOCK = 0x0100;
        const SCROLLLOCK = 0x8000;
    }
}

pub struct OutputDriver {
    session_state: SessionState,
    event_queue: EventQueue<SessionState>,
    modifiers: Modifiers,
    locks: Locks,
}

impl OutputDriver {
    pub fn new() -> Self {
        let conn = Connection::connect_to_env().unwrap();
        let display = conn.display();

        let mut event_queue = conn.new_event_queue();
        let qh = event_queue.handle();

        let _registry = display.get_registry(&qh, ());

        let mut state = SessionState {
            keyboard_manager: None,
            keyboard: None,
            pointer_manager: None,
            pointer: None,
            seat: None,
        };

        // bind seat and virtual keyboard/pointer manager
        event_queue.roundtrip(&mut state).unwrap();
        // create virtual keyboard/pointer by seat and manager
        event_queue.roundtrip(&mut state).unwrap();

        Self {
            session_state: state,
            event_queue: event_queue,
            modifiers: Modifiers::empty(),
            locks: Locks::empty(),
        }
    }

    pub fn key_press(&mut self, key: evdev::KeyCode) {
        if let Some(keyboard) = &self.session_state.keyboard {
            info!("Key pressed: {:?}", key);
            keyboard.key(0, key.code().into(), KeyState::Pressed.into());
            self.event_queue.roundtrip(&mut self.session_state).unwrap();
        }
    }

    pub fn key_release(&mut self, key: evdev::KeyCode) {
        if let Some(keyboard) = &self.session_state.keyboard {
            info!("Key released: {:?}", key);
            keyboard.key(0, key.code().into(), KeyState::Released.into());
            self.event_queue.roundtrip(&mut self.session_state).unwrap();
        }
    }

    pub fn append_mod(&mut self, mkey: Modifiers) {
        info!("Mod appended: {:?}", mkey);
        self.modifiers.insert(mkey);
        self.update_state();
    }

    pub fn remove_mod(&mut self, mkey: Modifiers) {
        info!("Mod removed: {:?}", mkey);
        self.modifiers.remove(mkey);
        self.update_state();
    }

    pub fn append_lock(&mut self, lkey: Locks) {
        info!("Lock appended: {:?}", lkey);
        self.locks.insert(lkey);
        self.update_state();
    }

    pub fn remove_lock(&mut self, lkey: Locks) {
        info!("Lock removed: {:?}", lkey);
        self.locks.remove(lkey);
        self.update_state();
    }

    pub fn test(&mut self) {
        if let Some(pointer) = &self.session_state.pointer {
            pointer.motion(0, 50.0, 50.0);
            self.event_queue.roundtrip(&mut self.session_state).unwrap();
        }
    }

    fn update_state(&mut self) {
        if let Some(keyboard) = &self.session_state.keyboard {
            keyboard.modifiers(self.modifiers.bits(), 0, self.locks.bits(), 0);
            self.event_queue.roundtrip(&mut self.session_state).unwrap();
        }
    }
}

impl Drop for OutputDriver {
    fn drop(&mut self) {
        if let Some(keyboard) = &self.session_state.keyboard {
            keyboard.destroy();
            self.event_queue.roundtrip(&mut self.session_state).unwrap();
        } else {
            warn!("OutputDriver failed to destroy keyboard");
        }
        if let Some(pointer) = &self.session_state.pointer {
            pointer.destroy();
            self.event_queue.roundtrip(&mut self.session_state).unwrap();
        } else {
            warn!("OutputDriver failed to destroy pointer");
        }
    }
}
