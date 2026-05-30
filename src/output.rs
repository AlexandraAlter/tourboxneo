use bitflags::Flags;
/// Substantially copied from https://github.com/ptazithos/wkeys/tree/main/wkeys/src/native
use log::{info, warn};

use std::time::SystemTime;
use std::{fs::File, io::Write, os::fd::AsFd, path::PathBuf};
use wayland_client::{
    Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum,
    protocol::{
        wl_keyboard::{self, KeyState},
        wl_pointer::{Axis as WlAxis, AxisSource, ButtonState},
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

use crate::actions::{Axis, Modifiers};

impl From<Axis> for WlAxis {
    fn from(value: Axis) -> Self {
        match value {
            Axis::VerticalScroll => WlAxis::VerticalScroll,
            Axis::HorizontalScroll => WlAxis::HorizontalScroll,
        }
    }
}

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

pub struct OutputDriver {
    session_state: SessionState,
    event_queue: EventQueue<SessionState>,
    modifiers: Modifiers,
    latches: Modifiers,
    locks: Modifiers,
    start_time: SystemTime,
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
            latches: Modifiers::empty(),
            locks: Modifiers::empty(),
            start_time: SystemTime::now(),
        }
    }

    fn get_cur_ms(&self) -> u32 {
        self.start_time.elapsed().unwrap().as_millis() as u32
    }

    pub fn key_press(&mut self, key: evdev::KeyCode) {
        if let Some(keyboard) = &self.session_state.keyboard {
            info!("Key pressed: {:?}", key);
            keyboard.key(
                self.get_cur_ms(),
                key.code().into(),
                KeyState::Pressed.into(),
            );
            self.event_queue.roundtrip(&mut self.session_state).unwrap();
        }
    }

    pub fn key_repeat(&mut self, key: evdev::KeyCode) {
        if let Some(keyboard) = &self.session_state.keyboard {
            info!("Key repeated: {:?}", key);
            keyboard.key(
                self.get_cur_ms(),
                key.code().into(),
                KeyState::Repeated.into(),
            );
            self.event_queue.roundtrip(&mut self.session_state).unwrap();
        }
    }

    pub fn key_release(&mut self, key: evdev::KeyCode) {
        if let Some(keyboard) = &self.session_state.keyboard {
            info!("Key released: {:?}", key);
            keyboard.key(
                self.get_cur_ms(),
                key.code().into(),
                KeyState::Released.into(),
            );
            self.event_queue.roundtrip(&mut self.session_state).unwrap();
        }
    }

    pub fn mod_append(&mut self, mkey: Modifiers) {
        info!("Mod appended: {:?}", mkey);
        self.modifiers.insert(mkey);
        self.update_state();
    }

    pub fn mod_remove(&mut self, mkey: Modifiers) {
        info!("Mod removed: {:?}", mkey);
        self.modifiers.remove(mkey);
        self.update_state();
    }

    pub fn mods_clear(&mut self) {
        info!("Mods cleared");
        self.modifiers.clear();
        self.update_state();
    }

    pub fn latch_append(&mut self, mkey: Modifiers) {
        info!("Latch appended: {:?}", mkey);
        self.latches.insert(mkey);
        self.update_state();
    }

    pub fn latch_remove(&mut self, mkey: Modifiers) {
        info!("Latch removed: {:?}", mkey);
        self.latches.remove(mkey);
        self.update_state();
    }

    pub fn latches_clear(&mut self) {
        info!("Latches cleared");
        self.latches.clear();
        self.update_state();
    }

    pub fn lock_append(&mut self, lkey: Modifiers) {
        info!("Lock appended: {:?}", lkey);
        self.locks.insert(lkey);
        self.update_state();
    }

    pub fn lock_remove(&mut self, lkey: Modifiers) {
        info!("Lock removed: {:?}", lkey);
        self.locks.remove(lkey);
        self.update_state();
    }

    pub fn locks_clear(&mut self) {
        info!("Locks cleared");
        self.locks.clear();
        self.update_state();
    }

    fn update_state(&mut self) {
        if let Some(keyboard) = &self.session_state.keyboard {
            keyboard.modifiers(
                self.modifiers.bits(),
                self.latches.bits(),
                self.locks.bits(),
                0,
            );
            self.event_queue.roundtrip(&mut self.session_state).unwrap();
        }
    }

    pub fn ptr_motion(&mut self, dx: f64, dy: f64) {
        if let Some(pointer) = &self.session_state.pointer {
            pointer.motion(self.get_cur_ms(), dx, dy);
            self.event_queue.roundtrip(&mut self.session_state).unwrap();
        }
    }

    pub fn ptr_motion_absolute(&mut self, x: u32, y: u32, x_extent: u32, y_extent: u32) {
        if let Some(pointer) = &self.session_state.pointer {
            pointer.motion_absolute(self.get_cur_ms(), x, y, x_extent, y_extent);
            self.event_queue.roundtrip(&mut self.session_state).unwrap();
        }
    }

    pub fn ptr_button(&mut self, button: u32, released: bool) {
        if let Some(pointer) = &self.session_state.pointer {
            let state = if released {
                ButtonState::Released
            } else {
                ButtonState::Pressed
            };
            warn!("Pointer button: {}, {:?}", button, state);
            pointer.button(self.get_cur_ms(), button, state);
            self.event_queue.roundtrip(&mut self.session_state).unwrap();
        }
    }

    pub fn ptr_axis(&mut self, axis: Axis, value: f64) {
        if let Some(pointer) = &self.session_state.pointer {
            info!("Scrolled: {:?} by {:?}", axis, value);
            pointer.axis(self.get_cur_ms(), axis.into(), value);
            self.event_queue.roundtrip(&mut self.session_state).unwrap();
        }
    }

    pub fn ptr_frame(&mut self) {
        if let Some(pointer) = &self.session_state.pointer {
            pointer.frame();
            self.event_queue.roundtrip(&mut self.session_state).unwrap();
        }
    }

    pub fn ptr_axis_source(&mut self, axis_source: AxisSource) {
        if let Some(pointer) = &self.session_state.pointer {
            pointer.axis_source(axis_source);
            self.event_queue.roundtrip(&mut self.session_state).unwrap();
        }
    }

    pub fn ptr_axis_stop(&mut self, axis: Axis) {
        if let Some(pointer) = &self.session_state.pointer {
            pointer.axis_stop(self.get_cur_ms(), axis.into());
            self.event_queue.roundtrip(&mut self.session_state).unwrap();
        }
    }

    pub fn ptr_axis_discrete(&mut self, axis: Axis, value: f64, discrete: i32) {
        if let Some(pointer) = &self.session_state.pointer {
            pointer.axis_discrete(self.get_cur_ms(), axis.into(), value, discrete);
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
