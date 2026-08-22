//! Mouse event handling: top-bar navigation, scrolling, clicks, and modals.

use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};

use crate::ui;

use super::{App, SCROLL_STEP, Tab};

impl App {
    /// Processes one mouse event.
    pub(super) fn handle_mouse(&mut self, mouse: MouseEvent) {
        let (mx, my) = (mouse.column, mouse.row);

        // Blocking modal dialogs capture all clicks until dismissed.
        if self.error_popup.is_some() {
            if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
                self.error_popup = None;
            }
            return;
        }
        if self.kill_confirmation.is_some() {
            if let (MouseEventKind::Down(MouseButton::Left), Some((yes, no))) =
                (mouse.kind, self.modal_btn_rects)
            {
                let hit = |r: ratatui::layout::Rect| {
                    mx >= r.x && mx < r.right() && my >= r.y && my < r.bottom()
                };
                if hit(yes) {
                    self.execute_kill_confirmation();
                } else if hit(no) {
                    self.kill_confirmation = None;
                }
            }
            return;
        }

        // Clicking a title in the top bar switches tabs directly.
        if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left))
            && my == self.tabs_area.y + 1
            && mx >= self.tabs_area.x
            && mx < self.tabs_area.right()
        {
            let mut tab_x = self.tabs_area.x + 1;
            for (idx, title) in ui::TAB_TITLES.iter().enumerate() {
                let tab_w = 1 + title.chars().count() as u16 + 1;
                if mx >= tab_x && mx < tab_x + tab_w {
                    self.select_tab(Tab::from_index(idx));
                    break;
                }
                tab_x += tab_w + 1;
            }
            return;
        }

        match self.current_tab {
            Tab::General => self.handle_general_mouse(mouse),
            Tab::Processes => self.handle_processes_mouse(mouse),
            Tab::Gpu => self.handle_gpu_mouse(mouse),
            Tab::Network => self.handle_network_mouse(mouse),
            Tab::Disks => self.handle_disks_mouse(mouse),
            Tab::CpuRam => {}
        }
    }

    /// General dashboard: sub-tab wheel cycling, sub-tab strip clicks, copy button.
    fn handle_general_mouse(&mut self, mouse: MouseEvent) {
        match mouse.kind {
            MouseEventKind::ScrollDown => {
                self.general_sub_tab = (self.general_sub_tab + 1) % 4;
                self.needs_redraw = true;
            }
            MouseEventKind::ScrollUp => {
                self.general_sub_tab = if self.general_sub_tab == 0 {
                    3
                } else {
                    self.general_sub_tab - 1
                };
                self.needs_redraw = true;
            }
            MouseEventKind::Down(MouseButton::Left) => {
                let area = self.table_area;

                // Compact-mode layout mirrors the General tab renderer's heuristics.
                let num_cores = self.live.core_usages.len();
                let cpu_min_rows = 1 + num_cores.div_ceil(4) as u16 + 2;
                let min_height_needed = 14 + 4 + (area.height * 30 / 100).max(6) + cpu_min_rows;
                let is_compact = area.height < min_height_needed || area.width < 80;

                if is_compact && mouse.row == area.y + 1 {
                    const SUB_TABS: [&str; 4] = ["Overview", "Hardware", "High", "Partitions"];
                    let mut tab_x = area.x + 2;
                    for (i, title) in SUB_TABS.iter().enumerate() {
                        let w = title.chars().count() as u16 + 2;
                        if mouse.column >= tab_x && mouse.column < tab_x + w {
                            self.general_sub_tab = i;
                            self.needs_redraw = true;
                            break;
                        }
                        tab_x += w + 1;
                    }
                } else {
                    let top_box_w = if is_compact && area.width < 70 {
                        area.width
                    } else {
                        area.width / 2
                    };
                    let top_box_right = area.x + top_box_w;
                    let top_box_top = if is_compact { area.y + 3 } else { area.y };
                    let over_copy_button = mouse.column >= top_box_right.saturating_sub(18)
                        && mouse.column <= top_box_right
                        && mouse.row >= top_box_top
                        && mouse.row <= top_box_top + 1;
                    if over_copy_button {
                        self.copy_system_overview();
                    }
                }
            }
            _ => {}
        }
    }

    /// Process table: row selection, group activation, sort-by-header clicks.
    fn handle_processes_mouse(&mut self, mouse: MouseEvent) {
        let num_rows = self.visible_process_count();
        match mouse.kind {
            MouseEventKind::ScrollDown => {
                self.move_table_selection(SCROLL_STEP as isize, num_rows);
                self.needs_redraw = true;
            }
            MouseEventKind::ScrollUp => {
                self.move_table_selection(-(SCROLL_STEP as isize), num_rows);
                self.needs_redraw = true;
            }
            MouseEventKind::Down(MouseButton::Left) => {
                let area = self.table_area;
                let header_y = area.y + 1;

                if mouse.row == header_y {
                    self.handle_header_click(mouse.column);
                } else if mouse.row > header_y + 1 && mouse.row < area.bottom() {
                    let clicked_row = (mouse.row - (header_y + 2)) as usize;
                    let proc_idx = self.procs.table_state.offset() + clicked_row;
                    if proc_idx < num_rows {
                        if self.procs.table_state.selected() == Some(proc_idx) {
                            self.activate_row_at(proc_idx);
                        } else {
                            self.procs.table_state.select(Some(proc_idx));
                        }
                        self.needs_redraw = true;
                    }
                }
            }
            _ => {}
        }
    }

    /// Clicks on the process table header toggle or change the sort column.
    fn handle_header_click(&mut self, mx: u16) {
        let area = self.table_area;
        let show_pid = area.width >= ui::PID_COLUMN_MIN_WIDTH;
        let columns = ui::process_columns(self.procs.advanced, show_pid);

        // The Name column flexes to absorb remaining width (minimum 10 cells).
        let fixed_total: u16 = columns.iter().map(|c| c.width).sum();
        let name_w = area.width.saturating_sub(fixed_total).max(10);

        let mut current_x = area.x + 1;
        for col in columns {
            let w = if col.width == 0 { name_w } else { col.width };
            if mx >= current_x && mx < current_x + w {
                if self.procs.sort_col == col.sort {
                    self.procs.ascending = !self.procs.ascending;
                } else {
                    self.procs.sort_col = col.sort;
                }
                self.resort_processes();
                self.needs_redraw = true;
                return;
            }
            current_x += w;
        }
    }

    /// GPU dashboard: wheel/click switching between utilization and VRAM graphs.
    fn handle_gpu_mouse(&mut self, mouse: MouseEvent) {
        let is_tabbed = ui::is_gpu_overflow(self.table_area, &self.active_snapshot().gpu_metrics);
        match mouse.kind {
            MouseEventKind::ScrollDown => {
                if is_tabbed {
                    self.gpu_sub_tab = (self.gpu_sub_tab + 1) % 2;
                    self.needs_redraw = true;
                }
            }
            MouseEventKind::ScrollUp => {
                if is_tabbed {
                    self.gpu_sub_tab = if self.gpu_sub_tab == 0 {
                        1
                    } else {
                        self.gpu_sub_tab - 1
                    };
                    self.needs_redraw = true;
                }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                let area = self.table_area;
                let top_w = (area.width * 60) / 100;
                if is_tabbed
                    && mouse.row == area.y + 1
                    && mouse.column >= area.x
                    && mouse.column < area.x + top_w
                {
                    const GPU_TABS: [&str; 2] = ["GPU Utilization", "VRAM History"];
                    let mut tab_x = area.x + 2;
                    for (i, title) in GPU_TABS.iter().enumerate() {
                        let w = title.chars().count() as u16 + 2;
                        if mouse.column >= tab_x && mouse.column < tab_x + w {
                            self.gpu_sub_tab = i;
                            self.needs_redraw = true;
                            break;
                        }
                        tab_x += w + 1;
                    }
                }
            }
            _ => {}
        }
    }

    /// Network dashboard: connections-list wheel scrolling.
    fn handle_network_mouse(&mut self, mouse: MouseEvent) {
        let num_conns = self
            .active_snapshot()
            .net_connections
            .as_ref()
            .map(|c| c.len())
            .unwrap_or(0);
        let visible_rows = (self.table_area.height * 55 / 100).saturating_sub(3) as usize;
        let max_scroll = num_conns.saturating_sub(visible_rows);
        match mouse.kind {
            MouseEventKind::ScrollDown => {
                self.net_scroll_offset = (self.net_scroll_offset + SCROLL_STEP).min(max_scroll);
                self.needs_redraw = true;
            }
            MouseEventKind::ScrollUp => {
                self.net_scroll_offset = self.net_scroll_offset.saturating_sub(SCROLL_STEP);
                self.needs_redraw = true;
            }
            _ => {}
        }
    }

    /// Disk dashboard: box tabs, storage category tabs, tree row activation, wheel scrolling.
    fn handle_disks_mouse(&mut self, mouse: MouseEvent) {
        let is_tabbed = self.disks_is_tabbed();
        let (tab_row_y, storage_w) = if is_tabbed {
            (self.table_area.y + 4, self.table_area.width)
        } else {
            (
                self.table_area.y + self.table_area.height / 2 + 1,
                (self.table_area.width * 70) / 100,
            )
        };

        match mouse.kind {
            MouseEventKind::ScrollDown => {
                // The wheel scrolls the raw item list; hidden items still occupy rows.
                let num_items = self.raw_storage_rows();
                let max_scroll = num_items.saturating_sub(self.storage_visible_rows());
                self.disks.scroll_offset = (self.disks.scroll_offset + 2).min(max_scroll);
                self.needs_redraw = true;
            }
            MouseEventKind::ScrollUp => {
                self.disks.scroll_offset = self.disks.scroll_offset.saturating_sub(2);
                self.needs_redraw = true;
            }
            MouseEventKind::Down(MouseButton::Left)
                if is_tabbed
                    && mouse.row == self.table_area.y + 1
                    && mouse.column > self.table_area.x =>
            {
                // Physical-disks card tab strip: four evenly labelled boxes.
                let click_col = mouse.column.saturating_sub(self.table_area.x + 1);
                self.disks.box_tab = if click_col < 12 {
                    0
                } else if click_col < 33 {
                    1
                } else if click_col < 47 {
                    2
                } else {
                    3
                };
                self.needs_redraw = true;
            }
            MouseEventKind::Down(MouseButton::Left)
                if (!is_tabbed || self.disks.box_tab == 2)
                    && (mouse.row == tab_row_y || mouse.row == tab_row_y + 1)
                    && mouse.column > self.table_area.x
                    && mouse.column < self.table_area.x + storage_w =>
            {
                self.handle_storage_tab_click(mouse.row, mouse.column, tab_row_y, storage_w);
            }
            MouseEventKind::Down(MouseButton::Left)
                if (!is_tabbed || self.disks.box_tab == 2)
                    && mouse.row > tab_row_y + 1
                    && mouse.column > self.table_area.x
                    && mouse.column < self.table_area.x + storage_w =>
            {
                self.activate_storage_row_at(mouse.row, tab_row_y);
            }
            _ => {}
        }
    }

    /// Resolves a click on the wrapped storage-category tab strip.
    fn handle_storage_tab_click(&mut self, my: u16, mx: u16, tab_row_y: u16, storage_w: u16) {
        let inner_w = storage_w.saturating_sub(2);
        let click_col = mx.saturating_sub(self.table_area.x + 1);

        let mut cur_row: u16 = 0;
        let mut cur_col: u16 = 0;
        for (i, cat) in self.storage_categories.iter().enumerate() {
            let tab_len = if cat.total_str.is_empty() {
                format!("[ ▶ {} ] ", cat.name).chars().count() as u16
            } else {
                format!("[ ▶ {} ({}) ] ", cat.name, cat.total_str)
                    .chars()
                    .count() as u16
            };

            // Wrap to the second row once the first is exhausted.
            if cur_row == 0 && cur_col > 0 && cur_col + tab_len > inner_w {
                cur_row = 1;
                cur_col = 0;
            }

            if my == tab_row_y + cur_row && click_col >= cur_col && click_col < cur_col + tab_len {
                self.disks.sub_tab = i;
                self.disks.selected_item = 0;
                self.disks.scroll_offset = 0;
                self.needs_redraw = true;
                return;
            }
            cur_col += tab_len;
        }
    }

    /// Activates (or selects) the storage-tree row under the cursor.
    fn activate_storage_row_at(&mut self, my: u16, tab_row_y: u16) {
        let clicked_row = (my.saturating_sub(tab_row_y + 2)) as usize;
        let view_idx = self.disks.scroll_offset + clicked_row;

        let cat_idx = self
            .disks
            .sub_tab
            .min(self.storage_categories.len().saturating_sub(1));
        let Some(cat) = self.storage_categories.get(cat_idx) else {
            return;
        };
        let visible_indices = self.visible_storage_indices(cat);

        if view_idx < visible_indices.len() {
            self.disks.selected_item = view_idx;
            let actual_idx = visible_indices[view_idx];
            if cat.name == "All" {
                self.toggle_storage_expansion(actual_idx);
            }
            self.needs_redraw = true;
        } else if cat.name == "All" && cat.items.is_empty() {
            self.ensure_root_scan_started();
        }
    }
}
