//! Settings the slicer hands to a post-processing script.
//!
//! PrusaSlicer, SuperSlicer, OrcaSlicer and Bambu Studio all export their whole
//! print configuration as `SLIC3R_<OPTION>` before running the script, so the
//! settings a file was sliced with are readable without parsing the file.
//! Bambu renamed many options when it forked PrusaSlicer, so each one is looked
//! up under both spellings.

use std::env;

/// The order a region's walls are printed in, which decides whether the first
/// internal perimeter loop of a layer is the one against the external wall.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WallOrder {
    ExternalFirst,
    InternalFirst,
}

/// Everything the environment told us. Every field is `None` when the tool was
/// run by hand rather than by a slicer.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Settings {
    pub layer_height: Option<f64>,
    /// Slicers print the first layer thicker than the rest by default, so the
    /// bricking of that layer has a different gap to fill.
    pub first_layer_height: Option<f64>,
    pub wall_order: Option<WallOrder>,
    /// Walls per region, external one included.
    pub walls: Option<u32>,
    pub spiral_vase: Option<bool>,
    /// Arc fitting emits extrusions as `G2`/`G3`, which neither transform can
    /// reshape.
    pub arc_fitting: Option<bool>,
    /// Where the slicer will finally put the file. Post-processing scripts are
    /// handed a temporary path, so this is the only name the user recognises.
    pub output_name: Option<String>,
}

impl Settings {
    pub fn from_env() -> Self {
        Self::read(|name| env::var(name).ok())
    }

    /// Reads the settings from an arbitrary lookup, so tests never have to
    /// touch the process environment.
    pub fn read(lookup: impl Fn(&str) -> Option<String>) -> Self {
        let get = |options: &[&str]| -> Option<String> {
            options.iter().find_map(|option| {
                let value = lookup(&format!("SLIC3R_{}", option.to_ascii_uppercase()))?;
                let value = value.trim();
                (!value.is_empty()).then(|| value.to_owned())
            })
        };

        Self {
            layer_height: get(&["layer_height"])
                .and_then(|value| value.parse::<f64>().ok())
                .filter(|height| height.is_finite() && *height > 0.0),
            first_layer_height: get(&["first_layer_height", "initial_layer_print_height"])
                .and_then(|value| value.parse::<f64>().ok())
                .filter(|height| height.is_finite() && *height > 0.0),
            wall_order: get(&["external_perimeters_first", "wall_sequence"])
                .as_deref()
                .map(wall_order),
            walls: get(&["perimeters", "wall_loops"]).and_then(|value| value.parse().ok()),
            spiral_vase: get(&["spiral_vase", "spiral_mode"])
                .as_deref()
                .and_then(flag),
            arc_fitting: get(&["arc_fitting", "enable_arc_fitting"])
                .as_deref()
                .map(arc_fitting),
            output_name: get(&["pp_output_name"]),
        }
    }
}

/// Slic3r serialises booleans as `0` and `1`.
fn flag(value: &str) -> Option<bool> {
    match value.to_ascii_lowercase().as_str() {
        "1" | "true" => Some(true),
        "0" | "false" => Some(false),
        _ => None,
    }
}

/// PrusaSlicer's `external_perimeters_first` is a flag; Orca's `wall_sequence`
/// names the order instead, as in `outer wall/inner wall`.
pub(crate) fn wall_order(value: &str) -> WallOrder {
    let external_first =
        flag(value).unwrap_or_else(|| value.to_ascii_lowercase().starts_with("outer"));
    if external_first {
        WallOrder::ExternalFirst
    } else {
        WallOrder::InternalFirst
    }
}

/// PrusaSlicer's `arc_fitting` names a mode and is `disabled` when off; Orca's
/// `enable_arc_fitting` is a flag.
fn arc_fitting(value: &str) -> bool {
    flag(value).unwrap_or_else(|| !value.eq_ignore_ascii_case("disabled"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(pairs: &[(&str, &str)]) -> Settings {
        let owned: Vec<(String, String)> = pairs
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect();
        Settings::read(|name| {
            owned
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.clone())
        })
    }

    #[test]
    fn an_empty_environment_says_nothing() {
        assert_eq!(read(&[]), Settings::default());
    }

    #[test]
    fn reads_prusaslicer_settings() {
        let settings = read(&[
            ("SLIC3R_LAYER_HEIGHT", "0.25"),
            ("SLIC3R_FIRST_LAYER_HEIGHT", "0.3"),
            ("SLIC3R_EXTERNAL_PERIMETERS_FIRST", "1"),
            ("SLIC3R_PERIMETERS", "3"),
            ("SLIC3R_SPIRAL_VASE", "0"),
            ("SLIC3R_ARC_FITTING", "disabled"),
            ("SLIC3R_PP_OUTPUT_NAME", "/media/sd/part.gcode"),
        ]);
        assert_eq!(settings.layer_height, Some(0.25));
        assert_eq!(settings.first_layer_height, Some(0.3));
        assert_eq!(settings.wall_order, Some(WallOrder::ExternalFirst));
        assert_eq!(settings.walls, Some(3));
        assert_eq!(settings.spiral_vase, Some(false));
        assert_eq!(settings.arc_fitting, Some(false));
        assert_eq!(
            settings.output_name.as_deref(),
            Some("/media/sd/part.gcode")
        );
    }

    #[test]
    fn reads_orca_and_bambu_settings() {
        let settings = read(&[
            ("SLIC3R_LAYER_HEIGHT", "0.16"),
            ("SLIC3R_INITIAL_LAYER_PRINT_HEIGHT", "0.2"),
            ("SLIC3R_WALL_SEQUENCE", "inner wall/outer wall"),
            ("SLIC3R_WALL_LOOPS", "2"),
            ("SLIC3R_SPIRAL_MODE", "1"),
            ("SLIC3R_ENABLE_ARC_FITTING", "1"),
        ]);
        assert_eq!(settings.layer_height, Some(0.16));
        assert_eq!(settings.first_layer_height, Some(0.2));
        assert_eq!(settings.wall_order, Some(WallOrder::InternalFirst));
        assert_eq!(settings.walls, Some(2));
        assert_eq!(settings.spiral_vase, Some(true));
        assert_eq!(settings.arc_fitting, Some(true));
    }

    #[test]
    fn recognises_orcas_wall_orders() {
        assert_eq!(
            wall_order("outer wall/inner wall"),
            WallOrder::ExternalFirst
        );
        assert_eq!(
            wall_order("inner wall/outer wall"),
            WallOrder::InternalFirst
        );
        assert_eq!(
            wall_order("inner-outer-inner wall"),
            WallOrder::InternalFirst
        );
    }

    #[test]
    fn prusaslicer_arc_modes_other_than_disabled_are_on() {
        assert!(arc_fitting("emit_center"));
        assert!(arc_fitting("bambu"));
        assert!(!arc_fitting("disabled"));
    }

    #[test]
    fn rejects_layer_heights_that_are_not_a_length() {
        assert_eq!(read(&[("SLIC3R_LAYER_HEIGHT", "0")]).layer_height, None);
        assert_eq!(read(&[("SLIC3R_LAYER_HEIGHT", "-0.2")]).layer_height, None);
        assert_eq!(read(&[("SLIC3R_LAYER_HEIGHT", "nan")]).layer_height, None);
        assert_eq!(read(&[("SLIC3R_LAYER_HEIGHT", "auto")]).layer_height, None);
        assert_eq!(
            read(&[("SLIC3R_FIRST_LAYER_HEIGHT", "0")]).first_layer_height,
            None
        );
    }

    #[test]
    fn blank_values_count_as_unset() {
        assert_eq!(read(&[("SLIC3R_LAYER_HEIGHT", "  ")]).layer_height, None);
        assert_eq!(read(&[("SLIC3R_PP_OUTPUT_NAME", "")]).output_name, None);
    }
}
