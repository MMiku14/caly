//! Runtime observations that do not imply desired or applied state.

/// Bounded aggregate counters observed from a proxy core.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ObservedState {
    upload_bytes_per_second: u64,
    download_bytes_per_second: u64,
    active_connections: u32,
    telemetry_dropped: u64,
    /// Cumulative automatic core restarts performed by the supervisor.
    core_restart_count: u64,
    /// Current crash-loop backoff in milliseconds between restarts.
    core_restart_backoff_ms: u64,
}

impl ObservedState {
    /// Constructs an observed-state sample without self-heal counters.
    pub const fn new(upload_bps: u64, download_bps: u64, connections: u32, dropped: u64) -> Self {
        Self {
            upload_bytes_per_second: upload_bps,
            download_bytes_per_second: download_bps,
            active_connections: connections,
            telemetry_dropped: dropped,
            core_restart_count: 0,
            core_restart_backoff_ms: 0,
        }
    }

    /// Constructs a full sample including self-heal observability.
    pub const fn with_self_heal(
        upload_bps: u64,
        download_bps: u64,
        connections: u32,
        dropped: u64,
        restart_count: u64,
        restart_backoff_ms: u64,
    ) -> Self {
        Self {
            upload_bytes_per_second: upload_bps,
            download_bytes_per_second: download_bps,
            active_connections: connections,
            telemetry_dropped: dropped,
            core_restart_count: restart_count,
            core_restart_backoff_ms: restart_backoff_ms,
        }
    }

    /// Returns upload throughput in bytes per second.
    pub const fn upload_bytes_per_second(self) -> u64 {
        self.upload_bytes_per_second
    }
    /// Returns download throughput in bytes per second.
    pub const fn download_bytes_per_second(self) -> u64 {
        self.download_bytes_per_second
    }
    /// Returns the number of currently observed connections.
    pub const fn active_connections(self) -> u32 {
        self.active_connections
    }
    /// Returns cumulative telemetry loss caused by explicit bounded policies.
    pub const fn telemetry_dropped(self) -> u64 {
        self.telemetry_dropped
    }
    /// Returns cumulative automatic core restarts performed by the supervisor.
    pub const fn core_restart_count(self) -> u64 {
        self.core_restart_count
    }
    /// Returns the current crash-loop backoff in milliseconds.
    pub const fn core_restart_backoff_ms(self) -> u64 {
        self.core_restart_backoff_ms
    }
}
