//! Linux desktop system-proxy backend (GNOME / KDE Plasma / niri).

use caly_domain::{BoundedText, PlatformEffectView};
use caly_platform::desktop::{capture_proxy_state, detect_desktop_mode, DesktopProxyMode};
use caly_platform::command::{
    CommandArguments, CommandRequest, CommandResult, CommandRunner, LinuxCommandRunner,
};
use caly_ports::{ActorFailure, PlatformCommandBackend};

/// Linux desktop system-proxy backend using shell-free bounded commands.
pub struct LinuxSystemProxyBackend {
    runner: LinuxCommandRunner,
    host: String,
    port: u16,
    mode: DesktopProxyMode,
}

impl LinuxSystemProxyBackend {
    /// Detects the desktop and prepares the matching backend.
    pub fn new(host: String, port: u16) -> Result<Self, ActorFailure> {
        if host.is_empty() || port == 0 {
            return Err(crate::failure(
                "system proxy endpoint is invalid",
                "configure a non-empty host and port",
            ));
        }
        Ok(Self {
            runner: LinuxCommandRunner,
            host,
            port,
            mode: detect_desktop_mode(),
        })
    }

    /// Returns the detected desktop mode (used by tests and diagnostics).
    pub const fn mode(&self) -> DesktopProxyMode {
        self.mode
    }

    /// Returns the configured proxy host (used by durable recovery records).
    pub fn host(&self) -> &str {
        self.host.as_str()
    }

    /// Returns the configured proxy port (used by durable recovery records).
    pub const fn port(&self) -> u16 {
        self.port
    }
}

impl PlatformCommandBackend for LinuxSystemProxyBackend {
    fn set_system_proxy(&mut self, enabled: bool) -> Result<PlatformEffectView, ActorFailure> {
        let result = match self.mode {
            DesktopProxyMode::Gnome => self.set_gnome(enabled),
            DesktopProxyMode::Kde => self.set_kde(enabled),
            DesktopProxyMode::Niri => self.set_niri(enabled),
            DesktopProxyMode::Unsupported => Err(crate::unsupported_failure(
                "no supported system-proxy backend for this desktop",
                "use GNOME, KDE Plasma, or niri, or configure proxy manually",
            )),
        };
        result?;
        Ok(PlatformEffectView::proxy(enabled))
    }

    fn set_system_proxy_pac(&mut self, url: &str) -> Result<PlatformEffectView, ActorFailure> {
        match self.mode {
            // GNOME has first-class PAC support (mode=auto +
            // autoconfig-url). KDE/niri keep the trait default
            // (unsupported) — `kioslaverc`/environment.d have no
            // portable PAC knob.
            DesktopProxyMode::Gnome => gnome::apply_pac(&mut self.runner, url)?,
            other => {
                return Err(crate::failure(
                    &format!("system proxy PAC mode is not supported on this desktop ({other:?})"),
                    "use GNOME for PAC mode, or `caly sysproxy on` for a manual endpoint",
                ));
            }
        }
        Ok(PlatformEffectView::proxy(true))
    }
}

impl LinuxSystemProxyBackend {
    /// GNOME: write `org.gnome.system.proxy` via gsettings.
    fn set_gnome(&mut self, enabled: bool) -> Result<(), ActorFailure> {
        gnome::apply(&mut self.runner, &self.host, self.port, enabled)
    }

    /// KDE Plasma: write `kioslaverc` via kwriteconfig and notify KIO.
    fn set_kde(&mut self, enabled: bool) -> Result<(), ActorFailure> {
        let tool = kde::kwriteconfig_tool();
        kde::apply_kde_proxy(&mut self.runner, tool, &self.host, self.port, enabled)
    }

    /// niri: write a user `environment.d` proxy file consumed by the session.
    fn set_niri(&mut self, enabled: bool) -> Result<(), ActorFailure> {
        let contents = niri::proxy_environment_contents(&self.host, self.port, enabled);
        niri::write_environment_d(&contents)
    }

    /// Captures the current desktop proxy state as `(mode, endpoint)` before
    /// engagement. Best-effort: unreadable state degrades to `("none", "")`,
    /// which restores as a disabled proxy, never blocking engagement.
    pub fn capture_original_state(&mut self) -> (String, String) {
        // P8b: read side lives in caly-platform::desktop (single source of
        // truth for both the daemon capture and the CLI's offline view).
        capture_proxy_state(self.mode)
    }

    /// Restores a previously captured desktop proxy state. `manual` with an
    /// empty endpoint degrades to disabling the proxy (no guessed endpoints).
    pub fn restore_original(
        &mut self,
        mode: &str,
        endpoint: &str,
    ) -> Result<PlatformEffectView, ActorFailure> {
        match self.mode {
            DesktopProxyMode::Gnome => gnome::restore(&mut self.runner, mode, endpoint),
            DesktopProxyMode::Kde => kde::restore(&mut self.runner, mode, endpoint),
            DesktopProxyMode::Niri => niri::restore(endpoint, mode == "manual"),
            DesktopProxyMode::Unsupported => Err(crate::unsupported_failure(
                "no supported system-proxy backend for this desktop",
                "use GNOME, KDE Plasma, or niri, or configure proxy manually",
            )),
        }?;
        Ok(PlatformEffectView::none())
    }
}

fn push_argument(
    arguments: &mut CommandArguments,
    value: &str,
    message: &str,
    action: &str,
) -> Result<(), ActorFailure> {
    let argument =
        BoundedText::new(value.to_owned()).map_err(|_| crate::failure(message, action))?;
    arguments.try_push(argument).map_err(|_| {
        crate::failure(
            "system proxy argument list is full",
            "reduce system proxy arguments",
        )
    })
}

/// Runs one desktop-backend command and requires exit code zero. Shared by
/// the GNOME and KDE backends so the failure wording cannot drift.
pub(super) fn run_ok<R: CommandRunner>(
    runner: &mut R,
    request: CommandRequest,
) -> Result<CommandResult, ActorFailure> {
    let result = runner.run_bounded(request).map_err(|_| {
        crate::failure(
            "system proxy command failed",
            "install the desktop backend or use a supported desktop",
        )
    })?;
    if result.exit_code != Some(0) {
        return Err(crate::failure(
            "system proxy command returned failure",
            "inspect desktop proxy permissions",
        ));
    }
    Ok(result)
}

/// Builds a bounded argument vector from string slices.
pub(crate) fn argv(parts: &[&[&str]]) -> Result<CommandArguments, ActorFailure> {
    let mut arguments = CommandArguments::new();
    for part in parts {
        for value in *part {
            push_argument(
                &mut arguments,
                value,
                "system proxy argument is too long",
                "shorten the system proxy command",
            )?;
        }
    }
    Ok(arguments)
}

mod durable;
mod durable_support;
mod durable_tun;
mod gnome;
mod kde;
mod niri;
mod tun;

pub use durable::{DesktopProxyControl, DurableSystemProxyBackend, SharedProxyRecoveryStore};
pub use durable_tun::{DurableTunBackend, SharedTunRecoveryStore, TunControl};
pub use tun::LinuxTunCommandBackend;

/// The durable platform backend used by the daemon composition.
pub type PlatformBackend = DurableSystemProxyBackend<LinuxSystemProxyBackend>;

#[cfg(test)]
mod tests {
    use super::*;


}
