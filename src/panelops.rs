//! The project panel's file operations as the person meets them: a name typed in the command-line row
//! (`a` makes a file, `f` a folder) and a question answered with `y` (`d` deletes a file, `dd` a
//! folder). The file system work, and the checks that keep it safe, are in `fileops.rs`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::editor::Editor;
use crate::fileops;
use crate::input::Key;
use crate::tree::Kind;

/// Longest name that can be typed (characters).
const MAX_INPUT: usize = 1000;

pub enum PromptKind {
    NewFile(PathBuf),
    NewFolder(PathBuf),
    Delete { path: PathBuf, is_dir: bool },
}

/// A question in the command-line row: a name to type, or a yes / no.
pub struct Prompt {
    pub kind: PromptKind,
    pub label: String,
    pub input: String,
}

impl Prompt {
    pub fn is_question(&self) -> bool {
        matches!(self.kind, PromptKind::Delete { .. })
    }
}

impl Editor {
    /// The folder `a` and `f` work in: the selected folder, or the one the selected file is in.
    fn panel_dir(&self) -> PathBuf {
        match self.tree.rows.get(self.tree.sel) {
            Some(r) if matches!(r.kind, Kind::Dir | Kind::More) => r.path.clone(),
            Some(r) => r.path.parent().map(Path::to_path_buf).unwrap_or_else(|| self.tree.root.clone()),
            None => self.tree.root.clone(),
        }
    }

    /// `path` as shown to the person: relative to the project.
    fn rel(&self, path: &Path) -> String {
        path.strip_prefix(&self.tree.root).unwrap_or(path).to_string_lossy().to_string()
    }

    /// `a` / `f` in the panel: ask for the name.
    pub fn panel_new(&mut self, folder: bool) {
        let dir = self.panel_dir();
        let rel = self.rel(&dir);
        let what = if folder { "folder" } else { "file" };
        let label = if rel.is_empty() { format!("New {}: ", what) } else { format!("New {} in {}/: ", what, rel) };
        self.msg.clear();
        let kind = if folder { PromptKind::NewFolder(dir) } else { PromptKind::NewFile(dir) };
        self.prompt = Some(Prompt { kind, label, input: String::new() });
    }

    /// `d` in the panel (`second` when it is the second `d` in a row): a file goes after a question,
    /// a folder after `dd` and a question (unless `set noconfirmdelete`).
    pub fn panel_delete(&mut self, second: bool) {
        let Some(row) = self.tree.rows.get(self.tree.sel).cloned() else { return };
        let is_dir = row.kind == Kind::Dir;
        match row.kind {
            Kind::More => return self.set_err("nothing to delete here"),
            Kind::Dir if !second => {
                self.tree.pending = Some('d');
                return self.set_msg(format!("press d again to delete the folder {} and everything in it", row.name));
            }
            _ => {}
        }
        if !self.opts.confirm_delete {
            return self.panel_do_delete(&row.path, is_dir);
        }
        let label = if is_dir {
            let (n, cut) = fileops::count_below(&row.path, 10_000, Duration::from_millis(300));
            format!("Delete the folder {} and its {}{} items? (y/N) ", row.name, n, if cut { "+" } else { "" })
        } else {
            format!("Delete {}? (y/N) ", row.name)
        };
        self.msg.clear();
        self.prompt = Some(Prompt { kind: PromptKind::Delete { path: row.path, is_dir }, label, input: String::new() });
    }

    /// A key while a prompt is up. `y` answers a question yes, anything else no; a name is typed with
    /// Enter to accept it and Esc (or Backspace on nothing) to give up.
    pub fn prompt_key(&mut self, k: Key) {
        let Some(mut p) = self.prompt.take() else { return };
        if let PromptKind::Delete { path, is_dir } = &p.kind {
            if matches!(k, Key::Char('y') | Key::Char('Y')) {
                self.panel_do_delete(path, *is_dir);
            } else {
                self.set_msg("not deleted");
            }
            return;
        }
        match k {
            Key::Esc | Key::Ctrl('c') => return self.msg.clear(),
            Key::Enter => return self.prompt_accept(p),
            Key::Backspace | Key::Ctrl('h') => {
                if p.input.pop().is_none() {
                    return self.msg.clear();
                }
            }
            Key::Ctrl('u') => p.input.clear(),
            Key::Ctrl('w') => {
                while p.input.ends_with(' ') {
                    p.input.pop();
                }
                while p.input.chars().last().is_some_and(|c| c != ' ' && c != '/') {
                    p.input.pop();
                }
            }
            Key::Char(c) if !c.is_control() => {
                if p.input.chars().count() < MAX_INPUT {
                    p.input.push(c);
                }
            }
            Key::Paste(s) => {
                for c in s.chars().take_while(|c| *c != '\n' && *c != '\r').filter(|c| !c.is_control()) {
                    if p.input.chars().count() < MAX_INPUT {
                        p.input.push(c);
                    }
                }
            }
            _ => {}
        }
        self.prompt = Some(p);
    }

    /// Enter on a typed name: make the file or folder.
    fn prompt_accept(&mut self, p: Prompt) {
        let (dir, folder) = match &p.kind {
            PromptKind::NewFile(d) => (d.clone(), false),
            PromptKind::NewFolder(d) => (d.clone(), true),
            PromptKind::Delete { .. } => return,
        };
        let mut name = match fileops::parse_name(&p.input) {
            Ok(n) => n,
            Err(e) => return self.set_err(e),
        };
        name.is_dir |= folder;
        let root = self.tree.root.clone();
        match fileops::create(&root, &dir, &name) {
            Ok(path) => {
                self.tree.rebuild();
                self.tree.request_git();
                self.tree.reveal(&path);
                let shown = self.rel(&path);
                if name.is_dir {
                    self.set_msg(format!("created folder {}/", shown));
                } else {
                    match self.open_file(&path) {
                        Ok(idx) => {
                            self.switch_to(idx);
                            self.unfocus_sidebar();
                            self.set_msg(format!("created {}", shown));
                        }
                        Err(e) => self.set_err(format!("created {}, but: {}", shown, e)),
                    }
                }
            }
            Err(e) => self.set_err(e),
        }
    }

    /// Does buffer `i` show `target` (or, for a folder, something inside it)?
    fn doc_is_under(&self, i: usize, target: &Path, is_dir: bool) -> bool {
        let Some(p) = self.docs[i].buf.path.as_ref() else { return false };
        let abs = if p.is_absolute() { p.clone() } else { std::env::current_dir().map(|c| c.join(p)).unwrap_or_else(|_| p.clone()) };
        // also with the folder resolved (the paths of the panel are)
        let resolved = abs.parent().and_then(|d| d.canonicalize().ok()).zip(abs.file_name()).map(|(d, n)| d.join(n));
        [Some(abs), resolved].into_iter().flatten().any(|a| a == target || is_dir && a.starts_with(target))
    }

    /// Delete `path` (the checks are in `fileops::delete`), close the buffers that showed it and
    /// refresh the panel. A buffer with unsaved changes stays open.
    fn panel_do_delete(&mut self, path: &Path, is_dir: bool) {
        let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        // found now, while the paths still resolve; the indexes are used from the back
        let affected: Vec<usize> = (0..self.docs.len()).filter(|&i| self.doc_is_under(i, path, is_dir)).collect();
        let root = self.tree.root.clone();
        if let Err(e) = fileops::delete(&root, path, is_dir) {
            return self.set_err(format!("{}: {}", name, e));
        }
        let mut kept = 0;
        for &i in affected.iter().rev() {
            if self.docs[i].buf.modified {
                kept += 1;
            } else {
                let _ = self.delete_buffer_at(i, true);
            }
        }
        self.tree.rebuild();
        self.tree.request_git();
        let mut msg = format!("deleted {}{}", name, if is_dir { "/" } else { "" });
        if kept > 0 {
            msg.push_str(&format!(" ({} buffer{} with unsaved changes kept open: :w would make the file again)", kept, if kept == 1 { "" } else { "s" }));
        }
        self.set_msg(msg);
    }
}
