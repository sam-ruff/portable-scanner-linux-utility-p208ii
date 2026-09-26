# p208ii

A Linux driver and desktop app for the Canon imageFORMULA P-208II, written in Rust. It talks to the scanner over USB from userspace and saves each receipt as a numbered PNG in `~/Pictures/Scans`.

The USB protocol is ported from the SANE `canon_dr` backend, which has supported the P-208 family for years. But this driver has so far only run against its built-in simulator, so treat the first scans on real hardware as a test. If something goes wrong, `scanimage` from SANE drives the same scanner and makes a useful comparison.

## Setting up the scanner

Set the AUTO START switch on the back of the scanner to OFF. With it on, the P-208II pretends to be a CD drive holding Canon's Windows software, and the driver will say it found a Canon device that is not in scanner mode.

Install the udev rule so you can use the scanner without root, then unplug it and plug it back in:

```sh
sudo cp packaging/60-canon-p208ii.rules /etc/udev/rules.d/
sudo udevadm control --reload
```

## Using it

Running `p208ii` with no arguments opens the app. Pick a folder, press Start and feed receipts in one at a time. The app watches the paper sensor, scans each sheet as it goes in and keeps going until you press Stop. Settings has colour mode, resolution (150 to 600 dpi), paper width and double-sided scanning. Its advanced mode shows the driver's log.

Scans are named `scan-0001.png`, `scan-0002.png` and so on, carrying on from the highest number already in the folder. The back of a double-sided scan is `scan-0001-back.png`, except in PDF mode, where both sides go into one two-page document. PNG, JPEG, TIFF and PDF are available. Each file is written under a hidden `.part` name and renamed once complete, so a script watching the folder never picks up half an image. PNG, JPEG and PDF record the scan resolution, which OCR tools use to judge text size.

Smart crop, on by default, trims the grey scanner backing from around the receipt. It measures the backing at the left and right edges of the scan, so it works best with the paper width left at Full. If it can't tell the receipt from the backing, it saves the whole scan unchanged.

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
