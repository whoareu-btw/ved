# ved

A small vim-style terminal editor in Rust, with **built-in LSP** and an **embedded terminal**.

- **No dependencies.** `std` plus a handful of direct libc calls. One ~1 MB binary.
- **Light.** First frame in a few milliseconds, about 3 MB of memory, quits in ~20 ms.
- **Vim keys** you already know (see below), including counts, registers, macros, text objects and dot-repeat.
- **LSP on by default.** The right language server is started automatically when you open a file.
- **A project panel** on the left, like Zed's: file tree with git colours, a `≡` button in the top-left corner to hide it, buffer tabs, mouse support, and new file / new folder / delete from the keyboard.
- **Indent guides** like neovim's indent-blankline: a faint `│` for every indentation level, carried through blank lines, with the block around the cursor lit up.
- **A terminal inside the editor**: `Alt-q` opens it, `Alt-w` hides it, `Alt-e` closes it. A real VT100/xterm emulator with scrollback.
- **Dark by default.** ved paints its own dark colour scheme, whatever your terminal's theme is.

Unix only (Linux first, macOS should work). Needs a truecolor/256-colour terminal.

## Build and run

```sh
cargo build --release
cp target/release/ved ~/.local/bin/      # or anywhere on your PATH
ved src/main.rs
```

`:help` inside the editor shows the full cheat sheet.

### With Nix

The repository is a flake (`flake.nix`, with the package in `nix/package.nix`) and also has `default.nix` / `shell.nix` for
people who do not use flakes. Linux (x86_64, aarch64) and macOS on Apple silicon are listed.

```sh
nix run . -- main.c          # run it without installing
nix build                    # ./result/bin/ved
nix develop                  # shell with cargo, clippy, rustfmt, rust-analyzer and python3 (for `cargo test`)

nix-build                    # without flakes
nix-shell                    # without flakes: the same development shell
```

On NixOS or with home-manager, add it as an input and use the overlay (or `ved.packages.${system}.default` directly):

```nix
inputs.ved.url = "path:/path/to/ved";            # or the git URL where you keep it

# ... in your configuration:
nixpkgs.overlays = [ ved.overlays.default ];
environment.systemPackages = [ pkgs.ved ];       # or home.packages = [ pkgs.ved ];
```

The first `nix build` / `nix flake lock` pins nixpkgs in `flake.lock`. Inside a git checkout, flakes only see tracked
files, so `git add flake.nix flake.lock nix` first. The test suite is not run by the Nix build (it needs a
pseudo-terminal and starts language servers): run `cargo test` inside `nix develop`.

## What you get

| Area | Details |
| --- | --- |
| Modes | normal, insert, visual (`v`, `V`), command line, search |
| Motions | `` hjkl w W e E b B ge 0 ^ $ g_ f F t T ; , { } % gg G H M L n N * # ' ` `` all with counts |
| Operators | `d c y > < g~ gu gU` with motions and text objects (`iw aw ip ap i" a" i( a( i{ a{ i[ a[ i< a<`) |
| Editing | `x X s S C D Y p P J gJ ~ r . u Ctrl-r Ctrl-a Ctrl-x`, `3ifoo<Esc>`, autoindent |
| Indentation | `Tab` always inserts 2 spaces (`>>`, `<<`, autoindent and `:format` use 2 too); Makefiles get real tabs. `set shiftwidth=4 tabstop=4` for 4 |
| Autopairs | `` ( [ { " ' ` `` close themselves; typing the closer steps over it, Backspace on an empty pair removes both, `Enter` between `{ }` opens an indented block (and typing `}` on its empty line takes the waiting one), Python `"""` closes as a docstring; smart about apostrophes (`don't`), string prefixes (`f"…"`), and Rust lifetimes (`&'a`) |
| Registers | `a-z`, `A-Z` (append), `0`, `1-9`, `-`, `_`, `+`/`*` (system clipboard via OSC 52 / wl-paste / xclip / pbpaste) |
| Macros and marks | `qa…q`, `@a`, `@@`, `ma`, `'a`, `` `a ``, jump list (`Ctrl-o`, `Tab`) |
| Ex commands | `:w :q :wq :x :wa :qa :e :bn :bp :bd :b :ls :s :g :v :d :y :> :< :j :m :t :sort :normal :set :r :!cmd :cd` with ranges |
| Project panel and mouse | file tree on the left (`Alt-b`, the `≡` button, `Ctrl-w h`), buffer tabs on the top row, clicks, wheel, new / delete files and folders, see below |
| Indent guides | a `│` at every indentation level, also on blank lines between indented lines; the block the cursor is in is brighter (`:set noindentguides` turns them off) |
| Finding things | `Ctrl-p` fuzzy file finder, `:grep text` over the project, buffer picker |
| Highlighting | Rust, Python, JS/TS, C/C++, Go, Java, Lua, shell, JSON, TOML, YAML, HTML/XML, Markdown (no CSS yet) |

### LSP

Servers start when you open a file (found in `PATH`, plus `~/.cargo/bin`, `~/.local/bin`, `~/go/bin`):

| Files | Servers tried, in order |
| --- | --- |
| Rust | `rust-analyzer` |
| Python | `pyright-langserver`, `basedpyright-langserver`, `pylsp`, `jedi-language-server`, `ruff server` |
| JS / TS | `typescript-language-server`, `vtsls` |
| C / C++ | `clangd`, `ccls` |
| Go, Lua, Zig, Java, Ruby, PHP, Bash, JSON, HTML, CSS, YAML, TOML, Markdown, Nix, … | `gopls`, `lua-language-server`, `zls`, `jdtls`, … |

| Keys | Action |
| --- | --- |
| `gd` or `Ctrl-]` | go to definition |
| `K` | hover documentation |
| `gr` or `:refs` | references (picker) |
| `]d` / `[d` | next / previous diagnostic |
| `:diag` | list diagnostics |
| `:rename name` | rename symbol (across files) |
| `:format` | format the buffer |
| `:lsp` / `:lsprestart` | server status / restart |
| insert mode | completion pops up while you type; `Tab` / `Shift-Tab` move down / up (also `Ctrl-n`/`Ctrl-p`, arrows), `Enter` or `Ctrl-y` accept, `Ctrl-e` dismiss, `Ctrl-Space`/`Ctrl-x` trigger. When the word is already complete, `Enter` is just a newline. |
| snippets | completions that carry a snippet (clangd's `#include <header>` and function calls, rust-analyzer's macros and calls, ...) insert their whole text and highlight the first placeholder: type to replace it, `Tab` / `Shift-Tab` jump to the next / previous placeholder (the last `Tab` lands on `$0` or just after the text), `Esc` keeps the defaults. Multi-line snippets follow the line's indentation. While placeholders are active no popup comes up on its own (`Tab` belongs to the placeholders); `Ctrl-Space` still opens one. On `#include <...>` / `"..."` lines the closing bracket that the directive already put there is reused: header names from the popup replace it instead of doubling it, typing `>` steps over it, and picking a directory (`sys/`) lists what is inside it. |

Errors and warnings show as `E3 W1` in the status line, underlines in the text and signs in the gutter; the
message for the cursor line appears on the bottom row. Edits are sent to the server incrementally.

### Project panel, tabs and mouse

The left side shows the project (the directory you started `ved` in, or the last `:cd`) as a tree, the way Zed does: folders
first, then files, the open file in bold, git state in colour (modified, added, deleted, conflicts; ignored files dimmed,
and a folder takes the colour of what is inside it). The top row has the `≡` button in its left corner, which hides and
shows the panel, and a tab for every open buffer (`+` marks unsaved ones).

| Key | Does |
| --- | --- |
| `Alt-b` | hide / show the panel (while the editor has focus; in the terminal `Alt-b` belongs to the shell) |
| `Ctrl-w h` | show the panel if need be and move the keyboard into it |
| `Ctrl-w l`, `Esc`, `q` | back to the editor (the panel stays) |
| `j` `k` `↓` `↑`, `gg` `G`, `Ctrl-d` `Ctrl-u` | move |
| `Enter` `o` `Space` | open a file, or open / close a folder |
| `l` `→` / `h` `←` | open a folder or step into it / close a folder or go to its parent |
| `a` | new **file**: asks for a name in the command row (in the selected folder, or next to the selected file) |
| `f` | new **folder**: the same; `a/b/c` makes the folders in between |
| `d` | delete the selected **file** (asks `y/N`) |
| `dd` | delete the selected **folder** and everything in it (asks `y/N`; a single `d` on a folder only tells you to press it again) |
| `R` | re-read the directory and the git state |
| `:tree` | the same as `Alt-b` from the command line |

Opening a file from the panel gives the keyboard to the editor. The panel follows the file you are editing (it opens the
folders above it and selects it). With the mouse: click a row to open it, a tab to switch to it, `≡` to hide the panel, a
text position to put the cursor there; the wheel scrolls the text under it, the panel, a popup or the terminal's history.

- **Resize the panel** by dragging its right edge (the `│` line, in the panel or on the top row); the line lights up while
  you drag. The width stays between 16 and 80 columns and always leaves 30 for the text. ved shows the number when you let
  go; `set sidebarwidth=N` in your config keeps it.
- **New file, new folder, delete** (`a`, `f`, `d`, `dd`, in the panel). The name is typed in the command row at the bottom
  (`Enter` makes it, `Esc` or `Ctrl-c` cancels, `Ctrl-u` clears, `Ctrl-w` deletes a word, `Backspace` on an empty name
  cancels). A new file opens in the editor; a new folder is selected in the panel. Deleting asks `Delete a.rs? (y/N)` (for a folder it also says how many items are in it)
  and only `y` confirms; `:set noconfirmdelete` skips the question (not recommended). The rules are strict, because the
  names come from a keyboard and a paste:
  - it never overwrites: a name that exists is refused, and nothing is created through a symbolic link;
  - nothing outside the project: absolute names, `..`, `.`, control characters, names longer than 255 bytes (4000 in
    all) and `.git` are refused; the folder it goes into must really be inside the project;
  - deleting refuses the project folder itself, `.git`, anything outside the project, the folder `ved` was started in and
    its parents, and anything that changed from a file to a folder (or back) since the panel listed it; a symbolic link
    is removed as a link, never followed, so what it points to survives;
  - **deletion is permanent** (there is no trash). Buffers showing a deleted file are closed, unless they have unsaved
    changes: those stay open (`:w` would make the file again) and the message says so.
- **Close a buffer** with the `×` at the end of its tab (or a middle click on the tab). A tab with unsaved changes (marked
  `●`) only asks the first time; click its `×` again to discard the changes, or save with `:w`. Closing the last buffer
  leaves an empty one.

While mouse reporting is on your terminal's own text selection needs `Shift` (`Shift-drag`); `set nomouse` gives it back
entirely. On a narrow window the panel gets narrower to leave 30 columns for the text, and disappears when it would be
under 16 columns, or on a window shorter than 6 rows. It never follows symbolic links, hides `.git`, and lists at most 5000 names per folder, 50 000 rows and 40 levels,
so a huge or hostile tree cannot slow the editor down; git is asked in the background and never blocks a key press.

### Embedded terminal

| Key | Does |
| --- | --- |
| `Alt-q` | open the terminal (a shell starts on first use; a hidden pane comes back) and focus it |
| `Alt-w` | hide the pane, or show it again. The shell keeps running while it is hidden |
| `Alt-e` | close the pane and stop the shell |
| `Alt-s` | move focus between the editor and the pane (opens it if there is none) |

All four work in every mode, including while the terminal has focus. Nothing else changes focus: `Ctrl-\` and
`Ctrl-w j` / `Ctrl-w k` are not used (`Ctrl-\` reaches the shell like any other key). `:term` opens / focuses the pane from
the command line, `:termhide` hides / shows it, `:termclose` or `Ctrl-w c` close it.

`Home` and `End` at a shell prompt jump to the start / end of the command line in any shell, with no setup: ved sends
`Ctrl-A` / `Ctrl-E`, which bash, zsh and fish all know. (A plain zsh has no binding for the xterm sequences that a
terminal sends for those keys and silently ignores them, which is why they seemed dead.) Ved knows the shell is at its
prompt because it is the process reading the terminal; while a program runs (`vim`, `less`, `cat`, a REPL, `tmux`, …) the
keys reach it as xterm sequences instead. `set termkeys=xterm` turns the prompt shortcut off if you would rather have the
sequences everywhere (for a shell whose Home / End are bound to something else).

All other keys go to the shell the way xterm sends them, whatever your terminal reports: `Delete`, `Insert`, `PageDown`,
`F1`–`F12`, the arrows, and modified keys such as `Ctrl-Left` / `Ctrl-Right` (word jumps) or `Ctrl-Delete`. Programs that
switch the terminal to application cursor mode (readline and zsh's line editor do while you type a command) get
`ESC O H` / `ESC O F` and `ESC O A` …, as with a real xterm. `PageUp` scrolls the pane's history; in the history `Home` /
`End` jump to the oldest / newest line and `Esc` or `i` returns to the shell. The pane handles full-screen programs (vim,
htop, less), 256-colour/truecolor, bracketed paste and resizing.

If some other key does nothing, the shell has no binding for it. Run `cat -v` in the pane and press the key to see what
arrives (for example `^[[3;5~` for `Ctrl-Delete`), then bind that in your shell, e.g. in zsh `bindkey '^[[3;5~' kill-word`.

`Alt` has to reach ved as an Escape prefix, which is the default in most Linux terminals. In macOS Terminal.app turn on
"Use Option as Meta key"; in iTerm2 set the left Option key to "Esc+". The shell inside the pane never sees `Alt-s`,
`Alt-q`, `Alt-w` or `Alt-e`, so shell bindings on those keys (zsh's `Alt-q`, for example) are not available there.

### Colours

ved paints its own dark theme (background, text, status line, popups, and the default colours inside the terminal pane),
so it looks the same in a light or a dark terminal. `set theme=terminal` leaves the background and foreground to your
terminal instead (`set theme=dark` switches back). There is no light theme yet.

**Black strips around the text?** Terminals draw a margin that no program can paint with characters: the window
padding, plus the leftover pixels when the window is not an exact number of rows and columns (up to almost a
whole row at the bottom, and a few pixels on the right and top). They are filled with the terminal's own
default background. With the dark theme, ved sets that colour to match (the OSC 11 escape sequence) and puts it
back when it exits, also after a crash that it can catch, so the margins disappear. This works in foot, kitty,
alacritty, wezterm, xterm, VTE terminals (GNOME Terminal and friends), konsole and iTerm2; it does nothing in macOS
Terminal.app or under tmux, and ved skips it on the Linux console. `set notermbg` turns it off. If a terminal ignores it,
set that terminal's own background to `#1c1c1c`, or reduce its padding (foot: `pad=0x0`, kitty:
`window_padding_width 0`, alacritty: `window.padding`). If ved is killed without a chance to clean up, the terminal keeps
the dark background until you run `printf '\e]111\e\\'` or `reset`.

## Configuration

`~/.config/ved/config` (or `$XDG_CONFIG_HOME/ved/config`) is a list of ex-style lines:

```
set shiftwidth=4 tabstop=4 expandtab relativenumber   # 2 spaces is the default
set noautopairs                 # autopairs are on by default
set detectindent                # follow each file's own indentation instead of always using shiftwidth
set ignorecase smartcase
set termheight=12
set scrolloff=4                 # keep 4 lines around the cursor when scrolling (default 0: the cursor goes to the very edge)
set noindentguides              # no indent guides (default: on); `set indentchar=¦` changes the character (one printable character)
set noconfirmdelete             # delete files and folders from the panel without asking (default: asks y/N)
set nosidebar                   # start with the project panel hidden (default: shown); `set sidebarwidth=36` sets its width (16-80; you can also drag it)
set nomouse                     # no mouse reporting: your terminal's own selection works as usual
set theme=terminal              # use the terminal's own colours (default: dark)
set notermbg                    # leave the terminal's own background (window margins) alone
set termkeys=xterm              # Home/End always sent to the shell as xterm sequences (default: auto)
shell /bin/zsh

lsp rs rust-analyzer            # choose or override the server for an extension
lsp py pylsp --some-flag
lsp md off                      # no server for markdown
set nolsp                       # turn LSP off completely
```

## Reliability and safety

`ved` has no dependencies (only the Rust standard library and libc) and is built with `panic = "unwind"` and overflow checks
on, so a bug turns into a recoverable error instead of a crash or silent wrong arithmetic.

| What | How it behaves |
| --- | --- |
| Internal error | The editor saves every modified buffer, repairs its state and carries on. After 3 errors in a short time it saves and exits. |
| Terminal closed, `SIGHUP`/`SIGTERM`/`SIGINT`/`SIGQUIT`, input or output broken | Modified buffers are saved, then `ved` exits cleanly. |
| Recovery files | Written to `$XDG_STATE_HOME/ved/recovery` (default `~/.local/state/ved/recovery`; fallback `/tmp/ved-recovery-<uid>`) as `<name>.recovered`, never over your file. The directory must be a real directory you own (mode 0700); files are created exclusively with mode 0600. Errors are logged to `recovery.log` there. |
| Saving | Written to a temporary file in the same directory and renamed over the original, so a crash or a full disk cannot leave a half-written file; the mode is kept. If the file changed on disk since you read it, `:w` refuses and `:w!` overwrites. Files that cannot be replaced safely (hard links, files owned by someone else) are written in place. |
| Limits | A buffer holds at most 64 Mi characters (bigger files are refused with a message). A single key press, macro or `:g` run is cut short after 5 s, and absurd repeat counts (`99999999999p`) are refused, so nothing can freeze the editor or exhaust memory. `:!cmd` and `:r !cmd` stop after 30 s or 16 MiB of output and kill the whole process group. |
| Pasting and rendering | Bracketed paste is sanitised before it reaches the shell pane, and control characters are never written to your terminal as text (a file cannot send escape sequences to it). |
| LSP | Each server runs in its own process group; a server that crashes is reported (with its last stderr line) and restarted with your documents re-sent, and given up on after 3 crashes in a minute (`:lsprestart` tries again). A server that stops reading, floods the editor or sends oversized messages cannot freeze or exhaust it. Changes are sent incrementally with UTF-16 columns, and when a server forgets a document (nil's "File not loaded" / "out of sync") the whole text is sent again automatically. |

Things to know:

- Opening a project starts the language servers found in `PATH` for it. Some servers (rust-analyzer: build scripts and
  proc-macros; others: project configuration) run code from the project, as they do in any editor, so open projects you
  trust, or `set nolsp` / `lsp rs off`.
- `ved` never reads a file bigger than the buffer limit and never follows a relative or empty `PATH` entry when looking
  for a server. It only reads regular files: a named pipe or a device is refused at once instead of blocking the editor.
- The panel's git colours run `git status` in the project. A repository's own configuration can name programs for git to
  run (`core.fsmonitor`, and content filters); `ved` switches those off for its call, so opening a hostile project does not
  start anything. The cost: a file kept in a per-repository Git LFS setup may show as modified in the panel.
- Creating and deleting from the panel is described above; the same checks apply to every name. The check and the
  action are two steps (as in every program that works on files), so another program changing the folder in
  between can still surprise it, but a file is always created exclusively (`O_EXCL`) and a delete re-checks the kind of
  thing it is about to remove.
- The file finder and `:grep` do not follow symbolic links and stop after 20 000 folders or 2 s when `git` cannot list the files.

## Not (yet) supported

- Search and `:s` are **literal text**, not regular expressions (`:s` flags `g i I`; no `c`).
- No `R` replace mode, no `Ctrl-v` block selection, no dot-repeat of visual-mode operators, no `=` / `gq`.
- LSP: no code actions, signature help, document symbols or semantic highlighting; workspace edits cannot
  create/rename/delete files.
- Snippets: a placeholder that is repeated (`$1 ... $1`) is inserted once and not kept in sync, variables like
  `$TM_FILENAME` use their default text, and dot-repeat replays the snippet's default text, not what you typed
  into its placeholders.
- Windows is not supported.

## Layout

```
src/app.rs        event loop (poll on stdin, wake pipe, terminal pty)
src/editor.rs     editor state, insert mode, command line, search, registers
src/normal.rs     normal / visual mode parser and operators
src/motion.rs     motions and text objects          src/ex.rs     ex commands
src/buffer.rs     line buffer + undo                src/highlight.rs   syntax colours
src/render.rs     drawing                           src/screen.rs      cell grid + diffing renderer
src/lsp.rs        LSP client                        src/json.rs        JSON / JSON-RPC
src/snippet.rs    LSP snippets (parser, placeholders)
src/vt.rs         terminal emulator                 src/term.rs        embedded terminal pane
src/sys.rs        libc bindings (termios, poll, pty, signals)
src/tree.rs       project panel model (lazy tree, git colours)   src/sidebar.rs   panel, top bar, tabs and mouse
src/fileops.rs    safe create / delete of files and folders      src/panelops.rs  the panel's `a` `f` `d` `dd` and their prompt
src/guides.rs     indent guides (which columns, which block)
src/recovery.rs   saving unsaved work when the editor cannot go on
flake.nix  nix/package.nix  default.nix  shell.nix      Nix packaging and dev shell
tests/            vim.rs (keystroke tests), sidebar.rs (panel, bar, mouse, file operations), guides.rs (indent guides), e2e.rs (real binary on a pty, incl. LSP), fake_lsp.py,
                  lsp_sync.rs + fake_nil.py (strict model of nil's document sync), fuzz.rs (fuzzers)
```

`cargo test` runs everything: unit tests, headless keystroke tests, end-to-end tests that drive the real binary on
a pseudo-terminal (including a scripted language server and, when installed, real pyright / rust-analyzer), the
language-server client against a strict model of nil, and fuzzers for the input parser, terminal emulator, JSON,
snippets, highlighter, undo and the editor itself. `VED_FUZZ=50 cargo test --release --test fuzz` runs the fuzzers for
longer; a failing seed is replayed with `VED_FUZZ_SEED=<n> cargo test --test fuzz replay_one_seed`.
