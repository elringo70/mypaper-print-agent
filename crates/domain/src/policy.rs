/// Which installed printer serves each role. More than two printers can be
/// installed, but only two are ever used for printing: one mono, one color.
/// The physical printer must already be configured for that ink on the PC —
/// the agent only routes jobs, it cannot verify mono/color mode.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PrinterRoles {
    /// Printer that prints black and white (the default role).
    #[serde(default)]
    pub bw: Option<String>,
    /// Printer that prints in color.
    #[serde(default)]
    pub color: Option<String>,
}

impl PrinterRoles {
    pub fn for_mode(&self, mode: crate::coverage::PrintMode) -> Option<&str> {
        match mode {
            crate::coverage::PrintMode::Bw => self.bw.as_deref(),
            crate::coverage::PrintMode::Color => self.color.as_deref(),
        }
    }

    /// Role a printer is assigned to, if any.
    pub fn role_of(&self, printer: &str) -> Option<crate::coverage::PrintMode> {
        if self.bw.as_deref() == Some(printer) {
            Some(crate::coverage::PrintMode::Bw)
        } else if self.color.as_deref() == Some(printer) {
            Some(crate::coverage::PrintMode::Color)
        } else {
            None
        }
    }
}

/// Which printers the agent intercepts. `Denylist` (default) watches every
/// printer except `ignored`; `Allowlist` watches only `watched`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PolicyMode {
    Allowlist,
    #[default]
    Denylist,
}

#[derive(Debug, Clone, Default)]
pub struct PrinterPolicy {
    pub mode: PolicyMode,
    pub watched: Vec<String>,
    pub ignored: Vec<String>,
}

impl PrinterPolicy {
    pub fn watches(&self, name: &str) -> bool {
        match self.mode {
            PolicyMode::Allowlist => self.watched.iter().any(|w| w == name),
            PolicyMode::Denylist => !self.ignored.iter().any(|w| w == name),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn denylist_watches_everything_except_ignored() {
        let policy = PrinterPolicy {
            mode: PolicyMode::Denylist,
            ignored: vec!["PDF Printer".into()],
            ..Default::default()
        };
        assert!(policy.watches("HP LaserJet"));
        assert!(!policy.watches("PDF Printer"));
    }

    #[test]
    fn allowlist_watches_only_listed() {
        let policy = PrinterPolicy {
            mode: PolicyMode::Allowlist,
            watched: vec!["Counter Printer".into()],
            ..Default::default()
        };
        assert!(policy.watches("Counter Printer"));
        assert!(!policy.watches("HP LaserJet"));
    }
}
