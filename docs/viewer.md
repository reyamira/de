# File viewer and previews

Return to the [README](../README.md) for installation and navigation controls.

The right pane and inline viewer support these file types:

- **Markdown:** `.md`, `.markdown`, `.mdown`, and `.markdn` share Markdown behavior.
  The right pane shows highlighted literal source. The full-width viewer uses
  native FMD rendering, including tables and supported local images.
  Press `m` in the viewer to show the same highlighted source as the right pane.
  Fenced code uses loaded grammar names, file extensions, and common aliases
  such as `shell`, `js`, and `c++`. Exact extension case takes priority when
  spellings differ only by case. Unknown or unlabeled fences stay plain.
- **JSON:** Formatted, colorized text that preserves key order and number text.
  Invalid JSON remains readable with an error notice.
- **Source and plain text:** UTF-8 text with terminal control characters
  replaced. Syntect highlights known syntaxes. These include TOML (`.toml` and
  `Cargo.lock`), JSONC (`.jsonc` and TypeScript and JavaScript config JSON),
  JSON5, JavaScript (`.js`, `.mjs`, `.cjs`), JSX, Dockerfiles, Vue, Svelte,
  GraphQL, Prisma, Terraform/HCL, TypeScript (`.ts`, `.mts`, `.cts`), and TSX.
  Ordinary `.json` files retain strict JSON validation and formatting.
  The viewer selects a grammar by filename, extension, or first line.
  Unknown types stay plain.
- **Images:** PNG, JPEG, GIF, WebP, and BMP when the terminal confirms PNG graphics support.
  Animated images show their first frame.
- **SVG:** Static vectors and text use the existing SVG-to-PNG renderer.
  Press `m` in the viewer to switch between the image and highlighted XML source.
  Local SVG images in Markdown use the same renderer.

Run `de languages` to list the grammars and file associations loaded by this
installation. The built-in Syntect grammars come from the
[Sublime Text syntax catalog](https://github.com/sublimehq/Packages).
Additional grammar sources, pinned revisions, local changes, and licenses are
recorded in [`assets/syntaxes`](https://github.com/reyamira/de/tree/main/assets/syntaxes).
TypeScript and TSX use grammars converted by
[bat](https://github.com/sharkdp/bat/tree/73dc3258bec83bd7c66334f05b13c2d8221859f9/assets/syntaxes/02_Extra).

The browser header keeps the current directory and adds details for the selected item when space permits.
File details include the detected language, byte size, and original source line count when available.
Line counts use the loaded source, not wrapped rows or formatted JSON.
CRLF, LF, and CR each end a logical line.
An empty file has one logical line, and a final newline adds an empty final line.
Folder counts describe immediate files and folders under the current hidden-file setting, not recursive totals.
Details use the loaded snapshot and require no file reads during drawing.
The app does not scan other files for line counts or calculate recursive folder sizes.
Search keeps its query-only header.
Viewer headers also show the first visible source line and the total source lines when source layout applies.
In reading layout, this position identifies the first source line of the visible paragraph.
Formatted JSON and rendered Markdown show total source lines and a scroll percentage when scrolling is possible.

The full-width viewer has a thin border around its contents. Text and images
use the space inside that border, including after a terminal resize. A scrollbar
uses the rightmost content column when there is enough space.

The viewer normally uses at most 30 terminal rows and leaves one terminal row free.
Press `f` to use every terminal row without an alternate screen.
Press `f` again to restore the normal inline size.
The header shows `inline` or `full`; the footer shows `f size`.
Expansion keeps the reading position unless the larger viewport requires a lower scroll limit.
Resizing keeps the selected size mode.
Text, rendered Markdown, images, and mouse controls use the new content area.
Expansion can move earlier shell output into scrollback.
Closing the viewer resets expansion for the next file.
Viewer-origin editing keeps the selected size; `f` inserts text while editing.
During the find prompt, `f` remains query text.

In the viewer, use arrows or `j`/`k` to scroll. Use `PageUp`/`PageDown` for pages
and `Home`/`End` for the beginning or end. For Markdown or SVG, press `m` to switch
between the rendered view and source. The header shows the current mode.
Press `Escape` or `q` to return to the same selection or search result.
Press `e` to edit the original source, including from rendered Markdown or SVG.
Press `o` to open the file externally.
During search input, letters such as `v`, `e`, and `m` remain query text.

The full-width plain text viewer starts in raw layout, which preserves hard
newlines. Press uppercase `R` to switch to reading layout and back. Reading
layout reflows paragraph lines without changing the file. Code and Markdown
source keep their newlines.

Source previews, raw source viewers, and the editor show original line numbers.
Only the first wrapped row has a number.
Very narrow text areas omit the gutter to keep text readable.
Notice rows, reading layout, formatted JSON, images, and rendered Markdown have no source numbers.
Content-search results open at the exact first matching byte, including wrapped continuation rows.
Visible Markdown matches open in rendered mode.
Hidden Markdown syntax opens in source mode.
JSON content results use original source layout.

Use the mouse wheel over the viewer or editor to scroll three rows per notch.
Click the scrollbar track to jump, or drag the thumb to scroll.
Editor scrolling keeps the caret, selection, text, and undo history unchanged.
Keyboard movement or editing brings the caret back into view.
Mouse capture runs only while the viewer or editor is open.
Mouse controls require a terminal that answers cursor-position queries.
If the terminal does not answer promptly, mouse capture stays off for that session.
Keyboard controls remain available.
After the terminal width shrinks, reflow can leave an old frame fragment above the viewer.

Drag source or rendered text to select it.
Press `y` to send selected text through OSC52 to the terminal clipboard.
With a selection, `Ctrl-C` does the same action.
Without a selection, `Ctrl-C` retains normal cancellation.
The terminal must support and permit OSC52.
The status reports transmission, not clipboard acceptance.
Copying has a 100,000-byte limit and never writes to directory-selection stdout.
Source copying preserves original UTF-8 bytes, tabs, and line endings.
Gutters and soft wraps are not part of copied text.
Rendered copying uses visible plain text, without Markdown delimiters or hidden link destinations.

In the viewer, press `/` to start in-file find.
Type literal, case-insensitive text to highlight matches and show the current match count.
Press `Enter` to accept the find.
Use `n` and `N` for the next and previous matches.
Matches wrap at the end or beginning.
During the find prompt, `Escape` cancels find and restores the previous mode and scroll.
Outside the prompt, `Escape` returns to the picker or existing browser results.
Find uses original source layout for formatted JSON.
It does not index formatted rows with raw source offsets.
Closing the viewer restores the formatted JSON snapshot, including after an accepted find.
It does not read a changed file again for this restoration.
In-file find does not change the browser query, results, or selected result.

Images and native FMD Markdown use direct PNG transfer through the
[Kitty graphics protocol](https://sw.kovidgoyal.net/kitty/graphics-protocol/).
At startup, `de` sends a small, non-displaying PNG query and waits at most 250 ms for confirmation.
It uses the reply, not terminal names or environment variables, to enable graphics for the session.
The probe confirms protocol support, not the absence of visual bugs in a terminal implementation.

The probe tests the current input/output path, including any terminal multiplexer.
A multiplexer that filters graphics will fail the probe, even when its outer terminal supports graphics.
`de` does not send multiplexer passthrough sequences.
Sixel and the iTerm2 inline-image protocol are not implemented.
If the probe fails or receives no reply, images show an explanation and retain the external-open option.
Markdown source remains available with `m`.
Video playback remains external.

The native FMD Markdown view uses the same measured graphics support.
It does not require a browser.
Native text supports selection, copying, and find through the renderer's text geometry.
Mouse events identify terminal cells.
Rendered hit testing uses each cell's center, not exact pointer pixels within that cell.
Images, diagrams, and typeset vector math remain graphics, not selectable text.
Links are not clickable.
Source mode does not require terminal graphics support.

Themes can set separate colors for source syntax, Markdown source, and rendered
Markdown. Rendered Markdown also supports separate body, heading, and code fonts,
sizes, and spacing. Source text always uses the terminal font. See
[theme configuration](configuration.md#themes) for settings and limits.

Markdown images must be local PNG, JPEG, GIF, WebP, BMP, or SVG files inside the
document folder or its subfolders. Remote assets are not loaded. Raw HTML is
inert. Animated GIFs show their first frame. Images are resized to at most 2,048
pixels per side. Raster files are limited to 16 MiB and 16,777,216 pixels.
SVG has the separate limits below.
Each document is limited to 128 image occurrences, 32 MiB of image source data,
and 33,554,432 decoded or rendered pixels in total.

SVG input must be UTF-8 and at most 1 MiB.
Parsing permits at most 4,096 XML nodes and 64 levels.
Element IDs must be unique.
Expanded symbol references permit at most 16,384 nodes and 4 MiB of text and attributes.
Stylesheets have a 16 KiB limit.
SVG canvases retain the image dimension and pixel limits below.
Transparent SVG pixels use the terminal background, or the page background when embedded in Markdown.

SVG text uses bundled fonts.
External files, network resources, embedded images, and external fonts are not loaded.
Scripts, animation, and HTML `foreignObject` content are not supported.
Supported static filters include blur, drop shadow, offset, flood, blend, composite, merge, color matrix, and component transfer.
Filter count, projected regions, and estimated raster work have additional limits.
Rejected SVGs show an explanation.
XML source remains available when the file meets the text preview limits.

Changing selection discards old results but cannot interrupt an individual synchronous SVG parse or raster call.

FMD supports some Mermaid diagrams. Unsupported diagrams remain as source.
Rendering runs in the background. The viewer keeps the last viewport while it
prepares a new one. New requests replace queued work.
Supported Mermaid figures fill their local content width and preserve their aspect ratio.
Their layout reserves the scaled height, including after a resize.
Ordinary images keep their intrinsic sizing.
The renderer reflows narrow pages without scaling the body text.

Text previews accept regular UTF-8 files up to 1 MiB. Styled text and source
previews show at most 20,000 lines. Raster image previews accept files up to 16 MiB
and 16,777,216 pixels, with each dimension at most 16,384 pixels.
Images use EXIF orientation and scale to at
most 2,048 pixels per side before terminal transfer. Press `r` in the picker to
reload a changed file.

Image preparation runs in a background worker. A loading message appears while
the image is prepared, and navigation remains available. New selections replace
queued work, and results from old selections do not replace the current preview.
The viewer sends compressed image data in bounded batches between input checks.

## Built-in file editor

Press `e` on a selected file or inside its viewer.
At normal widths, picker-origin editing stays in the right preview pane and keeps the directory list visible.
Viewer-origin editing keeps the full-width viewer frame in rendered and source modes.
The editor keeps the same content width and leaves the scrollbar column clear.
Wrapping, cursor movement, and page movement use the area inside the frame.
Small areas omit the frame when it cannot fit.
Below 58 columns, the preview editor uses full width.
It loads the original file, not highlighted, formatted, or truncated preview rows.
Leaving the editor returns to the same picker or viewer.

The editor header keeps the unsaved marker and shows live buffer size, line position, and column when space permits.
Buffer size includes the byte-order mark and the newline style that saving preserves.
Narrow headers remove details before the filename or unsaved marker.

The editor highlights unsaved source with the same grammars and theme colors as source previews.
This includes Markdown fences and filename, extension, and first-line grammar selection.
Unknown types use the theme's plain source color.
JSON stays in its original layout while you edit it.

Highlighting updates after typing, paste, undo, and redo, including changes to multiline comments and strings.
The cache keeps unchanged line states and updates later lines until their syntax state matches again.
Cursor movement reuses highlighting; resizing only changes wrapping.
Selections override syntax colors without changing source bytes, tabs, or grapheme boundaries.

| Key | Action |
| --- | --- |
| Arrows, `Home`, `End`, `PageUp`, `PageDown` | Move the cursor |
| `Ctrl-Left`, `Ctrl-Right` | Move by word |
| `Ctrl-Home`, `Ctrl-End` | Move to the buffer start or end |
| `Shift` with movement keys | Extend the selection |
| `Ctrl-A` | Select all |
| `Backspace`, `Delete` | Delete the selection or adjacent text |
| `Enter`, `Tab` | Insert a newline or tab |
| `Ctrl-C`, `Ctrl-X`, `Ctrl-V` | Copy, cut, or paste with the editor's internal clipboard |
| `Ctrl-Z`, `Ctrl-Y`, `Ctrl-Shift-Z` | Undo or redo |
| `Ctrl-S` | Save |
| `Escape`, `Ctrl-Q` | Return, with a changed-buffer prompt when needed |

Ordinary letters, including `q`, insert text.
`Ctrl-C` does not cancel the application while editing.
Terminal bracketed paste inserts text as one edit.
The editor does not access the system clipboard.
For terminal clipboard paste, use your terminal's paste command.

If the buffer changed, choose `s` to save, `d` to discard, or `c`/`Escape` to keep editing.
Save errors and external-change conflicts keep the changed buffer open.
The editor does not force an overwrite or silently reload a changed file.

### File safety policy

The editor accepts existing, writable, regular UTF-8 files up to 1 MiB and 20,000 logical lines.
It rejects symbolic links, multiply linked files, binary text, and unsupported control characters.
It rejects source files with mixed newline styles or lone carriage returns.
It preserves UTF-8 content, the byte-order mark, LF or CRLF style, the final-newline state, and permissions.
Inserted newlines and pasted CR, CRLF, or LF line breaks use the file's newline style.
Edits can intentionally add or remove the final newline.

Saving checks the original file identity and contents before replacement.
It writes a temporary file in the same folder, then replaces the original atomically.
This requires write access to the containing folder.
The editor does not provide autosave, backup files, multiple buffers, or a force-save command.
Atomic replacement does not preserve ownership, inode identity, extended attributes, ACLs, or hard-link relationships.
The final external-change check cannot lock out another process that writes during replacement.
File data is synced before replacement; directory-entry crash durability is not guaranteed.

## Search limits

Search runs in one background worker below the folder where search started.
New queries cancel old work, and stale results cannot replace current results.
Names and text use literal case-insensitive matching.
The browser offers `Both`, `Files & folders`, and `Content` scopes.
`Both` is the initial scope.
During search input, `Shift+Tab` cycles scopes without clearing the query.
The app remembers the scope for the current session, not in configuration.
The header shows only `/ query▏`, with the query tail visible on narrow terminals.
The footer shows only the active mode and its `Shift+Tab` control.
`Up`, `Down`, `PageUp`, `PageDown`, `Home`, and `End` select only filtered results.
`Left`, `Right`, and plain `Tab` do not browse folders during search.
Press `Escape` before using folder-browsing keys.
`Escape` restores the folder and selection from before search.
`Enter` still chooses the selected folder or opens the selected file at its first match.
From a viewer, `Escape` first returns to search.
Name matching still uses the full relative path, including parent directory names.
Content matching uses original source text.
Each matching original line produces one content result.
Results highlight matching excerpt text and retain exact original-byte activation targets.
Each excerpt contains up to 240 source characters around the first match.
Each file has one header above its matching lines.
Headers show line counts when space permits.
Unselected directory names, source numbers, and line counts use muted colors.
Unselected filenames and excerpts use the normal foreground color.
Only matching name paths and source excerpts receive query highlights.
Content-only file headers do not receive path highlights.
The selected result uses the theme's reverse or filled emphasis across the full row, including padding.
Its directories, source numbers, counts, and matching text keep the same selection colors.
Query matches use underlines within the selected row.
Unselected matches use the theme accent and underlines.
Long paths retain the basename when space permits.
Excerpt windows keep the first match visible without splitting Unicode graphemes.
Selecting a content-only file header opens its first matching source line.
Source and rendered previews highlight all matches in the loaded document.
The search skips file contents above 1 MiB or 20,000 logical lines, including an empty final line.
It skips binary, non-UTF-8, and symlink contents without hiding matching names.
It respects ignore rules and hides dotfiles unless the current hidden setting enables them.
It does not traverse directory symlinks.
Results are ordered by path, then header, then source line.
The result limit is 10,000 name and line matches, excluding content-only file headers.
The footer prioritizes the active mode and its control, and retains the result-limit warning.
It adds name, line, and content-file counts, progress, scanned paths, and `Esc` when space permits.
On narrow terminals, `N`, `L`, `F`, and `S` mean names, lines, content files, and scanned paths.
`Names` abbreviates the `Files & folders` mode when necessary.
`⇧Tab` abbreviates `Shift+Tab` on the narrowest usable terminals.
If necessary, `B`, `N`, and `C` abbreviate `Both`, `Files & folders`, and `Content` to retain the limit warning.
