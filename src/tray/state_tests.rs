use super::*;
use pretty_assertions::assert_eq;

#[test]
fn test_tray_state_initialization() {
    let state = TrayState::new(vec![true, false]);
    assert!(!state.source_online());
    assert!(!state.watcher_active());
    assert_eq!(state.source_connectivity(), ConnectivityState::Offline);
    assert_eq!(state.watcher_state(), WatcherState::Inactive);
    assert_eq!(
        state.dest_online,
        vec![ConnectivityState::Online, ConnectivityState::Offline]
    );
    assert_eq!(state.online_dest_count(), 1);
}

#[test]
fn test_tray_state_healthy() {
    let mut state = TrayState::new(vec![true, true]);
    state.update_watcher_status(ConnectivityState::Online, WatcherState::Active);
    assert_eq!(state.overall_status(), EngineStatus::Healthy);
    assert_eq!(
        state.tooltip_text(),
        "syncdir — Src: Online | Dests: 2/2 Online"
    );
}

#[test]
fn test_tray_state_source_offline() {
    let mut state = TrayState::new(vec![true, true]);
    state.update_watcher_status(ConnectivityState::Offline, WatcherState::Active);
    assert_eq!(state.overall_status(), EngineStatus::SourceOffline);
    assert_eq!(
        state.tooltip_text(),
        "syncdir — Src: Offline | Dests: 2/2 Online"
    );
}

#[test]
fn test_tray_state_watcher_degraded() {
    let mut state = TrayState::new(vec![true, true]);
    state.update_watcher_status(ConnectivityState::Online, WatcherState::Inactive);
    assert_eq!(state.overall_status(), EngineStatus::SourceOffline);
    assert_eq!(
        state.tooltip_text(),
        "syncdir — Src: Degraded | Dests: 2/2 Online"
    );
}

#[test]
fn test_tray_state_degraded() {
    let mut state = TrayState::new(vec![true, true, false]);
    state.update_watcher_status(ConnectivityState::Online, WatcherState::Active);
    assert_eq!(state.overall_status(), EngineStatus::Degraded);
    assert_eq!(
        state.tooltip_text(),
        "syncdir — Src: Online | Dests: 2/3 Online"
    );
}

#[test]
fn test_tray_state_destination_offline() {
    let mut state = TrayState::new(vec![false, false]);
    state.update_watcher_status(ConnectivityState::Online, WatcherState::Active);
    assert_eq!(state.overall_status(), EngineStatus::DestinationOffline);
    assert_eq!(
        state.tooltip_text(),
        "syncdir — Src: Online | Dests: 0/2 Online"
    );
}

#[test]
fn test_tray_state_both_offline() {
    let mut state = TrayState::new(vec![false, false]);
    state.update_watcher_status(ConnectivityState::Offline, WatcherState::Active);
    assert_eq!(state.overall_status(), EngineStatus::BothOffline);
    assert_eq!(
        state.tooltip_text(),
        "syncdir — Src: Offline | Dests: 0/2 Online"
    );
}

#[test]
fn test_tray_state_update_target_out_of_bounds() {
    let mut state = TrayState::new(vec![true]);
    assert!(!state.update_target_status(5, false));
    assert_eq!(state.dest_online(), &[ConnectivityState::Online]);
}

#[test]
fn test_tray_state_empty_destinations() {
    let mut state = TrayState::empty();
    state.update_watcher_status(ConnectivityState::Online, WatcherState::Active);
    assert_eq!(state.overall_status(), EngineStatus::Healthy);
    assert_eq!(
        state.tooltip_text(),
        "syncdir — Src: Online | Dests: 0/0 Online"
    );
}

#[test]
fn test_tray_state_update_watcher_status_domain_enums() {
    let mut state = TrayState::new(vec![true]);
    let changed = state.update_watcher_status(ConnectivityState::Online, WatcherState::Active);
    assert!(changed);
    let not_changed = state.update_watcher_status(ConnectivityState::Online, WatcherState::Active);
    assert!(!not_changed);
}

#[test]
fn test_tray_state_scan_notice() {
    let mut state = TrayState::new(vec![true, true]);
    state.update_watcher_status(ConnectivityState::Online, WatcherState::Active);
    assert_eq!(
        state.tooltip_text(),
        "syncdir — Src: Online | Dests: 2/2 Online"
    );
    state.set_scan_notice(Some("Partial Scan (1 skipped)".to_string()));
    assert_eq!(
        state.tooltip_text(),
        "syncdir — Src: Online | Dests: 2/2 Online | Partial Scan (1 skipped)"
    );
    assert_eq!(state.scan_notice(), Some("Partial Scan (1 skipped)"));
}

#[test]
fn test_destination_state_builder() {
    let dest = DestinationState::new("D:\\Sync", true)
        .with_resolved_unc(PathBuf::from("\\\\server\\share"));
    assert_eq!(dest.path(), Path::new("D:\\Sync"));
    assert_eq!(dest.is_online(), ConnectivityState::Online);
    assert_eq!(dest.resolved_unc(), Some(Path::new("\\\\server\\share")));
}

#[test]
fn test_engine_status_all_variants() {
    use std::collections::HashSet;

    assert_eq!(EngineStatus::ALL.len(), 5);
    assert_eq!(EngineStatus::ALL[0], EngineStatus::Healthy);
    assert_eq!(EngineStatus::ALL[1], EngineStatus::Degraded);
    assert_eq!(EngineStatus::ALL[2], EngineStatus::SourceOffline);
    assert_eq!(EngineStatus::ALL[3], EngineStatus::DestinationOffline);
    assert_eq!(EngineStatus::ALL[4], EngineStatus::BothOffline);

    let unique: HashSet<EngineStatus> = EngineStatus::ALL.into_iter().collect();
    assert_eq!(unique.len(), 5);
}

#[test]
fn test_destination_state_display_label() {
    let dest_local = DestinationState::new("D:\\SyncFolder", true);
    assert_eq!(dest_local.display_label(), "D:\\SyncFolder");

    let dest_mapped = DestinationState::new("Z:\\Backup", true)
        .with_resolved_unc(PathBuf::from("\\\\192.168.1.50\\share\\backup"));
    assert_eq!(
        dest_mapped.display_label(),
        "Z:\\Backup -> \\\\192.168.1.50\\share\\backup"
    );
}

#[test]
fn test_repaint_icon_gating_invariants() {
    let mut last_icon_status: Option<EngineStatus> = None;
    let initial_status = EngineStatus::Healthy;

    let should_repaint_first = last_icon_status != Some(initial_status);
    assert!(should_repaint_first);
    last_icon_status = Some(initial_status);

    let should_repaint_second = last_icon_status != Some(initial_status);
    assert!(!should_repaint_second);

    let degraded_status = EngineStatus::Degraded;
    let should_repaint_transition = last_icon_status != Some(degraded_status);
    assert!(should_repaint_transition);
}
