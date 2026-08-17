//! Line-level G-code parsing.
//!
//! A [`Line`] borrows the text it was parsed from and carries the byte span of
//! its `E` word, so extrusion can be rewritten without disturbing the rest of
//! the line. [`Lines`] supplies that text one line at a time from any reader.
//! [`feature`] turns the region markers a slicer writes between walls into one
//! enum, whichever dialect it writes them in.

use std::borrow::Cow;
use std::io::{self, BufRead, Read, Write};

pub mod feature;

/// The length at which text stops being read as a line of G-code.
///
/// It is two bounds in one. The first is on memory: reading to a `\n` with no
/// ceiling puts the whole input in one buffer whenever the input has none — a
/// file written with bare `\r` by classic Mac tooling, a binary blob that
/// reached the text path — and the lossy repair below then copies it again, so
/// peak memory becomes a multiple of the file rather than the flat cost this
/// tool promises.
///
/// The second is on meaning. A command a firmware will run is tens of bytes;
/// Marlin's own serial line buffer is 96, and everything past it is dropped
/// before the line is even queued. Every long line a slicer writes is a
/// comment instead — a config block, or a thumbnail, which arrives already
/// wrapped at around eighty characters — and the longest of them is a user's
/// start G-code copied into one comment, kilobytes at the very worst. Sixty
/// four kilobytes is therefore hundreds of times the longest command and many
/// times the longest comment, while three copies of it still cost less than a
/// quarter of the write buffer this crate already keeps.
pub const MAX_LINE: usize = 64 * 1024;

/// How far past the cap a terminator is still looked for. A line that ends
/// inside this window is handed over whole, so the cap only ever splits text
/// with no line ending anywhere in reach of it, and every piece it does hand
/// over is at least [`MAX_LINE`] long — which is what [`Line::parse`] reads as
/// "not a command".
const READ_WINDOW: usize = 2 * MAX_LINE;

/// Reads a stream one line at a time through a buffer it reuses, so the input
/// side of a transform costs one line of memory however large the file is.
///
/// Terminators are stripped exactly as [`str::lines`] strips them, `\n` and
/// `\r\n`, so what a transform sees never depends on whether the G-code
/// arrived as a string or as a file. A bare `\r` is deliberately **not** one.
/// Marlin ends a line on it and Klipper does not, so splitting there would
/// turn the tail of a comment a Klipper print ignores into commands it obeys;
/// and [`Survey::of`](crate::scan::Survey::of) reads an in-memory file with
/// [`str::lines`], so recognising one here alone would leave the two passes
/// disagreeing about where the lines of a file are.
///
/// Anything longer than [`MAX_LINE`] with no terminator in reach is handed
/// over in pieces rather than buffered whole, and [`Lines::partial`] marks
/// them. No piece is ever read as a command — see [`Line::parse`] — so the
/// transforms copy such a line through exactly as they found it.
///
/// Slicers copy model and filament names into comments in whatever encoding the
/// host uses, so a file that is otherwise plain ASCII G-code can still carry a
/// few bytes that are not UTF-8. Those lines are repaired rather than rejected:
/// commands are ASCII, so the damage stays inside a comment that was already
/// unreadable.
pub struct Lines<R> {
    reader: R,
    buffer: Vec<u8>,
    /// What is left of a line too long to hand over whole, waiting to open the
    /// next piece of it.
    carry: Vec<u8>,
    partial: bool,
    /// A repaired line, which no longer matches the bytes in `buffer`.
    repaired: String,
}

impl<R: BufRead> Lines<R> {
    pub fn new(reader: R) -> Self {
        Self {
            reader,
            buffer: Vec::new(),
            carry: Vec::new(),
            partial: false,
            repaired: String::new(),
        }
    }

    /// True when the text just handed over is one piece of a line longer than
    /// [`MAX_LINE`] rather than a line of its own.
    ///
    /// A caller copying the stream through must not put a terminator after a
    /// piece: the pieces are one line of the input, and a terminator written
    /// between two of them makes the tail of that line a command the printer
    /// would obey.
    pub fn partial(&self) -> bool {
        self.partial
    }

    /// The next line, or `None` at the end of the stream.
    pub fn next_line(&mut self) -> io::Result<Option<Text<'_>>> {
        self.buffer.clear();
        if !self.carry.is_empty() {
            self.buffer.append(&mut self.carry);
        }
        let opened = self.buffer.len();

        let room = (READ_WINDOW - opened) as u64;
        let read = self
            .reader
            .by_ref()
            .take(room)
            .read_until(b'\n', &mut self.buffer)?;
        if read == 0 && opened == 0 {
            return Ok(None);
        }

        let ended = self.buffer.last() == Some(&b'\n');
        let mut cut = false;
        if !ended && self.buffer.len() >= READ_WINDOW {
            // Cutting through a character would come back from the repair
            // below as replacement bytes, which is the one thing a line too
            // long to transform must not suffer, so the cut moves forward off
            // any continuation byte it lands on.
            let mut at = MAX_LINE;
            while at < self.buffer.len().min(MAX_LINE + 3) && (self.buffer[at] & 0xC0) == 0x80 {
                at += 1;
            }
            self.carry.extend_from_slice(&self.buffer[at..]);
            self.buffer.truncate(at);
            cut = true;
        }
        self.partial = cut || opened > 0;

        if ended {
            self.buffer.pop();
            if self.buffer.last() == Some(&b'\r') {
                self.buffer.pop();
            }
        }

        match repaired(&self.buffer) {
            Cow::Borrowed(text) => Ok(Some(Text {
                text,
                bytes: &self.buffer,
            })),
            Cow::Owned(text) => {
                self.repaired = text;
                Ok(Some(Text {
                    text: &self.repaired,
                    bytes: &self.buffer,
                }))
            }
        }
    }
}

/// One line of the input: the bytes it arrived as, and text to parse it from.
///
/// The two are the same string wherever the line is UTF-8, which is every line
/// of nearly every file. Where it is not they are the same **length**, so a
/// span read off `text` indexes `bytes` identically — see [`repaired`].
#[derive(Clone, Copy, Debug)]
pub struct Text<'a> {
    pub text: &'a str,
    pub bytes: &'a [u8],
}

/// `bytes` as text, with each byte that is not part of a valid UTF-8 sequence
/// standing in as one `?`.
///
/// Length-preserving, and that is the whole point: the standard lossy repair
/// turns a run of bad bytes into a single three-byte `U+FFFD`, which moves
/// every span after it and leaves the caller no way to write the line back as
/// it found it. Slicers copy object and filament names into comments in the
/// host's encoding, and a name that came in as CP1252 has to go out as CP1252
/// — the file is the user's only copy, and a byte in a comment neither
/// transform touched must not be rewritten any more than a line ending must.
///
/// `?` rather than anything cleverer because it is ASCII, is not a word
/// letter, and cannot be read as a digit: a repaired byte in the body of a
/// command leaves the command as unreadable as it already was.
pub fn repaired(bytes: &[u8]) -> Cow<'_, str> {
    let mut rest = bytes;
    let mut text = match std::str::from_utf8(rest) {
        Ok(whole) => return Cow::Borrowed(whole),
        Err(_) => String::with_capacity(bytes.len()),
    };
    loop {
        match std::str::from_utf8(rest) {
            Ok(tail) => {
                text.push_str(tail);
                return Cow::Owned(text);
            }
            Err(error) => {
                let good = error.valid_up_to();
                text.push_str(std::str::from_utf8(&rest[..good]).unwrap_or_default());
                // No length means the line ended mid-sequence, so everything
                // left of it is the broken sequence.
                let bad = error.error_len().unwrap_or(rest.len() - good);
                for _ in 0..bad {
                    text.push('?');
                }
                rest = &rest[good + bad..];
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
    /// `G92` — set position, which redefines the extruder origin and states
    /// where the toolhead stands.
    SetPosition,
    /// `M82` — absolute extrusion distances.
    AbsoluteE,
    /// `M83` — relative extrusion distances.
    RelativeE,
    /// `G90` — coordinates name a place.
    AbsolutePosition,
    /// `G91` — coordinates name a displacement. A slicer's custom layer-change
    /// script can leave one in force over a `G1 Z`, where the number is a lift
    /// and not a plane. See [`Modal`].
    RelativePosition,
    /// `G20` — coordinates are in inches.
    Inches,
    /// `G21` — coordinates are in millimetres.
    Millimetres,
    #[default]
    Other,
}

/// One parsed line, borrowed from the source buffer.
#[derive(Clone, Copy, Debug)]
pub struct Line<'a> {
    pub raw: &'a str,
    /// The bytes the line arrived as, which are what is written back.
    ///
    /// G-code is not guaranteed UTF-8 — slicers copy object and filament
    /// names into comments in the host's encoding — so `raw` may be a repair
    /// of these rather than these themselves. [`repaired`] keeps the two the
    /// same length, so every span read off `raw` indexes this identically and
    /// a line can be parsed from one and written from the other.
    origin: &'a [u8],
    pub code: Code,
    pub x: Option<f64>,
    pub y: Option<f64>,
    pub z: Option<f64>,
    pub e: Option<f64>,
    pub f: Option<f64>,
    /// Offsets from the start of an arc to its centre, `I` and `J`. Either may
    /// be absent, which G-code reads as zero — see [`Line::arc`].
    pub i: Option<f64>,
    pub j: Option<f64>,
    /// The radius an arc names instead of a centre, signed: negative picks the
    /// major arc. Resolved to a centre by [`Line::arc_between`].
    pub r: Option<f64>,
    /// True for a `G2`. Which way an arc turns decides which side of the
    /// circle it draws, so a tracer that guesses gets the complement.
    pub clockwise: bool,
    e_span: Option<(usize, usize)>,
    z_span: Option<(usize, usize)>,
    x_span: Option<(usize, usize)>,
    y_span: Option<(usize, usize)>,
    i_span: Option<(usize, usize)>,
    j_span: Option<(usize, usize)>,
    r_span: Option<(usize, usize)>,
    comment_at: Option<usize>,
    /// An `X` or `Y` word was present, whether or not its value was read.
    has_xy: bool,
}

impl<'a> Line<'a> {
    /// Reads every word.
    ///
    /// Text at or over [`MAX_LINE`] carries no command and no words: it is a
    /// config comment, a blob, or one piece of something [`Lines`] could not
    /// hold whole, and a piece that happens to open on `G1 Z50 E9` would
    /// otherwise be read as the command it is only a fragment of. Its comment
    /// is still read, so a settings line that long says what it always did.
    pub fn parse(raw: &'a str) -> Self {
        Self::read(raw, raw.as_bytes(), true)
    }

    /// The same, for a line whose bytes are not the text: `raw` is what
    /// [`repaired`] made of `origin`, and `origin` is what gets written back.
    pub fn parse_bytes(raw: &'a str, origin: &'a [u8]) -> Self {
        Self::read(raw, origin, true)
    }

    /// Reads every word but `X` and `Y`, whose presence is still recorded.
    /// They are the two commonest words in a file, so a pass that only needs
    /// to know that a move went somewhere in the plane, not where, saves most
    /// of its per-line work here.
    pub fn scan(raw: &'a str) -> Self {
        Self::read(raw, raw.as_bytes(), false)
    }

    /// The bytes this line arrived as.
    pub fn origin(&self) -> &'a [u8] {
        self.origin
    }

    fn read(raw: &'a str, origin: &'a [u8], plane: bool) -> Self {
        let comment_at = raw.find(';');
        let body = &raw[..comment_at.unwrap_or(raw.len())];
        let too_long = raw.len() >= MAX_LINE;
        let (code, clockwise) = if too_long {
            (Code::Other, false)
        } else {
            command_of(body)
        };
        let mut line = Line {
            raw,
            origin,
            code,
            x: None,
            y: None,
            z: None,
            e: None,
            f: None,
            i: None,
            j: None,
            r: None,
            clockwise,
            e_span: None,
            z_span: None,
            x_span: None,
            y_span: None,
            i_span: None,
            j_span: None,
            r_span: None,
            comment_at,
            has_xy: false,
        };
        if too_long {
            return line;
        }

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
            // RS-274 puts a comment in parentheses and Marlin honours it as
            // `PAREN_COMMENTS`, so the `E5` in `G1 X10 (prime E5 first)` is
            // prose. Read as a word it books five millimetres of filament.
            if byte == b'(' {
                at = closing(bytes, at);
                continue;
            }
            if !byte.is_ascii_alphabetic() {
                continue;
            }
            let start = at;
            at = value_end(bytes, at);

            // Lowercased, so `X` and `x` reach the same arm.
            let letter = byte | 0x20;
            if matches!(letter, b'x' | b'y') {
                line.has_xy |= at > start;
                if !plane {
                    continue;
                }
            } else if matches!(letter, b'i' | b'j' | b'r') {
                // Only an arc has a centre or a radius; these letters mean
                // other things to other commands.
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
                b'x' => {
                    line.x = Some(value);
                    line.x_span = Some((start, at));
                }
                b'y' => {
                    line.y = Some(value);
                    line.y_span = Some((start, at));
                }
                b'z' => {
                    line.z = Some(value);
                    line.z_span = Some((start, at));
                }
                b'f' => line.f = Some(value),
                b'i' => {
                    line.i = Some(value);
                    line.i_span = Some((start, at));
                }
                b'j' => {
                    line.j = Some(value);
                    line.j_span = Some((start, at));
                }
                b'r' => {
                    line.r = Some(value);
                    line.r_span = Some((start, at));
                }
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

    /// True for a move that can put a bead down somewhere in the plane: a
    /// linear move naming an axis of it, or an arc, which sweeps through the
    /// plane whether or not it names where it ends.
    ///
    /// Both passes have to ask this, and have to ask it the same way. The
    /// survey draws every cell a wall runs through and the rewrite then asks
    /// which of those cells the layers either side of it hold, so a bead one
    /// pass counts and the other does not is a cell asked about that was never
    /// drawn — a column capped where it carries on. It is also what lays out a
    /// file that states no layers of its own, and there the two have to agree
    /// on which bead opened which layer or every per-layer set is consulted
    /// for the wrong layer.
    ///
    /// A bead that runs along one axis names one word, so asking for both
    /// reads it as a travel; an arc naming only `I`/`J` is a full circle, so
    /// asking for `X` or `Y` reads that as nothing at all. Whether it actually
    /// laid material down is the caller's own question: only the caller knows
    /// what the extruder has been told since.
    pub fn draws_in_plane(&self) -> bool {
        self.draws() && (self.has_xy || self.code == Code::Arc)
    }

    pub fn xy(&self) -> Option<(f64, f64)> {
        Some((self.x?, self.y?))
    }

    /// The arc this line draws from its `I`/`J` offsets, or `None` for anything
    /// else.
    ///
    /// An omitted offset is **zero**, which is what G-code says it is: a
    /// `G2 X10 Y0 I5` names a centre 5 mm along X, not a move that is somehow
    /// not an arc. Read as not-an-arc the footprint traces the chord and a full
    /// circle collapses onto one cell.
    ///
    /// An arc that names an `R` instead has no centre to give here, since
    /// resolving one needs both ends of the move. A caller that knows where the
    /// move starts and ends should use [`Line::arc_between`], which covers both
    /// forms.
    pub fn arc(&self) -> Option<crate::geometry::Arc> {
        (self.code == Code::Arc).then_some(())?;
        (self.i.is_some() || self.j.is_some()).then_some(())?;
        Some(crate::geometry::Arc {
            i: self.i.unwrap_or(0.0),
            j: self.j.unwrap_or(0.0),
            clockwise: self.clockwise,
        })
    }

    /// The arc this line draws between two known points, in whichever form it
    /// was written, so everything downstream sees one representation.
    ///
    /// An `R` names the radius rather than the centre, and is resolved here the
    /// way Marlin resolves it: the centre sits on the perpendicular bisector of
    /// the chord, on the side that makes the sweep the short way round for a
    /// positive radius and the long way round for a negative one.
    ///
    /// `None` where the line is not an arc, and where the numbers describe no
    /// circle at all — coincident ends, which the `R` form cannot express, and
    /// a radius shorter than half the chord, which would otherwise take the
    /// square root of a negative and sweep the arc to a NaN.
    pub fn arc_between(&self, from: (f64, f64), to: (f64, f64)) -> Option<crate::geometry::Arc> {
        if let Some(arc) = self.arc() {
            return Some(arc);
        }
        (self.code == Code::Arc).then_some(())?;
        let radius = self.r.filter(|value| value.is_finite())?;

        let (dx, dy) = (to.0 - from.0, to.1 - from.1);
        let chord = dx.hypot(dy);
        let half = chord / 2.0;
        if !chord.is_finite() || chord <= 0.0 {
            return None;
        }
        // A radius shorter than half the chord reaches no circle through both
        // ends, and its root would sweep the arc to a NaN.
        let height = radius * radius - half * half;
        if height < 0.0 {
            return None;
        }
        let height = height.sqrt();
        // Marlin's own rule: the centre changes sides when exactly one of
        // "turns clockwise" and "takes the major arc" holds.
        let side = if self.clockwise != (radius < 0.0) {
            -height / chord
        } else {
            height / chord
        };
        let centre = (
            (from.0 + to.0) / 2.0 - side * dy,
            (from.1 + to.1) / 2.0 + side * dx,
        );
        let (i, j) = (centre.0 - from.0, centre.1 - from.1);
        (i.is_finite() && j.is_finite()).then_some(crate::geometry::Arc {
            i,
            j,
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
    ///
    /// Neither is anything at [`MAX_LINE`]: a marker is a few bytes, so text
    /// that long is a comment nobody wrote as one, or a piece of one, and a
    /// region declared off a fragment relabels the wall that follows it.
    pub fn marker(&self) -> Option<&'a str> {
        let at = self.comment_at?;
        (self.raw.len() < MAX_LINE
            && self.raw[..at]
                .bytes()
                .all(|byte| byte.is_ascii_whitespace()))
        .then(|| &self.raw[at + 1..])
    }

    /// Byte range of the `E` word's digits within [`Line::raw`], for a caller
    /// that keeps the text and rewrites the value later.
    pub fn e_span(&self) -> Option<(usize, usize)> {
        self.e_span
    }

    /// Writes the line with its `X`, `Y` and `E` words replaced, an arc's `I`
    /// and `J` with them, and a `Z` where one is asked for — set in place, or
    /// added where the line has none. Everything else — the command, the
    /// feedrate, any comment — is copied byte for byte. Returns false, having
    /// written nothing, where the line names neither `X` nor `Y` to replace.
    ///
    /// Whichever of the two the line names is rewritten, and an axis it does
    /// not name is left unnamed: a slicer states only the axes that change, so
    /// the one left out is still standing where the move before it put it,
    /// which is where this loop's own previous vertex was just moved to.
    /// Refusing the whole line instead wrote one vertex of a moved loop back
    /// at the place the slicer chose, out by the offset — bounded, but up to
    /// 54 µm at the widest flow, and on the visible face.
    ///
    /// A move can be taken sideways and given a height at once, which is what
    /// lets the visible wall be drawn inward without costing the loop the
    /// travel its height change was going to ride.
    ///
    /// Coordinates keep the three decimals a slicer writes them at, which is
    /// one micron: finer than any printer resolves.
    pub fn write_moved<W: Write>(
        &self,
        out: &mut W,
        to: (f64, f64),
        centre: Option<(f64, f64)>,
        e: Option<f64>,
        z: Option<f64>,
    ) -> io::Result<bool> {
        if self.x_span.is_none() && self.y_span.is_none() {
            return Ok(false);
        }
        let mut edits: Vec<((usize, usize), f64, usize)> = [
            self.x_span.map(|span| (span, to.0, 3usize)),
            self.y_span.map(|span| (span, to.1, 3usize)),
        ]
        .into_iter()
        .flatten()
        .collect();
        let mut append: Vec<(u8, f64)> = Vec::new();
        if let Some(centre) = centre.filter(|_| self.code == Code::Arc) {
            match (self.i_span, self.j_span, self.r_span) {
                // An `R` names the radius rather than the centre, and moving
                // the ends changes the chord it spans, so the radius moves
                // with them. Its sign picks the major arc and is kept.
                (None, None, Some(span)) => {
                    let radius = centre.0.hypot(centre.1);
                    let signed = if self.r.is_some_and(f64::is_sign_negative) {
                        -radius
                    } else {
                        radius
                    };
                    edits.push((span, signed, 3));
                }
                (None, None, None) => {}
                _ => {
                    for (span, letter, value) in
                        [(self.i_span, b'I', centre.0), (self.j_span, b'J', centre.1)]
                    {
                        match span {
                            Some(span) => edits.push((span, value, 3)),
                            // An offset left out is zero, and zero is a place
                            // on the old start. Once the start has moved the
                            // line has to spell it out.
                            None => append.push((letter, value)),
                        }
                    }
                }
            }
        }
        if let (Some(span), Some(value)) = (self.e_span, e) {
            edits.push((span, value, 5));
        }
        match (z, self.z_span) {
            (Some(value), Some(span)) => edits.push((span, value, 3)),
            (Some(value), None) => append.push((b'Z', value)),
            (None, _) => {}
        }
        edits.sort_unstable_by_key(|((start, _), _, _)| *start);
        rewrite(out, self.origin, &edits, &append)?;
        Ok(true)
    }

    /// Writes the line with its `E` word replaced. The rest of it, including
    /// any comment, is copied byte for byte.
    pub fn write_e<W: Write>(&self, out: &mut W, value: f64) -> io::Result<()> {
        write_e(out, self.origin, self.e_span, value)
    }

    /// Writes the line with its `Z` word set to `value`, adding one where the
    /// line has none, and without a trailing newline so the caller can stamp
    /// it. Everything else is copied byte for byte.
    ///
    /// A move the slicer was already making can carry a height change this
    /// way, where a `G1 Z` of its own would stop the toolhead to make it.
    pub fn write_z<W: Write>(&self, out: &mut W, value: f64) -> io::Result<()> {
        match self.z_span {
            Some(span) => rewrite(out, self.origin, &[(span, value, 3)], &[]),
            None => rewrite(out, self.origin, &[], &[(b'Z', value)]),
        }
    }
}

/// Writes `raw` with the number at `span` replaced by `value`, straight to
/// `out` rather than through a `String` that is dropped a moment later.
pub fn write_e<W: Write>(
    out: &mut W,
    raw: &[u8],
    span: Option<(usize, usize)>,
    value: f64,
) -> io::Result<()> {
    let Some(span) = span else {
        return out.write_all(raw);
    };
    rewrite(out, raw, &[(span, value, 5)], &[])
}

/// Writes `raw` with the numbers at `edits` replaced and the words in `append`
/// added, keeping a Marlin line checksum true. `edits` must be sorted and must
/// not overlap.
///
/// Marlin's serial dialect ends a line with `*nn`, the XOR of every byte in
/// front of the `*`, and it stops parsing there. A word written after the `*`
/// is never seen, so the height change silently does not happen; a number
/// changed in front of one leaves the stated checksum stale, and the whole line
/// is rejected. New words therefore go before the `*`, and the checksum is
/// recomputed over what was actually written.
fn rewrite<W: Write>(
    out: &mut W,
    raw: &[u8],
    edits: &[((usize, usize), f64, usize)],
    append: &[(u8, f64)],
) -> io::Result<()> {
    let bytes = raw;
    let comment = raw
        .iter()
        .position(|&byte| byte == b';')
        .unwrap_or(raw.len());
    let checksum = raw[..comment].iter().position(|&byte| byte == b'*');
    let insert = checksum.unwrap_or(comment);

    let mut checked = Checked {
        out: &mut *out,
        sum: 0,
    };
    let mut cut = 0;
    for &((start, end), value, decimals) in edits {
        checked.write_all(&bytes[cut..start])?;
        write_fixed(&mut checked, value, decimals)?;
        cut = end;
    }
    // A word is only ever found in front of the checksum, so this holds; the
    // clamp is here so a line that broke that could not panic mid-file.
    let insert = insert.max(cut);
    if append.is_empty() {
        checked.write_all(&bytes[cut..insert])?;
    } else {
        let head = &bytes[cut..insert];
        let kept = head
            .iter()
            .rposition(|byte| !byte.is_ascii_whitespace())
            .map_or(0, |at| at + 1);
        checked.write_all(&head[..kept])?;
        for &(letter, value) in append {
            checked.write_all(&[b' ', letter])?;
            write_fixed(&mut checked, value, 3)?;
        }
    }

    let sum = checked.sum;
    let Some(star) = checksum else {
        return out.write_all(&bytes[insert..]);
    };
    write!(out, "*{sum}")?;
    out.write_all(&bytes[skip(bytes, star + 1, u8::is_ascii_digit)..])
}

/// A writer that keeps the running XOR of everything put through it, which is
/// what Marlin checks a line against.
struct Checked<'a, W> {
    out: &'a mut W,
    sum: u8,
}

impl<W: Write> Write for Checked<'_, W> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        for &byte in buffer {
            self.sum ^= byte;
        }
        self.out.write_all(buffer)?;
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.out.flush()
    }
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
    let mut at = 0;
    loop {
        at = skip(bytes, at, u8::is_ascii_whitespace);
        if bytes.get(at) != Some(&b'(') {
            break;
        }
        at = closing(bytes, at + 1);
    }
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
    // Matched on the value, not the spelling: `G01` is `G1`, and a line read as
    // neither drops its `E` word, so the next absolute one comes back as a
    // delta covering every move that was skipped.
    let Some(command) = code_number(&bytes[start..end]) else {
        return (Code::Other, false);
    };
    match (letter | 0x20, command) {
        (b'g', 0 | 1) => (Code::Move, false),
        (b'g', 2) => (Code::Arc, true),
        (b'g', 3) => (Code::Arc, false),
        (b'g', 20) => (Code::Inches, false),
        (b'g', 21) => (Code::Millimetres, false),
        (b'g', 90) => (Code::AbsolutePosition, false),
        (b'g', 91) => (Code::RelativePosition, false),
        (b'g', 92) => (Code::SetPosition, false),
        (b'm', 82) => (Code::AbsoluteE, false),
        (b'm', 83) => (Code::RelativeE, false),
        _ => (Code::Other, false),
    }
}

/// The value of a command's digits, or `None` where it has none. Saturating,
/// since no command this tool acts on is anywhere near the ceiling.
fn code_number(digits: &[u8]) -> Option<u32> {
    (!digits.is_empty()).then(|| {
        digits.iter().fold(0u32, |value, byte| {
            value
                .saturating_mul(10)
                .saturating_add(u32::from(byte - b'0'))
        })
    })
}

/// The end of the number that follows a word letter.
///
/// `strtof`, which is how both Marlin and Klipper read a value, takes an
/// exponent, so `X1e5` is one number. Stopping the run at the `e` starts a
/// fresh word there, and on a `G1` that reads as an `E` word: `X1e5` came out
/// as `x = 1` plus five millimetres of filament nobody asked for.
fn value_end(bytes: &[u8], from: usize) -> usize {
    let mut at = from;
    let mut digits = false;
    while at < bytes.len() {
        match bytes[at] {
            b'0'..=b'9' => digits = true,
            b'.' | b'-' | b'+' => {}
            b'e' | b'E' if digits => {
                let mut after = at + 1;
                after += usize::from(matches!(bytes.get(after), Some(b'+' | b'-')));
                let end = skip(bytes, after, u8::is_ascii_digit);
                // A trailing `e` with no digits behind it is not an exponent,
                // and swallowing it would eat the `E` word that follows.
                return if end > after { end } else { at };
            }
            _ => break,
        }
        at += 1;
    }
    at
}

/// The index just past the `)` that closes a parenthetical comment, or the end
/// of the line where nothing does.
fn closing(bytes: &[u8], from: usize) -> usize {
    bytes[from..]
        .iter()
        .position(|byte| *byte == b')')
        .map_or(bytes.len(), |at| from + at + 1)
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

/// Tracks the state a move's coordinates have to be read against: `G90`/`G91`
/// positioning, `G20`/`G21` units, and the place they leave the toolhead.
///
/// A coordinate does not say on its own where the toolhead ends up. Inside a
/// slicer's custom layer-change script a `G91` makes `G1 Z0.6` a lift of
/// 0.6 mm, not a plane at 0.6 mm, and under a `G20` every number is an inch.
///
/// # Using it
///
/// Feed it **every** line of the file, in the order they are read, including
/// the ones the caller does nothing with — the modes it tracks are set by lines
/// that move nothing — and take the move's real end from what
/// [`Modal::apply`] hands back. `None` means the line moved nothing; a caller
/// that needs the position anyway takes [`Modal::position`].
///
/// ```
/// use corbel::gcode::{Line, Modal};
///
/// let mut modal = Modal::new();
/// let mut end = None;
/// for raw in ["G21", "G90", "G1 X10 Y5 Z0.25", "G91", "G1 Z0.5"] {
///     end = modal.apply(&Line::parse(raw)).or(end);
/// }
/// assert_eq!(end, Some((10.0, 5.0, 0.75)));
/// ```
///
/// Lines must be read with [`Line::parse`]. [`Line::scan`] drops `X` and `Y`,
/// and a tracker that never sees them loses the position.
///
/// Extrusion is not tracked here — [`Extruder`] owns it, because `M82`/`M83`
/// override `G90`/`G91` for the `E` axis — but a file in inches states `E` and
/// `F` in inches too, so a caller scaling either takes [`Modal::scale`].
#[derive(Clone, Copy, Debug)]
pub struct Modal {
    absolute: bool,
    inches: bool,
    at: [f64; 3],
}

impl Modal {
    /// Marlin powers up in absolute positioning and millimetres, at the origin
    /// until it is homed.
    pub fn new() -> Self {
        Self {
            absolute: true,
            inches: false,
            at: [0.0; 3],
        }
    }

    /// True while coordinates name a place rather than a displacement.
    pub fn is_absolute(&self) -> bool {
        self.absolute
    }

    /// Millimetres one unit of the file stands for: 25.4 under `G20`.
    pub fn scale(&self) -> f64 {
        if self.inches { 25.4 } else { 1.0 }
    }

    /// True while a coordinate on a line means an absolute place in
    /// millimetres, which is the only state in which one this tool writes says
    /// what it means.
    ///
    /// A `G91` or `G20` section is custom G-code — a colour change, an MMU
    /// swap, a timelapse or a layer-change script — and never a perimeter or a
    /// top surface. What is in one is still measured, so nothing downstream is
    /// misplaced, and left exactly as it was found.
    pub fn is_plain(&self) -> bool {
        self.absolute && !self.inches
    }

    /// Where the toolhead now stands, in mm.
    pub fn position(&self) -> (f64, f64, f64) {
        (self.at[0], self.at[1], self.at[2])
    }

    /// Applies one line, returning where the move it makes ends, in mm, and
    /// `None` for a line that makes none.
    pub fn apply(&mut self, line: &Line<'_>) -> Option<(f64, f64, f64)> {
        let scale = self.scale();
        match line.code {
            Code::AbsolutePosition => self.absolute = true,
            Code::RelativePosition => self.absolute = false,
            Code::Inches => self.inches = true,
            Code::Millimetres => self.inches = false,
            // A `G92` states where the toolhead already is, so it is never a
            // displacement however the positioning mode reads a move.
            Code::SetPosition => {
                for (axis, word) in [line.x, line.y, line.z].into_iter().enumerate() {
                    if let Some(value) = word {
                        self.at[axis] = value * scale;
                    }
                }
            }
            Code::Move | Code::Arc => {
                for (axis, word) in [line.x, line.y, line.z].into_iter().enumerate() {
                    if let Some(value) = word {
                        self.at[axis] = if self.absolute {
                            value * scale
                        } else {
                            self.at[axis] + value * scale
                        };
                    }
                }
                return Some(self.position());
            }
            _ => {}
        }
        None
    }
}

impl Default for Modal {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::turn;

    fn with_e(line: &Line<'_>, value: f64) -> String {
        let mut out = Vec::new();
        line.write_e(&mut out, value)
            .expect("writing to a Vec cannot fail");
        String::from_utf8(out).expect("G-code lines are UTF-8")
    }

    /// Whatever `write` put out, as a string.
    fn written(write: impl FnOnce(&mut Vec<u8>) -> io::Result<()>) -> String {
        let mut out = Vec::new();
        write(&mut out).expect("writing to a Vec cannot fail");
        String::from_utf8(out).expect("G-code lines are UTF-8")
    }

    /// Whether a line's stated `*nn` matches the XOR of the bytes in front of
    /// it, which is what Marlin checks before it will run the line.
    fn verified(line: &str) -> bool {
        let Some((body, tail)) = line.split_once('*') else {
            return false;
        };
        let digits: String = tail.chars().take_while(char::is_ascii_digit).collect();
        digits.parse::<u8>() == Ok(body.bytes().fold(0u8, |sum, byte| sum ^ byte))
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
            out.push(line.text.to_owned());
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
            // A bare `\r` ends no line, here or in `str::lines`: the last one
            // keeps it, and one in the middle of a comment stays inside it.
            "G1 X1\r",
            "; object caf\re\nG1 X2\n",
        ] {
            let expected: Vec<String> = source.lines().map(str::to_owned).collect();
            assert_eq!(lines_of(source), expected, "{source:?}");
        }
    }

    /// Reading to a `\n` with no ceiling put the whole of a stream that has
    /// none into one buffer, and the repair copied it again — three times a
    /// file's size where the architecture promises a flat cost.
    #[test]
    fn a_stream_with_no_newline_is_read_within_the_window() {
        let size = 4 * 1024 * 1024;
        let mut lines = Lines::new(io::BufReader::new(io::repeat(b'x').take(size as u64)));

        let mut seen = 0;
        while let Some(line) = lines.next_line().expect("reading cannot fail") {
            let held = line.text.len();
            assert!(held <= READ_WINDOW, "handed over {held} bytes");
            assert!(lines.partial(), "no part of it is a line of its own");
            seen += held;
        }

        assert_eq!(seen, size, "nothing may be dropped");
        let held = lines.buffer.capacity() + lines.carry.capacity() + lines.repaired.capacity();
        assert!(
            held <= 4 * READ_WINDOW,
            "held {held} bytes of a {size}-byte line"
        );
    }

    /// A line past the window is handed over in pieces, and a piece that opens
    /// on a command must never be read as one: written back out as a line of
    /// its own it is a move the printer would make.
    #[test]
    fn a_line_over_the_cap_is_never_read_as_a_command() {
        let long = format!(
            ";{}G1 Z50 E9{}",
            "x".repeat(MAX_LINE - 1),
            "y".repeat(MAX_LINE)
        );
        let source = format!("G1 X1 E0.5\n{long}\nG1 X2 E0.5\n");

        let mut lines = Lines::new(source.as_bytes());
        let (mut pieces, mut whole) = (String::new(), Vec::new());
        while let Some(line) = lines.next_line().expect("reading a slice cannot fail") {
            let text = line.text.to_owned();
            if !lines.partial() {
                whole.push(text);
                continue;
            }
            let read = Line::parse(&text);
            assert_eq!(read.code, Code::Other, "a piece is not a command");
            assert_eq!((read.x, read.z, read.e), (None, None, None));
            assert_eq!(read.marker(), None, "nor a region marker");
            pieces.push_str(&text);
        }

        assert_eq!(pieces, long, "the line survives byte for byte");
        assert_eq!(whole, ["G1 X1 E0.5", "G1 X2 E0.5"]);
        assert!(
            long[MAX_LINE..].starts_with("G1 Z50"),
            "the cut has to land on the command for this to prove anything"
        );
    }

    /// The pieces are cut at the cap, which lands wherever it lands. Cutting
    /// through a character would come back from the UTF-8 repair as
    /// replacement bytes — the line altered, which is the one thing a line too
    /// long to transform must not be.
    #[test]
    fn an_over_long_line_is_cut_between_characters() {
        let long = "€".repeat(50_000);
        let mut lines = Lines::new(long.as_bytes());

        let mut joined = String::new();
        while let Some(line) = lines.next_line().expect("reading a slice cannot fail") {
            assert!(
                line.text.len() >= MAX_LINE,
                "a piece is never read as a command"
            );
            joined.push_str(line.text);
        }

        assert_eq!(joined, long);
    }

    /// The config block a slicer writes puts a whole custom start G-code on
    /// one line, escaped. It is nowhere near the cap, so it reads exactly as
    /// it always has.
    #[test]
    fn a_long_settings_comment_is_still_one_line() {
        let value = "M104 S200\\nG28\\nG1 Z5 F600\\n".repeat(400);
        let source = format!("; start_gcode = {value}\nG1 X1 E0.5\n");

        let read = lines_of(&source);
        assert_eq!(read, source.lines().map(str::to_owned).collect::<Vec<_>>());

        let line = Line::parse(&read[0]);
        let want = format!(" start_gcode = {value}");
        assert_eq!(line.code, Code::Other);
        assert_eq!(line.e, None);
        assert_eq!(line.comment(), Some(want.as_str()));
    }

    #[test]
    fn a_line_that_is_not_utf8_is_repaired_rather_than_refused() {
        // A slicer naming an object in the host's legacy encoding.
        let source: &[u8] = b"G1 X1 E0.5\n; printing object Caf\xe9\nG1 X2 E0.5\n";
        let mut lines = Lines::new(source);
        let mut out = Vec::new();
        while let Some(line) = lines.next_line().expect("reading a slice cannot fail") {
            out.push((line.text.to_owned(), line.bytes.to_owned()));
        }

        let text: Vec<&str> = out.iter().map(|(text, _)| text.as_str()).collect();
        assert_eq!(
            text,
            ["G1 X1 E0.5", "; printing object Caf?", "G1 X2 E0.5"],
            "only the offending byte may change, and only on its own line"
        );
        // And the bytes handed over beside it are the file's own, so the line
        // can be written back exactly as it arrived.
        assert_eq!(out[1].1, b"; printing object Caf\xe9");
        // The repair stands in one byte for one, so every span read off the
        // text indexes the bytes identically.
        for (text, bytes) in &out {
            assert_eq!(text.len(), bytes.len(), "{text}");
        }
    }

    /// A run of bad bytes is as many stand-ins as there were bytes, and a
    /// sequence cut off by the end of the line is too. The standard lossy
    /// repair collapses each run into one three-byte replacement character,
    /// which moves every span after it.
    #[test]
    fn repairing_a_line_never_changes_its_length() {
        for bytes in [
            &b"; Caf\xe9"[..],
            &b"; \xff\xfe\xfd run"[..],
            &b"; cut \xe2\x82"[..],
            &b"; \xc3\xa9 valid already"[..],
            &b"G1 X1 E0.5 ; \x80"[..],
        ] {
            let text = repaired(bytes);
            assert_eq!(text.len(), bytes.len(), "{text}");
            assert!(!text.contains('\u{fffd}'), "{text}");
        }
        assert!(matches!(repaired(b"; plain ascii"), Cow::Borrowed(_)));
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

    /// Only the three numbers may change; the command, the feedrate, the
    /// spacing and the comment are copied byte for byte.
    #[test]
    fn a_move_is_rewritten_in_place() {
        let mut out = Vec::new();
        let line = Line::parse("G1 X10.5 Y-2 E0.05 F1800 ; wall");
        assert!(
            line.write_moved(&mut out, (1.2345, 6.7), None, Some(0.06), None)
                .unwrap()
        );
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "G1 X1.234 Y6.700 E0.06000 F1800 ; wall"
        );
    }

    /// Word order is a slicer's choice, and a travel has no `E` to rewrite.
    #[test]
    fn a_move_is_rewritten_whatever_order_its_words_come_in() {
        for (raw, want) in [
            ("G1 Y2 X1 F9000", "G1 Y6.700 X1.234 F9000"),
            ("G1 X1 Y2", "G1 X1.234 Y6.700"),
            ("G2 X1 Y2 I3 J4 E1.0", "G2 X1.234 Y6.700 I3 J4 E1.0"),
        ] {
            let mut out = Vec::new();
            assert!(
                Line::parse(raw)
                    .write_moved(&mut out, (1.2345, 6.7), None, None, None)
                    .unwrap()
            );
            assert_eq!(String::from_utf8(out).unwrap(), want, "{raw}");
        }
    }

    /// An arc states its centre from wherever it starts, so a loop moved
    /// sideways has to restate it or the arc is swept round the old one.
    #[test]
    fn an_arc_keeps_its_centre_when_its_start_moves() {
        let mut out = Vec::new();
        assert!(
            Line::parse("G2 X1 Y2 I3 J4 E1.0")
                .write_moved(
                    &mut out,
                    (1.2345, 6.7),
                    Some((2.995, 4.004)),
                    Some(1.5),
                    None
                )
                .unwrap()
        );
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "G2 X1.234 Y6.700 I2.995 J4.004 E1.50000"
        );
    }

    /// A straight move has no centre to restate, and nothing may be added.
    #[test]
    fn a_straight_move_gains_no_centre() {
        let mut out = Vec::new();
        assert!(
            Line::parse("G1 X1 Y2 E1.0")
                .write_moved(&mut out, (1.0, 2.0), Some((3.0, 4.0)), None, None)
                .unwrap()
        );
        assert_eq!(String::from_utf8(out).unwrap(), "G1 X1.000 Y2.000 E1.0");
    }

    /// The travel that reaches a loop is both taken sideways and given the
    /// loop's height, on the one line. Splitting the two would put back the
    /// standalone `G1 Z` that stops the toolhead over the seam.
    #[test]
    fn a_move_can_be_taken_sideways_and_given_a_height_at_once() {
        for (raw, want) in [
            ("G1 X1 Y2 F9000", "G1 X3.000 Y4.000 F9000 Z0.850"),
            ("G1 X1 Y2 Z0.4 F9000", "G1 X3.000 Y4.000 Z0.850 F9000"),
        ] {
            let mut out = Vec::new();
            assert!(
                Line::parse(raw)
                    .write_moved(&mut out, (3.0, 4.0), None, None, Some(0.85))
                    .unwrap()
            );
            assert_eq!(String::from_utf8(out).unwrap(), want, "{raw}");
        }
    }

    #[test]
    fn a_line_with_no_xy_is_not_rewritten() {
        let mut out = Vec::new();
        assert!(
            !Line::parse("G1 Z0.4 F600")
                .write_moved(&mut out, (1.0, 2.0), None, None, None)
                .unwrap()
        );
        assert!(out.is_empty());
    }

    /// A slicer names only the axes that change, so a bead running along one
    /// of them names one word. Refusing the whole line put that vertex back
    /// where the slicer drew it while every other vertex of the loop moved —
    /// out by the offset, up to 54 µm at the widest flow, and on the visible
    /// face. The axis it does not name is left unnamed: the nozzle is already
    /// standing where this loop's previous vertex was just moved to.
    #[test]
    fn a_move_that_names_one_axis_is_still_moved_along_it() {
        for (raw, want) in [
            ("G1 Y10 E1.0", "G1 Y6.700 E1.50000"),
            ("G1 X10 E1.0 ; wall", "G1 X1.234 E1.50000 ; wall"),
        ] {
            let mut out = Vec::new();
            assert!(
                Line::parse(raw)
                    .write_moved(&mut out, (1.2345, 6.7), None, Some(1.5), None)
                    .unwrap()
            );
            assert_eq!(String::from_utf8(out).unwrap(), want, "{raw}");
        }
    }

    /// The one question both passes ask about a move, so every cell the
    /// rewrite meters a loop against is one the survey already drew.
    #[test]
    fn a_move_reaches_the_plane_however_few_words_it_names() {
        for raw in [
            "G1 X1 Y1 E1",
            "G1 Y1 E1",
            "G1 X1 E1",
            // A whole ring in one move, which is what arc fitting emits.
            "G2 I5 J0 E1",
            "G3 X1 Y1 R5 E1",
            // Where a bead was laid is the caller's question, not this one.
            "G1 X1 Y1 F9000",
        ] {
            assert!(Line::parse(raw).draws_in_plane(), "{raw}");
        }
        for raw in ["G1 Z0.4 F600", "G92 E0", "M104 S200", "; a comment"] {
            assert!(!Line::parse(raw).draws_in_plane(), "{raw}");
        }
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
        let stamped = Line::parse("G1 Z0.300 F600 ; corbel brick raised");
        assert_eq!(stamped.marker(), None);
        assert_eq!(stamped.comment(), Some(" corbel brick raised"));
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

    /// Marlin stops parsing at the `*` and rejects a line whose checksum does
    /// not match, so a `Z` appended behind one is a raise that never happens
    /// and an edited `E` is a line thrown away.
    #[test]
    fn a_line_numbered_command_keeps_its_checksum_true() {
        let raw = "N42 G1 X1 Y2 E0.5*57";
        let line = Line::parse(raw);

        let heightened = written(|out| line.write_z(out, 0.85));
        assert!(
            heightened.starts_with("N42 G1 X1 Y2 E0.5 Z0.850*"),
            "a new word goes in front of the checksum: {heightened}"
        );
        assert!(verified(&heightened), "{heightened}");

        let metered = with_e(&line, 0.075);
        assert!(metered.starts_with("N42 G1 X1 Y2 E0.07500*"), "{metered}");
        assert!(verified(&metered), "{metered}");

        let moved = written(|out| {
            line.write_moved(out, (3.0, 4.0), None, Some(0.06), Some(0.3))
                .map(|_| ())
        });
        assert!(
            moved.starts_with("N42 G1 X3.000 Y4.000 E0.06000 Z0.300*"),
            "{moved}"
        );
        assert!(verified(&moved), "{moved}");

        // A `Z` already on the line is set in place, and the tail behind the
        // checksum is copied whatever it holds.
        let in_place = written(|out| Line::parse("N7 G1 X1 Z0.2*13 ; wall").write_z(out, 0.85));
        assert!(in_place.starts_with("N7 G1 X1 Z0.850*"), "{in_place}");
        assert!(in_place.ends_with(" ; wall"), "{in_place}");
        assert!(verified(&in_place), "{in_place}");
    }

    /// `strtof` takes the exponent, so the firmware reads one number where the
    /// scanner used to read two — and the second was an `E`.
    #[test]
    fn an_exponent_belongs_to_its_number_and_is_not_an_e_word() {
        let line = Line::parse("G1 X1e5 Y2");
        assert_eq!(line.x, Some(1e5));
        assert_eq!(line.e, None, "no filament may be invented");
        assert_eq!(Line::parse("G1 X1E-2 Y2").x, Some(0.01));
        assert_eq!(Line::parse("G1 Y2.5e+1 X1").y, Some(25.0));

        // An ordinary move still names its extrusion.
        let plain = Line::parse("G1 X1 E5");
        assert_eq!((plain.x, plain.e), (Some(1.0), Some(5.0)));

        // A trailing `e` is not an exponent, and must not swallow what follows.
        let trailing = Line::parse("G1 X1e E5");
        assert_eq!((trailing.x, trailing.e), (Some(1.0), Some(5.0)));
    }

    /// A command written with a leading zero is the same command, and a line
    /// read as neither drops its `E` word — the next absolute one then comes
    /// back as a delta covering every move that was skipped.
    #[test]
    fn a_zero_padded_command_is_the_same_command() {
        for (raw, code) in [
            ("G00 X1 Y1 F9000", Code::Move),
            ("G01 X10 Y10 E0.5", Code::Move),
            ("G02 X1 Y1 I1 J0", Code::Arc),
            ("G03 X1 Y1 I1 J0", Code::Arc),
            ("G092 E0", Code::SetPosition),
            ("M082", Code::AbsoluteE),
            ("M083", Code::RelativeE),
        ] {
            assert_eq!(Line::parse(raw).code, code, "{raw}");
        }
        assert_eq!(Line::parse("G01 X10 Y10 E0.5").e, Some(0.5));
        assert!(Line::parse("G02 X1 Y1 I1 J0").clockwise);
        assert!(!Line::parse("G03 X1 Y1 I1 J0").clockwise);
        // A letter with no digits behind it still names no command.
        assert_eq!(Line::parse("G X1").code, Code::Other);
    }

    /// RS-274 puts a comment in parentheses and Marlin honours it, so a word
    /// inside one is prose. Read as a word, `G1 X10 (prime E5 first)` books
    /// five millimetres of filament.
    #[test]
    fn a_parenthetical_comment_names_no_words() {
        let line = Line::parse("G1 X10 (prime E5 first) Y2");
        assert_eq!(line.e, None);
        assert_eq!(line.xy(), Some((10.0, 2.0)));
        assert_eq!(Line::parse("G1 X10 (prime E5").e, None, "unterminated");
        assert_eq!(Line::parse("(start) G1 X1 E5").e, Some(5.0));

        // `;` and the markers that ride it are untouched.
        assert_eq!(Line::parse("G1 X1 ; (E5)").comment(), Some(" (E5)"));
        assert_eq!(Line::parse("G1 X1 ; (E5)").e, None);
        assert_eq!(
            Line::parse(";TYPE:Perimeter (outer)").marker(),
            Some("TYPE:Perimeter (outer)")
        );
    }

    /// G-code reads an omitted `I` or `J` as zero, and an `R` names the same
    /// circle from the other side. All three forms have to arrive downstream
    /// as one arc, or the footprint traces the chord and a ring covers nothing.
    #[test]
    fn an_elided_offset_and_an_r_name_the_same_arc_as_i_and_j() {
        let (from, to) = ((0.0, 0.0), (2.0, 0.0));
        let explicit = Line::parse("G2 X2 Y0 I1 J0").arc().unwrap();
        let (centre, radius, _, sweep) = turn(from, to, explicit).unwrap();
        for raw in ["G2 X2 Y0 I1 E1", "G2 X2 Y0 R1 E1"] {
            let arc = Line::parse(raw)
                .arc_between(from, to)
                .unwrap_or_else(|| panic!("{raw} is an arc"));
            let (got_centre, got_radius, _, got_sweep) = turn(from, to, arc).unwrap();
            assert!(
                (got_centre.0 - centre.0).abs() < 1e-12
                    && (got_centre.1 - centre.1).abs() < 1e-12
                    && (got_radius - radius).abs() < 1e-12
                    && (got_sweep - sweep).abs() < 1e-12,
                "{raw} turned about {got_centre:?} r{got_radius} through {got_sweep}"
            );
        }
        // An elided offset reaches the plain `I`/`J` reader too.
        let elided = Line::parse("G2 X2 Y0 I1 E1").arc().unwrap();
        assert_eq!((elided.i, elided.j), (1.0, 0.0));

        // A negative radius picks the major arc, a positive one the minor.
        let swept = |raw: &str| {
            let arc = Line::parse(raw)
                .arc_between((0.0, 0.0), (1.0, 0.0))
                .unwrap();
            let (_, _, _, sweep) = turn((0.0, 0.0), (1.0, 0.0), arc).unwrap();
            sweep.abs()
        };
        assert!((swept("G2 X1 Y0 R1") - std::f64::consts::FRAC_PI_3).abs() < 1e-12);
        assert!((swept("G3 X1 Y0 R1") - std::f64::consts::FRAC_PI_3).abs() < 1e-12);
        assert!((swept("G2 X1 Y0 R-1") - 5.0 * std::f64::consts::FRAC_PI_3).abs() < 1e-12);

        // Nothing that would take the root of a negative may come back as one.
        assert!(
            Line::parse("G2 X2 Y0 R0.5").arc_between(from, to).is_none(),
            "a radius under half the chord names no circle"
        );
        assert!(
            Line::parse("G2 X0 Y0 R1").arc_between(from, from).is_none(),
            "coincident ends name no circle in the R form"
        );
        assert!(Line::parse("G1 X2 Y0 I1 J0").arc().is_none());
    }

    /// An arc states its centre from where it starts, so a move taken sideways
    /// has to restate it — including the offset it never spelled out, and the
    /// radius, which the new chord changes.
    #[test]
    fn an_arc_restates_a_centre_it_did_not_spell_out() {
        for (raw, want) in [
            ("G2 X1 Y2 I3 E1.0", "G2 X1.234 Y6.700 I3.000 E1.0 J4.000"),
            ("G2 X1 Y2 J3 E1.0", "G2 X1.234 Y6.700 J4.000 E1.0 I3.000"),
            ("G2 X1 Y2 R-5 E1.0", "G2 X1.234 Y6.700 R-5.000 E1.0"),
            ("G2 X1 Y2 R5 E1.0", "G2 X1.234 Y6.700 R5.000 E1.0"),
        ] {
            let actual = written(|out| {
                Line::parse(raw)
                    .write_moved(out, (1.2345, 6.7), Some((3.0, 4.0)), None, None)
                    .map(|_| ())
            });
            assert_eq!(actual, want, "{raw}");
        }
    }

    /// The survey, both transforms and the scout each keep one of these over
    /// the same lines, so all four reach the same place for every move.
    #[test]
    fn the_modal_tracker_follows_relative_moves_and_inches() {
        fn ends(modal: &mut Modal, raw: &str) -> Option<(f64, f64, f64)> {
            modal.apply(&Line::parse(raw))
        }

        let mut modal = Modal::new();
        assert!(modal.is_plain(), "a file says nothing until it says so");
        assert_eq!(ends(&mut modal, "G1 X10 Y5 Z0.25"), Some((10.0, 5.0, 0.25)));
        assert_eq!(ends(&mut modal, "G91"), None);
        assert!(!modal.is_absolute());
        assert!(!modal.is_plain(), "a displacement is not a place");
        // A layer-change script's relative lift is not a plane at 0.5.
        assert_eq!(ends(&mut modal, "G1 Z0.5"), Some((10.0, 5.0, 0.75)));
        assert_eq!(ends(&mut modal, "G90"), None);
        assert!(modal.is_plain());
        assert_eq!(ends(&mut modal, "G20"), None);
        assert_eq!(modal.scale(), 25.4);
        assert!(!modal.is_plain(), "an inch is not a millimetre");
        assert_eq!(ends(&mut modal, "G1 X1"), Some((25.4, 5.0, 0.75)));
        assert_eq!(ends(&mut modal, "G21"), None);
        assert!(modal.is_plain());
        // A `G92` states a place, whatever the positioning mode.
        assert_eq!(ends(&mut modal, "G91"), None);
        assert_eq!(ends(&mut modal, "G92 X0 Z0"), None);
        assert_eq!(modal.position(), (0.0, 5.0, 0.0));
        assert_eq!(ends(&mut modal, "M104 S200"), None);
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
