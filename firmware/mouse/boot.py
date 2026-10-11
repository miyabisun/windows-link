# Runs before USB starts: the board becomes a mouse plus a second serial port
# (usb_cdc.data) for windows-link's commands. The REPL console stays available.
import supervisor
import usb_cdc
import usb_hid

usb_cdc.enable(console=True, data=True)
usb_hid.enable((usb_hid.Device.MOUSE,))
supervisor.set_usb_identification(manufacturer="miyabisun", product="windows-link mouse")
