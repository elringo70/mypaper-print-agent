use serde::{Deserialize, Serialize};

/// Print characteristics the agent can impose on a job it re-renders.
///
/// Only what the Ghostscript `mswinpr2` device actually honours lives here:
/// paper size (named, or explicit geometry in points) and duplex. Two
/// characteristics are deliberately absent because this path cannot change
/// them:
///
/// - **Resolution**: the mswinpr2 device documents that it does not support
///   setting the printer resolution.
/// - **Media type (paper thickness), tray and toner save**: they live in the
///   Windows driver's DEVMODE, which mswinpr2 inherits instead of rewriting.
///   They must be configured once per printer in the driver's own preferences
///   (Printer properties → Preferences), which is also where the mono/color
///   role of each printer is set.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrintOptions {
    /// Ghostscript paper size name: `a4`, `letter`, `legal`, `ledger`…
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paper: Option<String>,
    /// Explicit page width in points; wins over `paper` when both are set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width_points: Option<u32>,
    /// Explicit page height in points; wins over `paper` when both are set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height_points: Option<u32>,
    /// Two-sided printing. Ignored by drivers that do not support duplex.
    #[serde(default)]
    pub duplex: bool,
    /// Bind the verso on the short edge (top/bottom) instead of the long edge.
    #[serde(default)]
    pub tumble: bool,
    /// Media type (paper thickness) as the DRIVER spells it: `Plain`, `Thick`,
    /// `Cardstock`… Only names the printer reports (see `mpa printers --caps`)
    /// are accepted. Applied through the job's DEVMODE, never through
    /// Ghostscript: `mswinpr2` cannot set it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
}

impl PrintOptions {
    /// Nothing to apply: the job can be resumed on its own printer untouched.
    pub fn is_empty(&self) -> bool {
        self.paper.is_none()
            && self.width_points.is_none()
            && self.height_points.is_none()
            && !self.duplex
            && !self.tumble
            && self.media_type.is_none()
    }

    /// Whether these options need the job's DEVMODE (media type / paper
    /// thickness), which the Ghostscript reprint path cannot change.
    pub fn needs_devmode(&self) -> bool {
        self.media_type.is_some()
    }

    /// Ghostscript arguments for these options, in a stable order. Pure, so the
    /// vectors can be asserted without spawning a process.
    ///
    /// Explicit geometry wins over a named size: a preset that carries both is
    /// ambiguous, and `FIXEDMEDIA` makes the page size authoritative instead of
    /// letting the PDF's own MediaBox through.
    pub fn gs_args(&self) -> Vec<String> {
        let mut args = Vec::new();
        match (self.width_points, self.height_points) {
            (Some(width), Some(height)) => {
                args.push(format!("-dDEVICEWIDTHPOINTS={width}"));
                args.push(format!("-dDEVICEHEIGHTPOINTS={height}"));
                args.push("-dFIXEDMEDIA".to_string());
            }
            _ => {
                if let Some(paper) = &self.paper {
                    args.push(format!("-sPAPERSIZE={paper}"));
                }
            }
        }
        if self.duplex {
            args.push("-dDuplex=true".to_string());
            args.push(format!("-dTumble={}", self.tumble));
        }
        args
    }

    /// Short human label for the UI (`A4 · doble cara`, `Carta`, `—`).
    pub fn label(&self) -> String {
        let mut parts = Vec::new();
        if let (Some(w), Some(h)) = (self.width_points, self.height_points) {
            parts.push(format!("{w}×{h} pt"));
        } else if let Some(paper) = &self.paper {
            parts.push(paper.to_uppercase());
        }
        if self.duplex {
            parts.push(if self.tumble {
                "doble cara (borde corto)".to_string()
            } else {
                "doble cara".to_string()
            });
        }
        if let Some(media) = &self.media_type {
            parts.push(media.clone());
        }
        if parts.is_empty() {
            "—".to_string()
        } else {
            parts.join(" · ")
        }
    }
}

/// A named set of print options the operator can pick at print time. Presets
/// describe this PC's printers, so they live in `config.toml` next to the
/// mono/color roles.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrintPreset {
    pub name: String,
    #[serde(flatten)]
    pub options: PrintOptions,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_options_produce_no_arguments() {
        assert!(PrintOptions::default().is_empty());
        assert!(PrintOptions::default().gs_args().is_empty());
        assert_eq!(PrintOptions::default().label(), "—");
    }

    #[test]
    fn a_named_paper_size_maps_to_papersize() {
        let options = PrintOptions {
            paper: Some("a4".into()),
            ..Default::default()
        };
        assert_eq!(options.gs_args(), vec!["-sPAPERSIZE=a4"]);
        assert_eq!(options.label(), "A4");
        assert!(!options.is_empty());
    }

    #[test]
    fn explicit_geometry_wins_over_the_named_size() {
        // A preset carrying both is ambiguous; the explicit points are what the
        // operator measured, so they win and the media box is pinned.
        let options = PrintOptions {
            paper: Some("letter".into()),
            width_points: Some(612),
            height_points: Some(1008),
            ..Default::default()
        };
        assert_eq!(
            options.gs_args(),
            vec![
                "-dDEVICEWIDTHPOINTS=612",
                "-dDEVICEHEIGHTPOINTS=1008",
                "-dFIXEDMEDIA"
            ]
        );
        assert_eq!(options.label(), "612×1008 pt");
    }

    #[test]
    fn half_a_geometry_falls_back_to_the_named_size() {
        // Width without height cannot pin a page, so the name is used instead.
        let options = PrintOptions {
            paper: Some("legal".into()),
            width_points: Some(612),
            ..Default::default()
        };
        assert_eq!(options.gs_args(), vec!["-sPAPERSIZE=legal"]);
    }

    #[test]
    fn duplex_carries_the_tumble_orientation() {
        let options = PrintOptions {
            duplex: true,
            ..Default::default()
        };
        assert_eq!(options.gs_args(), vec!["-dDuplex=true", "-dTumble=false"]);
        assert_eq!(options.label(), "doble cara");

        let tumbled = PrintOptions {
            paper: Some("a4".into()),
            duplex: true,
            tumble: true,
            ..Default::default()
        };
        assert_eq!(
            tumbled.gs_args(),
            vec!["-sPAPERSIZE=a4", "-dDuplex=true", "-dTumble=true"]
        );
        assert_eq!(tumbled.label(), "A4 · doble cara (borde corto)");
    }

    #[test]
    fn a_media_type_is_a_devmode_option() {
        // Paper thickness cannot go through the Ghostscript reprint path.
        let options = PrintOptions {
            paper: Some("a4".into()),
            media_type: Some("Thick".into()),
            ..Default::default()
        };
        assert!(options.needs_devmode());
        assert_eq!(options.gs_args(), vec!["-sPAPERSIZE=a4"]);
        assert_eq!(options.label(), "A4 · Thick");
        assert!(!PrintOptions {
            paper: Some("a4".into()),
            ..Default::default()
        }
        .needs_devmode());
    }

    #[test]
    fn a_preset_flattens_into_toml() {
        // config.toml shape: [[presets]] name = "..." paper = "a4" duplex = true
        let parsed: PrintPreset =
            toml::from_str("name = \"A4 doble\"\npaper = \"a4\"\nduplex = true").unwrap();
        assert_eq!(parsed.name, "A4 doble");
        assert_eq!(parsed.options.paper.as_deref(), Some("a4"));
        assert!(parsed.options.duplex);
    }
}
