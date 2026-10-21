//! The only code that writes the window: the model, drawn, for the parts that changed.

use slint::{ModelRc, VecModel};
use yantrik_app::app::Changed;
use yantrik_app::rail::{self, RailView as _};
use yantrik_ipc_contracts::system_monitor::ProcessInfo;

use crate::model::{Model, PART_AI, PART_NOTICE, PART_PROCESSES, PART_READINGS};
use crate::{CpuCoreData, DiskData, MonitorProcessData, NetworkInterfaceData, SystemMonitorApp};

pub fn render(ui: &SystemMonitorApp, m: &Model, changed: &Changed) {
    if changed.has(PART_READINGS) {
        readings(ui, m);
    }
    if changed.has(PART_PROCESSES) {
        processes(ui, m);
    }
    if changed.has(PART_NOTICE) {
        ui.set_notice(m.notice().into());
    }
    if changed.has(PART_AI) {
        // The panel's open flag is the header's own (it opens on the press and closes on the
        // dismiss); only what is in it is drawn from here.
        ui.set_ai_is_working(m.ai.working);
        ui.set_ai_response(m.ai.response.as_str().into());
    }
    if changed.has(rail::PART) {
        ui.render_rail(&m.rail);
    }
}

fn readings(ui: &SystemMonitorApp, m: &Model) {
    let r = &m.readout;
    ui.set_cpu_measured(r.cpu_measured);
    ui.set_cpu_usage(r.cpu_usage);
    ui.set_load_avg_1(r.load_avg[0].as_str().into());
    ui.set_load_avg_5(r.load_avg[1].as_str().into());
    ui.set_load_avg_15(r.load_avg[2].as_str().into());
    let cores: Vec<CpuCoreData> = r.cores.iter().map(|c| CpuCoreData { core_id: c.id, usage: c.usage }).collect();
    ui.set_cpu_cores(ModelRc::new(VecModel::from(cores)));

    ui.set_memory_usage(r.memory_usage);
    ui.set_memory_used_text(r.memory_used.as_str().into());
    ui.set_memory_total_text(r.memory_total.as_str().into());
    ui.set_memory_available_text(r.memory_available.as_str().into());
    ui.set_memory_cached_text(r.memory_cached.as_str().into());
    ui.set_memory_buffers_text(r.memory_buffers.as_str().into());
    ui.set_swap_usage(r.swap_usage);
    ui.set_swap_used_text(r.swap_used.as_str().into());
    ui.set_swap_total_text(r.swap_total.as_str().into());

    let disks: Vec<DiskData> = r
        .disks
        .iter()
        .map(|d| DiskData {
            mount_point: d.mount_point.as_str().into(),
            filesystem: d.filesystem.as_str().into(),
            used_bytes: d.used.as_str().into(),
            total_bytes: d.total.as_str().into(),
            usage_percent: d.usage_percent,
            needs_you: d.needs_you,
        })
        .collect();
    ui.set_disks(ModelRc::new(VecModel::from(disks)));

    let nets: Vec<NetworkInterfaceData> = r
        .interfaces
        .iter()
        .map(|n| NetworkInterfaceData {
            name: n.name.as_str().into(),
            ip_address: "".into(),
            rx_bytes: n.rx_bytes.as_str().into(),
            tx_bytes: n.tx_bytes.as_str().into(),
            rx_speed: n.rx_speed.as_str().into(),
            tx_speed: n.tx_speed.as_str().into(),
        })
        .collect();
    ui.set_network_interfaces(ModelRc::new(VecModel::from(nets)));

    ui.set_uptime_text(r.uptime.as_str().into());

    // What it adds up to: the status row.
    ui.set_status_lead(m.status.lead.as_str().into());
    ui.set_status_rest(m.status.rest.as_str().into());
    ui.set_status_thresholds(m.status.thresholds.as_str().into());
    ui.set_status_needs_you(m.status.needs_you());
}

fn processes(ui: &SystemMonitorApp, m: &Model) {
    if ui.get_sort_column() != m.sort_column {
        ui.set_sort_column(m.sort_column);
    }
    // The box already holds what was typed; an intent's filter is written into it.
    if ui.get_process_search().as_str() != m.search {
        ui.set_process_search(m.search.as_str().into());
    }
    let shown = m.shown();
    let items: Vec<MonitorProcessData> = shown
        .iter()
        .map(|p| MonitorProcessData {
            pid: p.pid as i32,
            name: p.name.as_str().into(),
            cpu_percent: p.cpu_percent as f32,
            mem_percent: p.mem_percent as f32,
            status: p.state.as_str().into(),
        })
        .collect();
    ui.set_processes(ModelRc::new(VecModel::from(items)));
    // The list was just rebuilt under the selection, and the row the selection points at may
    // not have survived it. Dropping a stale pid here is what hides End and Force Kill with the
    // row they act on (#220). The selection is the list's own (a row press sets it), so it is
    // read from the window and held to the list drawn.
    ui.set_selected_process_pid(selection_after_refresh(ui.get_selected_process_pid(), &shown));
}

/// What the selection must be after the process list is refreshed.
///
/// End and Force Kill act on `selected-process-pid`, and the list under them is rebuilt every
/// poll: the selected process can end — here, or anywhere else on the machine — or a filter can
/// stop showing its row. The pid used to stay selected through all of it, so the buttons
/// remained on screen aimed at a process that was gone (#220). The selection now lives exactly
/// as long as the shown list does.
pub fn selection_after_refresh(selected: i32, procs: &[ProcessInfo]) -> i32 {
    if selected >= 0 && procs.iter().any(|p| p.pid as i32 == selected) {
        selected
    } else {
        -1
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machine::fake::process;

    fn running(pid: u32, name: &str) -> ProcessInfo {
        process(pid, name, 1.0, 1.0)
    }

    #[test]
    fn a_selection_the_refreshed_list_still_shows_survives_it() {
        let procs = vec![running(7, "pi"), running(9, "chromium")];
        assert_eq!(selection_after_refresh(7, &procs), 7);
    }

    #[test]
    fn a_selection_whose_process_is_gone_is_dropped() {
        // The person selected a row and the process ended — here or anywhere else on the
        // machine. The next poll rebuilds the list without it, and End and Force Kill must hide
        // with the row they act on instead of staying aimed at a dead pid (#220).
        let procs = vec![running(9, "chromium")];
        assert_eq!(selection_after_refresh(7, &procs), -1);
    }

    #[test]
    fn a_selection_a_filter_hides_is_dropped() {
        // The list is filtered before it is drawn, so a row the search stopped showing is gone
        // from the list the buttons belong to.
        let matching = vec![running(9, "chromium")];
        assert_eq!(selection_after_refresh(7, &matching), -1);
    }

    #[test]
    fn nothing_selected_stays_nothing_selected() {
        assert_eq!(selection_after_refresh(-1, &[]), -1);
        assert_eq!(selection_after_refresh(-1, &[running(7, "pi")]), -1);
    }
}
