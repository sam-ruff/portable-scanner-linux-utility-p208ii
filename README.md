# p208ii

A Linux driver and desktop app for the Canon imageFORMULA P-208II, written in Rust. It talks to the scanner over USB from userspace and saves each receipt as a numbered PNG in `~/Pictures/Scans`.

The USB protocol is ported from the SANE `canon_dr` backend. Scanning and the hardware start button have been tested with a P-208II on Linux. If something goes wrong, `scanimage` from SANE drives the same scanner and makes a useful comparison.

## Setting up the scanner

Set the AUTO START switch on the back of the scanner to OFF. With it on, the P-208II pretends to be a CD drive holding Canon's Windows software, and the driver will say it found a Canon device that is not in scanner mode.

Install the udev rule so you can use the scanner without root, then unplug it and plug it back in:

```sh
sudo cp packaging/60-canon-p208ii.rules /etc/udev/rules.d/
sudo udevadm control --reload
```

## Installing the app

After building, run `./target/release/p208ii install`. This installs the binary in `~/.local/bin`, adds Receipt Scanner to the applications menu and enables a user service that opens it when you press the scanner's blue button. The listener runs during your desktop session and leaves the scanner alone while the app is open.

Run the install command again to update. `p208ii uninstall` removes the app and listener, keeping your scans and preferences. USB access still needs the udev rule above; installation also saves a copy in `~/.local/share/p208ii`.

## Using it

Running `p208ii` with no arguments opens the app. Pick a folder, press Start and feed receipts in one at a time. The app watches the paper sensor, scans each sheet as it goes in and keeps going until you press Stop. Settings has colour mode, resolution (150 to 600 dpi), paper width and double-sided scanning. Its advanced mode shows the driver's log.

Feed paper requests a single feed and discharge without scanning. The scanner still requires its paper sensor to detect a sheet. It cannot currently force the rollers to run continuously to clear a jam.

Scans are named `scan-0001.png`, `scan-0002.png` and so on, carrying on from the highest number already in the folder. The back of a double-sided scan is `scan-0001-back.png`, except in PDF mode, where both sides go into one two-page document. PNG, JPEG, TIFF and PDF are available. Each file is written under a hidden `.part` name and renamed once complete, so a script watching the folder never picks up half an image. PNG, JPEG and PDF record the scan resolution, which OCR tools use to judge text size.

Smart crop, on by default, trims the grey scanner backing from around the receipt. It measures the backing at the left and right edges of the scan, so it works best with the paper width left at Full. If it can't tell the receipt from the backing, it saves the whole scan unchanged.

## Optional AI naming

Settings has an AI assistance section. The app looks for installed Claude and Codex CLIs in the background when it opens, and lets you choose which to use. AI naming is off by default; your choice is saved between launches. The selected tool uses its existing login and model settings and sends receipt images to its configured AI service.

New scans are saved immediately. AI works separately to name them using the merchant, date and total where readable, such as `2026-09-28-example-shop-12.30-GBP.png`. A side is removed only when AI identifies it as blank and a local image check agrees. A two-sided PDF is rebuilt when one page is blank. Both sides are kept when they contain information, and existing named receipts are never overwritten.

Missing tools, exhausted usage limits, timeouts, uncertain results and a busy AI worker leave the numbered scans intact. Replacement files are saved before originals are removed. Closing the app can interrupt pending AI work; the original scans remain available. AI naming applies to scans made in the desktop app.

## Command line

The same scanning is available from a terminal:

```sh
p208ii info
p208ii scan --output ~/receipts --mode grey --dpi 300 --width-mm 80
p208ii scan --continuous --smart-crop --format pdf
```

`scan` prints the path of each file it saves. Add `--simulate` to any command, or to the app, to use a pretend scanner that produces fake receipts.

## Building

```sh
cargo build --release
```

The result is one binary that links only against the C library, libm and libgcc_s. The window uses the system's graphics drivers (Vulkan or OpenGL, through X11 or Wayland), which it loads at runtime. Set `RUST_LOG=debug` to see the commands sent to the scanner.

## Licence

Licensed under either the [MIT licence](LICENSE-MIT) or the [Apache License, Version 2.0](LICENSE-APACHE), at your option. Unless you say otherwise, any contribution you submit is dual licensed in the same way.
