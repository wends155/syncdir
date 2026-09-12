use super::*;

#[test]
fn test_target_status_update_debug_and_clone() {
    let update = TargetStatusUpdate {
        target_index: 2,
        dest_online: ConnectivityState::Online,
    };
    let cloned = update.clone();
    assert_eq!(cloned.target_index, 2);
    assert_eq!(cloned.dest_online, ConnectivityState::Online);
    assert!(format!("{:?}", update).contains("TargetStatusUpdate"));
}

#[test]
fn test_user_event_debug() {
    let event = UserEvent::WatcherStatus {
        source_online: ConnectivityState::Online,
        watcher_active: WatcherState::Active,
    };
    let debug_str = format!("{:?}", event);
    assert!(debug_str.contains("WatcherStatus"));
    assert!(debug_str.contains("Online"));
    assert!(debug_str.contains("Active"));
}
