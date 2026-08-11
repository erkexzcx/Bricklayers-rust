mod cli;

use std::process::ExitCode;

use bricklayers::scan::Survey;
use bricklayers::slicer::{self, WallOrder};
use bricklayers::{Error, Result, Source, brick};
use clap::Parser;
use cli::Cli;

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(&cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("bricklayers: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: &Cli) -> Result<()> {
    let slicer = slicer::Settings::from_env();
    let source = Source::open(&cli.input)?;

    warn_slicer_settings(&slicer);
    if cli.verbose {
        if source.is_binary() {
            eprintln!("bricklayers: binary G-code container");
        }
        if let Some(name) = &slicer.output_name {
            eprintln!("bricklayers: slicer will save this as {name}");
        }
    }

    let survey = source.survey()?;
    if survey.bricked && !cli.force {
        return Err(Error::AlreadyProcessed {
            path: cli.input.clone(),
        });
    }

    let sink = source.sink(cli.output.as_ref().unwrap_or(&cli.input))?;
    let config = resolve(
        brick::Config {
            // The flag is a percentage; everything inside is a fraction.
            extra_flow: cli.extra_flow / 100.0,
            ..brick::Config::default()
        },
        &slicer,
        &source,
        &survey,
    );

    let stats = source.rewrite(sink, |reader, writer| {
        brick::stream(reader, writer, &config, &survey)
    })?;

    warn_layer_height(stats.layer_height, stats.layer_height_detected);
    warn_step(&stats, slicer.nozzle.or(survey.nozzle).or(source.nozzle()));
    if cli.verbose {
        if survey.objects() > 1 {
            eprintln!(
                "bricklayers: {} objects printed one after another, each built \
                 from the bed up",
                survey.objects()
            );
        }
        if survey.variable_layers() {
            eprintln!(
                "bricklayers: the slicer varied the layer height, so each layer \
                 is raised by half of its own"
            );
        }
        eprintln!(
            "bricklayers: {} layers, {} internal loops, {} raised by {}",
            stats.layers,
            stats.loops,
            stats.raised,
            raised_by(&stats)
        );
        if stats.capped > 0 {
            eprintln!(
                "bricklayers: {} more were left flat where the wall ends and \
                 something is printed over it",
                stats.capped
            );
        }
        if config.wall_width.is_none() {
            eprintln!(
                "bricklayers: the file states no internal wall width, so the flow \
                 below is the shipped default rather than this print's own geometry"
            );
        }
        report_filament(&stats, &applied(&config, &stats));
    }

    Ok(())
}

/// Fills in what the file and the slicer know: the layer height, and which end
/// of a wall the loop numbering starts from.
fn resolve(
    mut config: brick::Config,
    slicer: &slicer::Settings,
    source: &Source,
    survey: &Survey,
) -> brick::Config {
    config.layer_height = if survey.variable_layers() {
        // A nominal says what the slicer was asked for, not what each layer
        // came out at, so it cannot stand in for a file that measures several
        // heights.
        None
    } else {
        slicer.layer_height.or(source.layer_height())
    };
    config.external_perimeters_first =
        slicer.wall_order.or(survey.wall_order) == Some(WallOrder::ExternalFirst);
    config.wall_width = slicer
        .wall_width
        .or_else(|| source.wall_width())
        .or(survey.wall_width);
    config
}

/// Slicer settings under which the transform quietly does nothing, or the
/// wrong thing. Only ever reached when a slicer ran us, since they come from
/// the environment it exports.
fn warn_slicer_settings(slicer: &slicer::Settings) {
    if slicer.spiral_vase == Some(true) {
        eprintln!(
            "bricklayers: warning: spiral vase mode is on; it prints one continuously \
             rising wall, so there are no layer boundaries to interlock"
        );
    }
    if let Some(walls) = slicer.walls
        && walls < 2
    {
        eprintln!(
            "bricklayers: warning: {walls} wall(s) per region leaves no internal \
             perimeter behind the visible one, so there is nothing to raise; \
             bricking needs two walls or more"
        );
    }
}

fn warn_layer_height(height: f64, detected: bool) {
    if !detected {
        eprintln!("bricklayers: warning: no layer height found in the file, assuming {height} mm");
    }
}

/// A step this tool leaves standing that is large next to the nozzle laying
/// the layer above it.
///
/// The stagger is half a layer, so the step grows with the layer height while
/// the nozzle that has to clear it does not. Nothing here can be done about it
/// without giving up the stagger, so it is said rather than acted on: slicing
/// thinner is the answer, and it is the user's to make.
fn warn_step(stats: &brick::Stats, nozzle: Option<f64>) {
    let Some(nozzle) = nozzle.filter(|nozzle| *nozzle > 0.0) else {
        return;
    };
    let Some((_, step)) = stats.raise else {
        return;
    };
    if step > nozzle / 4.0 {
        eprintln!(
            "bricklayers: warning: loops are raised by up to {step:.3} mm against a \
             {nozzle} mm nozzle; a layer more than half the nozzle leaves a step the \
             nozzle drags through, so slice thinner if the walls come out rough"
        );
    }
}

/// What `--verbose` calls the flow the walls were metered at: one figure, or
/// the range an adaptive slice covers, since it follows each layer's own
/// height. A modifier is named too, or the figure looks like the geometry's
/// own answer when it is not.
fn applied(config: &brick::Config, stats: &brick::Stats) -> String {
    let flow = match stats.flow {
        Some((low, high)) if format!("{low:.3}") != format!("{high:.3}") => {
            format!("a flow of {low:.3} to {high:.3}")
        }
        Some((low, _)) => format!("a flow of {low:.3}"),
        None => return "the flow".to_owned(),
    };
    if config.extra_flow == brick::DEFAULT_EXTRA_FLOW {
        flow
    } else {
        format!("{flow} (--extra-flow {:.1}%)", config.extra_flow * 100.0)
    }
}

/// How far loops were raised, as one figure or as the range an adaptive slice
/// covers. A single number is what a file whose layers vary cannot honestly
/// report, and reading one is what sends a user looking for a bug.
fn raised_by(stats: &brick::Stats) -> String {
    let Some((low, high)) = stats.raise else {
        return format!("{:.3} mm", stats.layer_height / 2.0);
    };
    let (low, high) = (format!("{low:.3}"), format!("{high:.3}"));
    if low == high {
        format!("{low} mm")
    } else {
        format!("{low} to {high} mm")
    }
}

/// Prices what was applied against the whole part, since a multiplier reads
/// far larger than it costs: it is paid only on the walls no one sees.
fn report_filament(stats: &brick::Stats, applied: &str) {
    if stats.filament <= 0.0 {
        return;
    }
    let share = 100.0 * stats.raised_filament / stats.filament;
    let added = 100.0 * stats.multiplier_filament / stats.filament;
    eprintln!(
        "bricklayers: {:.1} mm filament, {share:.1}% of it in raised loops; \
         {applied} adds {added:.2}% to the part",
        stats.filament
    );
}
