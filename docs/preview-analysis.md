# How Review was derived from macOS Preview

Review is a clean-room reimplementation. Preview's **behaviour and feature
inventory** were studied with [REA — Reverse Engineer Anything](https://github.com/morluto/rea)
(`rea-agents` 6.4.0) on **macOS 27.0.1, Preview 11.0 (1147)**. Only shipped
metadata was inspected (Info.plist, compiled Interface Builder archives,
localisation tables, Mach-O load commands). No Apple code was decompiled,
copied or translated, and no Apple artwork ships with Review.

## REA commands used

| Command | Target | What it gave us |
|---|---|---|
| `rea inspect-plist` | `Preview.app` | Every document type Preview opens, and which it can *edit* vs only *view* |
| `rea inspect-macho` | `Contents/MacOS/Preview` | Linked frameworks and imported Objective-C classes (what Preview delegates to the OS) |
| `rea decode-interface-builder` | 64 compiled `.nib` archives | The complete menu tree with key equivalents and action selectors (`docs/rea/MainMenu-tree.txt`), plus the fields of every panel |
| `rea inspect-signature`, `inspect-asset-catalog` | `Preview.app` | Bundle identity / resources (not used further) |
| *(plist decoding of `*.loctable`)* | `Contents/Resources` | English labels for menus, panels, undo action names, inspector rows |

Reproduce with:

```sh
npx -y rea-agents@latest inspect-plist /System/Applications/Preview.app --format json
npx -y rea-agents@latest decode-interface-builder /System/Applications/Preview.app --format json --max-documents 64
npx -y rea-agents@latest inspect-macho /System/Applications/Preview.app --format json
```

## Key finding: Preview is a thin shell over system frameworks

`inspect-macho` shows the 7 MB Preview binary links **PDFKit + CorePDF** (PDF),
**ImageIO** (image codecs), **CoreImage** (colour adjustments), **PaperKit +
AnnotationKit** (markup — imported classes `AKController`, `AKArrowAnnotation`,
`AKNoteAnnotation`, …), **VisionKitCore** (Live Text / background removal:
`VKCImageAnalyzer`), **QuickLook**, **ImageCaptureCore** (scanners/cameras) and
**SceneKit/ModelIO/Hydra/SpatialPreview** (3D and Vision Pro). None of these exist
on Linux, so Review rebuilds each role:

| Preview relies on | Review uses |
|---|---|
| PDFKit / CorePDF | MuPDF (`mupdf` crate) in a worker thread per document, parallel tile rasteriser |
| ImageIO | `image` crate (PNG, JPEG, GIF, WebP, TIFF incl. multi-page, BMP, ICO, TGA, PNM, DDS, HDR, EXR, QOI), MuPDF for JPEG 2000 / PSD / JPEG XR, system libheif (loaded at runtime) for HEIC/HEIF/AVIF |
| CoreImage (`CIFilter`) | Own LUT + parallel per-pixel pipeline (`imaging::Adjust`) |
| PaperKit / AnnotationKit | Own shape model drawn with egui, written as standard PDF annotations (or burned into image pixels) |
| VisionKit "Remove Background" / Instant Alpha | Border flood-fill with feathering; tolerance-by-drag flood select |
| VisionKit Live Text (`VKCImageAnalyzer`) | Tesseract OCR mapped onto Review's selectable text model |
| ColorSync | Little CMS (embedded ICC conversion, soft proofing, assign profile) |
| SceneKit / ModelIO / Hydra (3D) | glTF, tobj, stl_io and openusd loaders + a wgpu renderer |
| ImageCaptureCore (scanners) | SANE `scanimage` |
| AppKit, NSDocument, NSUndoManager | egui/eframe on wgpu (Vulkan, with automatic OpenGL fallback), snapshot undo of a cloneable document model |
| NSPrintOperation | CUPS `lp` |
| Screenshot | Spectacle (KDE), gnome-screenshot, grim+slurp or scrot |

## Document types (CFBundleDocumentTypes)

* **Editor role:** PDF, JPEG, PNG, TIFF, BMP, GIF, TGA, SGI, ICNS, JPEG 2000.
* **Viewer role:** folders, OFD, Illustrator, camera RAW, HEIF/HEIC (+ sequences), AVIF,
  AVCI, PBM, PVR, ASTC, KTX/KTX2, WebP, DDS, XBM, OpenEXR, JPEG XL, ICO, PSD, MacPaint,
  FlashPix, fax, Radiance HDR, MPO, PICT, and 3D content (USD, glTF, MaterialX, OpenVDB, Alembic, STL, PLY…).

Review opens the common subset (including camera RAW, JPEG XL and 3D models in glTF, OBJ,
STL and USD/USDZ) plus XPS, EPUB, CBZ, FB2, MOBI and SVG through MuPDF.
Not supported: PICT, MacPaint, FlashPix, OFD, MaterialX, OpenVDB and Alembic.

## Menu tree (MainMenu.nib)

The full decoded tree is in `docs/rea/MainMenu-tree.txt`. Review keeps Preview's
structure and maps shortcuts to Linux conventions:

| Preview | Review |
|---|---|
| ⌘ | Ctrl |
| ⌥⌘ (sidebar modes, Adjust Color, Go to Page) | Ctrl+Alt |
| ⌃⌘ (markup tools: Highlight ⌃⌘H, Rectangle ⌃⌘R, Text ⌃⌘T, Note ⌃⌘N …) | Alt+Shift (Ctrl+Alt+T/L are taken by KDE) |
| ⌃⌘F full screen | F11 |
| ⌥⌘T hide toolbar | View menu (Ctrl+Alt+T opens Konsole on KDE) |
| ⌥ PageUp / PageDown — previous/next document | Alt+PgUp / Alt+PgDn |

New items found in Preview 11 that Review implements: **Use Dark Appearance for PDF**,
**Zoom All to Fit / Actual Size on All**, **Text Selection** tool, **Redact**, **Remove
Background** (⇧⌘K). Also implemented: Soft Proof with Profile and Assign Profile (Little CMS),
Import from Scanner / Insert ▸ Page from Scanner (SANE), Live Text (Tesseract) and 3D
viewing. Intentionally skipped: 3D Manipulator / Adjust Pivot (editing 3D scenes),
Import from Camera, Preview on Apple Vision Pro, Customize Toolbar.

## Panels (from the decoded nibs and loctables)

* **Adjust Color** (`PVAdjustColor`): levels histogram with black/white points, Exposure,
  Contrast, Highlights, Shadows, Saturation, Temperature, Tint, Sepia, Sharpness, Auto
  Levels, Reset All — all present, applied live on a background thread.
* **Image Dimensions** (`PVDocumentSizing`): Fit into presets, Width/Height with units
  (pixels, percent, inches, cm, mm, points), Resolution, Scale proportionally, Resample
  image, Resulting Size — all present.
* **Inspector** (`PVInspector*`): General info, PDF metadata (Title/Author/Subject/
  Keywords — editable), EXIF, GPS location, annotations list, encryption/permissions.
* **Preferences** (`PVPreferences`): window background, open groups of files in one
  window vs own window, define 100% scale for images, default PDF view, start on last
  viewed page, logical page numbers, add name to annotations.
* **Toolbar** (`CommonToolbar.proplist` default items): view menu, zoom, markup, remove
  background, rotate, form filling, edit banner, inspector, share, search — Review's
  toolbar follows this order (Share becomes Export).
* **Undo names** (`UndoStrings.loctable`): Review uses the same action names
  ("Rotate Left", "Crop", "Insert Blank Page", "Remove Background", …) in Edit ▸ Undo.
