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
