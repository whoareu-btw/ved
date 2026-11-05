pub const HELP: &str = "\
ved -- a small vim-style editor with built-in LSP and an embedded terminal
(q or Esc closes this window, j/k scroll)

MODES
  i a I A o O   insert          v V   visual / visual line        :   command line
  Esc / Ctrl-c  back to normal  gv    reselect last selection     /   ?  search

INSERT MODE
  Tab  inserts 2 spaces (a real tab in Makefiles)       Ctrl-d / Shift-Tab  dedent     Ctrl-t  indent
  autopairs   ( [ { \" ' `  close themselves; typing the closer steps over it, Backspace on an empty
              pair removes both, Enter between { } opens an indented block (typing } on its empty line takes the waiting one).   :set noautopairs  turns it off

MOTIONS (all take counts, all work after an operator)
  h j k l  w W e E b B ge  0 ^ $ g_  f F t T ; ,  { } %  gg G  H M L
  n N * #  'a `a  + - _  |        text objects: iw aw iW aW ip ap i\" a\" i' a' i( a( i{ a{ i[ a[ i< a<

OPERATORS   d c y > < g~ gu gU   (dd cc yy >> << g~~ guu gUU, or operator + motion/object)
  x X s S C D Y   p P   J gJ   ~   r<c>   u Ctrl-r   .   Ctrl-a Ctrl-x
  \"ay \"ap  registers a-z A-Z (append), 0 yank, 1-9 deletes, - small delete, + clipboard, _ black hole
  qa ... q   record a macro,  @a  play,  @@  repeat last macro,  3@a
  ma  'a  `a  marks (a-z),  ''  `` previous position,  Ctrl-o / Tab  jump list

FILES AND BUFFERS
  :w [file]  :q  :q!  :wq  :x  ZZ  ZQ  :wa  :qa  :e file  :e!  :enew
  :w! / :wq!  overwrite a file that changed on disk.  Unsaved work is kept in ~/.local/state/ved/recovery if ved dies
  :bn :bp :bd :b N|name  :ls   Ctrl-p or :files  fuzzy file finder (type to filter, Ctrl-n/Ctrl-p, Enter)
  :grep text   search all project files (literal)       :cd dir  :pwd

EDITING COMMANDS
  :[range]s/old/new/[gi]   :[range]d  :y  :>  :<  :j  :sort[!]  :normal keys
  :g/text/cmd   :v/text/cmd   :noh   :set opt  (:set sw=2 noet rnu ic scs ...)  :reg  :marks

LSP (starts automatically per file type; override with `lsp <ext> <cmd>` in the config)
  gd / Ctrl-]  go to definition        K   hover documentation        gr  references
  ]d  [d       next / previous diagnostic    :diag   list diagnostics
  :rename name   :format   :lsp (status)   :lsprestart
  Insert mode:  completion appears while typing: Tab / Shift-Tab move down / up (Ctrl-n / Ctrl-p too),
                Enter or Ctrl-y accept, Ctrl-e dismiss (Enter is a plain newline when the word is already complete)
                Ctrl-Space or Ctrl-x  trigger completion manually
                Snippets (function calls, #include <...>) insert their full text with the first placeholder
                highlighted: type to replace it, Tab / Shift-Tab jump to the next / previous placeholder,
                Esc keeps the defaults. No popup comes up by itself while placeholders are active.
                On #include <..> lines the bracket already there is reused (no doubled `>`); a picked directory
                such as sys/ lists its contents.

PROJECT PANEL AND MOUSE
  ≡ (top-left corner) or Alt-b or :tree   hide / show the project panel        Ctrl-w h  move into the panel   Ctrl-w l  back
  In the panel: j k  gg G  Ctrl-d Ctrl-u  move,  Enter / o / Space  open a file or fold a folder,  l / h  open / close or step in / out,
                R  refresh,  Esc or q  back to the editor.   Git colours: modified, added, deleted, conflict, ignored (dim).
                a  new file,  f  new folder  (type the name, Enter makes it, Esc cancels; a/b/c makes the folders too),
                d  delete the file,  dd  delete the folder and what is in it  (asks y/N; deleting is permanent).
  Mouse: click a row, a tab or a text position; the wheel scrolls what is under it.  Shift-drag selects text in your terminal;
         :set nomouse turns mouse reporting off.   :set nosidebar  /  :set sidebarwidth=36  (16-80)
         Drag the panel's right edge (the | line) to resize it.   x on a tab (or middle click) closes that buffer;
         with unsaved changes the first click only asks, a second one discards.   :set scrolloff=4 keeps a margin when scrolling
  Indent guides: a faint | at every indentation level, through blank lines, the block under the cursor brighter.
         :set noindentguides  /  :set indentchar=X  /  :set noconfirmdelete  (delete from the panel without asking)

TERMINAL
  Alt-q  open the terminal pane (starts a shell) and focus it           Alt-e  or  :termclose  close it (stops the shell)
  Alt-w  hide the pane / show it again (the shell keeps running)        Alt-s  move focus between editor and terminal
  :term opens / focuses it from the command line,  :termhide hides / shows it.
  The four Alt keys work in every mode, also inside the terminal; the shell keeps running while the editor has focus.
  In the terminal: at a prompt Home / End jump to the start / end of the line in any shell (sent as Ctrl-A / Ctrl-E;
  `set termkeys=xterm` sends the xterm sequences instead). Running programs get Home / End, Ctrl-arrows, Delete ... as in xterm.
  PageUp scrolls history (then Home / End jump to its top / bottom, Esc or i returns), everything else goes to the shell.

CONFIG   ~/.config/ved/config  (ex commands, one per line, e.g. `set sw=2`, `lsp py pylsp`, `shell /bin/zsh`)
         Colours: dark theme by default; `set theme=terminal` uses your terminal's own colours,
         `set notermbg` stops ved from recolouring the terminal's own background (the window margins).
";
