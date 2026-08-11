//! Slicer dialect handling.
//!
//! PrusaSlicer, SuperSlicer, OrcaSlicer, Bambu Studio and Cura all label the
//! same regions differently. Classifying the label directly means no slicer
//! detection pass and no per-slicer marker tables.

/// The regions this post-processor treats differently.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Feature {
    /// The visible outermost loop.
    ExternalPerimeter,
    /// Any hidden loop inside the external perimeter.
    InternalPerimeter,
    /// A stretch of wall printed over air. Slicers label it in place of the
    /// wall it belongs to, and it can interrupt an inner wall as readily as an
    /// outer one — measured on an OrcaSlicer 2.4.2 Benchy, mid-loop, with no
    /// travel between the two labels. So it names a condition, never which
    /// wall this is, and it must not decide that a loop is the visible one.
    Overhang,
    /// Sparse internal infill.
    SparseInfill,
    /// Solid, top and bottom surfaces.
    SolidInfill,
    #[default]
    Other,
}

impl Feature {
    /// Classifies a `;TYPE:`, `; FEATURE:` or Cura `;TYPE:` region comment.
    /// Returns `None` for lines that are not region markers.
    pub fn from_comment(line: &str) -> Option<Self> {
        Self::from_marker(line.trim_start().strip_prefix(';')?)
    }

    /// Classifies a region marker from the text after its `;`.
    pub fn from_marker(comment: &str) -> Option<Self> {
        Some(classify(region_label(comment)?))
    }

    pub fn is_perimeter(self) -> bool {
        matches!(
            self,
            Feature::ExternalPerimeter | Feature::InternalPerimeter | Feature::Overhang
        )
    }
}

fn region_label(comment: &str) -> Option<&str> {
    let text = comment.trim_start();
    ["TYPE:", "FEATURE:"].into_iter().find_map(|key| {
        let (head, tail) = text.split_at_checked(key.len())?;
        head.eq_ignore_ascii_case(key).then_some(tail)
    })
}

fn classify(label: &str) -> Feature {
    const EXTERNAL: [&str; 3] = ["external perimeter", "outer wall", "wall-outer"];
    const INTERNAL: [&str; 3] = ["inner wall", "wall-inner", "perimeter"];
    const SOLID: [&str; 5] = ["solid", "top surface", "bottom surface", "bridge", "skin"];

    let has = |needles: &[&str]| needles.iter().any(|needle| contains_fold(label, needle));
    // Before the wall tests: `Overhang perimeter` carries both words.
    if contains_fold(label, "overhang") {
        Feature::Overhang
    } else if has(&EXTERNAL) {
        Feature::ExternalPerimeter
    } else if has(&INTERNAL) {
        Feature::InternalPerimeter
    } else if has(&SOLID) {
        Feature::SolidInfill
    } else if contains_fold(label, "infill") || contains_fold(label, "fill") {
        Feature::SparseInfill
    } else {
        Feature::Other
    }
}

/// `haystack.to_ascii_lowercase().contains(needle)` without the `String` that
/// lowercasing every marker line would allocate. `needle` is already lowercase.
fn contains_fold(haystack: &str, needle: &str) -> bool {
    let (haystack, needle) = (haystack.as_bytes(), needle.as_bytes());
    let Some(last) = haystack.len().checked_sub(needle.len()) else {
        return false;
    };
    (0..=last).any(|at| haystack[at..at + needle.len()].eq_ignore_ascii_case(needle))
}

/// True for the layer-change markers emitted by common slicers: `;LAYER_CHANGE`
/// (PrusaSlicer, OrcaSlicer), `; CHANGE_LAYER` (Bambu Studio) and `;LAYER:<n>`
/// (Cura).
pub fn is_layer_change(line: &str) -> bool {
    line.trim_start()
        .strip_prefix(';')
        .is_some_and(is_layer_marker)
}

/// True for a layer-change marker given the text after its `;`.
pub fn is_layer_marker(comment: &str) -> bool {
    let text = comment.trim_start();
    text.eq_ignore_ascii_case("LAYER_CHANGE")
        || text.eq_ignore_ascii_case("CHANGE_LAYER")
        || text
            .split_at_checked(6)
            .is_some_and(|(head, tail)| head.eq_ignore_ascii_case("LAYER:") && !tail.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_prusaslicer_markers() {
        let of = Feature::from_comment;
        assert_eq!(
            of(";TYPE:External perimeter"),
            Some(Feature::ExternalPerimeter)
        );
        assert_eq!(of(";TYPE:Perimeter"), Some(Feature::InternalPerimeter));
        assert_eq!(of(";TYPE:Internal infill"), Some(Feature::SparseInfill));
        assert_eq!(of(";TYPE:Solid infill"), Some(Feature::SolidInfill));
        assert_eq!(of(";TYPE:Top solid infill"), Some(Feature::SolidInfill));
        assert_eq!(of(";TYPE:Bridge infill"), Some(Feature::SolidInfill));
        assert_eq!(of(";TYPE:Skirt/Brim"), Some(Feature::Other));
    }

    #[test]
    fn classifies_orca_and_bambu_markers() {
        let of = Feature::from_comment;
        assert_eq!(of(";TYPE:Outer wall"), Some(Feature::ExternalPerimeter));
        assert_eq!(of(";TYPE:Inner wall"), Some(Feature::InternalPerimeter));
        assert_eq!(
            of("; FEATURE: Inner wall"),
            Some(Feature::InternalPerimeter)
        );
        assert_eq!(of("; FEATURE: Sparse infill"), Some(Feature::SparseInfill));
        assert_eq!(of("; FEATURE: Top surface"), Some(Feature::SolidInfill));
    }

    #[test]
    fn classifies_cura_markers() {
        let of = Feature::from_comment;
        assert_eq!(of(";TYPE:WALL-OUTER"), Some(Feature::ExternalPerimeter));
        assert_eq!(of(";TYPE:WALL-INNER"), Some(Feature::InternalPerimeter));
        assert_eq!(of(";TYPE:FILL"), Some(Feature::SparseInfill));
        assert_eq!(of(";TYPE:SKIN"), Some(Feature::SolidInfill));
    }

    #[test]
    fn non_region_comments_are_not_markers() {
        assert_eq!(Feature::from_comment("G1 X1 Y1"), None);
        assert_eq!(Feature::from_comment("; layer_height = 0.2"), None);
        assert_eq!(Feature::from_comment(";LAYER_CHANGE"), None);
    }

    /// An overhanging stretch of wall is labelled in place of the wall it
    /// belongs to, and the marker never says which wall that was. Neither does
    /// anything else in the file: on an OrcaSlicer slice 874 of 1148 sat
    /// between two outer wall regions and 272 between two inner ones, and the
    /// line width was a flat 0.4 for every one of them where outer walls used
    /// 0.42 and inner ones 0.45.
    ///
    /// It used to classify as the visible wall, which read as "this loop is
    /// the outer one". That is false: OrcaSlicer 2.4.2 interrupts an **inner**
    /// wall with it mid-loop, with no travel between the two labels, so a loop
    /// that merely began over air was taken for the visible wall, anchored its
    /// contour and pushed the real outer wall into a contour of its own — 665
    /// of 21832 visible-wall extrusions came out raised on a 1000-wall Benchy.
    /// It is now its own class: never an anchor, and never raised on its own
    /// evidence, since ground truth says 83.7% of it is really the visible
    /// wall.
    #[test]
    fn an_overhang_is_its_own_class_and_names_no_wall() {
        for label in [
            ";TYPE:Overhang perimeter",
            "; FEATURE: Overhang wall",
            ";TYPE:OVERHANG WALL",
        ] {
            assert_eq!(
                Feature::from_comment(label),
                Some(Feature::Overhang),
                "{label}"
            );
        }
        // It is still a wall, so its loops are buffered and numbered with the
        // rest of the stack rather than passing through as infill would.
        assert!(Feature::Overhang.is_perimeter());
    }

    /// Nothing a prime tower, wipe or support region does may be mistaken for
    /// a wall, or the transform would shift material it cannot account for.
    #[test]
    fn auxiliary_regions_are_left_alone() {
        for label in [
            "; FEATURE: Prime tower",
            ";TYPE:Prime tower",
            ";TYPE:Skirt/Brim",
            "; FEATURE: Brim",
            ";TYPE:Support material",
            "; FEATURE: Support",
            ";TYPE:Custom",
            "; FEATURE: Custom",
        ] {
            assert_eq!(
                Feature::from_comment(label),
                Some(Feature::Other),
                "{label}"
            );
        }
    }

    /// The two entry points differ only in whether the `;` has been stripped,
    /// and a pass that has already split the line uses the second.
    #[test]
    fn classifying_from_a_marker_matches_classifying_the_line() {
        for line in [
            ";TYPE:External perimeter",
            ";TYPE:Perimeter",
            "; FEATURE: Inner wall",
            ";TYPE:WALL-OUTER",
            ";TYPE:Solid infill",
            ";TYPE:FILL",
            ";TYPE:Skirt/Brim",
            "; layer_height = 0.2",
            ";LAYER_CHANGE",
        ] {
            let marker = line.strip_prefix(';').expect("a comment");
            assert_eq!(
                Feature::from_marker(marker),
                Feature::from_comment(line),
                "{line}"
            );
            assert_eq!(is_layer_marker(marker), is_layer_change(line), "{line}");
        }
        // Only `from_comment` takes a whole line, so this is where they part.
        assert_eq!(Feature::from_comment("TYPE:Perimeter"), None);
    }

    /// Slicers disagree about the case of a region name, and classifying one
    /// must not depend on it.
    #[test]
    fn labels_classify_whatever_their_case() {
        for label in [
            "Inner wall",
            "INNER WALL",
            "inner wall",
            "iNnEr WaLl",
            "Overhang perimeter",
        ] {
            let expected = match label {
                label if label.eq_ignore_ascii_case("Overhang perimeter") => Feature::Overhang,
                _ => Feature::InternalPerimeter,
            };
            assert_eq!(
                Feature::from_marker(&format!("TYPE:{label}")),
                Some(expected),
                "{label}"
            );
        }
    }

    #[test]
    fn folded_contains_matches_a_lowercased_search() {
        for (haystack, needle) in [
            ("Inner wall", "inner wall"),
            ("INNER WALL", "inner wall"),
            ("Top solid infill", "solid"),
            ("Top solid infill", "infill"),
            ("wall", "inner wall"),
            ("", "solid"),
            ("solid", ""),
            ("Bridge infill", "skin"),
        ] {
            assert_eq!(
                contains_fold(haystack, needle),
                haystack.to_ascii_lowercase().contains(needle),
                "{haystack:?} in {needle:?}"
            );
        }
    }

    #[test]
    fn recognises_layer_change_markers() {
        assert!(is_layer_change(";LAYER_CHANGE"));
        assert!(is_layer_change("; CHANGE_LAYER"));
        assert!(is_layer_change(";LAYER:0"));
        assert!(is_layer_change(";LAYER:127"));
        assert!(!is_layer_change(";LAYER:"));
        assert!(!is_layer_change(";TYPE:Perimeter"));
        assert!(!is_layer_change("G1 Z0.4"));
    }
}
