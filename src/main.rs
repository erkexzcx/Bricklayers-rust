mod cli;

use std::process::ExitCode;

use clap::Parser;
use cli::Cli;
use corbel::scan::Survey;
use corbel::slicer::{self, WallOrder};
use corbel::{Error, Result, Source, brick, zaa};

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(&cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("corbel: {error}");
            ExitCode::FAILURE
        }
    }
}

/// What each transform did, for reporting. A transform that was not asked for
/// has nothing to say.
#[derive(Default)]
struct Report {
    brick: Option<brick::Stats>,
    zaa: Option<zaa::Stats>,
}

fn run(cli: &Cli) -> Result<()> {
    let slicer = slicer::Settings::from_env();
    let source = if cli.force {
        Source::open_forced(&cli.input)?
    } else {
        Source::open(&cli.input)?
    };
    let (bricks, contours) = (cli.bricks, cli.zaa);

    warn_slicer_settings(&slicer, bricks);
    if cli.verbose {
        if source.is_binary() {
            eprintln!("corbel: binary G-code container");
        }
        if let Some(name) = &slicer.output_name {
            eprintln!("corbel: slicer will save this as {}", printable(name));
        }
    }

    let survey = source.survey()?;
    if let Some(done) = already_done(&survey, bricks, contours)
        && !cli.force
    {
        return Err(Error::AlreadyProcessed {
            path: cli.input.clone(),
            done,
        });
    }
    if contours && !survey.layer_markers {
        eprintln!(
            "corbel: warning: this file has no layer-change markers, so there \
             are no layers to measure a surface against; nothing will be contoured"
        );
    }
    if let Some(warning) = unrecognised_regions(&survey) {
        eprintln!("corbel: warning: {warning}");
    } else if let Some(warning) = nothing_to_work_on(&survey) {
        eprintln!("corbel: warning: {warning}");
    }

    let sink = source.sink(cli.output.as_ref().unwrap_or(&cli.input))?;
    for warning in sink.warnings() {
        eprintln!("corbel: warning: {warning}");
    }
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
    let contour = zaa::Config {
        layer_height: config.layer_height,
        wall_width: config.wall_width,
        // A file bricked by an earlier run owns its hidden walls just as one
        // bricked in this pass does: the bead under them may stand half a
        // layer proud either way, and nothing in the stream says which.
        bricked: bricks || survey.bricked,
    };
    // A surface is measured against the layer printed over it, which the pass
    // writing the file has not reached. The second reader is opened before the
    // rewrite so a file it cannot open stops the run with the original intact.
    let lookahead = contours.then(|| source.reader()).transpose()?;

    let report = source.rewrite(sink, |reader, writer| {
        // The two transforms compose in one pass: bricking writes into the
        // surface pass, which writes the file. They own different regions —
        // the walls and the top surfaces — so neither sees the other's work.
        match (bricks, lookahead) {
            (true, Some(lookahead)) => {
                let mut pass = zaa::Pass::new(writer, lookahead, &contour, &survey);
                let bricked = brick::stream(reader, &mut pass, &config, &survey)?;
                Ok(Report {
                    brick: Some(bricked),
                    zaa: Some(pass.finish()?),
                })
            }
            (false, Some(lookahead)) => Ok(Report {
                brick: None,
                zaa: Some(zaa::stream(reader, lookahead, writer, &contour, &survey)?),
            }),
            _ => Ok(Report {
                brick: Some(brick::stream(reader, writer, &config, &survey)?),
                zaa: None,
            }),
        }
    })?;

    if let Some(stats) = report.brick {
        warn_layer_height(stats.layer_height, stats.layer_height_detected);
        warn_step(&stats, slicer.nozzle.or(survey.nozzle).or(source.nozzle()));
    }
    if cli.verbose {
        if survey.objects() > 1 {
            eprintln!(
                "corbel: {} objects printed one after another, each built \
                 from the bed up",
                survey.objects()
            );
        }
        if survey.variable_layers() {
            eprintln!(
                "corbel: the slicer varied the layer height, so each layer \
                 is measured against its own"
            );
        }
        if let Some(stats) = report.brick {
            report_bricks(&stats, &config, &applied(&config, &stats));
        }
        if let Some(stats) = report.zaa {
            report_surfaces(&stats);
        }
    }

    Ok(())
}

/// What this file already carries of the work about to be done again.
///
/// Both transforms measure against the plane the slicer wrote, so a second run
/// over their own output compounds a shift the file no longer describes.
fn already_done(survey: &Survey, bricks: bool, contours: bool) -> Option<&'static str> {
    match (bricks && survey.bricked, contours && survey.contoured) {
        (true, true) => Some("bricked and contoured"),
        (true, false) => Some("bricked"),
        (false, true) => Some("contoured"),
        (false, false) => None,
    }
}

/// How much of a name out of the environment reaches the terminal.
const NAME_LIMIT: usize = 120;

/// Makes a string this process did not choose safe to put on a terminal.
///
/// `SLIC3R_PP_OUTPUT_NAME` is a file name out of a project or 3MF the user may
/// not have authored, and the slicer hands it over exactly as it found it, so
/// it can carry escape sequences that repaint the terminal, a bidirectional
/// override that reverses what the rest of the line appears to say, and a
/// length that buries every warning printed after it.
fn printable(text: &str) -> String {
    let mut safe = String::with_capacity(text.len().min(NAME_LIMIT));
    for character in text.chars().take(NAME_LIMIT) {
        if character.is_control() {
            safe.extend(character.escape_debug());
        } else if is_a_bidi_override(character) {
            safe.extend(character.escape_unicode());
        } else {
            safe.push(character);
        }
    }
    if text.chars().nth(NAME_LIMIT).is_some() {
        safe.push('…');
    }
    safe
}

/// The characters that reorder a line without being control codes: the
/// explicit bidirectional overrides and the isolates.
fn is_a_bidi_override(character: char) -> bool {
    matches!(character, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
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

/// Slicer settings under which a transform quietly does nothing, or the
/// wrong thing. Only ever reached when a slicer ran us, since they come from
/// the environment it exports.
fn warn_slicer_settings(slicer: &slicer::Settings, bricks: bool) {
    if slicer.spiral_vase == Some(true) {
        eprintln!(
            "corbel: warning: spiral vase mode is on; it prints one continuously \
             rising wall, so there are no layer boundaries to interlock"
        );
    }
    if let Some(walls) = slicer.walls
        && walls < 2
        && bricks
    {
        eprintln!(
            "corbel: warning: {walls} wall(s) per region leaves no internal \
             perimeter behind the visible one, so there is nothing to raise; \
             bricking needs two walls or more"
        );
    }
}

/// What a file this tool recognises nothing in has to be told about.
///
/// Both transforms find their work through the region markers the slicer
/// wrote, so a file carrying none in either dialect — an unknown slicer, or
/// one an earlier post-processor stripped — is copied out unchanged while the
/// run still succeeds. It is a warning and never a failure: the print may
/// already be on the bed, and a file rewritten byte for byte prints exactly as
/// it was sliced. A slicer swallows what a post-processing script prints, so
/// it is said whether or not `--verbose` was asked for.
fn nothing_to_work_on(survey: &Survey) -> Option<&'static str> {
    (survey.perimeters == 0).then_some(
        "no perimeter regions were recognised in this file, so neither \
         transform has anything to work on and it comes out unchanged; the \
         region markers looked for are ';TYPE:' (PrusaSlicer, SuperSlicer, \
         Cura) and '; FEATURE:' (OrcaSlicer, Bambu Studio)",
    )
}

/// What a file whose region markers name something unknown has to be told.
///
/// The markers are there and every one of them is read — they simply classify
/// as no region this tool knows, so both transforms walk the file and find
/// nothing of theirs in it. Left uncounted that is indistinguishable from a
/// file with no markers at all, which is how a whole unsupported dialect goes
/// by in silence. A warning and never a failure, for the same reasons
/// [`nothing_to_work_on`] is one.
///
/// It stands in front of that warning rather than beside it: a file whose
/// regions are all unknown has no recognised perimeters either, so both would
/// otherwise fire for it, and quoting a label the file actually carries says
/// more than "nothing was recognised" ever can.
fn unrecognised_regions(survey: &Survey) -> Option<String> {
    let label = survey.unknown_region.as_deref()?;
    Some(format!(
        "{} region marker(s) in this file name something not recognised, the \
         first of them '{}'; those regions are copied out exactly as the \
         slicer wrote them, so anything of yours inside one is left alone",
        survey.unknown_regions,
        printable(label)
    ))
}

fn warn_layer_height(height: f64, detected: bool) {
    if !detected {
        eprintln!("corbel: warning: no layer height found in the file, assuming {height} mm");
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
            "corbel: warning: loops are raised by up to {step:.3} mm against a \
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
        "corbel: {:.1} mm filament, {share:.1}% of it in raised loops; \
         {applied} adds {added:.2}% to the part",
        stats.filament
    );
}

/// What bricking did.
fn report_bricks(stats: &brick::Stats, config: &brick::Config, applied: &str) {
    eprintln!(
        "corbel: {} layers, {} perimeter loops, {} raised by {}",
        stats.layers,
        stats.loops,
        stats.raised,
        raised_by(stats)
    );
    if stats.capped > 0 {
        eprintln!(
            "corbel: {} more {} left flat where the wall ends and \
             something is printed over it",
            stats.capped,
            if stats.capped == 1 { "was" } else { "were" }
        );
    }
    if config.wall_width.is_none() {
        eprintln!(
            "corbel: the file states no internal wall width, so the flow \
             below is the shipped default rather than this print's own geometry"
        );
    }
    report_filament(stats, applied);
}

/// What following the surfaces did. Zero moves is worth saying: it means the
/// part has no shallow surface, or that the file gave nothing to measure one
/// against.
fn report_surfaces(stats: &zaa::Stats) {
    let Some((low, high)) = stats.rise else {
        eprintln!(
            "corbel: no surface shallow enough to smooth; every top face \
             either faces straight up or is too steep to leave a step"
        );
        return;
    };
    eprintln!(
        "corbel: {} surface moves on {} layers followed from {:+.3} to {:+.3} mm \
         of their plane, written as {} moves",
        stats.moves, stats.layers, low, high, stats.segments
    );
    if stats.filament > 0.0 {
        let added = 100.0 * stats.added / stats.filament;
        eprintln!(
            "corbel: {:.1} mm filament in those surfaces, re-metered by {added:+.2}% \
             for the gaps they really cross",
            stats.filament
        );
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    /// A file whose region markers went unrecognised — an unknown slicer, or
    /// one an earlier post-processor stripped — gives both transforms nothing
    /// to buffer, so the run rewrites it to no effect. That is worth one line
    /// on stderr and nothing more: a warning, never a failure, because the
    /// print may already be running and the file itself is fine.
    #[test]
    fn a_file_with_nothing_to_work_on_warns_and_still_succeeds() {
        let gcode = "M83\n;LAYER_CHANGE\nG1 Z0.2\nG1 X0 Y0 F9000\nG1 X10 Y0 E0.5\n";
        let warning = nothing_to_work_on(&Survey::of(gcode)).expect("a file with no regions");
        assert!(warning.contains(";TYPE:"), "{warning}");
        assert!(warning.contains("; FEATURE:"), "{warning}");

        let path = std::env::temp_dir().join(format!(
            "corbel-nothing-to-work-on-{}.gcode",
            std::process::id()
        ));
        fs::write(&path, gcode).expect("write the sample");
        let target = path.display().to_string();
        let cli = Cli::parse_from(["corbel", "--bricks", "--zaa", target.as_str()]);
        let outcome = run(&cli);
        let _ = fs::remove_file(&path);
        assert!(outcome.is_ok(), "{outcome:?}");
    }

    /// A file whose walls it does recognise has nothing to say.
    #[test]
    fn a_file_whose_walls_are_recognised_is_not_warned_about() {
        let survey = Survey::of(
            "M83\n;LAYER_CHANGE\nG1 Z0.2\n\
             ;TYPE:Perimeter\nG1 X0 Y0 F9000\nG1 X10 Y0 E0.5\n",
        );
        assert!(nothing_to_work_on(&survey).is_none());
        assert!(unrecognised_regions(&survey).is_none());
    }

    /// A file whose region markers are all labels this tool has never met is
    /// written in a dialect it does not support, and saying so in the user's
    /// own words — the label itself — is the difference between a bug report
    /// that can be acted on and one that says only "it did nothing". It stays
    /// a warning: the file is copied out exactly as it was sliced, so the run
    /// succeeds and the print is fine.
    #[test]
    fn a_file_of_unknown_region_labels_names_one_of_them_and_still_succeeds() {
        let gcode = "M83\n;LAYER_CHANGE\nG1 Z0.2\n\
                     ;TYPE:Widget\nG1 X0 Y0 F9000\nG1 X10 Y0 E0.5\n\
                     ;TYPE:Flange\nG1 X10 Y10 E0.5\n";
        let survey = Survey::of(gcode);
        assert_eq!(survey.unknown_regions, 2);
        let warning = unrecognised_regions(&survey).expect("two labels named nothing");
        assert!(warning.contains("Widget"), "{warning}");

        // The general warning fits this file too — it has no recognised
        // perimeter either — and must not be printed beside this one. The
        // specific one wins because it can quote what the file actually says.
        assert!(nothing_to_work_on(&survey).is_some());

        let path = std::env::temp_dir().join(format!(
            "corbel-unknown-regions-{}.gcode",
            std::process::id()
        ));
        fs::write(&path, gcode).expect("write the sample");
        let target = path.display().to_string();
        let cli = Cli::parse_from(["corbel", "--bricks", "--zaa", target.as_str()]);
        let outcome = run(&cli);
        let _ = fs::remove_file(&path);
        assert!(outcome.is_ok(), "{outcome:?}");
    }

    /// The slicer copies this name out of a project the user may not have
    /// authored and exports it exactly as it found it, so nothing in it may
    /// reach the terminal as an instruction to the terminal.
    #[test]
    fn a_name_out_of_the_environment_cannot_repaint_the_terminal() {
        let hostile = "part\u{1b}[2J\u{1b}]0;pwned\u{7}\r\n\u{202e}edoc.g";
        let safe = printable(hostile);
        assert!(
            !safe
                .chars()
                .any(|character| character.is_control() || is_a_bidi_override(character)),
            "{safe}"
        );
        assert!(safe.starts_with("part"), "{safe}");
        assert!(safe.contains("\\u{1b}"), "{safe}");
        assert!(safe.contains("\\u{202e}"), "{safe}");

        // And it cannot bury the warnings printed after it either.
        let long = printable(&"a".repeat(NAME_LIMIT * 3));
        assert_eq!(long.chars().count(), NAME_LIMIT + 1, "{long}");
        assert!(long.ends_with('…'), "{long}");

        // An ordinary name is left exactly as it is, accents and all.
        assert_eq!(
            printable("Caffè – part (1).gcode"),
            "Caffè – part (1).gcode"
        );
    }
}
