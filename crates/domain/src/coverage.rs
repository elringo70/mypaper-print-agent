/// Per-page ink coverage in percent (0-100 per channel).
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Coverage {
    pub c: f32,
    pub m: f32,
    pub y: f32,
    pub k: f32,
}

impl Coverage {
    pub fn new(c: f32, m: f32, y: f32, k: f32) -> Self {
        Self { c, m, y, k }
    }

    /// Billable color coverage: the worst color channel drives the tier.
    /// A page with 80% yellow and 5% cyan bills as 80% color, never as the
    /// average. Parity with MyPaper `PrintTierService::colorCoverage`.
    pub fn color_coverage(&self) -> f32 {
        self.c.max(self.m).max(self.y)
    }
}

/// Coverage-to-product mapping synced read-only from MyPaper (`print_tiers`).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Tier {
    pub id: String,
    pub min_coverage: f32,
    pub max_coverage: f32,
    pub active: bool,
}

/// Tier resolution. Parity with MyPaper `PrintTierService::classifyCoverage`.
pub fn classify_coverage(coverage: f32, tiers: &[Tier]) -> Option<&Tier> {
    let mut sorted: Vec<&Tier> = tiers.iter().filter(|t| t.active).collect();
    sorted.sort_by(|a, b| {
        a.min_coverage
            .partial_cmp(&b.min_coverage)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    sorted
        .into_iter()
        .find(|t| coverage >= t.min_coverage && coverage <= t.max_coverage)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tiers() -> Vec<Tier> {
        vec![
            Tier {
                id: "t6".into(),
                min_coverage: 0.0,
                max_coverage: 50.0,
                active: true,
            },
            Tier {
                id: "t10".into(),
                min_coverage: 50.01,
                max_coverage: 90.0,
                active: true,
            },
            Tier {
                id: "t12".into(),
                min_coverage: 90.01,
                max_coverage: 100.0,
                active: true,
            },
        ]
    }

    #[test]
    fn worst_channel_drives_billable_coverage() {
        assert_eq!(Coverage::new(80.0, 5.0, 5.0, 90.0).color_coverage(), 80.0);
        assert_eq!(Coverage::new(10.0, 50.0, 30.0, 0.0).color_coverage(), 50.0);
        assert_eq!(Coverage::new(0.0, 0.0, 0.0, 0.0).color_coverage(), 0.0);
    }

    #[test]
    fn parity_with_mypaper_tier_vectors() {
        let tiers = tiers();
        assert_eq!(
            classify_coverage(50.0, &tiers).map(|t| t.id.as_str()),
            Some("t6")
        );
        assert_eq!(
            classify_coverage(80.0, &tiers).map(|t| t.id.as_str()),
            Some("t10")
        );
        assert_eq!(
            classify_coverage(95.0, &tiers).map(|t| t.id.as_str()),
            Some("t12")
        );
    }

    #[test]
    fn inactive_tiers_are_ignored() {
        let mut tiers = tiers();
        tiers[0].active = false;
        assert_eq!(
            classify_coverage(80.0, &tiers).map(|t| t.id.as_str()),
            Some("t10")
        );
        assert_eq!(classify_coverage(10.0, &tiers), None);
    }
}
