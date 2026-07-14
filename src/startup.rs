use crate::error::SyncError;

pub struct StartupRegistry;
impl StartupRegistry {
    pub fn is_registered() -> Result<bool, SyncError> {
        Err(SyncError::Config("stub".to_string()))
    }
    pub fn register() -> Result<(), SyncError> {
        Err(SyncError::Config("stub".to_string()))
    }
    pub fn unregister() -> Result<(), SyncError> {
        Err(SyncError::Config("stub".to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn test_startup_registration_toggle() {
        struct StateGuard(bool);
        impl Drop for StateGuard {
            fn drop(&mut self) {
                if self.0 {
                    let _ = StartupRegistry::register();
                } else {
                    let _ = StartupRegistry::unregister();
                }
            }
        }

        let initially_registered = StartupRegistry::is_registered().unwrap_or(false);
        let _guard = StateGuard(initially_registered);

        StartupRegistry::unregister().unwrap();
        assert!(!StartupRegistry::is_registered().unwrap());

        StartupRegistry::register().unwrap();
        assert!(StartupRegistry::is_registered().unwrap());

        StartupRegistry::unregister().unwrap();
        assert!(!StartupRegistry::is_registered().unwrap());
    }
}
