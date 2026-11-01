# WiFi Spanner

A simple Wi-Fi Scanner and Planner (hense the _awful_ name!). Your - hopefully - one stop shop for site surveys and layout planning for Linux: import a floor plan, model walls, walk the building, and record the signal strength and SSIDs the laptop sees.
Think NetSpot or Hamina, but native on Linux.

## Layout

| Crate | Purpose |
|---|---|
| `wifispanner-scan` | Scanner backends (Linux `iw` + NetworkManager, and a mock) and output parsers |
| `wifispanner-core` | Project model and RF prediction |
| `wifispanner-spike` | Command-line scanner for checking what the radio reports |
| `wifispanner-app` | egui desktop app |

On Linux the scanner asks NetworkManager to rescan (`nmcli`, allowed for the
logged-in user via polkit) and reads the kernel scan cache with `iw`. No root
is needed. Elsewhere the app runs on mock data so the UI can be developed
without Wi-Fi hardware.

## Running on Linux (AppImage)

Download `WiFiSpanner-x86_64.AppImage` from the latest Actions run, then:

```bash
chmod +x WiFiSpanner-x86_64.AppImage
./WiFiSpanner-x86_64.AppImage            # GUI
./WiFiSpanner-x86_64.AppImage --spike    # CLI scan (add --watch to repeat)
```

Requirements: a **Wayland** session (the default on current Fedora, Ubuntu,
openSUSE and other mainstream distributions), `iw`, and NetworkManager
(`nmcli`). Wayland is the primary target: pen and graphics-tablet input uses
Wayland's tablet protocol and needs a compositor that provides it (KDE Plasma,
GNOME and others do). An X11 session still runs the app for mouse and touch, but
pens are not supported there. Set `WS_PEN_DEBUG=1` to print pen events to the
terminal when diagnosing a tablet. On Fedora, if the AppImage
complains about FUSE, either `sudo dnf install fuse-libs` or run it with
`--appimage-extract-and-run`.

## Building

```bash
cargo test --workspace
cargo run -p wifispanner-spike -- --mock
cargo run -p wifispanner-app
```

The AppImage is built on Linux only (`packaging/build-appimage.sh`), by CI on
every push to `main`.

## Reporting bugs

Found a bug? Email [bugsplat@byte-size.co.za](mailto:bugsplat@byte-size.co.za) (the menu has a *Report a bug...* window with the details worth including). The mailbox is not monitored directly: reports are handled in order of how critical they are, and sending one does not guarantee a reply or a fix. Check the change log of each release to see whether it has been fixed. Feature requests are not taken.

## Licence

Copyright (C) 2026 Byte Size IT.

WiFi Spanner is free software under the
[GNU General Public License, version 3 or later](LICENSE), with additional
attribution terms under section 7 (see [NOTICE](NOTICE)): copies and
derivative works must keep the copyright notice and show "Based on WiFi
Spanner by Byte Size IT" with a link to this repository, and modified versions
must carry a different name and logo.

Third-party libraries, fonts and data files keep their own licences, listed in
[THIRD-PARTY-LICENSES.txt](THIRD-PARTY-LICENSES.txt). Regenerate it with
`python3 -I packaging/third-party-licenses.py > THIRD-PARTY-LICENSES.txt`.
The optional IBM Carbon look uses IBM Plex Sans (SIL OFL 1.1) and IBM Carbon icons (Apache 2.0,
recoloured and rasterised); their licence texts are in that file and in `packaging/licenses/`.
Reading dimensions off a plan uses the Ocrs engine and its two pre-trained models (CC BY-SA 4.0, trained on
HierText by Google Research), listed in the same file.
