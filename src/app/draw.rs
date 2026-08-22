//! Frame composition: layout split, tab dispatch, and modal overlay rendering.

use std::collections::HashMap;

use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
};

use crate::{process::ProcessInfo, ui};

use super::{App, TICK_INTERVAL, Tab};

impl App {
    /// Renders one complete frame of the dashboard.
    pub(super) fn draw(&mut self, frame: &mut Frame) {
        let area = frame.area();
        if area.width < ui::MIN_TERM_WIDTH || area.height < ui::MIN_TERM_HEIGHT {
            ui::render_min_size_warning(frame, area);
            return;
        }

        let [tabs_area, body] = Layout::vertical([Constraint::Length(3), Constraint::Min(0)])
            .margin(1)
            .areas(area);
        self.tabs_area = tabs_area;
        self.table_area = body;

        let total_snaps = self.snapshots.len();
        let steps_back = total_snaps.saturating_sub(1 + self.snapshot_idx);
        let is_copied = self
            .copy_feedback_until
            .map(|t| std::time::Instant::now() < t)
            .unwrap_or(false);

        ui::render_topbar(
            frame,
            tabs_area,
            &ui::TopbarState {
                selected_tab: self.current_tab.index(),
                paused: self.paused,
                seconds_back: steps_back as u64 * TICK_INTERVAL.as_secs(),
                position: if total_snaps > 0 {
                    self.snapshot_idx + 1
                } else {
                    0
                },
                total: total_snaps,
            },
        );

        match self.current_tab {
            Tab::General => {
                ui::render_general_tab(
                    frame,
                    body,
                    self.active_snapshot(),
                    &self.cpu_model,
                    is_copied,
                    self.general_sub_tab,
                );
            }
            Tab::Processes => self.draw_processes_tab(frame, body),
            Tab::CpuRam => {
                ui::render_cpu_ram_tab(frame, body, self.active_snapshot(), &self.cpu_model);
            }
            Tab::Gpu => {
                ui::render_gpu_tab(frame, body, self.active_snapshot(), self.gpu_sub_tab);
            }
            Tab::Network => {
                ui::render_network_tab(frame, body, self.active_snapshot(), self.net_scroll_offset);
            }
            Tab::Disks => {
                ui::render_disks_tab(
                    frame,
                    body,
                    self.active_snapshot(),
                    &self.storage_categories,
                    &self.disks,
                    steps_back > 0,
                    self.is_dust_scanning,
                );
            }
        }

        // Modal overlays paint above whichever tab is active.
        if let Some(err_popup) = self.error_popup.as_ref() {
            ui::render_process_error_popup(frame, area, err_popup);
            self.modal_btn_rects = None;
        } else {
            self.modal_btn_rects = self
                .kill_confirmation
                .as_ref()
                .map(|confirm| ui::render_kill_confirmation_modal(frame, area, confirm));
        };
    }

    /// Draws either the process table or the full-screen process detail view.
    fn draw_processes_tab(&mut self, frame: &mut Frame, body: Rect) {
        match self.procs.selected_detail {
            Some(detail_pid) => self.draw_process_detail(frame, body, detail_pid),
            None => self.draw_process_table(frame, body),
        }
    }

    /// Draws the sortable process table.
    ///
    /// Snapshot data and the table cursor are borrowed through disjoint fields
    /// so the stateful widget can update its selection during rendering.
    fn draw_process_table(&mut self, frame: &mut Frame, body: Rect) {
        let (processes, total_mem_mb, num_cores) = match self.snapshots.get(self.snapshot_idx) {
            Some(snap) => (
                &snap.processes,
                snap.mem.total_mem_mb,
                snap.core_usages.len().max(1),
            ),
            None => (
                &self.live.processes,
                self.live.mem.total_mem_mb,
                self.live.core_usages.len().max(1),
            ),
        };

        let view = ui::ProcessView {
            processes,
            advanced: self.procs.advanced,
            expanded_groups: &self.procs.expanded_groups,
            query: &self.procs.query,
            searching: self.procs.searching,
            sort_col: self.procs.sort_col,
            ascending: self.procs.ascending,
            total_mem_mb,
            num_cores,
        };
        ui::render_process_tab(frame, body, &view, &mut self.procs.table_state);
    }

    /// Draws the full-screen inspection view for one process.
    fn draw_process_detail(&mut self, frame: &mut Frame, body: Rect, detail_pid: u32) {
        let snap = self.active_snapshot();
        let proc_map: HashMap<u32, &ProcessInfo> =
            snap.processes.iter().map(|p| (p.pid, p)).collect();

        let dummy_proc;
        let proc_info = if let Some(p) = proc_map.get(&detail_pid) {
            *p
        } else {
            // The process exited between capture and inspection; show a stub.
            dummy_proc = ProcessInfo {
                pid: detail_pid,
                comm: format!("PID {detail_pid}"),
                name: format!("PID {detail_pid}"),
                state: "Exited".to_string(),
                ..ProcessInfo::default()
            };
            &dummy_proc
        };

        let detail = crate::process::read_process_detail(proc_info, &proc_map);
        ui::render_process_detail(frame, body, &detail, snap);
    }
}
