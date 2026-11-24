# rmux

rmux keeps named terminal sessions running independently of the terminal windows attached to them. Each session has one shared split layout and one or more independently running buffers.

## Contents

- [Terms](#terms)
- [Install and start](#install-and-start)
- [Keyboard controls](#keyboard-controls)
- [Vim navigation and selection](#vim-navigation-and-selection)
- [Lifetimes](#lifetimes)
- [Shared views and sizing](#shared-views-and-sizing)

## Terms

| Term | Meaning |
| --- | --- |
| **Session** | A named rmux instance containing buffers and a shared layout. |
| **Buffer** | One running terminal process and its PTY. A new buffer starts an interactive shell by default. A buffer can be hidden or displayed in more than one rmux window. |
| **rmux window** | One pane in the session's split layout. Each rmux window displays exactly one buffer. |
| **Terminal window** | The outer application window, such as an `st` window. It runs an rmux client attached to a session; it is not an rmux window. |

A session's layout and focused rmux window are shared by every attached terminal window.

## Install and start

From the rmux source directory, install the Rust client into the location used by the i3 launchers:

```sh
make install PREFIX="$HOME/.local"
```

This builds the release binary and installs it as `~/.local/bin/rmux`. It replaces an older `rmux` command at that path, if present.

The basic commands are:

```sh
rmux new NAME [--cwd DIRECTORY] [-- COMMAND [ARG...]]
rmux up NAME [--cwd DIRECTORY] [-- COMMAND [ARG...]]
rmux attach NAME
rmux ls
rmux kill NAME
rmux --version
rmux -v
```

`new` creates a session and attaches; it fails if the name already exists. `up` creates the session if needed, otherwise attaches to it. `attach` requires an existing session. Without a command, a new session starts the user's interactive shell. When creating a session, `--cwd` selects its working directory; `-- COMMAND ...` runs a specific program instead.

`ls` lists live sessions. `kill` forcibly terminates a session and its child processes, so unsaved work can be lost; normally exit the buffers you want to close instead.

## Keyboard controls

The buffer line is drawn across the top of each attached terminal. Labels appear in buffer order; an unrenamed buffer uses its numeric ID, and the focused buffer is shown in brackets, for example `shell  [editor]  3`. Renamed labels are shared across attachments and rmux windows.

| Keys | Action |
| --- | --- |
| `Ctrl-T` | Create an interactive-shell buffer and show it in the focused rmux window. |
| `Ctrl-W` | Close the focused buffer. Every rmux window showing it switches to a remaining buffer. |
| `Ctrl-H` / `Ctrl-L` | Show the previous / next buffer in the focused rmux window. |
| `Ctrl-R` | Rename the focused buffer. Enter saves, Escape cancels, Ctrl-U clears the name, Ctrl-Backspace deletes the previous word, and Ctrl-Left/Right move between words. An empty name restores the numeric ID. |
| `Ctrl-Shift-H` / `Ctrl-Shift-L` | Move the focused buffer left / right in the buffer line. |
| `Alt-V` / `Alt-B` | Split the focused rmux window vertically (side by side) / horizontally (top and bottom). The new window shows the same buffer. |
| `Alt-W` | Close the focused rmux window. The final rmux window cannot be closed. |
| `Alt-H` / `Alt-J` / `Alt-K` / `Alt-L` | Focus the rmux window left / down / up / right. |
| `Alt-Shift-H/J/K/L` | Move the focused rmux window left / down / up / right. |
| `Alt-Ctrl-H/J/K/L` | Move the nearest matching split boundary left / down / up / right by 5 percentage points. |
| `Alt+=` | Rebalance split ratios for approximately equal pane areas; terminal-cell rounding can leave small differences. |

Rmux consumes its shortcuts before forwarding other input to the application. The ordinary terminal encodings of Ctrl-T, Ctrl-W, Ctrl-H, Ctrl-L, and Ctrl-R are reserved for rmux, so their usual application functions are unavailable. In particular, Ctrl-H is often identical to Backspace, and Ctrl-L normally clears a terminal. Alt shortcuts use Escape-prefixed characters; Escape followed quickly by a bound character is indistinguishable from Alt. Ctrl-Shift-H/L use the sequences provided by the existing st keyboard-input patch. Build st without the rmux-specific `st-rmux-keybindings.diff`: that patch changes key encodings and is incompatible with the ordinary encodings rmux now expects.

## Vim navigation and selection

Vim mode is enabled by default for every buffer. Input starts in Insert mode: ordinary keys and bracketed paste go to the program running in the buffer. `Alt-Escape` enters Normal mode; `Escape` or `i` returns to Insert mode. `Alt-d` / `Alt-f` enter Normal mode and scroll up / down five rows. `Ctrl-f` opens forward search from Insert, Normal or Visual mode.

In Normal mode, `h`, `j`, `k`, `l` move the cursor; `w`, `b`, `e` and `W`, `B`, `E` move by small-word / whitespace-delimited-word motions. `0`, `^`, `$`, `gg`, and `G` move to line start, first nonblank, line end, oldest retained row, and newest row. Counts are supported for motions, for example `5j`. `H`, `M`, `L` move to the top, middle, and bottom visible row; `K` / `J` scroll up / down. `Ctrl-d` / `Ctrl-u` scroll half a page, and `Ctrl-b` scrolls back a page. `/` searches forward and `?` searches backward. Enter accepts a search and Escape cancels it. Search updates while typing; `,` and `;` move forward and backward through matches.

`v`, `V`, and `Ctrl-v` start character-, line-, and block-wise Visual selection. `o` switches the active selection end, and `t` cycles the selection type. `y` copies a Visual selection, exits Visual mode, and returns to Insert mode. In Normal mode, `yy` copies lines; `yw`, `yb`, `ye` and `yi{object}` / `ya{object}` copy supported motions and text objects (words, parentheses, brackets, braces, angle brackets, and quotes). `Escape` clears a Visual selection and returns to Normal mode. Numeric counts work with supported motions and yanks. The selected text is sent to both the X11 clipboard and primary selection; `xclip` must be installed and available in `PATH`. On failure rmux reports the clipboard error in the terminal status line.

Navigation, search, mode, and selection state belong to a buffer and are shared by every rmux window and attached client showing that buffer. Other buffers keep independent state. The screen history available to navigation, search, selection, and yanks is limited to the most recent 2,000 rows per buffer; rmux layout dividers and padding are not buffer contents.

Vim actions can be bound in `[bindings]` like rmux window actions. The configuration is read by each client at startup. For example, to use `n` and `N` for search-result navigation:

```toml
[bindings]
vim_match_forward = "n"
vim_match_backward = "N"
```

All configurable Vim actions and their default keys are:

| Action | Default key |
| --- | --- |
| `vim_left`, `vim_down`, `vim_up`, `vim_right` | `h`, `j`, `k`, `l` |
| `vim_word_forward`, `vim_word_backward`, `vim_word_end` | `w`, `b`, `e` |
| `vim_word_forward_big`, `vim_word_backward_big`, `vim_word_end_big` | `W`, `B`, `E` |
| `vim_line_start`, `vim_first_nonblank`, `vim_line_end` | `0`, `^`, `$` |
| `vim_document_start`, `vim_document_end` | `gg`, `G` |
| `vim_view_top`, `vim_view_middle`, `vim_view_bottom` | `H`, `M`, `L` |
| `vim_view_cursor_top`, `vim_view_cursor_bottom` | `K`, `J` (scroll up / down) |
| `vim_scroll_half_down`, `vim_scroll_half_up` | `Ctrl-d`, `Ctrl-u` |
| `vim_scroll_page_down`, `vim_scroll_page_up` | Page Down, `Ctrl-b` |
| `vim_insert_search`, `vim_search`, `vim_search_backward` | `Ctrl-f` in Insert, `/`, `?` |
| `vim_next_match`, `vim_match_next` | `n` |
| `vim_previous_match`, `vim_match_previous` | `N` |
| `vim_match_forward`, `vim_match_backward` | `,`, `;` |
| `vim_visual`, `vim_visual_line`, `vim_visual_block` | `v`, `V`, `Ctrl-v` |
| `vim_yank`, `vim_insert`, `vim_normal` | `y`, `i`, `Escape` |
| `vim_scroll_insert_down`, `vim_scroll_insert_up` | `Alt-d`, `Alt-f` |

`vim_match_next` / `vim_match_previous` are alternate action names for `vim_next_match` / `vim_previous_match`. Vim action bindings are context-sensitive. A configured binding replaces the corresponding default in its applicable modes; other actions retain their defaults.

For a Neovim buffer to temporarily pass its own keys through to Neovim instead of treating Vim-mode keys as rmux commands, install the provided `contrib/rmux.nvim.lua` as `~/.config/nvim/lua/rmux.lua` (or another module on `runtimepath`) and load it from `init.lua`:

```lua
require("rmux")
```

The module sends buffer-scoped acquire/release requests on `VimEnter`, `VimSuspend`, `VimResume`, and `VimLeavePre`; nested Neovim processes are tracked separately, and dead owners are pruned if Neovim exits abnormally. While application control is active, rmux passes input through to that buffer's program and does not interpret Vim mode shortcuts there; rmux window-management shortcuts remain available. This protocol is specific to Neovim's UI and the provided hook; other applications do not disable Vim mode automatically. Application control does not configure terminal behavior dynamically.

For initial testing with the `st` terminal, start it with `st -V`. The rmux-specific `st-rmux-keybindings.diff` patch mentioned above is not required and must not be applied: it changes key encodings expected by rmux.


## Configuration

rmux reads `~/.config/rmux/config.toml`, or `$XDG_CONFIG_HOME/rmux/config.toml` when `XDG_CONFIG_HOME` is set. The file is read once when a client starts; there is no hot reload. Missing settings keep rmux's defaults; this example overrides selected settings. Invalid TOML, unknown keys/actions/colors, invalid values, and ambiguous key sequences stop startup with an error before a new session is created.

```toml
[bindings]
new_buffer = "ctrl+n"
focus_left = "alt+x"

[buffer_line]
bold = true # set to false for normal-weight buffer names
# rename_foreground = "#ff9e64" # defaults to focused_foreground
# rename_background = "#16161e" # defaults to background
focused_foreground = "white"
focused_background = "dark_grey"
shown_foreground = "white"
shown_background = "dark_grey"
hidden_foreground = "white"
hidden_background = "dark_grey"
focused_prefix = "["
focused_suffix = "]"
shown_prefix = ""
shown_suffix = ""
hidden_prefix = ""
hidden_suffix = ""
focused_prefix_foreground = "cyan" # optional; marker-specific overrides
shown_suffix_background = "#202030"
marker_color = false
background = "dark_grey" # also fills unused cells to the right

[divider]
foreground = "dark_grey"
vertical = "│"
horizontal = "─"

[margin]
foreground = "dark_grey"
background = "reset"
fill = "."

[resize]
step_percent = 5
```

Each buffer state has independent `*_prefix` and `*_suffix` strings. They default to the current brackets around the focused buffer and no markers on shown or hidden buffers. Marker strings may contain Unicode glyphs and are clipped by terminal cell width; control characters are rejected. For each state and side, optional `*_prefix_foreground`, `*_prefix_background`, `*_suffix_foreground`, and `*_suffix_background` values independently set marker colors; for example, `focused_prefix_foreground` or `shown_suffix_background`. Omit an individual color to use the existing fallback: with `marker_color = false` the marker uses that state's buffer foreground and background; with `marker_color = true` it uses the buffer-line background as foreground and that state's buffer background. `marker_color` defaults to `false`. Optional colors use the same accepted color names and `#rrggbb` syntax and invalid values stop startup.

For Neovim-style slanted separators in every state:

```toml
[buffer_line]
background = "#16161e"
focused_background = "#1a1b26"
shown_background = "#1a1b26"
hidden_background = "#16161e"
focused_prefix = ""
focused_suffix = ""
shown_prefix = ""
shown_suffix = ""
hidden_prefix = ""
hidden_suffix = ""
marker_color = true
```

Set `focused_background`, `shown_background`, and `hidden_background` to the corresponding buffer colors; `background` supplies the line color used as each separator's foreground.

Binding actions are `previous_buffer`, `next_buffer`, `rename_buffer`, `new_buffer`, `close_buffer`, `reorder_buffer_left`, `reorder_buffer_right`, `resize_left`, `resize_down`, `resize_up`, `resize_right`, `move_left`, `move_down`, `move_up`, `move_right`, `focus_left`, `focus_down`, `focus_up`, `focus_right`, `split_vertical`, `split_horizontal`, `close_window`, and `equalize`. Key values are one ASCII character or `ctrl+x`, `ctrl+shift+x`, `alt+x`, `alt+shift+x`, or `alt+ctrl+x` (for example `alt+=`); literal plus is `+` or `alt++`. Key sequences must be unique and may not prefix one another. Escape-prefixed Alt bindings retain the terminal's usual Escape-versus-Alt ambiguity.

Colors accept the standard names `reset`, `black`, `dark_grey`, `red`, `dark_red`, `green`, `dark_green`, `yellow`, `dark_yellow`, `blue`, `dark_blue`, `magenta`, `dark_magenta`, `cyan`, `dark_cyan`, `white`, and `grey`, or `#rrggbb`. Divider glyphs and the margin fill glyph must each occupy one printable terminal column. Margins retain their alternating glyph-and-space pattern, with configured foreground and background colors.

Buffer-line focused styling applies to the buffer in the focused rmux window; shown styling applies to buffers displayed in other visible windows; hidden styling applies to buffers not currently displayed. Buffer ordering and names remain shared session state. Bindings and colors belong to each attached client, so a new client reads its own config even when attaching to an existing session. The resize step is sent in the attach request and stored per client; it affects that client's resize shortcuts without changing other clients' settings. An older session server can still be attached to with the default 5% step; the client reports an actionable incompatibility if a different step is requested.

## Lifetimes

- Closing an **rmux window** only removes that view. Its buffer keeps running, even when no rmux window displays it.
- Closing a **buffer** with `Ctrl-W` stops that buffer's terminal process. If several windows display it, they all switch to a remaining buffer.
- Closing the last buffer ends the session. Exiting the last buffer's shell has the same effect.
- The last rmux window stays open; `Alt-W` cannot close it. This keeps the session available while buffers remain.
- Disconnecting or closing an outer terminal window detaches that client but leaves the session and buffers running. Reattach with `rmux attach NAME` or `rmux up NAME`.
- Sessions are local processes and do not survive a system reboot.

## Shared views and sizing

All attached terminal windows show the same rmux split layout, focus, and buffer changes. Splitting copies the focused buffer into the new rmux window; switching a buffer in one rmux window does not change the buffer shown in other rmux windows.

A buffer displayed in multiple windows is rendered at the smallest available rows and columns across all of those panes and attached terminal clients. In a larger pane, rmux centers that buffer screen and fills the unused area with a background pattern. Resizing a client, changing a split or buffer view, attaching, and detaching clients can change the buffer's dimensions and redraw its views.
