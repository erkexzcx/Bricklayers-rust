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
    /// A bead laid where two loops of one wall could not both fit. It belongs
    /// to the wall it fills and a slicer emits it in the middle of one, so it
    /// must not end the wall's region — but it is a one-off, not a loop of the
    /// stack, so it takes no place in the alternation and is never raised.
    GapFill,
    /// A feature too narrow to hold two loops, printed as one bead. It is the
    /// part, so it belongs in the footprint and in the coverage sets capping
    /// is measured against — but it is not a loop of a wall: its two faces are
    /// both the visible one, so raising it puts a step on the outside, the
    /// same reason [`Feature::Overhang`] is never raised on its own evidence.
    ThinWall,
    /// Sparse internal infill.
    SparseInfill,
    /// Solid infill, bottom surfaces and bridges — anything solid that is not
    /// the face of the part.
    SolidInfill,
    /// The solid layer left facing the air, which is where a shallow slope
    /// shows its stair steps.
    TopSurface,
    /// A second pass over a top surface with the nozzle nearly dry. It follows
    /// the surface it is smoothing, so it is contoured with it, but it is
    /// deliberately not metered for the gap it crosses.
    Ironing,
    #[default]
    Other,
}

impl Feature {
    /// Classifies a `;TYPE:`, `; FEATURE:` or Simplify3D `; feature ` region
    /// comment. Returns `None` for lines that are not region markers.
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

    /// True where the region lays down part of the object itself, and so says
    /// where the object *is* on this layer.
    ///
    /// Both callers ask that one question of it — the box
    /// [`Survey`](crate::scan::Survey) grows to size the surface grid, and the
    /// per-layer outline [`Scout`](crate::zaa::scout::Scout) traces for the
    /// surface model to measure a strip against. Neither asks "is any plastic
    /// here": both want an *outline of the part*, so both want the same answer,
    /// and a region left out of one would have to be left out of the other.
    ///
    /// A skirt, a brim, a prime tower, a wipe tower and support material all
    /// put plastic on the bed without being the part. The wipe tower is the
    /// loudest case — a tall column standing well outside the object, which
    /// would take the box with it — but support is excluded for the same
    /// reason and not a weaker one: it stands under and beside the part, so
    /// its outline is not the part's outline on any layer it appears on.
    ///
    /// That does mean [`Feature::Other`] answers for both of them, and a third
    /// caller wanting "is any plastic standing here, whoever it belongs to"
    /// cannot be served from this enum as it stands — support would have to
    /// become a variant of its own first. Do not widen this predicate to serve
    /// such a caller; it would take the box with it.
    pub fn builds_the_part(self) -> bool {
        !matches!(self, Feature::Other)
    }

    /// True where the region lays the surface the print leaves facing the air,
    /// which is the only thing [`zaa`](crate::zaa) reshapes.
    pub fn is_surface(self) -> bool {
        matches!(self, Feature::TopSurface | Feature::Ironing)
    }
}

fn region_label(comment: &str) -> Option<&str> {
    // Simplify3D names the region with the bare word and no colon —
    // `; feature outer perimeter` — so there the space is the separator, and
    // it has to be one, or `; features = 3` would read as a region.
    const KEYS: [&str; 3] = ["TYPE:", "FEATURE:", "FEATURE "];

    let text = comment.trim_start();
    KEYS.into_iter().find_map(|key| {
        let (head, tail) = text.split_at_checked(key.len())?;
        head.eq_ignore_ascii_case(key).then_some(tail)
    })
}

/// The label of a region marker whose words named nothing this module knows.
///
/// Takes the same text as [`Feature::from_marker`] — a comment line with its
/// `;` already stripped — and answers `Some(label)` only where all of these
/// hold: the line is a region marker, its label classified as
/// [`Feature::Other`], and it does not name one of the auxiliary regions
/// listed below, which are understood perfectly well and merely are not the
/// part. So a file full of `;TYPE:Skirt/Brim` reports nothing at all, and a
/// dialect this module has never met reports every region of it. An empty
/// label (a bare `;TYPE:`) is not one either — there is no text in it to
/// report.
///
/// The label is borrowed from `comment` and trimmed, so counting these costs
/// no allocation on the caller's hot path; a caller that wants to keep one
/// past the line it came from has to copy it itself.
pub fn unrecognised_region(comment: &str) -> Option<&str> {
    let label = region_label(comment)?.trim();
    let known = label.is_empty() || is_auxiliary(label) || classify(label) != Feature::Other;
    (!known).then_some(label)
}

/// Regions every slicer puts on the bed that are not the object.
///
/// Tested before anything else, because these are the labels that carry
/// another region's words: a raft or a support region spelled with the word
/// `wall` in it would otherwise land among the loops of a stack and be
/// bricked, which moves material the transform cannot account for.
const AUXILIARY: &[&str] = &[
    "support", "skirt", "brim", "raft", "tower", "wipe", "prime", "purge", "shield", "custom",
];

/// True for a region that is recognised *and* is not the part.
///
/// It is the other half of [`Feature::Other`]: a label naming one of these is
/// understood, where any other label landing there is not — see
/// [`unrecognised_region`].
fn is_auxiliary(label: &str) -> bool {
    names(label, AUXILIARY)
}

fn classify(label: &str) -> Feature {
    // Every slicer names the same handful of things, using the same words in a
    // different order and joined by a different separator: `WALL-OUTER`,
    // `External perimeter`, `Outer wall` and `outer perimeter` are one region
    // under four spellings, and only one of the four is guessable from the
    // other three. So the test is on the words, never on a phrase, and the
    // order of the arms below is the whole of the precedence — a label
    // carrying two of these words means the first arm that reaches it.
    const WALL: &[&str] = &["perimeter", "wall", "shell"];
    const OUTER: &[&str] = &["outer", "outside", "external", "exterior"];
    const FACE: &[&str] = &["skin", "surface"];
    const SOLID: &[&str] = &["solid", "bridge"];
    const INFILL: &[&str] = &["infill", "fill", "sparse"];

    if is_auxiliary(label) {
        Feature::Other
    // Before the wall tests: `Overhang perimeter` carries both words.
    } else if names(label, &["overhang"]) {
        Feature::Overhang
    // Before the surface tests: ironing runs over a top surface, and a slicer
    // that says so names the surface in the same label.
    } else if names(label, &["iron"]) {
        Feature::Ironing
    // Before the wall tests, so that a slicer spelling it `External thin wall`
    // cannot land among the loops of a stack it has no place in.
    } else if names(label, &["thin"]) {
        Feature::ThinWall
    // Before the infill test: PrusaSlicer calls it `Gap fill` and OrcaSlicer
    // `Gap infill`, so both carry the word the infill arm reads. And before
    // the wall tests, for the same reason a thin wall is — it is laid between
    // two loops of a wall and takes no place in their alternation, so a slicer
    // naming the wall in the label must not put it back among them.
    } else if names(label, &["gap"]) {
        Feature::GapFill
    } else if names(label, WALL) {
        if names(label, OUTER) {
            Feature::ExternalPerimeter
        } else {
            Feature::InternalPerimeter
        }
    // Before the top test: a bottom surface faces the plate rather than the
    // air, and a slicer names it with the same words a top surface uses.
    } else if names(label, &["bottom"]) {
        Feature::SolidInfill
    // Cura names both faces of a part `SKIN`, so a bottom surface with no word
    // of its own lands here too. Nothing rests on that: the geometry decides
    // what is contoured, and a surface with a layer printed over it is not
    // exposed to begin with.
    } else if names(label, &["top"]) || names(label, FACE) {
        Feature::TopSurface
    } else if names(label, SOLID) {
        Feature::SolidInfill
    } else if names(label, INFILL) {
        Feature::SparseInfill
    } else {
        Feature::Other
    }
}

/// True where any word of `label` begins with one of `stems`.
///
/// A label's words are whatever the slicer's separator left between them, so
/// `WALL-OUTER`, `Top solid infill` and `top_surface` are all read the same
/// way. A stem rather than a whole word, so that a plural or a slicer's own
/// suffix still names the same thing: `perimeters` is `perimeter` and
/// `filling` is `fill`.
fn names(label: &str, stems: &[&str]) -> bool {
    label
        .split(|c: char| !c.is_ascii_alphanumeric())
        .any(|word| stems.iter().any(|stem| starts_fold(word, stem)))
}

/// `word.to_ascii_lowercase().starts_with(stem)` without the `String` that
/// lowercasing every word of every marker line would allocate. `stem` is
/// already lowercase.
fn starts_fold(word: &str, stem: &str) -> bool {
    let (word, stem) = (word.as_bytes(), stem.as_bytes());
    word.len() >= stem.len() && word[..stem.len()].eq_ignore_ascii_case(stem)
}

/// True for the layer-change markers emitted by common slicers: `;LAYER_CHANGE`
/// (PrusaSlicer, OrcaSlicer), `; CHANGE_LAYER` (Bambu Studio), `;LAYER:<n>`
/// (Cura) and `; layer <n>, Z = <height>` (Simplify3D).
pub fn is_layer_change(line: &str) -> bool {
    line.trim_start()
        .strip_prefix(';')
        .is_some_and(is_layer_marker)
}

/// True for a layer-change marker given the text after its `;`.
///
/// Simplify3D's form carries an `=`, so every caller has to ask this before it
/// reads a comment as a `key = value` setting, or `; layer 2, Z = 0.4` is
/// booked as a setting named `layer 2, Z`.
pub fn is_layer_marker(comment: &str) -> bool {
    let text = comment.trim_start();
    text.eq_ignore_ascii_case("LAYER_CHANGE")
        || text.eq_ignore_ascii_case("CHANGE_LAYER")
        || text
            .split_at_checked(6)
            .is_some_and(|(head, tail)| head.eq_ignore_ascii_case("LAYER:") && !tail.is_empty())
        || numbered_layer(text)
}

/// True for Simplify3D's `layer 2, Z = 0.4`.
///
/// The number is the whole of the test: the separator alone would take
/// `layer_height = 0.2` and `LAYER_COUNT:33`, both of which merely begin with
/// the word.
fn numbered_layer(text: &str) -> bool {
    let Some((head, tail)) = text.split_at_checked(6) else {
        return false;
    };
    head.eq_ignore_ascii_case("LAYER ")
        && tail
            .trim_start()
            .split([',', ' '])
            .next()
            .is_some_and(|count| !count.is_empty() && count.bytes().all(|b| b.is_ascii_digit()))
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
        assert_eq!(of(";TYPE:Top solid infill"), Some(Feature::TopSurface));
        assert_eq!(of(";TYPE:Ironing"), Some(Feature::Ironing));
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
        assert_eq!(of("; FEATURE: Top surface"), Some(Feature::TopSurface));
        assert_eq!(of("; FEATURE: Bottom surface"), Some(Feature::SolidInfill));
        assert_eq!(
            of("; FEATURE: Internal solid infill"),
            Some(Feature::SolidInfill)
        );
        assert_eq!(of("; FEATURE: Ironing"), Some(Feature::Ironing));
    }

    #[test]
    fn classifies_cura_markers() {
        let of = Feature::from_comment;
        assert_eq!(of(";TYPE:WALL-OUTER"), Some(Feature::ExternalPerimeter));
        assert_eq!(of(";TYPE:WALL-INNER"), Some(Feature::InternalPerimeter));
        assert_eq!(of(";TYPE:FILL"), Some(Feature::SparseInfill));
        assert_eq!(of(";TYPE:SKIN"), Some(Feature::TopSurface));
    }

    /// The two regions Z anti-aliasing reshapes, and nothing else. A top
    /// surface is the one the print leaves facing the air; ironing follows it
    /// and has to follow it in Z as well, or it scrapes what it is smoothing.
    #[test]
    fn the_surface_regions_are_the_top_face_and_the_ironing_over_it() {
        for label in [
            ";TYPE:Top solid infill",
            "; FEATURE: Top surface",
            ";TYPE:SKIN",
            ";TYPE:Ironing",
            "; FEATURE: Ironing",
        ] {
            let feature = Feature::from_comment(label).expect("a region marker");
            assert!(feature.is_surface(), "{label}");
        }
        for label in [
            ";TYPE:Perimeter",
            ";TYPE:External perimeter",
            "; FEATURE: Overhang wall",
            ";TYPE:Solid infill",
            "; FEATURE: Bottom surface",
            ";TYPE:Bridge infill",
            ";TYPE:Internal infill",
            ";TYPE:Skirt/Brim",
        ] {
            let feature = Feature::from_comment(label).expect("a region marker");
            assert!(!feature.is_surface(), "{label}");
        }
    }

    /// The footprint of a layer is what the *object* covers. A skirt, a brim,
    /// a prime tower and support material all put plastic down beside it, and
    /// counting them would put the object's outline wherever the skirt ran.
    #[test]
    fn only_the_object_itself_counts_toward_a_layer_footprint() {
        for label in [
            ";TYPE:Perimeter",
            ";TYPE:External perimeter",
            "; FEATURE: Overhang wall",
            ";TYPE:Internal infill",
            ";TYPE:Solid infill",
            ";TYPE:Top solid infill",
            ";TYPE:Ironing",
            ";TYPE:Bridge infill",
            ";TYPE:SKIN",
            ";TYPE:Gap fill",
        ] {
            let feature = Feature::from_comment(label).expect("a region marker");
            assert!(feature.builds_the_part(), "{label}");
        }
        for label in [
            ";TYPE:Skirt/Brim",
            "; FEATURE: Prime tower",
            ";TYPE:Support material",
            "; FEATURE: Support",
            ";TYPE:Custom",
        ] {
            let feature = Feature::from_comment(label).expect("a region marker");
            assert!(!feature.builds_the_part(), "{label}");
        }
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

    /// Gap fill is laid between two loops of one wall, so a slicer drops its
    /// marker into the middle of that wall. It used to carry the word "fill"
    /// into the infill arm and classify as sparse infill, which ended the
    /// wall's region: the half left without the visible wall in it was then
    /// numbered from a fixed end and came out with its stagger inverted.
    #[test]
    fn gap_fill_is_its_own_class_rather_than_infill() {
        for label in [
            ";TYPE:Gap fill",
            "; FEATURE: Gap infill",
            ";TYPE:GAP FILL",
            "; FEATURE: Gap Fill",
        ] {
            assert_eq!(
                Feature::from_comment(label),
                Some(Feature::GapFill),
                "{label}"
            );
        }
        // Not a wall: it has no loop behind it to alternate with. Not a
        // surface either, so nothing it lays is ever reshaped.
        assert!(!Feature::GapFill.is_perimeter());
        assert!(!Feature::GapFill.is_surface());
        assert!(Feature::GapFill.builds_the_part());
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
    fn folded_prefix_matches_a_lowercased_comparison() {
        for (word, stem) in [
            ("wall", "wall"),
            ("WALL", "wall"),
            ("Perimeters", "perimeter"),
            ("perimeter", "perimeters"),
            ("filling", "fill"),
            ("infill", "fill"),
            ("interface", "internal"),
            ("", "solid"),
            ("solid", ""),
            ("Bridge", "skin"),
        ] {
            assert_eq!(
                starts_fold(word, stem),
                word.to_ascii_lowercase().starts_with(stem),
                "{word:?} starts with {stem:?}"
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

    /// Simplify3D names a region with the bare word and no colon, so nothing
    /// in a file of its output classified: `feature` never became a perimeter,
    /// no loop was ever buffered, and the run exited having changed nothing.
    #[test]
    fn classifies_simplify3d_markers() {
        let of = Feature::from_comment;
        assert_eq!(
            of("; feature outer perimeter"),
            Some(Feature::ExternalPerimeter)
        );
        assert_eq!(
            of("; feature inner perimeter"),
            Some(Feature::InternalPerimeter)
        );
        assert_eq!(of("; feature solid layer"), Some(Feature::SolidInfill));
        assert_eq!(of("; feature infill"), Some(Feature::SparseInfill));
        assert_eq!(of("; feature bridge"), Some(Feature::SolidInfill));
        assert_eq!(of("; feature gap fill"), Some(Feature::GapFill));
        assert_eq!(of("; feature skirt"), Some(Feature::Other));
        assert_eq!(of("; feature support"), Some(Feature::Other));
        // The colon-less form must not swallow the colon one, which four other
        // slicers write.
        assert_eq!(
            of("; FEATURE: Inner wall"),
            Some(Feature::InternalPerimeter)
        );
    }

    /// `; layer 2, Z = 0.4` carries an `=`, so a caller that read it as a
    /// setting would book a layer as one. It has to answer as a layer marker,
    /// and the word alone must not be enough to make one: `layer_height` is a
    /// setting and `LAYER_COUNT` is a count.
    #[test]
    fn a_simplify3d_layer_marker_is_a_layer_and_a_setting_is_not() {
        assert!(is_layer_change("; layer 2, Z = 0.4"));
        assert!(is_layer_change("; layer 0, Z = 0.2"));
        assert!(is_layer_change("; LAYER 137, Z = 27.400"));
        assert!(!is_layer_change("; layer_height = 0.2"));
        assert!(!is_layer_change(";LAYER_COUNT:33"));
        assert!(!is_layer_change("; layer"));
        assert!(!is_layer_change("; layer , Z = 0.4"));
        // And it is not a region either, or it would end the wall it opens.
        assert_eq!(Feature::from_comment("; layer 2, Z = 0.4"), None);
    }

    /// A region marker is only ever a bare comment line. Accepting a prefix
    /// without a colon widens what counts as one, and a trailing comment on a
    /// move must still not classify.
    #[test]
    fn a_trailing_comment_is_not_a_region_marker() {
        assert_eq!(
            Feature::from_comment("G1 X1 Y1 ; feature outer perimeter"),
            None
        );
        assert_eq!(
            Feature::from_comment("G1 Z0.4 ; corbel feature infill"),
            None
        );
        // The word on its own, with no label behind it, names no region.
        assert_eq!(Feature::from_comment("; feature"), None);
        assert_eq!(Feature::from_comment("; features = 3"), None);
    }

    /// A thin wall is a feature too narrow for two loops, printed as one bead.
    /// It used to classify as `Other`, which said it was not the part: its
    /// extrusions were left out of the footprint the surface grid is sized
    /// from and out of the coverage sets capping is measured against, so a
    /// part whose narrow features are thin walls was measured as if they were
    /// not there.
    ///
    /// It is still not a perimeter. Its two faces are both the visible one, so
    /// raising it puts a step on the outside — the same reason an overhang is
    /// never raised on its own evidence. Nor is it a surface: `is_surface` is
    /// the face the print leaves to the air, and a single upright bead is not
    /// one, which leaves it on the plane the slicer chose.
    #[test]
    fn a_thin_wall_is_part_of_the_object_without_being_a_wall_of_it() {
        for label in [
            ";TYPE:Thin wall",
            ";TYPE:THIN WALL",
            "; FEATURE: Thin wall",
            ";TYPE:External thin wall",
        ] {
            assert_eq!(
                Feature::from_comment(label),
                Some(Feature::ThinWall),
                "{label}"
            );
        }
        assert!(Feature::ThinWall.builds_the_part());
        assert!(!Feature::ThinWall.is_perimeter());
        assert!(!Feature::ThinWall.is_surface());
    }

    /// A dialect is half-supported until an unseen spelling lands where a seen
    /// one does, and half-supported is worse than plainly unsupported: the
    /// file still runs, some of its regions classify, and a wall gets numbered
    /// against a picture with holes in it.
    ///
    /// Simplify3D is the case that forced this. Its `outer perimeter` was
    /// added here from a description rather than from a slice, and nothing
    /// else of its vocabulary was, so the rest of a file of it read as
    /// `Other`. Rather than guess at a longer list of phrases, the classifier
    /// reads the words: every slicer names the same handful of things and they
    /// all reach for the same words in a different order with a different
    /// separator. The rows below that no slicer here is known to emit —
    /// `Outer shell`, `Internal wall`, `Top skin`, `Bottom skin`, `Solid
    /// fill`, `Sparse fill`, `Gap filling`, `Thin walls`, `Ironing pass`,
    /// `WALL_OUTER`, `perimeters` — are the point of it: each is a spelling of
    /// something every slicer names, and each has to land on the right variant
    /// the day a dialect arrives carrying it.
    #[test]
    fn an_unseen_spelling_of_a_known_region_lands_on_the_right_variant() {
        for (label, expected) in [
            (";TYPE:External perimeter", Feature::ExternalPerimeter),
            ("; FEATURE: Outer wall", Feature::ExternalPerimeter),
            (";TYPE:WALL-OUTER", Feature::ExternalPerimeter),
            ("; feature outer perimeter", Feature::ExternalPerimeter),
            (";TYPE:Outer shell", Feature::ExternalPerimeter),
            (";TYPE:WALL_OUTER", Feature::ExternalPerimeter),
            ("; FEATURE: Exterior perimeter", Feature::ExternalPerimeter),
            (";TYPE:Internal wall", Feature::InternalPerimeter),
            (";TYPE:Inner shell", Feature::InternalPerimeter),
            ("; feature perimeters", Feature::InternalPerimeter),
            (";TYPE:Overhang perimeter", Feature::Overhang),
            (";TYPE:Overhang shell", Feature::Overhang),
            (";TYPE:Internal solid infill", Feature::SolidInfill),
            (";TYPE:Solid fill", Feature::SolidInfill),
            (";TYPE:Bottom skin", Feature::SolidInfill),
            (";TYPE:Top solid infill", Feature::TopSurface),
            (";TYPE:Top skin", Feature::TopSurface),
            (";TYPE:Sparse fill", Feature::SparseInfill),
            (";TYPE:Gap filling", Feature::GapFill),
            (";TYPE:Thin walls", Feature::ThinWall),
            (";TYPE:Ironing pass", Feature::Ironing),
        ] {
            assert_eq!(Feature::from_comment(label), Some(expected), "{label}");
        }

        // A region that is not the object keeps its own answer even where it
        // carries another region's word — a raft is not a wall of the part,
        // and bricking one would move material nothing accounts for.
        for label in [
            ";TYPE:Raft perimeter",
            ";TYPE:Support interface",
            "; FEATURE: Skirt wall",
            ";TYPE:Wipe tower",
            ";TYPE:Ooze shield",
        ] {
            assert_eq!(
                Feature::from_comment(label),
                Some(Feature::Other),
                "{label}"
            );
        }
    }

    /// A label that classifies as `Other` used to leave no trace anywhere, so
    /// a file written in a dialect this module has never met ran to completion
    /// looking exactly like a file with nothing to do in it. This is the means
    /// for a caller to say otherwise, and it reports only the labels that are
    /// genuinely unknown: a skirt and a prime tower are understood, and a
    /// warning naming them on every file would be noise that hides the one
    /// that matters.
    #[test]
    fn a_label_this_module_cannot_name_is_reportable() {
        assert_eq!(unrecognised_region("TYPE:Wibble"), Some("Wibble"));
        assert_eq!(
            unrecognised_region(" FEATURE: Something Else"),
            Some("Something Else")
        );
        assert_eq!(unrecognised_region(" feature ooze pass"), Some("ooze pass"));

        // Every region that classifies, and every one that is understood as
        // not being the part, stays silent.
        for marker in [
            "TYPE:External perimeter",
            "TYPE:Perimeter",
            " FEATURE: Inner wall",
            "TYPE:Top solid infill",
            "TYPE:Gap fill",
            "TYPE:Skirt/Brim",
            " FEATURE: Prime tower",
            "TYPE:Support material",
            "TYPE:Custom",
        ] {
            assert_eq!(unrecognised_region(marker), None, "{marker}");
        }

        // Not a region marker at all, and a marker with no label in it.
        for marker in ["LAYER_CHANGE", " layer_height = 0.2", "TYPE:", " feature "] {
            assert_eq!(unrecognised_region(marker), None, "{marker}");
        }

        // Borrowed from the caller's own line rather than copied out of it, so
        // counting these costs nothing on a line that has already been read.
        let marker = "TYPE:Nothing I know";
        let label = unrecognised_region(marker).expect("a label");
        assert!(std::ptr::eq(&marker.as_bytes()[5], &label.as_bytes()[0]));
    }

    /// What the two callers of `builds_the_part` ask is one question, not two:
    /// the box the survey grows and the per-layer cells the scout traces are
    /// both *an outline of the object*, and a region left out of one would
    /// have to be left out of the other. So it stays one predicate.
    ///
    /// The price is pinned here. Support material stands under the part and a
    /// wipe tower stands beside it, and this module cannot tell them apart —
    /// both are `Other`. That is right for an outline, since neither outline
    /// is the object's, and it means a caller asking the different question
    /// "is any plastic standing here at all" cannot be served from this enum
    /// until support has a variant of its own. Widening `builds_the_part` to
    /// serve such a caller would put the wipe tower in the box.
    #[test]
    fn support_and_a_wipe_tower_are_the_same_answer_here_and_neither_is_the_part() {
        for label in [
            ";TYPE:Support material",
            "; FEATURE: Support",
            ";TYPE:SUPPORT-INTERFACE",
            ";TYPE:Wipe tower",
            "; FEATURE: Prime tower",
        ] {
            let feature = Feature::from_comment(label).expect("a region marker");
            assert_eq!(feature, Feature::Other, "{label}");
            assert!(!feature.builds_the_part(), "{label}");
        }
    }
}
