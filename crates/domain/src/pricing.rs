use crate::coverage::{classify_coverage, metric_coverage, Coverage, MetricsByMode, PrintMode};
use serde::{Deserialize, Serialize};

/// Billing rules beyond the tabulador itself (Fase 6, extras del modelo
/// CyberPlanet). Every rule is applied in this exact order, and the webapp
/// mirrors these functions (`PrintTierService.ts`) so the agent's local labels
/// match what MyPaper charges:
///
///   1. `intensity_threshold`: a channel below it counts as no ink.
///   2. blank / grey / tier classification (both rule sets, dearest wins).
///   3. `paper_surcharge` per printed page.
///   4. `volume_discounts` on the subtotal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PrintPricing {
    /// Percentage below which a channel counts as no ink (0 = count everything).
    #[serde(default, rename = "intensityThreshold")]
    pub intensity_threshold: f32,
    /// Grey ink (c≈m≈y) counts as color. `true` is the historical behaviour.
    #[serde(default = "default_true", rename = "greysAsColor")]
    pub greys_as_color: bool,
    /// A sheet with no ink at all is not charged (its paper surcharge still is).
    #[serde(default = "default_true", rename = "blankPageFree")]
    pub blank_page_free: bool,
    /// Added to every printed page: the paper cost.
    #[serde(default, rename = "paperSurcharge")]
    pub paper_surcharge: f32,
    /// Thresholds, not a running total: the highest `min_pages` reached wins.
    #[serde(default, rename = "volumeDiscounts")]
    pub volume_discounts: Vec<VolumeDiscount>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VolumeDiscount {
    #[serde(rename = "minPages")]
    pub min_pages: u32,
    pub percent: f32,
}

fn default_true() -> bool {
    true
}

/// Spread (in percentage points) within which c, m and y count as the same ink,
/// i.e. grey. Must match `GREY_TOLERANCE` in the webapp.
pub const GREY_TOLERANCE: f32 = 1.0;

impl Default for PrintPricing {
    fn default() -> Self {
        Self {
            intensity_threshold: 0.0,
            greys_as_color: true,
            blank_page_free: true,
            paper_surcharge: 0.0,
            volume_discounts: Vec::new(),
        }
    }
}

impl PrintPricing {
    /// Rule 1: drop ink below the threshold.
    pub fn apply_intensity(&self, page: Coverage) -> Coverage {
        if self.intensity_threshold <= 0.0 {
            return page;
        }
        let cut = |value: f32| {
            if value < self.intensity_threshold {
                0.0
            } else {
                value
            }
        };
        Coverage::new(cut(page.c), cut(page.m), cut(page.y), cut(page.k))
    }

    /// Rule 4: the discount percentage that applies to `page_count` pages.
    pub fn volume_discount_percent(&self, page_count: u32) -> f32 {
        let mut percent: f32 = 0.0;
        for step in &self.volume_discounts {
            if page_count >= step.min_pages && step.percent > percent {
                percent = step.percent;
            }
        }
        percent
    }

    /// Rule 4: the subtotal after the discount.
    pub fn apply_volume_discount(&self, subtotal: f32, page_count: u32) -> f32 {
        let percent = self.volume_discount_percent(page_count);
        if percent <= 0.0 {
            return subtotal;
        }
        subtotal * (1.0 - percent / 100.0)
    }

    /// Rule 2b: whether the COLOR rule set applies to this (already trimmed)
    /// page. Mirrors `hasColorInk(page, greysAsColor)` in the webapp.
    pub fn has_color_ink(&self, page: Coverage) -> bool {
        if page.c + page.m + page.y == 0.0 {
            return false;
        }
        if !self.greys_as_color && is_grey_ink(page) {
            return false;
        }
        true
    }

    /// Rule 2a: no ink at all on the sheet.
    pub fn is_blank(&self, page: Coverage) -> bool {
        page.c == 0.0 && page.m == 0.0 && page.y == 0.0 && page.k == 0.0
    }
}

/// Rule 2b: the three color channels carry the same ink → grey, not color.
pub fn is_grey_ink(page: Coverage) -> bool {
    if page.c == 0.0 && page.m == 0.0 && page.y == 0.0 {
        return false;
    }
    let max = page.c.max(page.m).max(page.y);
    let min = page.c.min(page.m).min(page.y);
    max - min <= GREY_TOLERANCE
}

/// Rule set a page would be billed by, mirroring the server: the Fase 6 rules
/// first, then both rule sets, and the DEAREST one wins (ties go to COLOR).
/// `price_of` returns the tier's price when it is known; without prices there is
/// nothing to compare and COLOR keeps its precedence.
pub fn dearest_tier<'a>(
    page: &Coverage,
    tiers: &'a [crate::coverage::Tier],
    metrics: MetricsByMode,
    pricing: &PrintPricing,
    price_of: impl Fn(&str) -> Option<f32>,
) -> Option<&'a crate::coverage::Tier> {
    let inked = pricing.apply_intensity(*page);
    let bw = classify_coverage(metric_coverage(metrics.bw, &inked), tiers, PrintMode::Bw);
    let color = if pricing.has_color_ink(inked) {
        classify_coverage(
            metric_coverage(metrics.color, &inked),
            tiers,
            PrintMode::Color,
        )
    } else {
        None
    };
    match (bw, color) {
        (Some(bw), Some(color)) => {
            match (price_of(&bw.id), price_of(&color.id)) {
                // Only a strictly dearer mono tier takes the page away from
                // color, so an unknown price never flips the historical label.
                (Some(bw_price), Some(color_price)) if bw_price > color_price => Some(bw),
                _ => Some(color),
            }
        }
        (Some(bw), None) => Some(bw),
        (None, Some(color)) => Some(color),
        (None, None) => None,
    }
}

/// Coverage of a page under a rule set, after the intensity rule. The agent uses
/// this to label pages locally with the same number the server will bill.
pub fn priced_coverage(
    mode: PrintMode,
    page: Coverage,
    metrics: MetricsByMode,
    pricing: &PrintPricing,
) -> f32 {
    metric_coverage(metrics.for_mode(mode), &pricing.apply_intensity(page))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(c: f32, m: f32, y: f32, k: f32) -> Coverage {
        Coverage::new(c, m, y, k)
    }

    #[test]
    fn intensity_below_the_threshold_counts_as_no_ink() {
        // 2% of cyan is anti-aliasing, not ink, once the shop says 5%.
        let rules = PrintPricing {
            intensity_threshold: 5.0,
            ..Default::default()
        };
        let trimmed = rules.apply_intensity(page(2.0, 4.99, 5.0, 80.0));
        assert_eq!(trimmed.c, 0.0);
        assert_eq!(trimmed.m, 0.0);
        // Exactly at the threshold stays: the rule is "below", not "at or below".
        assert_eq!(trimmed.y, 5.0);
        assert_eq!(trimmed.k, 80.0);

        // Threshold 0 is a no-op and must not copy-zero anything.
        assert_eq!(
            PrintPricing::default().apply_intensity(page(0.4, 0.0, 0.0, 0.0)),
            page(0.4, 0.0, 0.0, 0.0)
        );
    }

    #[test]
    fn a_blank_sheet_has_no_ink_at_all() {
        let rules = PrintPricing::default();
        assert!(rules.is_blank(page(0.0, 0.0, 0.0, 0.0)));
        assert!(!rules.is_blank(page(0.0, 0.0, 0.0, 0.01)));
        // Ink trimmed by the intensity rule leaves a blank sheet.
        let strict = PrintPricing {
            intensity_threshold: 10.0,
            ..Default::default()
        };
        assert!(strict.is_blank(strict.apply_intensity(page(0.0, 0.0, 0.0, 3.0))));
    }

    #[test]
    fn greys_count_as_color_only_when_the_shop_says_so() {
        let grey = page(12.0, 12.0, 12.0, 0.0);
        assert!(is_grey_ink(grey));

        // Default (historical): grey ink is color.
        assert!(PrintPricing::default().has_color_ink(grey));

        // Off: the grey sheet bills through the mono set.
        let saving = PrintPricing {
            greys_as_color: false,
            ..Default::default()
        };
        assert!(!saving.has_color_ink(grey));

        // A real color page is unaffected either way.
        let colorful = page(30.0, 5.0, 0.0, 0.0);
        assert!(!is_grey_ink(colorful));
        assert!(saving.has_color_ink(colorful));

        // Pure black never counts as color, whatever the setting.
        assert!(!saving.has_color_ink(page(0.0, 0.0, 0.0, 60.0)));
        assert!(!PrintPricing::default().has_color_ink(page(0.0, 0.0, 0.0, 60.0)));

        // The tolerance is 1 point: 12 vs 13 is still grey, 12 vs 14 is not.
        assert!(is_grey_ink(page(12.0, 13.0, 12.0, 0.0)));
        assert!(!is_grey_ink(page(12.0, 14.0, 12.0, 0.0)));
    }

    #[test]
    fn volume_discounts_are_thresholds_not_a_sum() {
        let rules = PrintPricing {
            volume_discounts: vec![
                VolumeDiscount {
                    min_pages: 10,
                    percent: 5.0,
                },
                VolumeDiscount {
                    min_pages: 50,
                    percent: 12.0,
                },
                VolumeDiscount {
                    min_pages: 100,
                    percent: 10.0,
                },
            ],
            ..Default::default()
        };
        assert_eq!(rules.volume_discount_percent(9), 0.0);
        assert_eq!(rules.volume_discount_percent(10), 5.0);
        assert_eq!(rules.volume_discount_percent(49), 5.0);
        assert_eq!(rules.volume_discount_percent(50), 12.0);
        // A lower percentage at a higher threshold never wins.
        assert_eq!(rules.volume_discount_percent(150), 12.0);
        assert_eq!(rules.apply_volume_discount(100.0, 50), 88.0);
        assert_eq!(rules.apply_volume_discount(100.0, 5), 100.0);
    }

    #[test]
    fn the_webapp_payload_deserialises_with_its_own_defaults() {
        // Shape sent by `GET /api/agent/tiers` (camelCase, Fase 6).
        let parsed: PrintPricing = serde_json::from_str(
            r#"{"intensityThreshold":5,"greysAsColor":false,"blankPageFree":true,
                "paperSurcharge":0.5,"volumeDiscounts":[{"minPages":10,"percent":5}]}"#,
        )
        .unwrap();
        assert_eq!(parsed.intensity_threshold, 5.0);
        assert!(!parsed.greys_as_color);
        assert!(parsed.blank_page_free);
        assert_eq!(parsed.paper_surcharge, 0.5);
        assert_eq!(parsed.volume_discounts.len(), 1);

        // An older server that sends nothing keeps the historical behaviour.
        let empty: PrintPricing = serde_json::from_str("{}").unwrap();
        assert_eq!(empty, PrintPricing::default());
        assert!(empty.greys_as_color && empty.blank_page_free);
    }

    fn tier(id: &str, mode: PrintMode, min: f32, max: f32) -> crate::coverage::Tier {
        crate::coverage::Tier {
            id: id.into(),
            mode,
            min_coverage: min,
            max_coverage: max,
            active: true,
        }
    }

    #[test]
    fn the_dearest_rule_set_takes_the_page() {
        let tiers = vec![
            tier("bw", PrintMode::Bw, 0.0, 100.0),
            tier("color", PrintMode::Color, 0.0, 100.0),
        ];
        let metrics = MetricsByMode::default();
        let rules = PrintPricing::default();
        // A page with color ink: both sets match, so the price decides.
        let colorful = page(30.0, 5.0, 0.0, 10.0);
        let cheap_color = dearest_tier(&colorful, &tiers, metrics, &rules, |id| {
            Some(if id == "color" { 2.0 } else { 6.0 })
        })
        .unwrap();
        assert_eq!(cheap_color.id, "bw");

        let dear_color = dearest_tier(&colorful, &tiers, metrics, &rules, |id| {
            Some(if id == "color" { 6.0 } else { 2.0 })
        })
        .unwrap();
        assert_eq!(dear_color.id, "color");

        // Tie → COLOR (same rule as the server).
        let tie = dearest_tier(&colorful, &tiers, metrics, &rules, |_| Some(4.0)).unwrap();
        assert_eq!(tie.id, "color");

        // Unknown prices: nothing to compare, color keeps precedence.
        let unknown = dearest_tier(&colorful, &tiers, metrics, &rules, |_| None).unwrap();
        assert_eq!(unknown.id, "color");

        // A black-only page never reaches the color set.
        let mono = dearest_tier(&page(0.0, 0.0, 0.0, 40.0), &tiers, metrics, &rules, |_| {
            Some(1.0)
        })
        .unwrap();
        assert_eq!(mono.id, "bw");
    }

    #[test]
    fn the_rules_decide_the_label_before_the_prices() {
        // Grey page with greys_as_color off: the color set is not even a
        // candidate, so the dearest-price comparison never happens.
        let tiers = vec![
            tier("bw", PrintMode::Bw, 0.0, 100.0),
            tier("color", PrintMode::Color, 0.0, 100.0),
        ];
        let rules = PrintPricing {
            greys_as_color: false,
            ..Default::default()
        };
        let labelled = dearest_tier(
            &page(20.0, 20.0, 20.0, 0.0),
            &tiers,
            MetricsByMode::default(),
            &rules,
            |id| Some(if id == "color" { 99.0 } else { 1.0 }),
        )
        .unwrap();
        assert_eq!(labelled.id, "bw");

        // With the intensity rule the page is blank. It still MATCHES the 0–100
        // range (coverage 0 is inside it): "free blank sheet" is the server's
        // `blank_page_free` rule, not a missing tier. A range that starts above
        // 0 is what leaves a blank page unlabelled.
        let strict = PrintPricing {
            intensity_threshold: 25.0,
            ..Default::default()
        };
        let blank = dearest_tier(
            &page(20.0, 20.0, 20.0, 0.0),
            &tiers,
            MetricsByMode::default(),
            &strict,
            |_| Some(1.0),
        )
        .unwrap();
        assert_eq!(blank.id, "bw");

        let above_zero = vec![tier("bw", PrintMode::Bw, 0.01, 100.0)];
        assert!(dearest_tier(
            &page(20.0, 20.0, 20.0, 0.0),
            &above_zero,
            MetricsByMode::default(),
            &strict,
            |_| Some(1.0)
        )
        .is_none());
    }

    #[test]
    fn priced_coverage_uses_the_trimmed_ink() {
        let rules = PrintPricing {
            intensity_threshold: 10.0,
            ..Default::default()
        };
        let metrics = MetricsByMode::default();
        // SUM_CMYK (color default): 8 + 8 + 8 trimmed to 0, K survives.
        assert_eq!(
            priced_coverage(PrintMode::Color, page(8.0, 8.0, 8.0, 40.0), metrics, &rules),
            40.0
        );
        assert_eq!(
            priced_coverage(PrintMode::Bw, page(8.0, 8.0, 8.0, 40.0), metrics, &rules),
            40.0
        );
    }
}
