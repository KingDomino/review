# Review

**A fast, Preview-style viewer and editor for PDFs and images on Linux, written in Rust.**

Review brings the macOS Preview workflow to Linux: open PDFs and photos instantly, mark
them up, sign, fill forms, rearrange pages, crop, resize, adjust colour and export. Its
menus, shortcuts and panels follow Preview's own (recovered with
[REA](https://github.com/morluto/rea), see [docs/preview-analysis.md](docs/preview-analysis.md)),
but every line is original code built on [egui](https://github.com/emilk/egui) and
[MuPDF](https://mupdf.com).

## Install (Fedora / any Linux)

```sh
cd review
./install.sh            # per-user install into ~/.local
./install.sh --default  # optional: make Review the default for PDFs and images
```

The installer uses the prebuilt binary in `dist/` when it matches your CPU (aarch64 or
x86_64, glibc 2.28+). Otherwise, or with `./install.sh --build`, it compiles from source.
It installs `gcc gcc-c++ make clang-devel pkgconf-pkg-config` with dnf if they're missing,
plus a user-local Rust via rustup if your system Rust is older than 1.95. The first source
build takes a few minutes because MuPDF is compiled too.

It adds `review` to `~/.local/bin` and **Review** to the application menu with every
supported file type, so you can double-click files in Dolphin or use *Open With → Review*.
`./install.sh --uninstall` removes it.

Optional runtime helpers (the installer tells you which ones are missing):

| For | Package |
|---|---|
| HEIC/HEIF/AVIF photos | `libheif`, plus `libheif-freeworld` (RPM Fusion) for iPhone HEVC photos |
| Printing | `cups-client` (`lp`) |
| File ▸ Take Screenshot | `spectacle` (KDE), `gnome-screenshot`, or `grim` + `slurp` |
| Edit ▸ Speech | `speech-dispatcher-utils` |

## What it does

| Preview feature | Review |
|---|---|
| PDF, XPS, EPUB, CBZ, FB2, MOBI, SVG; PNG, JPEG, GIF, WebP, TIFF (multi-page), BMP, ICO, TGA, PNM, DDS, HDR, EXR, QOI, JPEG 2000, PSD, HEIC/HEIF/AVIF | ✓ |
| Several files in one window, grouped in the sidebar; opening more files from Dolphin adds them to the running window | ✓ |
| Thumbnails, Table of Contents, Highlights and Notes, Bookmarks, Contact Sheet | ✓ |
| Continuous / Single Page / Two Pages; Actual Size, Zoom to Fit, Ctrl+scroll zoom anchored at the pointer, Zoom to Selection | ✓ |
| Use Dark Appearance for PDF (keeps colours readable, not a plain invert) | ✓ |
| Text selection (drag, double-click word, triple-click line) and copy; links; search with a result list, snippets and highlighting | ✓ |
| Markup: highlight / underline / strikethrough, sketch, line, arrow, rectangle, rounded rectangle, oval, star, polygon, speech bubble, text boxes, notes; border and fill colour, line width, font size and alignment | ✓ saved as standard PDF annotations, editable in other apps; existing annotations are editable too |
| Signatures: draw once, reuse | ✓ |
| Fill PDF forms (text, checkboxes, radio buttons, choice lists; Tab moves to the next field) | ✓ |
| Redact (removes the content underneath when you save) | ✓ |
| Reorder pages by dragging thumbnails, even between documents; delete, rotate, crop pages; insert blank pages or pages from files | ✓ non-destructive until you save |
| Image: crop, rotate, flip, Adjust Size, Adjust Color (levels, exposure, contrast, highlights, shadows, saturation, temperature, tint, sepia, sharpness, auto levels — live preview) | ✓ |
| Instant Alpha (drag to grow a colour selection), Remove Background, delete/invert selection | ✓ |
| Export As (PNG, JPEG with quality, WebP, TIFF, BMP, GIF, PDF); Export as PDF with AES-256 encryption | ✓ |
| Password-protected PDFs; Edit Permissions | ✓ |
| Inspector: file info, editable PDF metadata and keywords, EXIF, GPS location (opens OpenStreetMap), annotations, security | ✓ |
| Magnifier (\`), Slideshow, Full Screen, Print (CUPS) | ✓ |
| New from Clipboard, Take Screenshot, Duplicate, Rename (double-click the title), Move to Trash, Revert to Saved | ✓ |
| Undo/redo for everything, with Preview's action names | ✓ |
| Live Text (OCR), camera RAW, JPEG XL, 3D models, ColorSync soft-proofing, scanner import | not yet |

### Shortcuts

Preview's shortcuts with **Ctrl** for ⌘ and **Alt** for ⌥. Preview's ⌃⌘ markup shortcuts
become **Alt+Shift** (Alt+Shift+H highlight, R rectangle, O oval, A arrow, I line, T text,
N note), because KDE reserves Ctrl+Alt+T and Ctrl+Alt+L. Full screen is **F11**. Hold
**Space** and drag, or middle-drag, to pan. Help ▸ Keyboard Shortcuts lists them all.

## Why it's fast and reliable

* **Nothing blocks the window.** Each PDF has its own MuPDF worker thread. Pages are
  rasterised in 512 px tiles on a thread pool, newest-first, and stale work is cancelled
  while you scroll. You see a sharp preview immediately, then full resolution.
  Measured: a 2,000-page PDF opens in **9 ms** and shows its first page in **16 ms**.
  Searching it streams 90,000 hits while you keep scrolling.
* **Vulkan rendering** through wgpu, falling back to OpenGL automatically on systems
  without a Vulkan driver.
* **Opens instantly from Dolphin.** A second launch hands its files to the running window
  over a socket and exits.
* **Edits are non-destructive** until you save. Saving writes a fresh copy of the
  original file and then atomically replaces it, so a crash can't corrupt your document.
* **Bounded memory.** Images decode on a small pool, and unedited images you aren't
  looking at are unloaded once decoded pixels exceed 1.5 GB.
* **Tested.** The engine, imaging pipeline and real UI flows (mouse-drawn shapes, text
  selection, thumbnail drag & drop, keyboard undo, save and re-open) run headless in CI-style tests.

## Building and developing

```sh
cargo build --release            # needs a C compiler, make and libclang (MuPDF)
cargo test                       # engine, imaging and headless UI tests
cargo test -- --ignored          # also writes screenshots to $TMPDIR/review-shots
```

`scripts/cross-build.sh` builds the Linux binaries in `dist/` from macOS using Zig.

| Module | Role |
|---|---|
| `src/pdf.rs` | MuPDF worker per document, tile raster pool, text, search, annotation import/export, saving |
| `src/imaging.rs`, `src/heif.rs` | Image decoding, EXIF, colour pipeline, background removal, burning in markup, export |
| `src/model.rs`, `src/document.rs` | Cloneable document model (pages, annotations, form values), undo, save plans |
| `src/canvas.rs` | Page layout, zoom/scroll, every pointer tool, form fields, magnifier, slideshow |
| `src/render.rs`, `src/annots.rs` | Tile/texture caches and drawing pages and annotations |
| `src/layout.rs`, `src/sidebar.rs`, `src/inspector.rs`, `src/dialogs.rs` | Window chrome, panels and dialogs |
| `src/app.rs` | Commands, shortcuts, file handling, events |

## License

GNU Affero General Public License v3.0 or later (required by MuPDF).
Fonts: Inter (SIL Open Font License), Phosphor Icons (MIT).
Review is not affiliated with Apple. "Preview" and "macOS" are trademarks of Apple Inc.
