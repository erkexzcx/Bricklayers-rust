use std::ops::RangeInclusive;
use std::path::PathBuf;

use bricklayers::brick;
use bricklayers::slicer::WallOrder;
use clap::{Args, Parser, Subcommand, ValueEnum};

/// What `--version` reports. The release workflow stamps the published GitHub
/// tag in, so the version is whatever that release was called; nothing in the
/// source tracks it, and a build from source has no release behind it.
const VERSION: &str = match option_env!("BRICKLAYERS_VERSION") {
    Some(tag) => tag,
    None => "dev",
};

/// Post-process sliced G-code so layers interlock instead of stacking as flat
/// sheets.
///
/// Add the chosen sub-command to your slicer's post-processing scripts field;
/// the slicer appends the G-code path automatically.
#[derive(Debug, Parser)]
#[command(name = "bricklayers", version = VERSION, about, long_about = None)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Raise every other internal perimeter loop by half a layer height.
    Brick(BrickArgs),
}

#[derive(Debug, Args)]
pub struct Common {
    /// G-code file to process.
    #[arg(value_name = "GCODE")]
    pub input: PathBuf,

    /// Write here instead of overwriting the input.
    #[arg(short, long, value_name = "PATH")]
    pub output: Option<PathBuf>,

    /// Report what was changed.
    #[arg(short, long)]
    pub verbose: bool,

    /// Run even if this file already carries the transform's marks.
    #[arg(long)]
    pub force: bool,
}

/// What `--extrusion-multiplier` accepts.
///
/// Below 1.0 would starve the seam the raise opens, which is the opposite of
/// the point. Above 1.3 puts more into one loop than the gap beside it can
/// take, so it blobs and the nozzle starts dragging through it.
const EXTRUSION_MULTIPLIER: RangeInclusive<f64> = 1.0..=1.3;

/// Every one of these ends up as a coordinate a printer will act on, so a
/// value that is not a finite number inside its range is refused before any
/// work starts rather than written into the G-code.
fn within(value: &str, range: RangeInclusive<f64>) -> Result<f64, String> {
    let number: f64 = value
        .parse()
        .map_err(|_| format!("`{value}` is not a number"))?;
    if !number.is_finite() {
        return Err(format!("`{value}` is not a finite number"));
    }
    if !range.contains(&number) {
        return Err(format!(
            "{number} is outside {}..={}",
            range.start(),
            range.end()
        ));
    }
    Ok(number)
}

fn extrusion_multiplier(value: &str) -> Result<f64, String> {
    within(value, EXTRUSION_MULTIPLIER)
}

/// Wider than any nozzle prints, so only a typo or a unit mix-up is refused.
fn layer_height(value: &str) -> Result<f64, String> {
    within(value, 0.01..=2.0)
}

#[derive(Debug, Args)]
pub struct BrickArgs {
    #[command(flatten)]
    pub common: Common,

    /// Layer height in mm, forced onto every layer. Measured from the file
    /// when omitted, which is the only right answer for an adaptive slice.
    #[arg(long, value_parser = layer_height, value_name = "MM")]
    pub layer_height: Option<f64>,

    /// Extrusion scale for raised loops on middle layers. Accepts 1.0 to 1.3.
    #[arg(
        long,
        default_value = "1.0",
        value_parser = extrusion_multiplier,
        value_name = "FACTOR"
    )]
    pub extrusion_multiplier: f64,

    /// Order the slicer prints a region's walls in. Detected from the slicer's
    /// settings and the file's own config block; set it only to override that.
    #[arg(long, value_enum, default_value_t = WallOrderArg::Auto, value_name = "ORDER")]
    pub wall_order: WallOrderArg,

    /// Print each layer's unraised loops before its raised ones, grouping them
    /// by height instead of alternating. A height change rides a move the
    /// slicer was already making, so this no longer saves anything.
    #[arg(long)]
    pub reorder_loops: bool,
}

/// Which end of a wall the loop numbering starts from.
///
/// Both directions have to be forceable, not just external-first: detection
/// reads prose a slicer wrote (`wall_sequence = outer wall/inner wall`), and an
/// unrecognised dialect can land on either answer. Getting it wrong doubles the
/// stagger inversions on a real file, so there is an override for each way.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum WallOrderArg {
    /// Take it from the slicer's settings, then from the file.
    #[default]
    Auto,
    /// The external perimeter is printed before the loops behind it.
    ExternalFirst,
    /// The loops behind the external perimeter are printed first. This is what
    /// every mainstream slicer does by default.
    InternalFirst,
}

impl WallOrderArg {
    /// `None` when the choice is left to detection.
    pub fn chosen(self) -> Option<WallOrder> {
        match self {
            Self::Auto => None,
            Self::ExternalFirst => Some(WallOrder::ExternalFirst),
            Self::InternalFirst => Some(WallOrder::InternalFirst),
        }
    }
}

impl Command {
    pub fn common(&self) -> &Common {
        match self {
            Command::Brick(args) => &args.common,
        }
    }
}

impl From<&BrickArgs> for brick::Config {
    fn from(args: &BrickArgs) -> Self {
        Self {
            layer_height: args.layer_height,
            extrusion_multiplier: args.extrusion_multiplier,
            external_perimeters_first: args.wall_order.chosen() == Some(WallOrder::ExternalFirst),
            reorder_loops: args.reorder_loops,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn brick_defaults() {
        let cli = Cli::parse_from(["bricklayers", "brick", "part.gcode"]);
        let Command::Brick(args) = &cli.command;
        let config = brick::Config::from(args);
        assert_eq!(config.extrusion_multiplier, 1.0);
        assert!(!config.reorder_loops);
        assert!(!config.external_perimeters_first);
        assert_eq!(args.wall_order, WallOrderArg::Auto);
        assert_eq!(args.wall_order.chosen(), None);
        assert_eq!(config.layer_height, None);
        assert_eq!(cli.command.common().input, PathBuf::from("part.gcode"));
    }

    /// The old `--external-perimeters-first` could only turn the order on, so a
    /// file whose settings were misread as external-first had no way back.
    #[test]
    fn either_wall_order_can_be_forced() {
        for (argument, expected) in [
            ("auto", None),
            ("external-first", Some(WallOrder::ExternalFirst)),
            ("internal-first", Some(WallOrder::InternalFirst)),
        ] {
            let cli = Cli::parse_from([
                "bricklayers",
                "brick",
                "--wall-order",
                argument,
                "part.gcode",
            ]);
            let Command::Brick(args) = &cli.command;
            assert_eq!(
                args.wall_order.chosen(),
                expected,
                "--wall-order {argument}"
            );
        }

        assert!(
            Cli::try_parse_from([
                "bricklayers",
                "brick",
                "--wall-order",
                "sideways",
                "p.gcode"
            ])
            .is_err()
        );
    }

    #[test]
    fn the_extrusion_multiplier_is_held_to_its_range() {
        for accepted in ["1.0", "1.05", "1.3"] {
            let cli = Cli::parse_from([
                "bricklayers",
                "brick",
                "--extrusion-multiplier",
                accepted,
                "part.gcode",
            ]);
            let Command::Brick(args) = &cli.command;
            assert_eq!(args.extrusion_multiplier, accepted.parse::<f64>().unwrap());
        }
        for rejected in ["0.9", "1.31", "2", "thick"] {
            let parsed = Cli::try_parse_from([
                "bricklayers",
                "brick",
                "--extrusion-multiplier",
                rejected,
                "part.gcode",
            ]);
            assert!(parsed.is_err(), "{rejected} should be rejected");
        }
    }

    #[test]
    fn brick_takes_the_layer_height() {
        let cli = Cli::parse_from([
            "bricklayers",
            "brick",
            "--layer-height",
            "0.2",
            "part.gcode",
        ]);
        let Command::Brick(args) = &cli.command;
        assert_eq!(brick::Config::from(args).layer_height, Some(0.2));
    }

    /// The layer laid on the bed is never raised now, so the first layer's own
    /// height no longer changes any output and the flag that set it is gone.
    #[test]
    fn the_first_layer_height_is_no_longer_an_argument() {
        assert!(
            Cli::try_parse_from([
                "bricklayers",
                "brick",
                "--first-layer-height",
                "0.3",
                "part.gcode",
            ])
            .is_err()
        );
    }

    #[test]
    fn a_file_argument_is_required() {
        assert!(Cli::try_parse_from(["bricklayers", "brick"]).is_err());
        assert!(Cli::try_parse_from(["bricklayers"]).is_err());
    }

    /// The transform this replaced is gone, and the arguments that only it took
    /// went with it. Anything still passing them has to be told, not silently
    /// given a file back untouched.
    #[test]
    fn the_wave_transform_is_no_longer_a_command() {
        assert!(Cli::try_parse_from(["bricklayers", "wave", "part.gcode"]).is_err());
        for gone in [
            "--amplitude=0.3",
            "--frequency=1.1",
            "--resolution=0.2",
            "--max-step=0.1",
            "--infill",
            "--alternate-loops",
        ] {
            assert!(
                Cli::try_parse_from(["bricklayers", "brick", gone, "part.gcode"]).is_err(),
                "brick {gone} should be rejected"
            );
        }
    }

    /// Every one of these becomes a coordinate a printer acts on, and each was
    /// once accepted: `--layer-height nan` put `ZNaN` in the file, and a
    /// negative layer height drove the nozzle into the bed.
    #[test]
    fn numeric_arguments_refuse_what_a_printer_cannot_act_on() {
        let brick = [
            "--layer-height=0",
            "--layer-height=nan",
            "--layer-height=-0.4",
            "--layer-height=inf",
            "--layer-height=3",
            "--extrusion-multiplier=0.9",
            "--extrusion-multiplier=nan",
        ];
        for rejected in brick {
            assert!(
                Cli::try_parse_from(["bricklayers", "brick", rejected, "part.gcode"]).is_err(),
                "brick {rejected} should be rejected"
            );
        }
    }

    #[test]
    fn the_settings_a_print_actually_uses_are_still_accepted() {
        for accepted in ["--layer-height=0.2", "--extrusion-multiplier=1.05"] {
            assert!(
                Cli::try_parse_from(["bricklayers", "brick", accepted, "part.gcode"]).is_ok(),
                "brick {accepted} should be accepted"
            );
        }
        // The defaults have to survive their own parsers.
        Cli::parse_from(["bricklayers", "brick", "part.gcode"]);
    }
}
