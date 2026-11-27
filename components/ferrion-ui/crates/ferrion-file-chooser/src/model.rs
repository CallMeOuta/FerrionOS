#![forbid(unsafe_code)]

use std::cmp::Ordering;
use std::path::{Path, PathBuf};

use rune::file_dialog::chooser::{
    ChooserReply, ChooserRequest, GrantMode, MAX_PATH_BYTES, is_plain_file_name,
};
use rune::{FileDialogKind, TextEdit};

pub const MAX_DIR_ENTRIES: usize = 4_096;
const MAX_SCAN_ENTRIES: usize = 1_000_000;
pub const VISIBLE_ROWS: usize = 64;

#[derive(Clone, Debug)]
pub struct DirEntryItem {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
}

#[derive(Clone, Debug)]
pub enum ListingState {
    Ok {
        entries: Vec<DirEntryItem>,
        truncated_more: usize,
        truncated_capped: bool,
    },
    Error(String),
}

pub struct ChooserModel {
    pub kind: FileDialogKind,
    pub title: String,
    pub current_dir: PathBuf,
    pub filters: Vec<(String, Vec<String>)>,
    pub active_filter: Option<usize>,
    pub listing: ListingState,
    pub selected: Option<usize>,
    pub scroll_offset: usize,
    pub save_name: TextEdit,
    pub inline_error: Option<String>,
    pub overwrite_confirm: Option<PathBuf>,
    pub decided: Option<ChooserReply>,
}

impl ChooserModel {
    pub fn from_request(req: ChooserRequest) -> Self {
        let title = if req.options.title.is_empty() {
            match req.kind {
                FileDialogKind::Open => "Open File".to_owned(),
                FileDialogKind::Save => "Save File".to_owned(),
                FileDialogKind::Folder => "Choose Folder".to_owned(),
            }
        } else {
            req.options.title
        };
        let initial_dir = req
            .options
            .directory
            .filter(|p| p.is_absolute())
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("/"));
        let active_filter = if req.kind == FileDialogKind::Folder || req.options.filters.is_empty()
        {
            None
        } else {
            Some(0)
        };
        let mut save_name = TextEdit::new(req.options.file_name.as_deref().unwrap_or(""));
        if req.kind == FileDialogKind::Save {
            save_name.focused = true;
        }
        let mut model = Self {
            kind: req.kind,
            title,
            current_dir: initial_dir,
            filters: req.options.filters,
            active_filter,
            listing: ListingState::Ok {
                entries: Vec::new(),
                truncated_more: 0,
                truncated_capped: false,
            },
            selected: None,
            scroll_offset: 0,
            save_name,
            inline_error: None,
            overwrite_confirm: None,
            decided: None,
        };
        model.reload_dir();
        model
    }

    pub fn reload_dir(&mut self) {
        self.inline_error = None;
        self.overwrite_confirm = None;
        self.scroll_offset = 0;
        let read = match std::fs::read_dir(&self.current_dir) {
            Ok(rd) => rd,
            Err(err) => {
                self.listing = ListingState::Error(format!(
                    "Cannot open {}: {err}",
                    self.current_dir.display()
                ));
                self.selected = None;
                return;
            }
        };
        let mut all = Vec::new();
        let mut capped = false;
        for (scanned, item) in read.enumerate() {
            if scanned >= MAX_SCAN_ENTRIES {
                capped = true;
                break;
            }
            let Ok(entry) = item else {
                continue;
            };
            let os_name = entry.file_name();
            let Some(name) = os_name.to_str() else {
                continue;
            };
            if name.contains('\0') {
                continue;
            }
            let path = self.current_dir.join(name);
            let is_dir = std::fs::metadata(&path)
                .map(|m| m.is_dir())
                .unwrap_or(false);
            if self.kind == FileDialogKind::Folder && !is_dir {
                continue;
            }
            if !is_dir && !self.matches_active_filter(name) {
                continue;
            }
            all.push(DirEntryItem {
                name: name.to_owned(),
                path,
                is_dir,
            });
        }
        all.sort_by(compare_entries);
        let truncated_more = all.len().saturating_sub(MAX_DIR_ENTRIES);
        all.truncate(MAX_DIR_ENTRIES);
        self.selected = if all.is_empty() { None } else { Some(0) };
        self.listing = ListingState::Ok {
            entries: all,
            truncated_more,
            truncated_capped: capped,
        };
    }

    fn matches_active_filter(&self, name: &str) -> bool {
        let Some(idx) = self.active_filter else {
            return true;
        };
        let Some((_, exts)) = self.filters.get(idx) else {
            return true;
        };
        if exts.is_empty() {
            return true;
        }
        let lower_name = name.to_ascii_lowercase();
        for ext in exts {
            let trimmed = ext.trim_start_matches('*').trim_start_matches('.');
            if trimmed.is_empty() || trimmed == "*" {
                return true;
            }
            let suffix = format!(".{}", trimmed.to_ascii_lowercase());
            if lower_name.ends_with(&suffix) {
                return true;
            }
        }
        false
    }

    pub fn active_filter_label(&self) -> String {
        let Some(idx) = self.active_filter else {
            return if self.kind == FileDialogKind::Folder {
                "Folders".to_owned()
            } else {
                "All files".to_owned()
            };
        };
        let Some((name, exts)) = self.filters.get(idx) else {
            return "All files".to_owned();
        };
        let joined = exts
            .iter()
            .map(|e| format!("*.{}", e.trim_start_matches('*').trim_start_matches('.')))
            .collect::<Vec<_>>()
            .join(", ");
        if name.is_empty() {
            joined
        } else {
            format!("{name} ({joined})")
        }
    }

    pub fn cycle_filter(&mut self) {
        if self.kind == FileDialogKind::Folder || self.filters.is_empty() {
            return;
        }
        self.active_filter = match self.active_filter {
            None => Some(0),
            Some(i) if i + 1 < self.filters.len() => Some(i + 1),
            Some(_) => None,
        };
        self.reload_dir();
    }

    pub fn navigate_up(&mut self) {
        if let Some(parent) = self.current_dir.parent() {
            let next = parent.to_path_buf();
            if next != self.current_dir {
                self.current_dir = next;
                self.reload_dir();
            }
        }
    }

    pub fn navigate_to(&mut self, dir: PathBuf) {
        if dir.is_absolute() {
            self.current_dir = dir;
            self.reload_dir();
        }
    }

    pub fn select_index(&mut self, idx: usize) {
        let ListingState::Ok { entries, .. } = &self.listing else {
            return;
        };
        if idx >= entries.len() {
            return;
        }
        self.selected = Some(idx);
        self.inline_error = None;
        self.overwrite_confirm = None;
        if self.kind == FileDialogKind::Save && !entries[idx].is_dir {
            let focused = self.save_name.focused;
            self.save_name = TextEdit::new(&entries[idx].name);
            self.save_name.focused = focused;
        }
        self.ensure_selected_visible();
    }

    pub fn move_selection(&mut self, delta: isize) {
        let ListingState::Ok { entries, .. } = &self.listing else {
            return;
        };
        if entries.is_empty() {
            return;
        }
        let cur = self.selected.unwrap_or(0) as isize;
        let max_idx = (entries.len() - 1) as isize;
        let next = (cur + delta).clamp(0, max_idx) as usize;
        self.select_index(next);
    }

    fn ensure_selected_visible(&mut self) {
        let Some(sel) = self.selected else {
            return;
        };
        if sel < self.scroll_offset {
            self.scroll_offset = sel;
        } else if sel >= self.scroll_offset + VISIBLE_ROWS {
            self.scroll_offset = sel + 1 - VISIBLE_ROWS;
        }
    }

    pub fn activate_index(&mut self, idx: usize) -> Option<ChooserReply> {
        let item = match &self.listing {
            ListingState::Ok { entries, .. } => entries.get(idx).cloned(),
            ListingState::Error(_) => None,
        }?;
        if item.is_dir {
            self.navigate_to(item.path);
            return None;
        }
        match self.kind {
            FileDialogKind::Open => {
                let reply = ChooserReply::Ok {
                    mode: GrantMode::Read,
                    path: item.path,
                    overwrite_confirmed: false,
                };
                self.decided = Some(reply.clone());
                Some(reply)
            }
            FileDialogKind::Save => {
                let focused = self.save_name.focused;
                self.save_name = TextEdit::new(&item.name);
                self.save_name.focused = focused;
                self.confirm_primary()
            }
            FileDialogKind::Folder => None,
        }
    }

    pub fn confirm_primary(&mut self) -> Option<ChooserReply> {
        if let Some(target) = self.overwrite_confirm.take() {
            let reply = ChooserReply::Ok {
                mode: GrantMode::Write,
                path: target,
                overwrite_confirmed: true,
            };
            self.decided = Some(reply.clone());
            return Some(reply);
        }
        match self.kind {
            FileDialogKind::Open => {
                let idx = self.selected?;
                self.activate_index(idx)
            }
            FileDialogKind::Folder => {
                let target = match (&self.listing, self.selected) {
                    (ListingState::Ok { entries, .. }, Some(idx)) => entries
                        .get(idx)
                        .filter(|e| e.is_dir)
                        .map_or_else(|| self.current_dir.clone(), |e| e.path.clone()),
                    _ => self.current_dir.clone(),
                };
                if !std::fs::metadata(&target).is_ok_and(|m| m.is_dir()) {
                    self.inline_error = Some("Selected folder is not accessible".to_owned());
                    return None;
                }
                let reply = ChooserReply::Ok {
                    mode: GrantMode::Dir,
                    path: target,
                    overwrite_confirmed: false,
                };
                self.decided = Some(reply.clone());
                Some(reply)
            }
            FileDialogKind::Save => self.try_save_target(),
        }
    }

    fn try_save_target(&mut self) -> Option<ChooserReply> {
        let name = self.save_name.text.trim();
        if !is_plain_file_name(name) {
            self.inline_error =
                Some("Enter a plain file name without '/' or relative segments".to_owned());
            return None;
        }
        if !std::fs::metadata(&self.current_dir).is_ok_and(|m| m.is_dir()) {
            self.inline_error = Some("Target directory is not accessible".to_owned());
            return None;
        }
        let target = self.current_dir.join(name);
        let Some(target_str) = target.to_str() else {
            self.inline_error = Some("Target path is not valid UTF-8".to_owned());
            return None;
        };
        if target_str.len() > MAX_PATH_BYTES || target_str.contains('\0') {
            self.inline_error = Some("Target path exceeds maximum length".to_owned());
            return None;
        }
        if std::fs::symlink_metadata(&target).is_ok() {
            if std::fs::metadata(&target).is_ok_and(|m| m.is_dir()) {
                self.inline_error = Some("Cannot overwrite an existing directory".to_owned());
                return None;
            }
            self.inline_error = None;
            self.overwrite_confirm = Some(target);
            return None;
        }
        let reply = ChooserReply::Ok {
            mode: GrantMode::Write,
            path: target,
            overwrite_confirmed: false,
        };
        self.decided = Some(reply.clone());
        Some(reply)
    }

    pub fn cancel_overwrite_or_dialog(&mut self) -> Option<ChooserReply> {
        if self.overwrite_confirm.take().is_some() {
            return None;
        }
        let reply = ChooserReply::Cancel;
        self.decided = Some(reply.clone());
        Some(reply)
    }

    pub fn can_confirm(&self) -> bool {
        if self.overwrite_confirm.is_some() {
            return true;
        }
        match self.kind {
            FileDialogKind::Open => match (&self.listing, self.selected) {
                (ListingState::Ok { entries, .. }, Some(i)) => {
                    entries.get(i).is_some_and(|e| !e.is_dir)
                }
                _ => false,
            },
            FileDialogKind::Folder => std::fs::metadata(&self.current_dir)
                .map(|m| m.is_dir())
                .unwrap_or(false),
            FileDialogKind::Save => is_plain_file_name(self.save_name.text.trim()),
        }
    }

    pub fn truncated_message(&self) -> Option<String> {
        let ListingState::Ok {
            truncated_more,
            truncated_capped,
            ..
        } = &self.listing
        else {
            return None;
        };
        if *truncated_more == 0 {
            return None;
        }
        if *truncated_capped {
            Some(format!("{truncated_more}+ more not shown"))
        } else {
            Some(format!("{truncated_more} more not shown"))
        }
    }
}

fn compare_entries(a: &DirEntryItem, b: &DirEntryItem) -> Ordering {
    match (a.is_dir, b.is_dir) {
        (true, false) => Ordering::Less,
        (false, true) => Ordering::Greater,
        _ => a
            .name
            .to_ascii_lowercase()
            .cmp(&b.name.to_ascii_lowercase())
            .then_with(|| a.name.cmp(&b.name)),
    }
}

pub fn display_name(path: &Path) -> String {
    path.file_name()
        .and_then(|n| n.to_str())
        .map(str::to_owned)
        .unwrap_or_else(|| path.display().to_string())
}
