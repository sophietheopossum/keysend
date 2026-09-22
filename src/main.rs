//! keysend - send one key press+release to whatever Wayland surface has keyboard focus.
//!
//! A test harness, not a desktop component. QML key handling cannot be judged by
//! reading it: whether a `Shortcut` in a Quickshell window ever matches, and whether
//! a `Keys` handler is reached, both depend on runtime focus state. This makes that
//! observable without asking a human to press a key.
//!
//! Blast radius: smithay pushes the virtual keyboard's keymap to the FOCUSED client
//! only (virtual_keyboard_handle.rs sends the keymap ahead of every key), so this
//! reads the seat's real keymap and hands that exact fd back. The focused client
//! therefore sees the layout it already had, and no other client is touched at all.
//!
//! Build it OUT of the repo, the way everything else here is built:
//!
//!     CARGO_TARGET_DIR=~/.cache/claude-builds/keysend-target \
//!         cargo build --release --offline --manifest-path tools/keysend/Cargo.toml
//!
//! Focusing the target first is protocol-specific, and the two shapes differ:
//!   - a toplevel (MinkaLedger): activate it over the ShojiWM IPC,
//!     {"method":"windows.activate","params":{"windowId":"0x91"}}
//!   - a layer surface (MinkaShell's start menu, MinkaShot's capture overlay):
//!     there is nothing to activate -- they set WlrLayershell.keyboardFocus from
//!     their own open/armed state, so trigger that state and focus follows.
//!
//! Pair it with a probe copy of the app carrying an IpcHandler that reports
//! Window.activeFocusItem. Reading the source cannot tell you where focus is: on
//! 21/9/2026 four separate readings of MinkaLedger's dead Escape all looked correct
//! and all were wrong, and three injected keypresses found the cause.

use std::os::fd::{AsFd, OwnedFd};
use std::time::{SystemTime, UNIX_EPOCH};

use wayland_client::protocol::{wl_keyboard, wl_registry, wl_seat};
use wayland_client::{Connection, Dispatch, QueueHandle};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::{
    zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1,
    zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1,
};

/// evdev codes, which is what wl_keyboard carries (XKB keycode minus 8).
fn keycode(name: &str) -> Option<u32> {
    Some(match name.to_ascii_lowercase().as_str() {
        "escape" | "esc" => 1,
        "return" | "enter" => 28,
        "tab" => 15,
        "space" => 57,
        "up" => 103,
        "down" => 108,
        "left" => 105,
        "right" => 106,
        "delete" => 111,
        other => return other.parse().ok(),
    })
}

#[derive(Default)]
struct App {
    seat: Option<wl_seat::WlSeat>,
    manager: Option<ZwpVirtualKeyboardManagerV1>,
    keymap: Option<(u32, OwnedFd, u32)>,
}

impl Dispatch<wl_registry::WlRegistry, ()> for App {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global { name, interface, version } = event {
            match interface.as_str() {
                "wl_seat" => {
                    state.seat =
                        Some(registry.bind::<wl_seat::WlSeat, _, _>(name, version.min(7), qh, ()));
                }
                "zwp_virtual_keyboard_manager_v1" => {
                    state.manager = Some(registry.bind::<ZwpVirtualKeyboardManagerV1, _, _>(
                        name, 1, qh, (),
                    ));
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<wl_keyboard::WlKeyboard, ()> for App {
    fn event(
        state: &mut Self,
        _: &wl_keyboard::WlKeyboard,
        event: wl_keyboard::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        // The seat hands every client the keymap on get_keyboard, focus or not; that
        // is the copy handed straight back to the virtual keyboard below.
        if let wl_keyboard::Event::Keymap { format, fd, size } = event {
            state.keymap = Some((format.into(), fd, size));
        }
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for App {
    fn event(_: &mut Self, _: &wl_seat::WlSeat, _: wl_seat::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}
impl Dispatch<ZwpVirtualKeyboardManagerV1, ()> for App {
    fn event(_: &mut Self, _: &ZwpVirtualKeyboardManagerV1, _: <ZwpVirtualKeyboardManagerV1 as wayland_client::Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}
impl Dispatch<ZwpVirtualKeyboardV1, ()> for App {
    fn event(_: &mut Self, _: &ZwpVirtualKeyboardV1, _: <ZwpVirtualKeyboardV1 as wayland_client::Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let name = std::env::args().nth(1).unwrap_or_else(|| "escape".into());
    let code = keycode(&name).ok_or_else(|| format!("unknown key: {name}"))?;

    let conn = Connection::connect_to_env()?;
    let mut queue = conn.new_event_queue();
    let qh = queue.handle();
    conn.display().get_registry(&qh, ());

    let mut app = App::default();
    queue.roundtrip(&mut app)?;

    let seat = app.seat.clone().ok_or("no wl_seat")?;
    let manager = app.manager.clone().ok_or("compositor has no zwp_virtual_keyboard_manager_v1")?;

    let keyboard = seat.get_keyboard(&qh, ());
    queue.roundtrip(&mut app)?;
    let (format, fd, size) = app.keymap.take().ok_or("seat sent no keymap")?;

    let vk = manager.create_virtual_keyboard(&seat, &qh, ());
    vk.keymap(format, fd.as_fd(), size);
    // No modifiers held: a bare key, whatever the user happens to be leaning on.
    vk.modifiers(0, 0, 0, 0);

    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as u32;
    vk.key(now, code, 1);
    vk.key(now + 12, code, 0);
    queue.roundtrip(&mut app)?;

    vk.destroy();
    keyboard.release();
    queue.roundtrip(&mut app)?;
    println!("sent {name} (evdev {code}) to the focused surface");
    Ok(())
}
