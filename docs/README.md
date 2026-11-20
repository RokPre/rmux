# rmux

rmux keeps named terminal sessions running independently of the terminal windows attached to them. Each session has one shared split layout and one or more independently running buffers.

## Contents

- [Terms](#terms)
- [Install and start](#install-and-start)
- [Keyboard controls](#keyboard-controls)
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
| `Ctrl-R` | Rename the focused buffer. Enter saves, Escape cancels, and Ctrl-U clears the name back to its numeric ID. |
| `Ctrl-Shift-H` / `Ctrl-Shift-L` | Move the focused buffer left / right in the buffer line. |
| `Alt-V` / `Alt-B` | Split the focused rmux window vertically (side by side) / horizontally (top and bottom). The new window shows the same buffer. |
| `Alt-W` | Close the focused rmux window. The final rmux window cannot be closed. |
| `Alt-H` / `Alt-J` / `Alt-K` / `Alt-L` | Focus the rmux window left / down / up / right. |
| `Alt-Shift-H/J/K/L` | Move the focused rmux window left / down / up / right. |
| `Alt-Ctrl-H/J/K/L` | Move the nearest matching split boundary left / down / up / right by 5 percentage points. |
| `Alt+=` | Rebalance split ratios for approximately equal pane areas; terminal-cell rounding can leave small differences. |

Rmux consumes its shortcuts before forwarding other input to the application. The ordinary terminal encodings of Ctrl-T, Ctrl-W, Ctrl-H, Ctrl-L, and Ctrl-R are reserved for rmux, so their usual application functions are unavailable. In particular, Ctrl-H is often identical to Backspace, and Ctrl-L normally clears a terminal. Alt shortcuts use Escape-prefixed characters; Escape followed quickly by a bound character is indistinguishable from Alt. Ctrl-Shift-H/L use the sequences provided by the existing st keyboard-input patch. Build st without the rmux-specific `st-rmux-keybindings.diff`: that patch changes key encodings and is incompatible with the ordinary encodings rmux now expects.

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
