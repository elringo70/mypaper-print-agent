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

/// Which ink a tier bills. Each workspace keeps two independent rule sets
/// (CyberPlanet model): `Bw` classifies on black coverage (K), `Color` on the
/// worst color channel. On a color printer MyPaper evaluates both sets and the
/// more expensive tier wins; on a mono printer only `Bw` applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum PrintMode {
    #[default]
    Bw,
    Color,
}

/// How much ink a level bills for. Mirrors MyPaper `print_metric`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum PrintMetric {
    /// Black channel only (toner control on a mono printer).
    #[default]
    KOnly,
    /// Total ink printed, capped at 100%.
    SumCmyk,
    /// Worst color channel.
    MaxChannel,
}

/// Metric in use per mode, as configured in MyPaper (`print_settings`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub struct MetricsByMode {
    #[serde(default)]
    pub bw: PrintMetric,
    #[serde(default = "default_color_metric")]
    pub color: PrintMetric,
}

fn default_color_metric() -> PrintMetric {
    PrintMetric::SumCmyk
}

impl Default for MetricsByMode {
    fn default() -> Self {
        Self {
            bw: PrintMetric::KOnly,
            color: PrintMetric::SumCmyk,
        }
    }
}

impl MetricsByMode {
    pub fn for_mode(&self, mode: PrintMode) -> PrintMetric {
        match mode {
            PrintMode::Bw => self.bw,
            PrintMode::Color => self.color,
        }
    }
}

/// How much ink a page is billed for, per metric. Parity with MyPaper
/// `PrintTierService::metricCoverage` (SUM_CMYK caps at 100%: a full CMYK photo
/// would otherwise read ~250-300% and no tabulador range covers it).
pub fn metric_coverage(metric: PrintMetric, coverage: &Coverage) -> f32 {
    match metric {
        PrintMetric::KOnly => coverage.k,
        PrintMetric::MaxChannel => coverage.color_coverage(),
        PrintMetric::SumCmyk => (coverage.c + coverage.m + coverage.y + coverage.k).min(100.0),
    }
}

/// Whether the page carries any color ink at all. The COLOR rule set only
/// applies then: under SUM_CMYK a black-only page also has a non-zero total.
pub fn has_color_ink(coverage: &Coverage) -> bool {
    coverage.c + coverage.m + coverage.y > 0.0
}

/// Coverage-to-product mapping synced read-only from MyPaper (`print_tiers`).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Tier {
    pub id: String,
    pub mode: PrintMode,
    pub min_coverage: f32,
    pub max_coverage: f32,
    pub active: bool,
}

/// Tier resolution inside ONE mode. Parity with MyPaper
/// `PrintTierService::classifyCoverage`; tiers of the other mode never match.
pub fn classify_coverage(coverage: f32, tiers: &[Tier], mode: PrintMode) -> Option<&Tier> {
    let mut sorted: Vec<&Tier> = tiers
        .iter()
        .filter(|t| t.active && t.mode == mode)
        .collect();
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

    fn tier(id: &str, min: f32, max: f32) -> Tier {
        Tier {
            id: id.into(),
            mode: PrintMode::Color,
            min_coverage: min,
            max_coverage: max,
            active: true,
        }
    }

    fn tiers() -> Vec<Tier> {
        vec![
            tier("t6", 0.0, 50.0),
            tier("t10", 50.01, 90.0),
            tier("t12", 90.01, 100.0),
        ]
    }

    #[test]
    fn worst_channel_drives_billable_coverage() {
        assert_eq!(Coverage::new(80.0, 5.0, 5.0, 90.0).color_coverage(), 80.0);
        assert_eq!(Coverage::new(10.0, 50.0, 30.0, 0.0).color_coverage(), 50.0);
        assert_eq!(Coverage::new(0.0, 0.0, 0.0, 0.0).color_coverage(), 0.0);
    }

    #[test]
    fn metric_coverage_matches_mypaper() {
        let page = Coverage::new(30.0, 25.0, 28.0, 15.0);
        assert_eq!(metric_coverage(PrintMetric::KOnly, &page), 15.0);
        assert_eq!(metric_coverage(PrintMetric::MaxChannel, &page), 30.0);
        assert_eq!(metric_coverage(PrintMetric::SumCmyk, &page), 98.0);
        // A full-bleed CMYK photo reads ~250-300%: capped at 100%.
        let photo = Coverage::new(90.0, 80.0, 70.0, 60.0);
        assert_eq!(metric_coverage(PrintMetric::SumCmyk, &photo), 100.0);
    }

    #[test]
    fn color_rule_set_needs_color_ink() {
        assert!(!has_color_ink(&Coverage::new(0.0, 0.0, 0.0, 62.0)));
        assert!(has_color_ink(&Coverage::new(0.24, 0.0, 0.0, 2.54)));
    }

    #[test]
    fn metrics_default_per_mode() {
        let metrics = MetricsByMode::default();
        assert_eq!(metrics.for_mode(PrintMode::Bw), PrintMetric::KOnly);
        assert_eq!(metrics.for_mode(PrintMode::Color), PrintMetric::SumCmyk);
    }

    #[test]
    fn parity_with_mypaper_tier_vectors() {
        let tiers = tiers();
        assert_eq!(
            classify_coverage(50.0, &tiers, PrintMode::Color).map(|t| t.id.as_str()),
            Some("t6")
        );
        assert_eq!(
            classify_coverage(80.0, &tiers, PrintMode::Color).map(|t| t.id.as_str()),
            Some("t10")
        );
        assert_eq!(
            classify_coverage(95.0, &tiers, PrintMode::Color).map(|t| t.id.as_str()),
            Some("t12")
        );
    }

    #[test]
    fn inactive_tiers_are_ignored() {
        let mut tiers = tiers();
        tiers[0].active = false;
        assert_eq!(
            classify_coverage(80.0, &tiers, PrintMode::Color).map(|t| t.id.as_str()),
            Some("t10")
        );
        assert_eq!(classify_coverage(10.0, &tiers, PrintMode::Color), None);
    }

    #[test]
    fn each_rule_set_only_sees_its_own_tiers() {
        let tiers = vec![
            Tier {
                id: "bw-100".into(),
                mode: PrintMode::Bw,
                min_coverage: 0.0,
                max_coverage: 100.0,
                active: true,
            },
            tier("color-100", 0.0, 100.0),
        ];
        assert_eq!(
            classify_coverage(40.0, &tiers, PrintMode::Bw).map(|t| t.id.as_str()),
            Some("bw-100")
        );
        assert_eq!(
            classify_coverage(40.0, &tiers, PrintMode::Color).map(|t| t.id.as_str()),
            Some("color-100")
        );
        assert_eq!(classify_coverage(40.0, &tiers[..1], PrintMode::Color), None);
    }
}
