# windows-link's mouse: one command per line on the data serial port.
#   ?         -> "windows-link mouse <version>"
#   W <n>     -> turn the wheel n notches (negative scrolls down), then "ok"
#   B <mask>  -> hold down the buttons of mask (1 left, 2 right, 4 middle) and let
#                the others go, then "ok"
#   M <x> <y> -> move by x to the right and y down (each -127 to 127), then "ok"
import time

import usb_cdc
import usb_hid

VERSION = "2"
MAX_NOTCHES = 20
# Pause between notches, so programs see each one.
NOTCH_PAUSE = 0.02
BUTTONS = 0b111
MAX_MOVE = 127
# A button stays down this long without a command, then the board lets go by itself:
# a PC program that stopped in the middle must not leave the button held.
HOLD_LIMIT = 3.0

port = usb_cdc.data
mouse = next(d for d in usb_hid.devices if d.usage_page == 0x01 and d.usage == 0x02)

held = 0
last_command = time.monotonic()


def report(x=0, y=0, wheel=0):
    # Report: buttons, x, y, wheel (signed bytes).
    mouse.send_report(bytes((held, x & 0xFF, y & 0xFF, wheel & 0xFF)))


def turn(notches):
    step = 1 if notches > 0 else -1
    for _ in range(abs(notches)):
        report(wheel=step)
        time.sleep(NOTCH_PAUSE)


def number(text):
    try:
        return int(text)
    except ValueError:
        return None


def answer(line):
    global held
    if line == "?":
        return "windows-link mouse " + VERSION
    if line.startswith("W "):
        notches = number(line[2:])
        if notches is None:
            return "error not a number"
        if notches == 0 or abs(notches) > MAX_NOTCHES:
            return "error out of range"
        turn(notches)
        return "ok"
    if line.startswith("B "):
        mask = number(line[2:])
        if mask is None:
            return "error not a number"
        if mask < 0 or mask > BUTTONS:
            return "error out of range"
        held = mask
        report()
        return "ok"
    if line.startswith("M "):
        steps = [number(part) for part in line[2:].split()]
        if len(steps) != 2 or None in steps:
            return "error not two numbers"
        if max(abs(step) for step in steps) > MAX_MOVE:
            return "error out of range"
        report(steps[0], steps[1])
        return "ok"
    return "error unknown command"


pending = b""
while True:
    waiting = port.in_waiting
    if waiting:
        pending += port.read(waiting)
        while b"\n" in pending:
            raw, pending = pending.split(b"\n", 1)
            reply = answer(raw.decode("utf-8", "replace").strip())
            port.write((reply + "\n").encode())
            last_command = time.monotonic()
    elif held and time.monotonic() - last_command > HOLD_LIMIT:
        held = 0
        report()
    else:
        time.sleep(0.001)
