# What the hell is this?

Tourbox is a small usb device by Tourbox Tech Inc. Which lets you assign custom program actions and hotkeys.
This Rust program translates the serial IO to simulated Wayland inputs.

## Execution

    cargo run -- -vv --config=<config>

## Attributions

Based heavily on the work of https://github.com/bloodywing/tourboxneo
MIO Timer copied from https://github.com/idletea/mio-timerfd 
Wayland output based on https://github.com/ptazithos/wkeys/tree/main/wkeys/src/native

