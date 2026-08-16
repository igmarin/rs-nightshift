//! Host-side command and toolchain probes.

/// Host-side command and toolchain probes.
pub trait HostCommands: Send + Sync {
    /// `rustc` is on PATH.
    fn rustc_available(&self) -> bool;

    /// `mise` is on PATH. Missing mise is a warning when `rustc` exists.
    fn mise_available(&self) -> bool;

    /// Named executable is on PATH.
    fn command_on_path(&self, name: &str) -> bool;
}

/// PATH lookups via the `which` crate.
pub struct PathHost;

impl HostCommands for PathHost {
    fn rustc_available(&self) -> bool {
        which::which("rustc").is_ok()
    }

    fn mise_available(&self) -> bool {
        which::which("mise").is_ok()
    }

    fn command_on_path(&self, name: &str) -> bool {
        which::which(name).is_ok()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) struct FakeHost {
        pub rustc: bool,
        pub mise: bool,
        pub commands: Vec<&'static str>,
    }

    impl HostCommands for FakeHost {
        fn rustc_available(&self) -> bool {
            self.rustc
        }

        fn mise_available(&self) -> bool {
            self.mise
        }

        fn command_on_path(&self, name: &str) -> bool {
            self.commands.contains(&name)
        }
    }

    pub(crate) fn healthy_host() -> FakeHost {
        FakeHost {
            rustc: true,
            mise: true,
            commands: vec!["codegraph", "graphify"],
        }
    }

    #[test]
    fn path_host_probes_common_binaries() {
        assert!(PathHost.rustc_available());
        assert!(!PathHost.command_on_path("definitely-not-a-nightshift-binary"));
        let _ = PathHost.mise_available();
    }
}
