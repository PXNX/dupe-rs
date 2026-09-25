<div align="center">
  <img src="assets/icon.png" alt="dupe-rs logo" width="96" height="96">

  # dupe-rs

  A fast, native Windows duplicate-file finder.
</div>

<p align="center">
  <img src="assets/screenshot.png" alt="dupe-rs screenshot">
</p>

<!--
TODO: assets/screenshot.png needs to be a real screenshot of the app (e.g. the
table view with a scan result loaded). None exists yet — take one locally
(cargo run --release, scan a folder, screenshot the window) and drop it in
assets/screenshot.png.
-->

## Features

- **Exact duplicates** — byte-identical files found via content hashing (blake3), with a
  cheap partial-hash pre-filter so large trees don't pay for a full read of every file.
- **Similar media** — images/videos that look like the same shot at a different
  resolution (re-encodes, resizes, thumbnails), found via perceptual hashing. Needs
  [ffmpeg](https://ffmpeg.org/) on `PATH` to compare videos.
- **Copy-named filter** — narrow results to duplicates whose filename looks like an
  OS/user-generated copy of the original, e.g. `photo (2).jpg` or `photo - Kopie.jpg`
  next to `photo.jpg`.
- Table and thumbnail grid views, with per-group highlighting of the largest file,
  highest resolution, and oldest created/modified copy.
- Bulk selection by criterion (oldest, newest, shortest/longest path), size and
  extension filters, "same folder only" matching, and safe deletion straight to the
  Recycle Bin with progress reporting.
- **Drive fill** — copy whole top-level folders of a source folder onto a target drive,
  choosing the combination that fills its free space as completely as possible. Copied
  files are hashed on the way and added to the reverse-search index, so you can later
  look up which drive a file ended up on.
- **Re-encode** — shrink PNG/BMP/TIFF/... images losslessly (lossless WebP or
  max-compression PNG, verified pixel for pixel), and with ffmpeg on `PATH` re-encode
  lossless-codec videos to FFV1, or optionally everything to visually lossless H.265.

## Installing

Prebuilt Windows binaries are published on the
[Releases page](https://github.com/PXNX/dupe-rs/releases).

## Building from source

Requires a recent [Rust toolchain](https://rustup.rs/).

```sh
cargo build --release
```

The binary is written to `target/release/dupe-rs.exe`.

## License

No license has been declared for this project yet.
