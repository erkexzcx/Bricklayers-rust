//! Line-level G-code parsing.
//!
//! A [`Line`] borrows the text it was parsed from and carries the byte span of
//! its `E` word, so extrusion can be rewritten without disturbing the rest of
//! the line. [`Lines`] supplies that text one line at a time from any reader.

use std::borrow::Cow;
use std::io::{self, BufRead, Write};

/// Reads a stream one line at a time through a buffer it reuses, so the input
/// side of a transform costs one line of memory however large the file is.
///
/// Terminators are stripped exactly as [`str::lines`] strips them, both `\n`
/// and `\r\n`, so what a transform sees never depends on whether the G-code
/// arrived as a string or as a file.
///
/// Slicers copy model and filament names into comments in whatever encoding the
/// host uses, so a file that is otherwise plain ASCII G-code can still carry a
/// few bytes that are not UTF-8. Those lines are repaired rather than rejected:
/// commands are ASCII, so the damage stays inside a comment that was already
/// unreadable.
pub struct Lines<R> {
    reader: R,
    buffer: Vec<u8>,
    /// A repaired line, which no longer matches the bytes in `buffer`.
    repaired: String,
}

impl<R: BufRead> Lines<R> {
    pub fn new(reader: R) -> Self {
        Self {
            reader,
            buffer: Vec::new(),
            repaired: String::new(),
        }
    }

    /// The next line, or `None` at the end of the stream.
    pub fn next_line(&mut self) -> io::Result<Option<&str>> {
        self.buffer.clear();
        if self.reader.read_until(b'\n', &mut self.buffer)? == 0 {
            return Ok(None);
        }
        if self.buffer.last() == Some(&b'\n') {
            self.buffer.pop();
        }
        if self.buffer.last() == Some(&b'\r') {
            self.buffer.pop();
        }

        match String::from_utf8_lossy(&self.buffer) {
            Cow::Borrowed(text) => Ok(Some(text)),
            Cow::Owned(text) => {
                self.repaired = text;
                Ok(Some(&self.repaired))
            }
        }
    }
}

/// The commands this post-processor has to understand. Everything else is
/// passed through untouched.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Code {
    /// `G0` or `G1` — linear move.
    Move,
    /// `G2` or `G3` — arc move, which neither transform can reshape.
    Arc,
    /// `G92` — set position, which redefines the extruder origin.
    SetPosition,
    /// `M82` — absolute extrusion distances.
    AbsoluteE,
    /// `M83` — relative extrusion distances.
    RelativeE,
    #[default]
    Other,
}

/// One parsed line, borrowed from the source buffer.
#[derive(Clone, Copy, Debug)]
pub struct Line<'a> {
    pub raw: &'a str,
    pub code: Code,
    pub x: Option<f64>,
    pub y: Option<f64>,
    pub z: Option<f64>,
    pub e: Option<f64>,
    pub f: Option<f64>,
    /// Offsets from the start of an arc to its centre, `I` and `J`.
    pub i: Option<f64>,
    pub j: Option<f64>,
    /// True for a `G2`. Which way an arc turns decides which side of the
    /// circle it draws, so a tracer that guesses gets the complement.
    pub clockwise: bool,
    e_span: Option<(usize, usize)>,
    z_span: Option<(usize, usize)>,
    comment_at: Option<usize>,
    /// An `X` or `Y` word was present, whether or not its value was read.
    has_xy: bool,
}

impl<'a> Line<'a> {
    /// Reads every word.
    pub fn parse(raw: &'a str) -> Self {
        Self::read(raw, true)
    }

    /// Reads every word but `X` and `Y`, whose presence is still recorded.
    /// They are the two commonest words in a file, so a pass that only needs
    /// to know that a move went somewhere in the plane, not where, saves most
    /// of its per-line work here.
    pub fn scan(raw: &'a str) -> Self {
        Self::read(raw, false)
    }

    fn read(raw: &'a str, plane: bool) -> Self {
        let comment_at = raw.find(';');
        let body = &raw[..comment_at.unwrap_or(raw.len())];
        let (code, clockwise) = command_of(body);
        let mut line = Line {
            raw,
            code,
            x: None,
            y: None,
            z: None,
            e: None,
            f: None,
            i: None,
            j: None,
            clockwise,
            e_span: None,
            z_span: None,
            comment_at,
            has_xy: false,
        };

        let bytes = body.as_bytes();
        // `M201`, `M203` and `M205` carry a per-axis limit under the same
        // letter an extrusion distance uses, and a start G-code that sets
        // `E5000` would otherwise book five metres of filament and move the
        // tracked extruder position with it.
        let feeds = matches!(line.code, Code::Move | Code::Arc | Code::SetPosition);
        let mut at = 0;
        while at < bytes.len() {
            let byte = bytes[at];
            at += 1;
            if !byte.is_ascii_alphabetic() {
                continue;
            }
            let start = at;
            while at < bytes.len() && matches!(bytes[at], b'0'..=b'9' | b'.' | b'-' | b'+') {
                at += 1;
            }

            // Lowercased, so `X` and `x` reach the same arm.
            let letter = byte | 0x20;
            if matches!(letter, b'x' | b'y') {
                line.has_xy |= at > start;
                if !plane {
                    continue;
                }
            } else if matches!(letter, b'i' | b'j') {
                // Only an arc has a centre; `I` and `J` mean other things to
                // other commands.
                if line.code != Code::Arc {
                    continue;
                }
            } else if !matches!(letter, b'z' | b'e' | b'f') || (letter == b'e' && !feeds) {
                continue;
            }
            let Some(value) = number(&body[start..at]) else {
                continue;
            };
            match letter {
                b'x' => line.x = Some(value),
                b'y' => line.y = Some(value),
                b'z' => {
                    line.z = Some(value);
                    line.z_span = Some((start, at));
                }
                b'f' => line.f = Some(value),
                b'i' => line.i = Some(value),
                b'j' => line.j = Some(value),
                _ => {
                    line.e = Some(value);
                    line.e_span = Some((start, at));
                }
            }
        }
        line
    }

    pub fn is_move(&self) -> bool {
        self.code == Code::Move
    }

    /// True for any move that can lay down material, arcs included.
    pub fn draws(&self) -> bool {
        matches!(self.code, Code::Move | Code::Arc)
    }

    /// True for a move that goes somewhere in the XY plane.
    pub fn is_xy_move(&self) -> bool {
        self.is_move() && self.has_xy
    }

    pub fn xy(&self) -> Option<(f64, f64)> {
        Some((self.x?, self.y?))
    }

    /// The arc this line draws, or `None` for anything else. `R`-form arcs are
    /// not one: no slicer measured here emits them.
    pub fn arc(&self) -> Option<crate::footprint::Arc> {
        (self.code == Code::Arc).then_some(())?;
        Some(crate::footprint::Arc {
            i: self.i?,
            j: self.j?,
            clockwise: self.clockwise,
        })
    }

    /// The comment text, without the leading `;`.
    pub fn comment(&self) -> Option<&'a str> {
        Some(&self.raw[self.comment_at? + 1..])
    }

    /// The comment text of a line that carries nothing else, which is the only
    /// shape a region or layer-change marker takes. A trailing comment on a
    /// move is not one, or the stamps this tool leaves on the Z moves it
    /// inserts would read as markers on the next pass.
    pub fn marker(&self) -> Option<&'a str> {
        let at = self.comment_at?;
        self.raw[..at]
            .bytes()
            .all(|byte| byte.is_ascii_whitespace())
            .then(|| &self.raw[at + 1..])
    }

    /// Byte range of the `E` word's digits within [`Line::raw`], for a caller
    /// that keeps the text and rewrites the value later.
    pub fn e_span(&self) -> Option<(usize, usize)> {
        self.e_span
    }

    /// Writes the line with its `E` word replaced. The rest of it, including
    /// any comment, is copied byte for byte.
    pub fn write_e<W: Write>(&self, out: &mut W, value: f64) -> io::Result<()> {
        write_e(out, self.raw, self.e_span, value)
    }

    /// Writes the line with its `Z` word set to `value`, adding one where the
    /// line has none, and without a trailing newline so the caller can stamp
    /// it. Everything else is copied byte for byte.
    ///
    /// A move the slicer was already making can carry a height change this
    /// way, where a `G1 Z` of its own would stop the toolhead to make it.
    pub fn write_z<W: Write>(&self, out: &mut W, value: f64) -> io::Result<()> {
        let bytes = self.raw.as_bytes();
        if let Some((start, end)) = self.z_span {
            out.write_all(&bytes[..start])?;
            write_fixed(out, value, 3)?;
            return out.write_all(&bytes[end..]);
        }
        let at = self.comment_at.unwrap_or(self.raw.len());
        out.write_all(self.raw[..at].trim_end().as_bytes())?;
        out.write_all(b" Z")?;
        write_fixed(out, value, 3)?;
        out.write_all(&bytes[at..])
    }
}

/// Writes `raw` with the number at `span` replaced by `value`, straight to
/// `out` rather than through a `String` that is dropped a moment later.
pub fn write_e<W: Write>(
    out: &mut W,
    raw: &str,
    span: Option<(usize, usize)>,
    value: f64,
) -> io::Result<()> {
    let Some((start, end)) = span else {
        return out.write_all(raw.as_bytes());
    };
    out.write_all(&raw.as_bytes()[..start])?;
    write_fixed(out, value, 5)?;
    out.write_all(&raw.as_bytes()[end..])
}

/// Powers of ten a double names exactly, which is what makes the single
/// division in [`number`] correctly rounded.
const POWERS_OF_TEN: [f64; 16] = [
    1e0, 1e1, 1e2, 1e3, 1e4, 1e5, 1e6, 1e7, 1e8, 1e9, 1e10, 1e11, 1e12, 1e13, 1e14, 1e15,
];

/// The longest mantissa [`number`] reads itself. Fifteen digits stay below
/// 2^53, where a double still names every integer.
const MAX_EXACT_DIGITS: usize = 15;

/// The most digits [`write_fixed`] lays after the point before handing over.
const MAX_FIXED_DECIMALS: usize = 9;

/// A sign, sixteen digits and a point, which is the widest [`write_fixed`]
/// will produce before it hands over.
const FIXED_WIDTH: usize = 24;

/// Writes `value` with `decimals` digits after the point, byte for byte what
/// `write!("{value:.decimals$}")` produces.
///
/// `core` formats a float from its exact decimal expansion, which costs around
/// 67 ns a number where scaling it to an integer costs 13, and G-code is
/// mostly numbers. The scaled value is only trusted where the one rounding it
/// took provably cannot have crossed the half-way point it is about to be
/// rounded at; everything closer, including a value sitting exactly on one and
/// so rounding to even, falls back to `core`.
pub fn write_fixed<W: Write>(out: &mut W, value: f64, decimals: usize) -> io::Result<()> {
    let mut digits = [0u8; FIXED_WIDTH];
    match fixed(value, decimals, &mut digits) {
        Some(at) => out.write_all(&digits[at..]),
        None => write!(out, "{value:.decimals$}"),
    }
}

fn fixed(value: f64, decimals: usize, out: &mut [u8; FIXED_WIDTH]) -> Option<usize> {
    if decimals > MAX_FIXED_DECIMALS {
        return None;
    }
    let scaled = value * POWERS_OF_TEN[decimals];
    // Beyond this a double no longer names every integer, so the digits below
    // would not be the ones `core` prints.
    if scaled.is_nan() || scaled.abs() >= 1e15 {
        return None;
    }
    let rounded = scaled.round();
    if (scaled - rounded).abs() >= 0.5 - scaled.abs() * (4.0 * f64::EPSILON) {
        return None;
    }

    let mut remaining = rounded.abs() as u64;
    let mut at = FIXED_WIDTH;
    for _ in 0..decimals {
        at -= 1;
        out[at] = b'0' + (remaining % 10) as u8;
        remaining /= 10;
    }
    if decimals > 0 {
        at -= 1;
        out[at] = b'.';
    }
    loop {
        at -= 1;
        out[at] = b'0' + (remaining % 10) as u8;
        remaining /= 10;
        if remaining == 0 {
            break;
        }
    }
    // Taken from the value, since a negative that rounds to zero still prints
    // its sign.
    if value.is_sign_negative() {
        at -= 1;
        out[at] = b'-';
    }
    Some(at)
}

/// Reads a fixed-point decimal, which is every number a slicer writes.
///
/// A mantissa below 2^53 and a scale of at most 10^15 are both exact as
/// doubles, so the division that follows is a single correctly rounded
/// operation and the result is bit for bit what [`f64::from_str`] returns at
/// roughly half the cost. Anything outside that is handed to `from_str`.
fn number(text: &str) -> Option<f64> {
    let bytes = text.as_bytes();
    let (negative, bytes) = match bytes.first()? {
        b'-' => (true, &bytes[1..]),
        b'+' => (false, &bytes[1..]),
        _ => (false, bytes),
    };

    let mut mantissa = 0u64;
    let mut decimals = -1i32;
    let mut digits = 0usize;
    for &byte in bytes {
        match byte {
            b'0'..=b'9' => {
                digits += 1;
                if digits > MAX_EXACT_DIGITS {
                    return text.parse().ok();
                }
                mantissa = mantissa * 10 + u64::from(byte - b'0');
                decimals += i32::from(decimals >= 0);
            }
            b'.' if decimals < 0 => decimals = 0,
            _ => return text.parse().ok(),
        }
    }
    if digits == 0 {
        return text.parse().ok();
    }

    let value = mantissa as f64 / POWERS_OF_TEN[decimals.max(0) as usize];
    Some(if negative { -value } else { value })
}

/// The command a line carries, and whether an arc turns clockwise.
fn command_of(body: &str) -> (Code, bool) {
    let bytes = body.as_bytes();
    let mut at = skip(bytes, 0, u8::is_ascii_whitespace);
    // Marlin's serial dialect numbers each line; the command follows it.
    if bytes.get(at).is_some_and(|byte| byte | 0x20 == b'n') {
        let after = skip(bytes, at + 1, u8::is_ascii_digit);
        if after > at + 1 {
            at = skip(bytes, after, u8::is_ascii_whitespace);
        }
    }

    let Some(&letter) = bytes.get(at).filter(|byte| byte.is_ascii_alphabetic()) else {
        return (Code::Other, false);
    };
    let start = at + 1;
    let end = skip(bytes, start, u8::is_ascii_digit);
    match (letter | 0x20, &body[start..end]) {
        (b'g', "0" | "1") => (Code::Move, false),
        (b'g', "2") => (Code::Arc, true),
        (b'g', "3") => (Code::Arc, false),
        (b'g', "92") => (Code::SetPosition, false),
        (b'm', "82") => (Code::AbsoluteE, false),
        (b'm', "83") => (Code::RelativeE, false),
        _ => (Code::Other, false),
    }
}

/// The first index at or after `from` whose byte fails `wanted`.
fn skip(bytes: &[u8], from: usize, wanted: fn(&u8) -> bool) -> usize {
    let mut at = from;
    while at < bytes.len() && wanted(&bytes[at]) {
        at += 1;
    }
    at
}

/// Keeps an extrusion stream consistent while individual moves are rescaled or
/// split.
///
/// In relative mode (`M83`) an `E` word is already a delta and passes straight
/// through. In absolute mode (`M82`) rescaling one move shifts every later
/// value, so input and output positions are tracked separately.
///
/// Whether a line has to be written again is decided by comparing what
/// [`Extruder::advance`] hands back against the value the line already
/// carries. There is deliberately no "is it drifting" flag: a caller that
/// buffers a region reads the whole of it before emitting any of it, so the
/// input position runs ahead of the output and the two say nothing about each
/// other until the region is replayed.
#[derive(Clone, Copy, Debug)]
pub struct Extruder {
    absolute: bool,
    input: f64,
    output: f64,
}

impl Extruder {
    /// Marlin powers up in absolute mode; slicers override it in their start
    /// G-code.
    pub fn new() -> Self {
        Self {
            absolute: true,
            input: 0.0,
            output: 0.0,
        }
    }

    pub fn is_absolute(&self) -> bool {
        self.absolute
    }

    /// Applies `M82` / `M83`.
    pub fn set_mode(&mut self, code: Code) {
        match code {
            Code::AbsoluteE => self.absolute = true,
            Code::RelativeE => self.absolute = false,
            _ => {}
        }
    }

    /// Applies `G92 E<value>`, which redefines the origin of both streams.
    pub fn set_position(&mut self, value: f64) {
        self.input = value;
        self.output = value;
    }

    /// Applies the reset to the input stream only, at the point the `G92` is
    /// read.
    ///
    /// A pass that buffers a region has not written that region out when it
    /// parses the line, so the output stream is still behind and must not be
    /// moved with it. See [`Extruder::advance_origin`].
    pub fn observe_origin(&mut self, value: f64) {
        self.input = value;
    }

    /// Applies the reset to the output stream, at the point the `G92` is
    /// written.
    pub fn advance_origin(&mut self, value: f64) {
        self.output = value;
    }

    /// Reads an `E` word from the input and returns the filament delta it asks
    /// for.
    pub fn observe(&mut self, value: f64) -> f64 {
        if self.absolute {
            let delta = value - self.input;
            self.input = value;
            delta
        } else {
            value
        }
    }

    /// Reserves `delta` mm of filament and returns the `E` word to emit.
    pub fn advance(&mut self, delta: f64) -> f64 {
        if self.absolute {
            self.output += delta;
            self.output
        } else {
            delta
        }
    }
}

impl Default for Extruder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_e(line: &Line<'_>, value: f64) -> String {
        let mut out = Vec::new();
        line.write_e(&mut out, value)
            .expect("writing to a Vec cannot fail");
        String::from_utf8(out).expect("G-code lines are UTF-8")
    }

    fn formatted(value: f64, decimals: usize) -> String {
        let mut out = Vec::new();
        write_fixed(&mut out, value, decimals).expect("writing to a Vec cannot fail");
        String::from_utf8(out).expect("digits are UTF-8")
    }

    fn lines_of(source: &str) -> Vec<String> {
        let mut lines = Lines::new(source.as_bytes());
        let mut out = Vec::new();
        while let Some(line) = lines.next_line().expect("reading a slice cannot fail") {
            out.push(line.to_owned());
        }
        out
    }

    #[test]
    fn reading_lines_matches_the_str_iterator() {
        for source in [
            "",
            "\n",
            "G1 X1",
            "G1 X1\n",
            "G1 X1\nG1 X2\n",
            "G1 X1\r\nG1 X2\r\n",
            "G1 X1\n\nG1 X2",
            "\r\n\r\n",
        ] {
            let expected: Vec<String> = source.lines().map(str::to_owned).collect();
            assert_eq!(lines_of(source), expected, "{source:?}");
        }
    }

    #[test]
    fn a_line_that_is_not_utf8_is_repaired_rather_than_refused() {
        // A slicer naming an object in the host's legacy encoding.
        let source: &[u8] = b"G1 X1 E0.5\n; printing object Caf\xe9\nG1 X2 E0.5\n";
        let mut lines = Lines::new(source);
        let mut out = Vec::new();
        while let Some(line) = lines.next_line().expect("reading a slice cannot fail") {
            out.push(line.to_owned());
        }

        assert_eq!(
            out,
            ["G1 X1 E0.5", "; printing object Caf\u{fffd}", "G1 X2 E0.5"],
            "only the offending byte may change, and only on its own line"
        );
    }

    #[test]
    fn parses_words_and_command() {
        let line = Line::parse("G1 X10.5 Y-2 Z0.4 E0.05 F1800");
        assert_eq!(line.code, Code::Move);
        assert_eq!(line.xy(), Some((10.5, -2.0)));
        assert_eq!(line.z, Some(0.4));
        assert_eq!(line.e, Some(0.05));
        assert_eq!(line.f, Some(1800.0));
    }

    #[test]
    fn ignores_words_inside_comments() {
        let line = Line::parse("G1 X1 Y1 ; travel to Z99 E42");
        assert_eq!(line.z, None);
        assert_eq!(line.e, None);
        assert_eq!(line.comment(), Some(" travel to Z99 E42"));
    }

    #[test]
    fn a_machine_limit_is_not_an_extrusion() {
        for raw in [
            "M201 X20000 Y20000 Z500 E5000",
            "M203 X500 Y500 Z20 E30",
            "M205 X9.00 Y9.00 Z3.00 E2.50 ; sets the jerk limits, mm/sec",
        ] {
            let line = Line::parse(raw);
            assert_eq!(line.e, None, "{raw}");
            assert_eq!(with_e(&line, 1.0), raw, "{raw} must survive a rewrite");
        }
        assert_eq!(Line::parse("G92 E0").e, Some(0.0));
        assert_eq!(Line::parse("G1 X1 E0.5").e, Some(0.5));
    }

    #[test]
    fn recognises_extrusion_mode_commands() {
        assert_eq!(Line::parse("M82").code, Code::AbsoluteE);
        assert_eq!(Line::parse("M83 ; relative E").code, Code::RelativeE);
        assert_eq!(Line::parse("G92 E0").code, Code::SetPosition);
        assert_eq!(Line::parse("M104 S200").code, Code::Other);
        assert_eq!(Line::parse("").code, Code::Other);
        assert_eq!(Line::parse(";TYPE:Perimeter").code, Code::Other);
    }

    #[test]
    fn rewrites_only_the_e_word() {
        let line = Line::parse("G1 X1 Y1 E0.05 F1800 ; keep E0.05 here");
        assert_eq!(
            with_e(&line, 0.075),
            "G1 X1 Y1 E0.07500 F1800 ; keep E0.05 here"
        );
    }

    #[test]
    fn rewriting_a_line_without_e_is_a_copy() {
        let line = Line::parse("G1 X1 Y1");
        assert_eq!(with_e(&line, 1.0), "G1 X1 Y1");
    }

    #[test]
    fn scanning_skips_the_plane_but_still_sees_it() {
        let line = Line::scan("G1 X10.5 Y-2 Z0.4 E0.05 F1800");
        assert_eq!((line.x, line.y), (None, None));
        assert!(line.is_xy_move(), "an X or Y word still marks a plane move");
        assert_eq!(line.z, Some(0.4));
        assert_eq!(line.e, Some(0.05));
        assert_eq!(line.f, Some(1800.0));

        assert!(!Line::scan("G1 Z0.4 F600").is_xy_move());
    }

    #[test]
    fn only_a_bare_comment_is_a_marker() {
        assert_eq!(
            Line::parse("  ;TYPE:Perimeter").marker(),
            Some("TYPE:Perimeter")
        );
        assert_eq!(Line::parse(";LAYER_CHANGE").marker(), Some("LAYER_CHANGE"));
        // The stamp this tool leaves rides a move, and must not read as one.
        let stamped = Line::parse("G1 Z0.300 F600 ; bricklayers brick raised");
        assert_eq!(stamped.marker(), None);
        assert_eq!(stamped.comment(), Some(" bricklayers brick raised"));
        assert_eq!(Line::parse("G1 X1 Y1").marker(), None);
        assert_eq!(Line::parse("G1 X1 ;TYPE:Perimeter").marker(), None);
    }

    /// The survey reads its lines with the plane skipped, so the two ways of
    /// reading one have to agree about everything the survey looks at.
    #[test]
    fn scanning_agrees_with_a_full_parse_apart_from_the_plane() {
        for raw in [
            "G1 X10.5 Y-2 Z0.4 E0.05 F1800",
            "G1 Z0.4 F600",
            "G0 X1 Y1 F9000",
            "G2 X3 Y3 I1 J1 E0.5",
            "G92 E0",
            "M83",
            "M201 X20000 Y20000 Z500 E5000",
            ";TYPE:Perimeter",
            "G1 X1 Y1 ; travel to Z99 E42",
            "N42 G1 X1 Y2 E0.5*57",
            "",
        ] {
            let (full, scanned) = (Line::parse(raw), Line::scan(raw));
            assert_eq!(full.code, scanned.code, "{raw}");
            assert_eq!(full.z, scanned.z, "{raw}");
            assert_eq!(full.e, scanned.e, "{raw}");
            assert_eq!(full.f, scanned.f, "{raw}");
            assert_eq!(full.e_span(), scanned.e_span(), "{raw}");
            assert_eq!(full.comment(), scanned.comment(), "{raw}");
            assert_eq!(full.marker(), scanned.marker(), "{raw}");
            assert_eq!(full.is_xy_move(), scanned.is_xy_move(), "{raw}");
            assert_eq!(full.draws(), scanned.draws(), "{raw}");
        }
    }

    #[test]
    fn reads_a_line_numbered_command() {
        // Marlin's serial dialect, line number in front and checksum behind.
        let line = Line::parse("N42 G1 X1 Y2 E0.5*57");
        assert_eq!(line.code, Code::Move);
        assert_eq!(line.xy(), Some((1.0, 2.0)));
        assert_eq!(line.e, Some(0.5));
        assert_eq!(Line::parse("N7 M83").code, Code::RelativeE);
        assert_eq!(Line::parse("n7 g92 E0").code, Code::SetPosition);
        // `N` without digits is a word, not a line number.
        assert_eq!(Line::parse("NG1 X1").code, Code::Other);
    }

    #[test]
    fn numbers_parse_bit_for_bit_like_the_standard_library() {
        let texts = [
            "0",
            "-0",
            "1",
            "+1",
            "1.",
            ".5",
            "-.5",
            "0.05",
            "10.5",
            "-2",
            "1800",
            "123456789.123456",
            "0.000000001",
            "1e3",
            "1.2.3",
            "--1",
            "",
            "-",
            ".",
            "1-2",
            "123456789012345",
            "1234567890123456",
            "000000000000000000001",
            "99999999999999999999",
        ];
        for text in texts {
            let expected = text.parse::<f64>().ok();
            let actual = number(text);
            assert_eq!(
                actual.map(f64::to_bits),
                expected.map(f64::to_bits),
                "{text:?} parsed as {actual:?}, expected {expected:?}"
            );
        }
    }

    /// Every shape a slicer writes a coordinate, a feedrate or an extrusion
    /// in, at every precision it uses.
    #[test]
    fn every_slicer_shaped_number_parses_like_the_standard_library() {
        let mut checked = 0;
        let mut mismatches = 0;
        for whole in [0u64, 1, 7, 42, 250, 9000, 123456] {
            for fraction in 0..1000u64 {
                for decimals in [0usize, 1, 2, 3, 5] {
                    for sign in ["", "-"] {
                        let text = if decimals == 0 {
                            format!("{sign}{whole}")
                        } else {
                            format!("{sign}{whole}.{fraction:0>decimals$}")
                        };
                        let expected = text.parse::<f64>().expect("well formed");
                        checked += 1;
                        mismatches += usize::from(
                            number(&text).map(f64::to_bits) != Some(expected.to_bits()),
                        );
                    }
                }
            }
        }
        assert_eq!(mismatches, 0, "over {checked} numbers");
    }

    /// Every number this tool writes goes out at one of these precisions, so a
    /// value has to survive being written and read back.
    #[test]
    fn a_written_number_reads_back_as_itself() {
        for decimals in [0usize, 3, 5] {
            for step in 0..2000 {
                let text = formatted(step as f64 * 0.0137 - 13.7, decimals);
                assert_eq!(
                    number(&text).map(f64::to_bits),
                    text.parse::<f64>().ok().map(f64::to_bits),
                    "{text}"
                );
            }
        }
    }

    #[test]
    fn fixed_point_output_matches_the_standard_formatter() {
        let mut value = 0.0f64;
        let mut mismatches = 0;
        for step in 0..200_000u64 {
            value = (value + 0.0137).rem_euclid(250.0);
            for (candidate, decimals) in [
                (value, 3),
                (value, 5),
                (value, 0),
                (-value, 3),
                // Odd sixteenths sit exactly on a half-way point at three
                // decimals, where `core` rounds to even and scaling does not.
                (step as f64 / 16.0, 3),
                (step as f64 * 0.5, 0),
            ] {
                mismatches += usize::from(
                    formatted(candidate, decimals) != format!("{candidate:.decimals$}"),
                );
            }
        }
        assert_eq!(mismatches, 0);
    }

    #[test]
    fn fixed_point_output_handles_the_awkward_values() {
        for (value, decimals) in [
            (0.0, 3),
            (-0.0, 3),
            (-0.0001, 3),
            (0.0625, 3),
            (0.0015, 3),
            (0.5, 0),
            (1.5, 0),
            (2.5, 0),
            (-2.5, 0),
            (f64::NAN, 3),
            (f64::INFINITY, 3),
            (f64::NEG_INFINITY, 3),
            (1e20, 3),
            (-1e20, 5),
            (f64::MAX, 3),
            (f64::MIN_POSITIVE, 5),
            // Wider than the fixed-point path lays down, so `core` takes it.
            (1.0, 12),
        ] {
            assert_eq!(
                formatted(value, decimals),
                format!("{value:.decimals$}"),
                "{value} at {decimals} decimals"
            );
        }
    }

    #[test]
    fn relative_extruder_passes_deltas_through() {
        let mut extruder = Extruder::new();
        extruder.set_mode(Code::RelativeE);
        assert_eq!(extruder.observe(0.5), 0.5);
        assert_eq!(extruder.advance(0.75), 0.75);
    }

    #[test]
    fn absolute_extruder_keeps_the_stream_continuous() {
        let mut extruder = Extruder::new();
        // 1.0 -> 2.0 asks for 1 mm; emitting 1.5 mm shifts everything after it.
        assert_eq!(extruder.observe(1.0), 1.0);
        assert_eq!(extruder.advance(1.5), 1.5);
        assert_eq!(extruder.observe(2.0), 1.0);
        assert_eq!(extruder.advance(1.0), 2.5);

        extruder.set_position(0.0);
        assert_eq!(extruder.observe(1.0), 1.0);
        assert_eq!(extruder.advance(1.0), 1.0, "a reset origin starts over");
    }

    /// A caller that buffers a region observes all of it before emitting any
    /// of it, so the two positions are unrelated until replay catches up. The
    /// value handed back is still the right one for each line in turn.
    #[test]
    fn an_extruder_read_ahead_of_its_output_still_meters_correctly() {
        let mut extruder = Extruder::new();
        let deltas: Vec<f64> = [1.0, 2.0, 3.0, 4.0]
            .into_iter()
            .map(|value| extruder.observe(value))
            .collect();
        assert_eq!(deltas, [1.0, 1.0, 1.0, 1.0]);

        let emitted: Vec<f64> = deltas
            .iter()
            .map(|delta| extruder.advance(delta * 1.5))
            .collect();
        assert_eq!(emitted, [1.5, 3.0, 4.5, 6.0]);
    }
}
