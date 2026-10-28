# GUI design rules

Rules for building `chatty-gui` screens and dialogs with `iced`. Shared styles,
icons, and widget helpers live in `crates/chatty-gui/src/ui/`; reuse them
instead of styling widgets inline. The Characters dialog
(`crates/chatty-gui/src/characters/view.rs`) is the reference implementation.

## Wiring

- Handle every message a view can emit. A button whose message falls into a
  catch-all arm does nothing and fails silently. When a view is rewritten, check
  that each message still has a handler, and add a test for non-trivial ones.
- Keep `view` functions pure: no I/O, no allocation-heavy work per frame, no
  hidden state. File dialogs and file reads belong in `update`.
- The app has one appearance: the dark glass theme. Do not add a light theme
  or theme switches. Pass `glass_mode` into views that draw surfaces and take
  colours from `ui` (`PALETTE`, `SECONDARY_TEXT`, `ACCENT_TEXT`, surface
  helpers) rather than hard-coding them.
- Disable an action with `on_press_maybe(None)` when it cannot run, and say why
  in its tooltip or nearby text. Do not hide controls that move the layout.

## Actions: icons first

An icon replaces a label when its meaning is clear from the icon and tooltip.

- Use `ui::action_icon(icon, tooltip, on_press, tone)` for actions.
  Every icon button needs a tooltip naming the action.
- `Tone::Primary` is the one main action of a view (Save, New). `Tone::Danger`
  marks deletion. Everything else is `Tone::Secondary`.
- Use `ui::ghost_icon_button` with `SidebarToggle::Collapse` to close dialogs.
- Draw icons as strokes in `ui::draw_message_icon` (or the footer and sidebar
  drawers) on a roughly ±9 unit grid centred on the origin. Reuse an existing
  icon for the same meaning: `Delete` is always the bin, `Sparkle` is always
  AI generation.
- Keep text buttons only where an icon would be ambiguous. Style them with
  `primary_button_style`, `secondary_button_style`, or `danger_button_style`.

## Layout

- Group related fields in `ui::card` sections. Start each card with a semibold
  14 px heading and a 12 px explanation in `ui::SECONDARY_TEXT`.
- Put a 12 px secondary-colour caption above each input rather than relying on
  placeholder text. Placeholders show an example value.
- Spacing: 4 px between a caption and its control, 10 px between fields,
  12 px between cards, 6 px between icon buttons.
- Dialog titles are 22 px bold; view titles inside a dialog are 18 px bold.
  Body text is 13–14 px; captions, help, and tooltips are 12 px.
- Adapt with `iced::widget::responsive` and width breakpoints: stack rows into
  columns when the available width is too small. Use `.wrap()` on action rows.
  Do not put a `Length::Fill` spacer inside a wrapping row.
- Wrap long content in one `scrollable`, and leave right padding so the
  scrollbar does not cover content.

## Inputs

- Single-line values use `text_input(...).style(ui::input_style)` with
  `.size(14)` and `.padding([8, 10])`.
- Text that can exceed one line (descriptions, prompts, dialogue) uses
  `text_editor`, never `text_input`. Keep its `text_editor::Content` beside the
  domain draft, reload it whenever the draft is replaced, and copy edits back
  only for `Action::Edit`. See `DraftEditors` in `characters/mod.rs`.
- `text_editor` has no scrollbar. To cap its height, place it in a
  `scrollable` with a stable `Id` inside a container with `max_height`, draw the
  input plate on the container, and snap the scrollable to keep the cursor
  visible after typing or keyboard movement.
- Use `ui::toggle_switch` or `toggler` for booleans, not checkboxes.

## Verification

Run the GUI checks from [AGENTS.md](../AGENTS.md), then inspect the change:

```sh
./chatty-gui-inspect.sh start 1100 720
./chatty-gui-inspect.sh screenshot /tmp/chatty-view.png
```

Inspection mode loads demo data. To open a dialog, write `characters`, `worlds`,
or `memories` to the control FIFO directly (`/tmp/chatty-gui-control` by
default); the script does not forward those commands. `zoom <factor>` fits more
of a tall view into one screenshot.
