//! Process & resource monitor — polls via sysinfo.
//!
//! Tracks running processes (detects start/stop), CPU, memory, disk.
//! Pure synchronous polling — no async, no D-Bus.

use crossbeam_channel::Sender;
use std::collections::HashMap;
use sysinfo::{Disks, ProcessRefreshKind, ProcessesToUpdate, System};

use crate::events::SystemEvent;

/// Whether this entry is a THREAD rather than a process.
///
/// sysinfo enumerates tasks, and on Linux every thread is a task with its own /proc entry and
/// its own `comm`. So the monitor was announcing a process start for every thread any program
/// spawned, under names like `pool-269`, `async-io`, `PerfettoTrace` and `smithay-clipboa` --
/// that last one fifteen characters, because `comm` is truncated to fifteen and the truncation
/// is the giveaway.
///
/// Downstream this went into the companion's memory as "App opened: pool-269". A sample of
/// twenty recalled memories on a machine that had been running for a few hours came back
/// nineteen-twentieths thread churn, and the database was 15 MB with two notes on the machine.
/// Nothing on this path was a bug in the filter above it -- the events should never have been
/// emitted.
fn is_thread(process: &sysinfo::Process) -> bool {
    process.thread_kind().is_some()
}

/// Main loop for the process/resource monitor thread.
/// Polls every `process_secs` for process changes, every `resource_secs` for CPU/RAM/disk.
pub fn run_process_monitor(tx: Sender<SystemEvent>, process_secs: u64, resource_secs: u64) {
    let mut sys = System::new();
    let mut known_pids: HashMap<u32, String> = HashMap::new();

    // Initial snapshot — record all currently running processes
    // We only consume PID, name, thread kind and CPU, never command lines,
    // environments, executable links, per-process memory or disk I/O.
    let process_fields = ProcessRefreshKind::nothing().with_cpu();
    sys.refresh_processes_specifics(ProcessesToUpdate::All, true, process_fields);
    sys.refresh_cpu_usage();
    for (pid, process) in sys.processes() {
        if is_thread(process) {
            continue;
        }
        known_pids.insert(pid.as_u32(), process.name().to_string_lossy().to_string());
    }

    let mut tick: u64 = 0;
    let sleep_secs = gcd(process_secs, resource_secs);

    loop {
        std::thread::sleep(std::time::Duration::from_secs(sleep_secs));
        tick += sleep_secs;

        // Process diff
        if tick % process_secs == 0 {
            sys.refresh_processes_specifics(ProcessesToUpdate::All, true, process_fields);

            let mut current_pids: HashMap<u32, String> = HashMap::new();
            for (pid, process) in sys.processes() {
                if is_thread(process) {
                    continue;
                }
                let pid_u32 = pid.as_u32();
                let name = process.name().to_string_lossy().to_string();
                current_pids.insert(pid_u32, name.clone());

                // New process?
                if !known_pids.contains_key(&pid_u32) {
                    let cpu = process.cpu_usage();
                    let _ = tx.send(SystemEvent::ProcessStarted {
                        name,
                        pid: pid_u32,
                        cpu_percent: cpu,
                    });
                }
            }

            // Stopped processes
            for (pid, name) in &known_pids {
                if !current_pids.contains_key(pid) {
                    let _ = tx.send(SystemEvent::ProcessStopped {
                        name: name.clone(),
                        pid: *pid,
                        exit_code: None,
                    });
                }
            }

            known_pids = current_pids;
        }

        // Resource pressure
        if tick % resource_secs == 0 {
            sys.refresh_cpu_usage();
            sys.refresh_memory();

            // CPU
            let cpu_usage = sys.global_cpu_usage();
            let _ = tx.send(SystemEvent::CpuPressure {
                usage_percent: cpu_usage,
            });

            // Memory
            let total = sys.total_memory();
            let available = sys.available_memory();
            let free = sys.free_memory();
            let used = total.saturating_sub(available);
            // cached/buffers = available - free (pages reclaimable by kernel)
            let cached = available.saturating_sub(free);
            let _ = tx.send(SystemEvent::MemoryPressure {
                used_bytes: used,
                total_bytes: total,
                cached_bytes: cached,
                free_bytes: free,
                swap_used_bytes: sys.used_swap(),
                swap_total_bytes: sys.total_swap(),
            });

            // Disk (root mount)
            let disks = Disks::new_with_refreshed_list();
            for disk in &disks {
                let mount = disk.mount_point().to_string_lossy().to_string();
                if mount == "/" {
                    let _ = tx.send(SystemEvent::DiskPressure {
                        mount_point: mount,
                        available_bytes: disk.available_space(),
                        total_bytes: disk.total_space(),
                    });
                    break;
                }
            }
        }
    }
}

/// Greatest common divisor (for computing the sleep interval).
fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 {
        a
    } else {
        gcd(b, a % b)
    }
}
