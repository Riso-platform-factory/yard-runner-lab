# ui_gallery

A test extension for the UI kit, the base components in `yard_ui`. When `app/yard/src/dev/review.rs` lists it as `("ui_gallery", ui_gallery::init)` among the extensions it passes to the startup, and `app/yard/Cargo.toml` depends on it as an optional dependency turned on by `dev`, `cargo run dev` loads it into the running app. Every feature it covers is in production, so it is not loaded now; add both back to check the UI kit again, or when the next UI kit feature is under review.

Without `UI_GALLERY` it does nothing, so a plain `cargo run dev` shows only what the app really does. With `UI_GALLERY=all`, it places a tabs container as the root, with one tab per page and one page per base component. A page shows every variant of its component, each naming itself, in groups with an upper-case muted title beside a hairline, as Zed's component preview does.

Use a fresh home, so that the user's real files are never touched: `YARD_HOME=/tmp/yard-gallery`.

## Feature 1: gallery-and-text

| Command | What to look for |
|---|---|
| `cargo run dev` | Nothing from the gallery. |
| `UI_GALLERY=all cargo run dev` | A tab `text`. Its page shows a headline per size, a label per size, a label per color, a row of three labels, and a column of three. `gallery-build{pages}`, with a `page placed` line per page. |
| The same, with `{"theme": {"mode": "light"}}` in `config/settings.json` | The same page in One Light: every color follows the theme. |
| The same, with `{"theme": {"ui_font_size": 20}}` | Every text larger, in the same proportions. |
| `UI_GALLERY=other cargo run dev` | A `WARN` that the case is unknown, and an empty window. |

## Feature 2: icon

| Command | What to look for |
|---|---|
| `UI_GALLERY=all cargo run dev`, then the tab `icon` | Under `SIZE`, a row per size with every icon; under `COLOR`, `Check` in each text color; under `NAME`, every icon beside its name. `gallery-build{pages=2}`. |
| The same, in One Light | The icons follow the theme's colors. |

## Feature 3: button

| Command and action | What to look for |
|---|---|
| `UI_GALLERY=all cargo run dev`, the tab `button` | Under `STYLE`, `SIZE`, `ICON`, and `STATE`, every style, size, icon, and state, as in Zed. `gallery-build{pages=3}`. |
| Hover and press a button | Its hover and pressed colors. |
| Click `clicked 0 times` twice | `clicked 2 times`. Two `button-click{id=count-clicks}` groups, each with `button clicked {count}` inside it. |
| Click the check under `CLICK` | It turns accent, with `button-click{id=toggle-clicks}`, then back on the next click. |
| Click the disabled buttons | Nothing, and no `button-click`. |
| With `{"theme": {"ui_density": "compact"}}` | The buttons get smaller. |

## Feature 4: divider

| Command and action | What to look for |
|---|---|
| `UI_GALLERY=all cargo run dev`, the tab `divider` | Under `HORIZONTAL`, a solid, a dashed, and an inset divider in each color; under `VERTICAL`, labels separated by solid and dashed dividers. `gallery-build{pages=4}`. |
| The same, in One Light | The lines follow the theme. |

## Feature 5: modal

Each modal is drawn in place on the page, not above the window.

| Command and action | What to look for |
|---|---|
| `UI_GALLERY=all cargo run dev`, the tab `modal` | Under `HEADER`, a modal with a headline, one with a description as well, and one with a close button as well; under `SECTIONS`, a section without a header and two with headers; under `FOOTER`, `Cancel` and a filled `Confirm` at the end, and the same with `Additional information` at the start; under `DISMISS`, a full modal saying `dismissed 0 times`. Each filled `Confirm` stands out from the modal's surface. `gallery-build{pages=5}`. |
| Click the `DISMISS` modal's close button, then its `Cancel` | `dismissed 2 times`. Two `button-click` groups, `id=dismiss-counter-dismiss` then `id=dismiss-cancel`, each with `modal dismissed {count}` inside it. |
| The same, with `{"theme": {"mode": "light", "ui_density": "compact"}}` | The surfaces follow One Light, and the spacing gets tighter. |
| The tab `button` | Every button as before. |

## Removing it

Remove it from the list of extensions in `app/yard/src/dev/review.rs` and from the dependencies and the `dev` feature in `app/yard/Cargo.toml`, and the app runs as it should without it.

## Feature 6: list

The `list` page shows, top to bottom: a list with a header `Themes` and five items, the second selected and the fourth disabled with its label in the disabled color, each with an icon in its start slot and the last with a key-like label in its end slot; a separator; a list of sparse, inset items, as Zed's pickers draw them; items at indent levels 0, 1, and 2; and an empty list with its message. Clicking an enabled item selects it.

| Command and action | What to look for |
|---|---|
| `UI_GALLERY=all cargo run dev`, open the `list` tab | The page as described. |
| Hover the items | Zed's hover color on enabled items; none on the disabled one. |
| Click an item | It becomes the selected one. `list-item-click{id=…}` in the log. Clicking the disabled item does nothing. |
| `{"theme": {"mode": "light"}}` | The page follows the theme. |

## Feature 7: text-input

The `text-input` page shows two inputs in frames drawn as Zed's picker draws its editor: the first empty with the placeholder `Search…`, the second with the text `hello world`. Under each, a muted label shows the last event: `changed: <text>` or `confirmed: <text>`.

| Command and action | What to look for |
|---|---|
| `UI_GALLERY=all cargo run dev`, open the `text-input` tab, click the first input | The cursor blinks at its start; the placeholder stays until the first character. |
| Type `xin chào` with the macOS Vietnamese input method (Telex), then press enter | The composed letters appear underlined, then final: `xin chào`. The label: `confirmed: xin chào`. |
| In the second input, `alt-left`, `shift-cmd-left`, `cmd-c`, click the first input, `cmd-a`, `cmd-v` | The first input reads `hello `, as copied. |
| Type until the text is wider than the input | The text scrolls; the cursor stays visible. |
| `cmd-z` several times | The edits come back word by word. |
| `{"theme": {"mode": "light"}}` | The text, the placeholder, the selection, and the cursor follow the theme. |

## Feature 8: scroll

The `scroll` page shows, on the left, a scroll area 300 px tall with forty paragraphs of text, and on the right a list 300 px tall of a thousand items, `Row 0` to `Row 999`, that draws only the rows in view, with a button `go to 500` above it.

| Command and action | What to look for |
|---|---|
| `UI_GALLERY=all cargo run dev`, open the `scroll` tab, scroll each side | Both scroll smoothly. The thumb appears while scrolling and fades after a moment. |
| Scroll the list fast to the end | `Row 999` at the bottom. With `YARD_LIFECYCLE=info,yard_ui=trace`, `rows drawn` in the trace with small counts. |
| Click `go to 500` | `Row 500` is in view. |
| Drag the list's thumb | The rows follow. |
| `{"theme": {"mode": "light"}}` | The thumb follows the theme. |

## Feature 9: key-hint

The `key-hint` page lists every action of the shell with a default key in the window's context (all but `yard::Dismiss`, whose key works only inside a modal): the action's name, then its keys at the right, as Zed's command palette shows a command. A second group shows the same keys in the label style, and one disabled.

| Command and action | What to look for |
|---|---|
| `UI_GALLERY=all cargo run dev`, open the `key-hint` tab | `yard::ToggleZoom` with `⇧⎋`, `yard::SelectNextTab` with `⌥⌘→` (the last binding, as Zed shows it), `yard::FocusLeft` with `⌘K ⌘←`, `yard::Quit` with `⌘Q`, and the others. |
| Beside Zed's command palette (`cmd-shift-p`) | The keys match in size, spacing, and colors. |
| `{"theme": {"mode": "light"}}` | The keys follow the theme. |

## Feature 10: tooltip

The `tooltip` page has buttons with every kind of tooltip: a title, a title with the keys of an action, a title with a detail line, an icon button, a disabled button, and a title long enough to wrap.

| Command and action | What to look for |
|---|---|
| `UI_GALLERY=all cargo run dev`, open the `tooltip` tab, rest the pointer on `Text` | After half a second, `Save the file` in a small raised box below the pointer. `tooltip-show{title=Save the file}` with `tooltip built`. |
| Rest on `Keys` | `Quit the gallery` with `⌘ Q` at its right, and `keys=Command-Q` in the log. |
| Rest on `Detail` | The title, and below it a muted line; `meta=true` in the log. |
| Rest on the icon button, and on `Disabled` | Each shows its tooltip. |
| Rest on `Long` | The title wrapped to two lines. |
| Move away, or press the button | The tooltip goes. |
| `{"theme": {"mode": "light"}}` | The box follows the theme. |

## Feature 11: window-controls

The `window-controls` page shows the window buttons that Linux draws:
- **Under `LAYOUT`:** the default layout (minimize, maximize, close) and the left layout (`close,minimize,maximize:`).
- **Under `STATE`:** maximized, where restore replaces maximize, and each button drawn hovered.

Clicking a button acts on the gallery's own window, so close closes it. On macOS the page shows the same buttons, though a macOS window never draws them.

| Command and action | What to look for |
|---|---|
| `UI_GALLERY=all cargo run dev`, open the `window-controls` tab | The page as described. `gallery-build{pages=11}`. |
| `UI_GALLERY=window-controls cargo run dev` | The page alone. `gallery-build{pages=1}`. |
| Click minimize | `window-control{action=minimize}` with `minimizing`. |
| `{"theme": {"mode": "light"}}` | The icons and their hover follow the theme. |
