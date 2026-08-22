//! Keyboard event handling for all tabs, search mode, and modals.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use rustix::process::Signal;
use std::collections::HashMap;

use crate::{
    process::{
        ADVANCED_SORT_COLUMNS, NORMAL_SORT_COLUMNS, ProcessInfo, ProcessSortColumn, ProcessTarget,
    },
    ui,
};

use super::{App, PAGE_STEP, STORAGE_PAGE_STEP, Tab};

impl App {
    /// Processes one keyboard event; returns `true` when the application should quit.
    pub(super) fn handle_key(&mut self, key: KeyEvent) -> bool {
        // Blocking modal dialogs capture all keys until dismissed.
        if self.error_popup.take().is_some() {
            return false;
        }
        if self.kill_confirmation.is_some() {
            match key.code {
                KeyCode::Char('y' | 'Y') | KeyCode::Enter => self.execute_kill_confirmation(),
                KeyCode::Char('n' | 'N') | KeyCode::Esc => self.kill_confirmation = None,
                _ => {}
            }
            return false;
        }

        if self.procs.searching {
            self.handle_search_key(key);
            return false;
        }

        // While zoomed into a process, 'q' closes the detail view instead of quitting.
        if self.current_tab == Tab::Processes
            && self.procs.selected_detail.is_some()
            && matches!(key.code, KeyCode::Char('q') | KeyCode::Char('Q'))
        {
            self.procs.selected_detail = None;
            return false;
        }

        match key.code {
            KeyCode::Char('q') => return true,
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => return true,
            KeyCode::Tab => self.select_tab(self.current_tab.next()),
            KeyCode::BackTab => self.select_tab(self.current_tab.prev()),
            KeyCode::Char('1') => self.select_tab(Tab::General),
            KeyCode::Char('2') => self.select_tab(Tab::Processes),
            KeyCode::Char('3') => self.select_tab(Tab::CpuRam),
            KeyCode::Char('4') => self.select_tab(Tab::Gpu),
            KeyCode::Char('5') => self.select_tab(Tab::Network),
            KeyCode::Char('6') => self.select_tab(Tab::Disks),
            KeyCode::Char('[') => self.travel_back(),
            KeyCode::Char(']') => self.travel_forward(),
            _ => match self.current_tab {
                Tab::General => self.handle_general_key(key),
                Tab::Processes => self.handle_processes_key(key),
                Tab::CpuRam => self.handle_pause_key(key),
                Tab::Gpu => self.handle_gpu_key(key),
                Tab::Network => self.handle_network_key(key),
                Tab::Disks => self.handle_disks_key(key),
            },
        }
        false
    }

    /// Keys active while the process search input has focus.
    fn handle_search_key(&mut self, key: KeyEvent) {
        let mut grouped = Vec::new();
        let num_rows = self
            .visible_process_rows(&self.live.processes, &mut grouped)
            .len();
        match key.code {
            KeyCode::Esc => {
                self.procs.searching = false;
                self.procs.query.clear();
                self.procs.table_state.select(Some(0));
            }
            KeyCode::Enter => self.procs.searching = false,
            KeyCode::Backspace => {
                self.procs.query.pop();
                self.refocus_best_match();
            }
            KeyCode::Char('w') if key.modifiers == KeyModifiers::CONTROL => {
                // Delete last word (vim Ctrl+W behaviour): trim trailing whitespace,
                // then drop characters until whitespace.
                let trimmed = self.procs.query.trim_end().to_string();
                self.procs.query = trimmed
                    .trim_end_matches(|c: char| !c.is_whitespace())
                    .to_string();
                self.refocus_best_match();
            }
            KeyCode::Char(c) => {
                self.procs.query.push(c);
                self.refocus_best_match();
            }
            KeyCode::Up => self.move_table_selection(-1, num_rows),
            KeyCode::Down => self.move_table_selection(1, num_rows),
            KeyCode::PageUp => self.move_table_selection(-PAGE_STEP, num_rows),
            KeyCode::PageDown => self.move_table_selection(PAGE_STEP, num_rows),
            _ => {}
        }
    }

    /// General dashboard: overview copy shortcut and sub-tab cycling.
    fn handle_general_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('c' | 'y') => self.copy_system_overview(),
            KeyCode::Left => self.general_sub_tab = (self.general_sub_tab + 3) % 4,
            KeyCode::Right => self.general_sub_tab = (self.general_sub_tab + 1) % 4,
            _ => self.handle_pause_key(key),
        }
    }

    /// Space toggles telemetry polling on any tab without a conflicting binding.
    fn handle_pause_key(&mut self, key: KeyEvent) {
        if key.code == KeyCode::Char(' ') {
            self.toggle_pause();
        }
    }

    /// GPU dashboard: sub-tab switching when the layout overflows into tabs.
    fn handle_gpu_key(&mut self, key: KeyEvent) {
        let is_tabbed = ui::is_gpu_overflow(self.table_area, &self.active_snapshot().gpu_metrics);
        match key.code {
            KeyCode::Left | KeyCode::Char('h') => {
                if is_tabbed {
                    self.gpu_sub_tab = if self.gpu_sub_tab == 0 {
                        1
                    } else {
                        self.gpu_sub_tab - 1
                    };
                }
            }
            KeyCode::Right | KeyCode::Char('l') => {
                if is_tabbed {
                    self.gpu_sub_tab = (self.gpu_sub_tab + 1) % 2;
                }
            }
            _ => self.handle_pause_key(key),
        }
    }

    /// Network dashboard: connections-list scrolling.
    fn handle_network_key(&mut self, key: KeyEvent) {
        let num_conns = self
            .active_snapshot()
            .net_connections
            .as_ref()
            .map(|c| c.len())
            .unwrap_or(0);
        let visible_rows = (self.table_area.height * 55 / 100).saturating_sub(3) as usize;
        let max_scroll = num_conns.saturating_sub(visible_rows);
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.net_scroll_offset = self.net_scroll_offset.saturating_sub(1)
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.net_scroll_offset = (self.net_scroll_offset + 1).min(max_scroll)
            }
            KeyCode::PageUp => {
                self.net_scroll_offset = self.net_scroll_offset.saturating_sub(PAGE_STEP as usize)
            }
            KeyCode::PageDown => {
                self.net_scroll_offset =
                    (self.net_scroll_offset + PAGE_STEP as usize).min(max_scroll)
            }
            KeyCode::Home | KeyCode::Char('g') => self.net_scroll_offset = 0,
            KeyCode::End | KeyCode::Char('G') => self.net_scroll_offset = max_scroll,
            _ => self.handle_pause_key(key),
        }
    }

    /// Sortable columns for the current view width; PID hides on narrow terminals.
    fn sortable_columns(&self) -> Vec<ProcessSortColumn> {
        let show_pid = self.table_area.width >= ui::PID_COLUMN_MIN_WIDTH;
        let all: &[ProcessSortColumn] = if self.procs.advanced {
            &ADVANCED_SORT_COLUMNS
        } else {
            &NORMAL_SORT_COLUMNS
        };
        all.iter()
            .copied()
            .filter(|&c| show_pid || c != ProcessSortColumn::Pid)
            .collect()
    }

    /// Cycles the sort column by `direction` within the visible columns.
    fn cycle_sort_column(&mut self, direction: isize) {
        let cols = self.sortable_columns();
        let cur_idx = cols
            .iter()
            .position(|&c| c == self.procs.sort_col)
            .unwrap_or(0);
        let next_idx = (cur_idx as isize + direction).rem_euclid(cols.len() as isize) as usize;
        self.procs.sort_col = cols[next_idx];
        self.resort_processes();
    }

    /// Shifts the table selection by `delta` rows, clamped to the list bounds.
    pub(super) fn move_table_selection(&mut self, delta: isize, num_rows: usize) {
        let current = self.procs.table_state.selected().unwrap_or(0) as isize;
        let next = current.clamp(0, num_rows.saturating_sub(1) as isize) + delta;
        let next = next.clamp(0, num_rows.saturating_sub(1) as isize) as usize;
        self.procs.table_state.select(Some(next));
    }

    /// Process table: navigation, sorting, grouping, search entry, signalling, and detail view.
    fn handle_processes_key(&mut self, key: KeyEvent) {
        if let Some(detail_pid) = self.procs.selected_detail {
            match key.code {
                KeyCode::Esc | KeyCode::Char('q' | 'Q') | KeyCode::Backspace | KeyCode::Enter => {
                    self.procs.selected_detail = None;
                }
                KeyCode::Char('c' | 'C') => {
                    self.signal_detail_target(detail_pid, Signal::TERM, false)
                }
                KeyCode::Char('t' | 'T') => {
                    self.signal_detail_target(detail_pid, Signal::KILL, true)
                }
                _ => {}
            }
            return;
        }

        match key.code {
            KeyCode::Enter => self.activate_selected_row(),
            KeyCode::Char('/') => {
                self.procs.searching = true;
                self.procs.query.clear();
                self.procs.table_state.select(Some(0));
            }
            KeyCode::Esc => {
                self.procs.query.clear();
                self.procs.table_state.select(Some(0));
            }
            KeyCode::Left => self.cycle_sort_column(-1),
            KeyCode::Right => self.cycle_sort_column(1),
            KeyCode::Char('r') => {
                self.procs.ascending = !self.procs.ascending;
                self.resort_processes();
            }
            KeyCode::Char('a') => {
                self.procs.advanced = !self.procs.advanced;
                if !self.procs.advanced && !NORMAL_SORT_COLUMNS.contains(&self.procs.sort_col) {
                    self.procs.sort_col = ProcessSortColumn::Mem;
                }
                self.resort_processes();
            }
            KeyCode::Char('c') => self.signal_selected_row(Signal::TERM, false),
            KeyCode::Char('t') => self.signal_selected_row(Signal::KILL, true),
            KeyCode::Up | KeyCode::Char('k') => {
                self.move_table_selection(-1, self.visible_process_count());
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.move_table_selection(1, self.visible_process_count());
            }
            KeyCode::Char('g') | KeyCode::Home => self.procs.table_state.select(Some(0)),
            KeyCode::Char('G') | KeyCode::End => {
                let num = self.visible_process_count();
                if num > 0 {
                    self.procs.table_state.select(Some(num - 1));
                }
            }
            KeyCode::PageUp => self.move_table_selection(-PAGE_STEP, self.visible_process_count()),
            KeyCode::PageDown => self.move_table_selection(PAGE_STEP, self.visible_process_count()),
            _ => self.handle_pause_key(key),
        }
    }
    /// Enter on the process table: toggles group expansion or opens the detail view.
    fn activate_selected_row(&mut self) {
        if let Some(sel) = self.procs.table_state.selected() {
            self.activate_row_at(sel);
        }
    }

    /// Toggles group expansion or opens the detail view for row `idx`.
    pub(super) fn activate_row_at(&mut self, idx: usize) {
        let target_info = {
            let active: &[ProcessInfo] = self.active_processes();
            let mut grouped = Vec::new();
            let rows = self.visible_process_rows(active, &mut grouped);
            rows.get(idx)
                .map(|t| (t.is_group_header, t.group_name.clone(), t.pid))
        };
        let Some((is_header, group_name, pid)) = target_info else {
            return;
        };
        if is_header {
            if let Some(grp) = group_name
                && !self.procs.expanded_groups.remove(&grp)
            {
                self.procs.expanded_groups.insert(grp);
            }
        } else {
            self.procs.selected_detail = Some(pid);
        }
    }

    /// Builds kill targets for the selected row, expanding grouped PIDs into individual entries.
    fn signal_selected_row(&mut self, signal: Signal, is_kill: bool) {
        let payload = {
            let Some(sel) = self.procs.table_state.selected() else {
                return;
            };
            let active: &[ProcessInfo] = self.active_processes();
            let mut grouped = Vec::new();
            let rows = self.visible_process_rows(active, &mut grouped);
            let Some(target) = rows.get(sel).copied() else {
                return;
            };

            let proc_map: HashMap<u32, &ProcessInfo> = active.iter().map(|p| (p.pid, p)).collect();
            let targets: Vec<ProcessTarget> = if target.grouped_pids.is_empty() {
                vec![ProcessTarget {
                    pid: target.pid,
                    comm: target.comm.clone(),
                    name: target.name.clone(),
                }]
            } else {
                target
                    .grouped_pids
                    .iter()
                    .map(|&pid| match proc_map.get(&pid) {
                        Some(p) => ProcessTarget {
                            pid,
                            comm: p.comm.clone(),
                            name: p.name.clone(),
                        },
                        None => ProcessTarget {
                            pid,
                            comm: target.comm.clone(),
                            name: target.name.clone(),
                        },
                    })
                    .collect()
            };
            let display_name = target.name.clone();
            Some((targets, display_name))
        };
        if let Some((targets, display_name)) = payload {
            self.request_signal(targets, signal, is_kill, &display_name);
        }
    }

    /// Queues a single-process signal from the detail view.
    fn signal_detail_target(&mut self, detail_pid: u32, signal: Signal, is_kill: bool) {
        let target = self
            .active_processes()
            .iter()
            .find(|p| p.pid == detail_pid)
            .map(|p| ProcessTarget {
                pid: p.pid,
                comm: p.comm.clone(),
                name: p.name.clone(),
            });
        if let Some(target) = target {
            let display_name = target.name.clone();
            self.queue_signal(vec![target], signal, is_kill, &display_name);
            self.procs.selected_detail = None;
        }
    }

    /// Disk tab: box selection, category tabs, hidden-file toggle, tree navigation.
    fn handle_disks_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('a' | 'A') => self.disks.box_tab = 0,
            KeyCode::Char('s' | 'S') => self.disks.box_tab = 1,
            KeyCode::Char('d' | 'D') => self.disks.box_tab = 2,
            KeyCode::Char('f' | 'F') => self.disks.box_tab = 3,
            KeyCode::Left | KeyCode::Char('h') => self.cycle_storage_category(-1),
            KeyCode::Right | KeyCode::Char('l') => self.cycle_storage_category(1),
            KeyCode::Char('.') => {
                self.disks.show_hidden = !self.disks.show_hidden;
                self.disks.selected_item = 0;
                self.disks.scroll_offset = 0;
            }
            KeyCode::Up | KeyCode::Char('k') => self.move_storage_selection(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_storage_selection(1),
            KeyCode::PageUp => self.move_storage_selection(-STORAGE_PAGE_STEP),
            KeyCode::PageDown => self.move_storage_selection(STORAGE_PAGE_STEP),
            KeyCode::Home | KeyCode::Char('g') => {
                self.disks.selected_item = 0;
                self.disks.scroll_offset = 0;
            }
            KeyCode::End | KeyCode::Char('G') => {
                let num_items = self.visible_storage_rows();
                if num_items > 0 {
                    self.disks.selected_item = num_items - 1;
                    let max_scroll = num_items.saturating_sub(self.storage_visible_rows());
                    self.disks.scroll_offset = max_scroll;
                }
            }
            KeyCode::Enter | KeyCode::Char('r' | 'R') => self.activate_storage_selection(),
            _ => self.handle_pause_key(key),
        }
    }

    /// Cycles storage categories by `direction`, resetting item selection.
    fn cycle_storage_category(&mut self, direction: isize) {
        let len = self.storage_categories.len();
        if len == 0 {
            return;
        }
        let cur = self.disks.sub_tab as isize;
        self.disks.sub_tab = (cur + direction).rem_euclid(len as isize) as usize;
        self.disks.selected_item = 0;
        self.disks.scroll_offset = 0;
    }

    /// Rows visible in the storage tree box given the current layout.
    pub(super) fn storage_visible_rows(&self) -> usize {
        let box_h = if self.disks_is_tabbed() {
            self.table_area.height.saturating_sub(3)
        } else {
            self.table_area
                .height
                .saturating_sub(self.table_area.height / 2)
        };
        (box_h.saturating_sub(4) as usize).max(1)
    }

    /// Whether the disk tab currently renders boxes as stacked full-width tabs.
    pub(super) fn disks_is_tabbed(&self) -> bool {
        let snap = self.active_snapshot();
        ui::is_disks_overflow(self.table_area, &snap.disk_io, &snap.disk_mounts[..])
    }

    /// Moves the storage-tree cursor by `delta` items, keeping it in view.
    ///
    /// When no selectable items exist the empty pane's scroll offset is adjusted
    /// directly (clamped to zero), mirroring the list behaviour.
    fn move_storage_selection(&mut self, delta: isize) {
        let num_items = self.visible_storage_rows();
        if num_items > 0 {
            let sel = self.disks.selected_item as isize;
            let next = (sel + delta).clamp(0, num_items.saturating_sub(1) as isize) as usize;
            self.disks.selected_item = next;
            if next < self.disks.scroll_offset {
                self.disks.scroll_offset = next;
            } else {
                let visible = self.storage_visible_rows();
                if next >= self.disks.scroll_offset + visible {
                    self.disks.scroll_offset = next.saturating_sub(visible.saturating_sub(1));
                }
            }
        } else if delta < 0 {
            self.disks.scroll_offset = self
                .disks
                .scroll_offset
                .saturating_sub(delta.unsigned_abs());
        } else {
            let max_scroll = num_items.saturating_sub(self.storage_visible_rows());
            self.disks.scroll_offset =
                (self.disks.scroll_offset + delta.unsigned_abs()).min(max_scroll);
        }
    }

    /// Enter on the storage tree: starts a root scan when empty, else expands/collapses the selection.
    fn activate_storage_selection(&mut self) {
        if self.disks_is_tabbed() && self.disks.box_tab != 2 {
            return;
        }
        let cat_idx = self
            .disks
            .sub_tab
            .min(self.storage_categories.len().saturating_sub(1));
        let is_all = self
            .storage_categories
            .get(cat_idx)
            .map(|c| c.name == "All")
            .unwrap_or(false);
        if !is_all {
            return;
        }

        // Invariant: the "All" category always exists once checked above.
        let cat = &self.storage_categories[cat_idx];
        if cat.items.is_empty() {
            self.ensure_root_scan_started();
            return;
        }

        let actual_idx = {
            let indices = self.visible_storage_indices(cat);
            indices.get(self.disks.selected_item).copied()
        };
        if let Some(actual_idx) = actual_idx {
            self.toggle_storage_expansion(actual_idx);
        }
    }
}
