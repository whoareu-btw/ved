//! Options, `:set` handling and the config file.
//!
//! The config file (`~/.config/ved/config`) is just a list of ex commands:
//!
//! ```text
//! set shiftwidth=2 noexpandtab
//! set relativenumber
//! set noautopairs               # autopairs are on by default
//! set detectindent              # follow each file's own indentation instead of always 4 spaces
//! lsp rs rust-analyzer          # override / add a language server for an extension
//! lsp py off                    # disable it
//! shell /bin/zsh
//! set theme=terminal            # use the terminal's own colours instead of the built-in dark theme
//! set notermbg                  # do not recolour the terminal's own background (window margins)
//! set termkeys=xterm            # Home/End always reach the shell as xterm sequences (default: auto)
//! ```

use std::path::PathBuf;

/// Where the colours come from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Theme {
    /// ved paints its own dark background and foreground, whatever the terminal's theme is.
    Dark,
    /// Leave the background and foreground to the terminal.
    Terminal,
}

impl Theme {
    pub fn name(self) -> &'static str {
        match self {
            Theme::Dark => "dark",
            Theme::Terminal => "terminal",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Options {
    pub tabstop: usize,
    pub shiftwidth: usize,
    pub expandtab: bool,
    pub number: bool,
    pub relativenumber: bool,
    pub ignorecase: bool,
    pub smartcase: bool,
    pub hlsearch: bool,
    pub wrapscan: bool,
    pub scrolloff: usize,
    /// Send yanks to the system clipboard via the OSC 52 escape sequence.
    pub osc52: bool,
    pub autoindent: bool,
    pub autocomplete: bool,
    /// Close brackets and quotes as you type them.
    pub autopairs: bool,
    /// Take the indentation style (tabs/width) from each file instead of always using `shiftwidth`.
    pub detectindent: bool,
    pub lsp: bool,
    /// The project panel (file tree) on the left, and its width in columns.
    pub sidebar: bool,
    pub sidebar_width: usize,
    /// Mouse support: click the ≡ button, tabs, tree rows and text; wheel scrolling.
    pub mouse: bool,
    /// Thin vertical lines in the indentation (they carry on through blank lines), and what they are drawn with.
    pub indent_guides: bool,
    pub indent_char: char,
    /// Ask before the project panel deletes a file or a folder.
    pub confirm_delete: bool,
    /// Terminal pane height as a percentage of the screen.
    pub term_height: usize,
    pub shell: String,
    /// `dark` (default) or `terminal`.
    pub theme: Theme,
    /// With the dark theme, also set the terminal's own default background (OSC 11) so the margins
    /// around the text area match it.
    pub termbg: bool,
    /// `termkeys=auto` (default): at a shell prompt Home / End move to the start / end of the line
    /// (Ctrl-A / Ctrl-E, which every shell understands without any key bindings of its own);
    /// `termkeys=xterm`: they are always sent as the xterm sequences.
    pub term_prompt_keys: bool,
    /// (extension, command line or None to disable)
    pub lsp_overrides: Vec<(String, Option<String>)>,
}

impl Default for Options {
    fn default() -> Options {
        Options {
            tabstop: 2,
            shiftwidth: 2,
            expandtab: true,
            number: true,
            relativenumber: false,
            ignorecase: false,
            smartcase: true,
            hlsearch: true,
            wrapscan: true,
            scrolloff: 0,
            osc52: true,
            autoindent: true,
            autocomplete: true,
            autopairs: true,
            detectindent: false,
            lsp: true,
            sidebar: true,
            sidebar_width: 30,
            mouse: true,
            indent_guides: true,
            indent_char: '\u{2502}',
            confirm_delete: true,
            term_height: 35,
            shell: std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string()),
            theme: Theme::Dark,
            termbg: true,
            term_prompt_keys: true,
            lsp_overrides: Vec::new(),
        }
    }
}

pub fn config_path() -> Option<PathBuf> {
    if let Ok(x) = std::env::var("XDG_CONFIG_HOME") {
        if !x.is_empty() {
            return Some(PathBuf::from(x).join("ved").join("config"));
        }
    }
    std::env::var("HOME").ok().map(|h| PathBuf::from(h).join(".config").join("ved").join("config"))
}

impl Options {
    /// Load the user config. Returns the options plus any warnings to show.
    pub fn load() -> (Options, Vec<String>) {
        let mut o = Options::default();
        let mut warn = Vec::new();
        if let Some(p) = config_path() {
            if let Ok(text) = crate::buffer::read_text(&p) {
                for (i, line) in text.lines().enumerate() {
                    if let Err(e) = o.apply_line(line) {
                        warn.push(format!("config line {}: {}", i + 1, e));
                    }
                }
            }
        }
        (o, warn)
    }

    /// Apply one config line (`set ...`, `lsp ...`, `shell ...`).
    pub fn apply_line(&mut self, line: &str) -> Result<(), String> {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() || line.starts_with('"') {
            return Ok(());
        }
        let (cmd, rest) = match line.split_once(char::is_whitespace) {
            Some((c, r)) => (c, r.trim()),
            None => (line, ""),
        };
        match cmd {
            "set" | "se" => {
                for item in rest.split_whitespace() {
                    self.set(item)?;
                }
                Ok(())
            }
            "shell" => {
                self.shell = rest.to_string();
                Ok(())
            }
            "lsp" => {
                let (ext, cmdline) = rest.split_once(char::is_whitespace).ok_or("usage: lsp <ext> <command|off>")?;
                let cmdline = cmdline.trim();
                let val = if cmdline == "off" { None } else { Some(cmdline.to_string()) };
                self.lsp_overrides.retain(|(e, _)| e != ext);
                self.lsp_overrides.push((ext.trim_start_matches('.').to_string(), val));
                Ok(())
            }
            other => Err(format!("unknown config command '{}'", other)),
        }
    }

    fn flag(&mut self, name: &str) -> Option<&mut bool> {
        Some(match name {
            "expandtab" | "et" => &mut self.expandtab,
            "number" | "nu" => &mut self.number,
            "relativenumber" | "rnu" => &mut self.relativenumber,
            "ignorecase" | "ic" => &mut self.ignorecase,
            "smartcase" | "scs" => &mut self.smartcase,
            "hlsearch" | "hls" => &mut self.hlsearch,
            "wrapscan" | "ws" => &mut self.wrapscan,
            "autoindent" | "ai" => &mut self.autoindent,
            "autocomplete" | "ac" => &mut self.autocomplete,
            "autopairs" | "ap" => &mut self.autopairs,
            "detectindent" | "di" => &mut self.detectindent,
            "osc52" => &mut self.osc52,
            "termbg" | "tbg" => &mut self.termbg,
            "lsp" => &mut self.lsp,
            "sidebar" | "sb" => &mut self.sidebar,
            "mouse" => &mut self.mouse,
            "indentguides" | "ig" => &mut self.indent_guides,
            "confirmdelete" | "cfd" => &mut self.confirm_delete,
            _ => return None,
        })
    }

    fn num(&mut self, name: &str) -> Option<&mut usize> {
        Some(match name {
            "tabstop" | "ts" => &mut self.tabstop,
            "shiftwidth" | "sw" => &mut self.shiftwidth,
            "scrolloff" | "so" => &mut self.scrolloff,
            "termheight" | "th" => &mut self.term_height,
            "sidebarwidth" | "sbw" => &mut self.sidebar_width,
            _ => return None,
        })
    }

    /// One `:set` item: `name`, `noname`, `name!`, `name=value`, `name?`.
    /// Returns a message to display (for queries) or an error.
    pub fn set(&mut self, item: &str) -> Result<(), String> {
        if let Some((name, val)) = item.split_once('=') {
            if name == "shell" {
                self.shell = val.to_string();
                return Ok(());
            }
            if name == "indentchar" {
                // one character that takes one cell and cannot drive the terminal
                let mut it = val.chars();
                return match (it.next(), it.next()) {
                    (Some(c), None) if crate::screen::char_width(c) == 1 && crate::screen::printable(c) == c && c != ' ' => {
                        self.indent_char = c;
                        Ok(())
                    }
                    _ => Err("indentchar needs one visible character, e.g. set indentchar=|".to_string()),
                };
            }
            if name == "termkeys" {
                self.term_prompt_keys = match val {
                    "auto" => true,
                    "xterm" => false,
                    _ => return Err(format!("unknown termkeys: {} (auto or xterm)", val)),
                };
                return Ok(());
            }
            if matches!(name, "theme" | "background" | "bg") {
                self.theme = match val {
                    "dark" => Theme::Dark,
                    "terminal" | "term" | "none" => Theme::Terminal,
                    "light" => return Err("there is no light theme: use dark or terminal".to_string()),
                    _ => return Err(format!("unknown theme: {} (dark or terminal)", val)),
                };
                return Ok(());
            }
            let n: usize = val.parse().map_err(|_| format!("number required after =: {}", item))?;
            let n = if name == "tabstop" || name == "ts" || name == "shiftwidth" || name == "sw" { n.max(1) } else { n };
            return match self.num(name) {
                Some(slot) => {
                    *slot = n;
                    Ok(())
                }
                None => Err(format!("unknown option: {}", name)),
            };
        }
        if let Some(name) = item.strip_suffix('!') {
            return match self.flag(name) {
                Some(f) => {
                    *f = !*f;
                    Ok(())
                }
                None => Err(format!("unknown option: {}", name)),
            };
        }
        if let Some(name) = item.strip_prefix("no") {
            if let Some(f) = self.flag(name) {
                *f = false;
                return Ok(());
            }
        }
        match self.flag(item) {
            Some(f) => {
                *f = true;
                Ok(())
            }
            None => Err(format!("unknown option: {}", item)),
        }
    }

    /// `:set name?` text.
    pub fn describe(&mut self, name: &str) -> Option<String> {
        if name == "shell" {
            return Some(format!("shell={}", self.shell));
        }
        if matches!(name, "theme" | "background" | "bg") {
            return Some(format!("theme={}", self.theme.name()));
        }
        if name == "termkeys" {
            return Some(format!("termkeys={}", if self.term_prompt_keys { "auto" } else { "xterm" }));
        }
        if let Some(n) = self.num(name) {
            return Some(format!("{}={}", name, n));
        }
        if let Some(f) = self.flag(name) {
            return Some(if *f { name.to_string() } else { format!("no{}", name) });
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_forms() {
        let mut o = Options::default();
        o.set("noexpandtab").unwrap();
        assert!(!o.expandtab);
        o.set("expandtab!").unwrap();
        assert!(o.expandtab);
        o.set("sw=2").unwrap();
        assert_eq!(o.shiftwidth, 2);
        assert!(o.set("bogus").is_err());
        assert_eq!(o.describe("sw").unwrap(), "sw=2");
    }

    #[test]
    fn termkeys_option() {
        let mut o = Options::default();
        assert!(o.term_prompt_keys);
        assert_eq!(o.describe("termkeys").unwrap(), "termkeys=auto");
        o.set("termkeys=xterm").unwrap();
        assert!(!o.term_prompt_keys);
        assert_eq!(o.describe("termkeys").unwrap(), "termkeys=xterm");
        o.set("termkeys=auto").unwrap();
        assert!(o.term_prompt_keys);
        assert!(o.set("termkeys=vt").is_err());
    }

    #[test]
    fn config_lines() {
        let mut o = Options::default();
        o.apply_line("set rnu tabstop=8  # comment").unwrap();
        assert!(o.relativenumber);
        assert_eq!(o.tabstop, 8);
        o.apply_line("lsp rs my-analyzer --flag").unwrap();
        o.apply_line("lsp py off").unwrap();
        assert_eq!(o.lsp_overrides.len(), 2);
        assert_eq!(o.lsp_overrides[1], ("py".to_string(), None));
        assert!(o.apply_line("frobnicate").is_err());
    }
}
