/// Returns true if the ViGEmBus driver is installed and reachable.
pub fn vigem_available() -> bool {
    #[cfg(windows)]
    {
        vigem_client::Client::connect().is_ok()
    }
    #[cfg(not(windows))]
    {
        false
    }
}
