mod cli;

use std::process::ExitCode;

use bricklayers::scan::Survey;
use bricklayers::slicer::{self, WallOrder};
use bricklayers::{Error, Result, Source, brick};
use clap::Parser;
use cli::{Cli, Command};

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
    let common = cli.command.common();
    let slicer = slicer::Settings::from_env();
    let source = Source::open(&common.input)?;

    warn_slicer_settings(&slicer);

    if common.verbose {
        if source.is_binary() {
            eprintln!("bricklayers: binary G-code container");
        }
        if let Some(name) = &slicer.output_name {
            eprintln!("bricklayers: slicer will save this as {name}");
        }
    }

    let survey = source.survey()?;
    if let Some(transform) = repeated(&cli.command, &survey)
        && !common.force
    {
        return Err(Error::AlreadyProcessed {
            path: common.input.clone(),
            transform,
        });
    }

    let sink = source.sink(common.output.as_ref().unwrap_or(&common.input))?;

    match &cli.command {
        Command::Brick(args) => {
            let config = resolve(
                args.into(),
                args.wall_order.chosen(),
                &slicer,
                &source,
                &survey,
            );

            let stats = source.rewrite(sink, |reader, writer| {
                brick::stream(reader, writer, &config, &survey)
            })?;

            warn_layer_height(stats.layer_height, stats.layer_height_detected);
            if common.verbose {
                if survey.objects() > 1 {
                    eprintln!(
                        "bricklayers: {} objects printed one after another, each built \
                         from the bed up",
                        survey.objects()
                    );
                }
                eprintln!(
                    "bricklayers: {} layers, {} internal loops, {} raised by {:.3} mm",
                    stats.layers,
                    stats.loops,
                    stats.raised,
                    stats.layer_height / 2.0
                );
                report_filament(
                    &stats,
                    &format!("--extrusion-multiplier {:.2}", config.extrusion_multiplier),
                );
            }
        }
    }

    Ok(())
}

/// The transform being asked for, if the file already carries its marks.
fn repeated(command: &Command, survey: &Survey) -> Option<&'static str> {
    match command {
        Command::Brick(_) => survey.bricked.then_some("brick"),
    }
}

/// Fills in what the command line left out: both layer heights, and which end
/// of a wall the loop numbering starts from.
fn resolve(
    mut config: brick::Config,
    chosen: Option<WallOrder>,
    slicer: &slicer::Settings,
    source: &Source,
    survey: &Survey,
) -> brick::Config {
    config.layer_height = detected(
        config.layer_height,
        slicer.layer_height,
        source.layer_height(),
    );
    config.first_layer_height = detected(
        config.first_layer_height,
        slicer.first_layer_height,
        source.first_layer_height(),
    );
    // The flag wins outright rather than only being able to turn the order on:
    // detection reads slicer prose, so it can be wrong in either direction and
    // both need an escape hatch.
    config.external_perimeters_first =
        chosen.or(slicer.wall_order).or(survey.wall_order) == Some(WallOrder::ExternalFirst);
    config
}

/// Command line first, then what the slicer exported, then what a binary
/// container states about itself. A plain file's own comment is left to the
/// survey, which reads it out of the G-code.
fn detected(requested: Option<f64>, exported: Option<f64>, stated: Option<f64>) -> Option<f64> {
    requested.or(exported).or(stated)
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
    if slicer.arc_fitting == Some(true) {
        eprintln!(
            "bricklayers: warning: arc fitting is on; extrusions emitted as G2/G3 arcs \
             pass through untouched"
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
        eprintln!(
            "bricklayers: warning: no layer height found in the file, assuming {height} mm; \
             pass --layer-height to be sure"
        );
    }
}

/// Prices what was applied against the whole part, since raised loops are only
/// a fraction of it and a multiplier reads far larger than it costs.
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
