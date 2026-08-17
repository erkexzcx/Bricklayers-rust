use std::ops::RangeInclusive;
use std::path::PathBuf;

use clap::{ArgGroup, Parser};
use corbel::brick;

/// What `--version` reports. The release workflow stamps the published GitHub
/// tag in, so the version is whatever that release was called; nothing in the
/// source tracks it, and a build from source has no release behind it.
const VERSION: &str = match option_env!("CORBEL_VERSION") {
    Some(tag) => tag,
    None => "dev",
};

/// Post-process sliced G-code with either or both of two transforms:
/// bricklayering, which makes layers interlock instead of stacking as flat
/// sheets, and Z anti-aliasing, which follows the model's surface inside a
/// layer so a shallow top ramps instead of stepping.
///
/// Name at least one of --bricks and --zaa; name both to run both. Everything
/// else either transform needs is read from the file, so the rest of the
/// command line is only about where the result goes.
///
/// In a slicer's post-processing scripts field, put the binary's path followed
/// by the transforms you want — the slicer appends the G-code path itself.
#[derive(Debug, Parser)]
#[command(name = "corbel", version = VERSION, about, long_about = None)]
#[command(group = ArgGroup::new(TRANSFORMS).args(["bricks", "zaa"]).multiple(true).required(true))]
pub struct Cli {
    /// G-code file to process.
    #[arg(value_name = "GCODE")]
    pub input: PathBuf,

    /// Write here instead of overwriting the input.
    #[arg(short, long, value_name = "PATH", value_parser = destination)]
    pub output: Option<PathBuf>,

    /// Report what was changed.
    #[arg(short, long)]
    pub verbose: bool,

    /// Run even if this file already carries a transform's marks, or does not
    /// read as G-code at all.
    #[arg(long)]
    pub force: bool,

    /// Raise alternate internal perimeter loops by half a layer, so the seams
    /// between them stagger and no longer line up into a channel through the
    /// wall. Combines with --zaa.
    #[arg(long, help_heading = BRICKS)]
    pub bricks: bool,

    /// Extra flow every wall takes, as a percentage, for a layer as thick as
    /// your nozzle. A layer half the nozzle takes about half of it, so the
    /// default 5 gives about 2.5% on a 0.2 mm layer through a 0.4 mm nozzle.
    /// Accepts 0 to 50; 0 meters every bead as sliced and only raises them.
    #[arg(
        long,
        default_value_t = brick::DEFAULT_EXTRA_FLOW * 100.0,
        value_parser = extra_flow,
        value_name = "PERCENT",
        help_heading = BRICKS
    )]
    pub extra_flow: f64,

    /// Follow the model's surface within a layer, so a shallow top comes out
    /// as a ramp rather than a staircase. Combines with --bricks.
    #[arg(long, help_heading = ZAA)]
    pub zaa: bool,
}

const BRICKS: &str = "Bricklayering";
const ZAA: &str = "Z anti-aliasing";

/// Names the two switches as a group so clap refuses a run that asks for
/// neither. What this tool does to a file is not something to be guessed at on
/// the user's behalf: a default would silently apply a transform to a print
/// nobody asked to have it applied to.
const TRANSFORMS: &str = "transforms";

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

/// Where `--output` may point.
///
/// A rewrite writes its temporary beside its target, so a path naming a
/// directory puts `/spool/prints.tmp` — a whole print's worth of G-code — in
/// that directory's PARENT, which the user never named, and only fails on the
/// rename once every byte of it has been written. The path is judged before
/// any of that starts: a trailing separator says a directory outright, and
/// `is_dir` catches the rest.
fn destination(value: &str) -> Result<PathBuf, String> {
    if value.ends_with(std::path::is_separator) {
        return Err(format!(
            "`{value}` ends in a path separator, so it names a directory; \
             name the file to write inside it"
        ));
    }
    let path = PathBuf::from(value);
    if path.is_dir() {
        return Err(format!(
            "`{value}` is a directory; name the file to write inside it"
        ));
    }
    if path.file_name().is_none() {
        return Err(format!("`{value}` does not name a file to write"));
    }
    Ok(path)
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
        let cli = Cli::parse_from(["corbel", "--bricks", "--zaa", "part.gcode"]);
        assert_eq!(cli.input, PathBuf::from("part.gcode"));
        assert_eq!(cli.output, None);
        assert!(!cli.verbose);
        assert!(!cli.force);
        assert_eq!(cli.extra_flow, 5.0);
    }

    /// Each transform is asked for by name, and asking for neither is refused
    /// rather than given a default. What this does to a file is not reversible
    /// from the file, and a slicer field that was only ever meant to brick
    /// must not start contouring surfaces because a release changed its mind.
    #[test]
    fn a_run_has_to_name_at_least_one_transform() {
        let bare = Cli::try_parse_from(["corbel", "part.gcode"]);
        let error = bare.expect_err("naming no transform is refused");
        assert_eq!(
            error.kind(),
            clap::error::ErrorKind::MissingRequiredArgument
        );
        // The message has to name both switches, since it is all a user in a
        // slicer's post-processing field gets to see.
        let message = error.to_string();
        assert!(message.contains("--bricks"), "{message}");
        assert!(message.contains("--zaa"), "{message}");

        let bricks = Cli::parse_from(["corbel", "--bricks", "part.gcode"]);
        assert!(bricks.bricks && !bricks.zaa);

        let zaa = Cli::parse_from(["corbel", "--zaa", "part.gcode"]);
        assert!(!zaa.bricks && zaa.zaa);

        let both = Cli::parse_from(["corbel", "--bricks", "--zaa", "part.gcode"]);
        assert!(both.bricks && both.zaa);
    }

    /// `--output` naming a directory used to run the whole transform and
    /// write the entire result into that directory's PARENT — the temporary
    /// beside `/spool/prints` is `/spool/prints.<token>.tmp` — before the
    /// final rename failed. It is refused before any work starts now.
    #[test]
    fn an_output_that_names_a_directory_is_refused_before_any_work() {
        let directory = std::env::temp_dir();
        let named = directory.to_str().expect("a printable temp directory");
        for refused in [named.to_owned(), format!("{named}/")] {
            let outcome =
                Cli::try_parse_from(["corbel", "--bricks", "-o", refused.as_str(), "part.gcode"]);
            let error = outcome.expect_err("a directory is not a file to write");
            assert!(error.to_string().contains("directory"), "{error}");
        }

        let file = directory.join("out.gcode");
        let cli = Cli::parse_from([
            "corbel",
            "--bricks",
            "-o",
            file.to_str().expect("a printable path"),
            "part.gcode",
        ]);
        assert_eq!(cli.output, Some(file));
    }

    /// A dial belonging to a transform that was not asked for is accepted and
    /// ignored: a slicer field is edited by hand, and refusing the leftover
    /// would fail a print for a word that changes nothing.
    #[test]
    fn a_dial_without_its_transform_is_accepted_and_does_nothing() {
        let cli = Cli::parse_from(["corbel", "--zaa", "--extra-flow=12", "part.gcode"]);
        assert!(cli.zaa && !cli.bricks);
        assert_eq!(cli.extra_flow, 12.0);
    }

    /// The dial is a percentage a reader can act on — the extra a wall takes
    /// where the layer is as thick as the nozzle — rather than a multiplier
    /// over some number they cannot see. Zero is a real setting: the raise
    /// with every bead metered as sliced.
    #[test]
    fn the_extra_flow_is_held_to_its_range() {
        for accepted in ["0", "2.5", "5", "12", "50"] {
            let cli =
                Cli::parse_from(["corbel", "--bricks", "--extra-flow", accepted, "part.gcode"]);
            assert_eq!(cli.extra_flow, accepted.parse::<f64>().unwrap());
        }
        // A bare `-1` is refused by clap as an unknown flag before the range
        // is ever consulted, so the negatives are spelled with an `=` to prove
        // the range check itself has teeth.
        for rejected in ["-0.1", "50.1", "-1", "nan", "inf", "more"] {
            assert!(
                Cli::try_parse_from([
                    "corbel",
                    "--bricks",
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
    ///
    /// `--zaa-reach` and `--zaa-resolution` are on it because both are now
    /// derived: the widest step worth following is a slope, so it comes from
    /// each layer's own height, and how finely a surface is sampled comes from
    /// the grid it is measured on. Neither had an answer a user could supply
    /// better than the file could.
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
            "--zaa-reach=8",
            "--zaa-resolution=0.1",
        ] {
            assert!(
                Cli::try_parse_from(["corbel", "--bricks", gone, "part.gcode"]).is_err(),
                "{gone} should be rejected"
            );
        }
    }

    /// A G-code path, three flags about where the result goes, one switch per
    /// transform and one dial between them. Everything else is read from the
    /// file, so a run is reproducible from the G-code alone.
    #[test]
    fn the_whole_command_line_is_a_file_two_transforms_and_their_dials() {
        let command = Cli::command();
        let mut named: Vec<&str> = command
            .get_arguments()
            .filter_map(|arg| arg.get_long())
            .collect();
        named.sort_unstable();
        assert_eq!(
            named,
            ["bricks", "extra-flow", "force", "output", "verbose", "zaa"]
        );
        assert_eq!(command.get_subcommands().count(), 0);
    }

    /// Each transform's own settings are grouped under its own heading, so a
    /// reader of `--help` can tell which dial belongs to which.
    #[test]
    fn each_transforms_settings_are_grouped_under_its_own_heading() {
        let command = Cli::command();
        let under = |heading: &str| {
            let mut found: Vec<&str> = command
                .get_arguments()
                .filter(|arg| arg.get_help_heading() == Some(heading))
                .filter_map(|arg| arg.get_long())
                .collect();
            found.sort_unstable();
            found
        };
        assert_eq!(under(BRICKS), ["bricks", "extra-flow"]);
        assert_eq!(under(ZAA), ["zaa"]);
        // What is shared stays where a reader looks first.
        let mut shared: Vec<&str> = command
            .get_arguments()
            .filter(|arg| arg.get_help_heading().is_none())
            .filter_map(|arg| arg.get_long())
            .collect();
        shared.sort_unstable();
        assert_eq!(shared, ["force", "output", "verbose"]);
    }

    /// There was a `brick` sub-command, and there is nothing to choose between
    /// any more, so the file is the only positional argument. A stale slicer
    /// line still passing the old word is told rather than handed a file back
    /// untouched — or worse, told to process a file called `brick`.
    #[test]
    fn the_brick_sub_command_is_gone() {
        assert!(Cli::try_parse_from(["corbel", "--bricks", "brick", "part.gcode"]).is_err());
        assert_eq!(
            Cli::parse_from(["corbel", "--bricks", "brick"]).input,
            PathBuf::from("brick"),
            "on its own it can only be read as a filename"
        );
    }

    #[test]
    fn a_file_argument_is_required() {
        assert!(Cli::try_parse_from(["corbel", "--bricks"]).is_err());
        assert!(Cli::try_parse_from(["corbel"]).is_err());
    }

    /// The transform this replaced is gone, and the arguments that only it took
    /// went with it. Anything still passing them has to be told, not silently
    /// given a file back untouched.
    #[test]
    fn the_wave_transform_is_no_longer_a_command() {
        assert!(Cli::try_parse_from(["corbel", "--bricks", "wave", "part.gcode"]).is_err());
        for gone in [
            "--amplitude=0.3",
            "--frequency=1.1",
            "--resolution=0.2",
            "--max-step=0.1",
            "--infill",
            "--alternate-loops",
        ] {
            assert!(
                Cli::try_parse_from(["corbel", "--bricks", gone, "part.gcode"]).is_err(),
                "{gone} should be rejected"
            );
        }
    }
}
