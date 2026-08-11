use std::ops::RangeInclusive;
use std::path::PathBuf;

use bricklayers::brick;
use clap::Parser;

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
/// Every other internal perimeter loop is raised by half a layer height. Put
/// the binary's path in your slicer's post-processing scripts field and
/// nothing else; the slicer appends the G-code path automatically, and
/// everything the transform needs is read from the file.
#[derive(Debug, Parser)]
#[command(name = "bricklayers", version = VERSION, about, long_about = None)]
pub struct Cli {
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

    /// Extra flow every wall takes, as a percentage, for a layer as thick as
    /// your nozzle. A layer half the nozzle takes about half of it, so the
    /// default 5 gives about 2.5% on a 0.2 mm layer through a 0.4 mm nozzle.
    /// Accepts 0 to 50; 0 meters every bead as sliced and only raises them.
    #[arg(
        long,
        default_value_t = brick::DEFAULT_EXTRA_FLOW * 100.0,
        value_parser = extra_flow,
        value_name = "PERCENT"
    )]
    pub extra_flow: f64,
}

/// What `--extra-flow` accepts, in percent.
const EXTRA_FLOW: RangeInclusive<f64> =
    brick::MIN_EXTRA_FLOW * 100.0..=brick::MAX_EXTRA_FLOW * 100.0;

/// It ends up as extruded plastic, so a value that is not a finite number
/// inside its range is refused before any work starts rather than written into
/// the G-code.
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

fn extra_flow(value: &str) -> Result<f64, String> {
    within(value, EXTRA_FLOW)
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
    fn defaults() {
        let cli = Cli::parse_from(["bricklayers", "part.gcode"]);
        assert_eq!(cli.input, PathBuf::from("part.gcode"));
        assert_eq!(cli.output, None);
        assert!(!cli.verbose);
        assert!(!cli.force);
        assert_eq!(cli.extra_flow, 5.0);
    }

    /// The dial is a percentage a reader can act on — the extra a wall takes
    /// where the layer is as thick as the nozzle — rather than a multiplier
    /// over some number they cannot see. Zero is a real setting: the raise
    /// with every bead metered as sliced.
    #[test]
    fn the_extra_flow_is_held_to_its_range() {
        for accepted in ["0", "2.5", "5", "12", "50"] {
            let cli = Cli::parse_from(["bricklayers", "--extra-flow", accepted, "part.gcode"]);
            assert_eq!(cli.extra_flow, accepted.parse::<f64>().unwrap());
        }
        // A bare `-1` is refused by clap as an unknown flag before the range
        // is ever consulted, so the negatives are spelled with an `=` to prove
        // the range check itself has teeth.
        for rejected in ["-0.1", "50.1", "-1", "nan", "inf", "more"] {
            assert!(
                Cli::try_parse_from([
                    "bricklayers",
                    &format!("--extra-flow={rejected}"),
                    "part.gcode"
                ])
                .is_err(),
                "{rejected} should be rejected"
            );
        }
    }

    /// Everything that decides how a wall is metered — the layer height, the
    /// width it was laid at, the wall order, how much extra flow the geometry
    /// asks for — is read from the file and the slicer, so none of it is an
    /// argument. A file that still passes one has to be told rather than
    /// silently given a different result.
    ///
    /// `--wall-flow` and `--extrusion-multiplier` are on this list because
    /// they pinned an absolute flow, and the flow is not a constant: it
    /// follows each layer's own height, which on an adaptive slice changes
    /// every layer. `--extra-flow` names the slope of that answer rather than
    /// for instead, which leaves the derivation doing its job.
    #[test]
    fn what_the_file_states_is_not_an_argument() {
        for gone in [
            "--layer-height=0.2",
            "--first-layer-height=0.3",
            "--wall-order=external-first",
            "--extrusion-scope=internal-walls",
            "--reorder-loops",
            "--wall-flow=1.05",
            "--extrusion-multiplier=1.05",
        ] {
            assert!(
                Cli::try_parse_from(["bricklayers", gone, "part.gcode"]).is_err(),
                "{gone} should be rejected"
            );
        }
    }

    /// A G-code path, three flags about where the result goes, and one dial
    /// over the flow. Everything else is read from the file, so a run is
    /// reproducible from the G-code alone.
    #[test]
    fn the_whole_command_line_is_a_file_three_flags_and_a_dial() {
        let command = Cli::command();
        let mut named: Vec<&str> = command
            .get_arguments()
            .filter_map(|arg| arg.get_long())
            .collect();
        named.sort_unstable();
        assert_eq!(named, ["extra-flow", "force", "output", "verbose"]);
        assert_eq!(command.get_subcommands().count(), 0);
    }

    /// There was a `brick` sub-command, and there is nothing to choose between
    /// any more, so the file is the only positional argument. A stale slicer
    /// line still passing the old word is told rather than handed a file back
    /// untouched — or worse, told to process a file called `brick`.
    #[test]
    fn the_brick_sub_command_is_gone() {
        assert!(Cli::try_parse_from(["bricklayers", "brick", "part.gcode"]).is_err());
        assert_eq!(
            Cli::parse_from(["bricklayers", "brick"]).input,
            PathBuf::from("brick"),
            "on its own it can only be read as a filename"
        );
    }

    #[test]
    fn a_file_argument_is_required() {
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
                Cli::try_parse_from(["bricklayers", gone, "part.gcode"]).is_err(),
                "{gone} should be rejected"
            );
        }
    }
}
