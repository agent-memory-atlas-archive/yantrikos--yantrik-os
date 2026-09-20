#[path = "../../crates/yantrik-ui/src/render_backend.rs"]
mod render_backend;
#[path = "../../crates/yantrik-os/src/events.rs"]
mod events;
#[path = "../../crates/yantrik-os/src/processes.rs"]
mod processes;

#[cfg(test)]
mod integration {
    use super::*;
    use std::{process::{Child,Command},time::{Duration,Instant}};
    struct ChildGuard(Child);
    impl Drop for ChildGuard { fn drop(&mut self) { let _=self.0.kill();let _=self.0.wait(); } }
    #[test]
    fn lean_monitor_still_reports_process_lifecycle_and_resource_readings() {
        let (tx,rx)=crossbeam_channel::unbounded();
        std::thread::spawn(move||processes::run_process_monitor(tx,1,1));
        // Wait for initialization before creating the observed process.
        let deadline=Instant::now()+Duration::from_secs(10);
        loop {
            assert!(Instant::now()<deadline,"Monitor did not initialize");
            if let Ok(events::SystemEvent::MemoryPressure{total_bytes,..})=rx.recv_timeout(Duration::from_millis(200)){assert!(total_bytes>0);break;}
        }
        let mut child=ChildGuard(Command::new("sleep").arg("20").spawn().unwrap());let pid=child.0.id();
        let deadline=Instant::now()+Duration::from_secs(10);let mut started=false;let mut cpu=false;
        while Instant::now()<deadline && !(started&&cpu) {
            match rx.recv_timeout(Duration::from_millis(200)) {
                Ok(events::SystemEvent::ProcessStarted{pid:p,name,..}) if p==pid=>{assert_eq!(name,"sleep");started=true;},
                Ok(events::SystemEvent::CpuPressure{usage_percent})=>{assert!(usage_percent.is_finite() && (0.0..=100.0).contains(&usage_percent));cpu=true;},
                _=>{}
            }
        }
        assert!(started&&cpu,"Process start or CPU samples missing");child.0.kill().unwrap();child.0.wait().unwrap();
        let deadline=Instant::now()+Duration::from_secs(10);
        loop {
            assert!(Instant::now()<deadline,"Process stop was not reported");
            if let Ok(events::SystemEvent::ProcessStopped{pid:p,..})=rx.recv_timeout(Duration::from_millis(200)){if p==pid{break;}}
        }
    }
}
