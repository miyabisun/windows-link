# windows-link's mouse: one command per line on the data serial port.
#   ?      -> "windows-link mouse <version>"
#   W <n>  -> turn the wheel n notches (negative scrolls down), then "ok"
import time

import usb_cdc
import usb_hid

VERSION = "1"
MAX_NOTCHES = 20
# Pause between notches, so programs see each one.
NOTCH_PAUSE = 0.02

port = usb_cdc.data
port.timeout = 0.1
mouse = next(d for d in usb_hid.devices if d.usage_page == 0x01 and d.usage == 0x02)


def turn(notches):
    step = 1 if notches > 0 else -1
    for _ in range(abs(notches)):
        # Report: buttons, x, y, wheel (a signed byte).
        mouse.send_report(bytes((0, 0, 0, step & 0xFF)))
        time.sleep(NOTCH_PAUSE)


def answer(line):
    if line == "?":
        return "windows-link mouse " + VERSION
    if line.startswith("W "):
        try:
            notches = int(line[2:])
        except ValueError:
            return "error not a number"
        if notches == 0 or abs(notches) > MAX_NOTCHES:
            return "error out of range"
        turn(notches)
        return "ok"
    return "error unknown command"


pending = b""
while True:
    pending += port.read(64) or b""
    while b"\n" in pending:
        raw, pending = pending.split(b"\n", 1)
        reply = answer(raw.decode("utf-8", "replace").strip())
        port.write((reply + "\n").encode())
