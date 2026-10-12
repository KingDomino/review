# Photo printing, modelled on Photoshop's Print Settings

Review's Print dialog for images follows Photoshop's **Print Settings** dialog
(File ▸ Print). Its behaviour was studied with
[REA — Reverse Engineer Anything](https://github.com/morluto/rea) (`rea-agents` 6.4.0) and
by reading Photoshop's shipped layout and string resources, on **Adobe Photoshop 2025**
(macOS). No Adobe code was decompiled, copied or translated.

## Sources

| Source | What it gave us |
|---|---|
| `Contents/Required/layouts/Printing/Dialogs/photoshopPrintSettings-3340.exv` | The dialog's complete layout (plain-text Adobe layout script): every control, its id, label and tooltip |
| `rea inspect-macho` on `Contents/MacOS/Adobe Photoshop 2025` | Linked frameworks and imported print APIs (how Photoshop drives the printer) |
| `strings` on the same binary | C++ class names that show how the preview behaves, warnings and option descriptions |

## What the layout defines

* A resizable dialog with the **preview** (`TPrintPreview` inside `TPrintPreviewFrame`) on the
  left and the settings on the right.
* **Printer Setup**: Printer, Copies, Print Settings…, Layout (portrait / landscape icon buttons).
* **Position and Size** (`TPrintPositionCluster`, `TPrintSizeCluster`):
  * Position: **Center** checkbox, **Top** and **Left** fields.
  * Scaled Print Size: **Scale**, **Height**, **Width**, **Scale to Fit Media**,
    and a read-out "**Print Resolution: ^0 PPI**".
  * Print Selected Area, and a **Units** menu used by every measurement.
* **Printing Marks**: Corner Crop Marks, Center Crop Marks, Registration Marks, Description, Labels.
* Color Management, Functions (Emulsion Down, Negative, Background, Border, Bleed) and
  PostScript options.

## How the preview behaves

The binary's C++ RTTI names describe the interaction:

* `TPrintPreviewTracker::Post(TPrintPreview*, FloatPoint const&, TToolboxEvent*, bool, QuadHitResults)`:
  a drag in the preview is hit-tested against the image's quad (corner, edge or inside),
  then moves or scales the image.
* `TPreviewMarginTracker::Post(TPrintPreviewFrame*, EPreviewFramePart, …)`: the page's
  margins can be dragged too.
* Messages: "The image is larger than the paper's printable area; some clipping will occur.",
  "Images need to be at least 8 pixels in size to print correctly."
* Scripting keys: `printScale` (percent), `center`, `top`, `left`, `cornerCropMarks`, …

## How Photoshop prints

`rea inspect-macho` lists the print APIs Photoshop imports from ApplicationServices:
`PMSessionBeginCGDocumentNoDialog`, `PMSessionBeginPageNoDialog`, `PMSessionGetCGGraphicsContext`,
`PMSessionEndPageNoDialog`, plus `PMCreatePrintSettings`, `PMSessionValidatePrintSettings` and
`PMSessionCreatePrinterList`. So Photoshop runs its own dialog and then **draws each page
itself** into the printer's graphics context, with the image at the size and position chosen in
the preview; the system print panel is only used for the printer's own settings
(`NSPrintPanel`, `NSPrintInfo`).

## What Review implements

| Photoshop | Review |
|---|---|
| Preview drag (inside the image) | Moves the photo; turns **Center** off |
| Preview drag (corner) | Scales the photo proportionally around the opposite corner, or around the centre when **Center** is on; turns **Scale to Fit Media** off |
| Center / Top / Left | Same |
| Scale / Width / Height, linked | Same (typing one updates the others; the aspect ratio is locked) |
| Scale to Fit Media | Fits the printable area (0.25 in margins) |
| Print Resolution: N PPI | Same; shown in orange below 150 PPI |
| Units | Inches, cm, mm, points (remembered) |
| Layout (portrait / landscape) | Same; a large wide photo starts in landscape |
| Paper (from the printer) | Paper menu (Letter, Legal, Tabloid, A3–A5, 4×6, 5×7, 8×10, 10×15 cm, 13×18 cm); the default comes from the printer (CUPS `lpoptions` / Windows print configuration), then the region |
| Corner Crop Marks | Same |
| Clipping warning | Same |

Not implemented: colour management options (use View ▸ Soft Proof), centre crop and
registration marks, description and labels, emulsion down / negative, bleed and border,
PostScript options, dragging page margins.

## Printing at the exact size

Like Photoshop, Review renders the page itself rather than handing the photo to another app:

* **Linux**: the page is written as a PDF of the chosen paper size, with the photo at its exact
  position, and sent with `lp -o media=<PWG name> -o print-scaling=none` so CUPS prints it at 100%.
* **Windows**: printed straight through .NET's `PrintDocument` (`System.Drawing.Printing`), with the
  paper size and orientation set and the photo drawn at its exact position in hundredths of an
  inch, relative to the printer's hard margins. No PDF viewer is involved, so nothing rescales it.
* **Save as PDF…** writes the laid-out page.
