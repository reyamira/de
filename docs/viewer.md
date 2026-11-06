# File viewer and previews

Return to the [README](../README.md) for installation and navigation controls.

The right pane and inline viewer support these file types:

- **Markdown:** The right pane shows highlighted literal source. The full-width
  viewer uses native FMD rendering, including tables and supported local images.
  Press `m` in the viewer to show the same highlighted source as the right pane.
  Fenced code uses loaded grammar names, file extensions, and common aliases
  such as `shell`, `js`, and `c++`. Exact extension case takes priority when
  spellings differ only by case. Unknown or unlabeled fences stay plain.
- **JSON:** Formatted, colorized text that preserves key order and number text.
  Invalid JSON remains readable with an error notice.
- **Source and plain text:** UTF-8 text with terminal control characters
  replaced. Syntect highlights known syntaxes. These include TOML (`.toml` and
  `Cargo.lock`), JSONC (`.jsonc` and TypeScript and JavaScript config JSON),
  JSON5, JSX, Dockerfiles, Vue, Svelte, GraphQL, Prisma, Terraform/HCL,
  TypeScript (`.ts`, `.mts`, `.cts`), and TSX. Ordinary `.json` files retain
  strict JSON validation and formatting. The viewer selects a grammar by
  filename, extension, or first line. Unknown types stay plain.
- **Images:** PNG, JPEG, GIF, WebP, and BMP in Kitty or Ghostty.
  Animated images show their first frame.

Run `de languages` to list the grammars and file associations loaded by this
installation. The built-in Syntect grammars come from the
[Sublime Text syntax catalog](https://github.com/sublimehq/Packages).
Additional grammar sources, pinned revisions, local changes, and licenses are
recorded in [`assets/syntaxes`](https://github.com/reyamira/de/tree/main/assets/syntaxes).
TypeScript and TSX use grammars converted by
[bat](https://github.com/sharkdp/bat/tree/73dc3258bec83bd7c66334f05b13c2d8221859f9/assets/syntaxes/02_Extra).

The full-width viewer has a thin border around its contents. Text and images
use the space inside that border, including after a terminal resize. A scrollbar
uses the rightmost content column when there is enough space.

In the viewer, use arrows or `j`/`k` to scroll. Use `PageUp`/`PageDown` for pages
and `Home`/`End` for the beginning or end. For Markdown, press `m` to switch
between the rendered page and source. The header shows the current mode.
Press `Escape` or `q` to return to the same selection. Press `o` to open the
file externally. During filter input, `v` and `m` remain query text.

The full-width plain text viewer starts in raw layout, which preserves hard
newlines. Press uppercase `R` to switch to reading layout and back. Reading
layout reflows paragraph lines without changing the file. Code and Markdown
source keep their newlines.

Use the mouse wheel over the viewer to scroll three rows per notch. Click the
scrollbar track to jump, or drag the thumb to scroll. Mouse capture is active
only while the viewer is open. The viewer stays next to the prompt, subject to
normal terminal scrolling when it needs more rows. Mouse controls require a
terminal that answers cursor-position queries. If it does not answer promptly,
mouse capture stays off for that session; keyboard controls remain available.
After the terminal width shrinks, reflow can leave a fragment of the old frame
above the viewer.

Image previews require the Kitty graphics protocol. They are unavailable in
other terminals and inside tmux, GNU Screen, or Zellij. When `de` cannot show a
file, it explains the reason and shows the external-open option. Video playback
remains external.

The native FMD Markdown view requires Kitty or Ghostty without a terminal
multiplexer. It does not require a browser. The page is a raster image, so text
selection and clickable links are not available. Source mode does not require
terminal graphics support.

Markdown images must be local PNG, JPEG, GIF, WebP, or BMP files inside the
document folder or its subfolders. Remote assets are not loaded. Raw HTML is
inert. Animated GIFs show their first frame. Images are resized to at most 2,048
pixels per side. Each file is limited to 16 MiB and 16,777,216 pixels. Each
document is limited to 128 image occurrences, 32 MiB of image source data, and
33,554,432 decoded or rendered pixels in total.

FMD supports some Mermaid diagrams. Unsupported diagrams remain as source.
Rendering runs in the background. The viewer keeps the last viewport while it
prepares a new one. New requests replace queued work.

Text previews accept regular UTF-8 files up to 1 MiB. Styled text and source
previews show at most 20,000 lines. Image previews accept files up to 16 MiB
and 16,777,216 pixels, with each dimension at most 16,384 pixels.
Images use EXIF orientation and scale to at
most 2,048 pixels per side before terminal transfer. Press `r` in the picker to
reload a changed file.

Image preparation runs in a background worker. A loading message appears while
the image is prepared, and navigation remains available. New selections replace
queued work, and results from old selections do not replace the current preview.
The viewer sends compressed image data in bounded batches between input checks.

