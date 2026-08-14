//! MeatPack, the G-code text encoding used inside binary G-code blocks.
//!
//! Fifteen characters cover most of a G-code stream, so each is packed into a
//! nibble and two share a byte; the sixteenth nibble value escapes to a
//! following full-width byte. Two `0xFF` bytes introduce a command that toggles
//! packing, which is how comment lines survive in the "keep comments" variant.
//!
//! Encoding is deliberately absent. It drops whitespace and inline comments, so
//! rewritten G-code is stored unencoded rather than passed through a second
//! lossy round.
//!
//! Ported from Prusa's reference decoder in `libbgcode`, itself derived from
//! Scott Mudge's MeatPack firmware.

const SIGNAL: u8 = 0xFF;
const ENABLE_PACKING: u8 = 251;
const DISABLE_PACKING: u8 = 250;
const RESET_ALL: u8 = 249;
const ENABLE_NO_SPACES: u8 = 247;
const DISABLE_NO_SPACES: u8 = 246;

/// The nibble values below 15; 11 is a space unless "no spaces" is active.
const PACKED: [u8; 15] = *b"0123456789. \nGX";

/// A MeatPack stream, decoded as the blocks carrying it arrive.
///
/// Nothing in the container format makes a G-code block begin on a line
/// boundary or re-issue `ENABLE_PACKING`, so a producer may cut a packed run
/// anywhere. One decoder therefore spans a whole file: starting a fresh one per
/// block reads the rest of the run as raw bytes, and the result still passes
/// that block's checksum.
#[derive(Default)]
pub struct Decoder {
    packing: bool,
    no_spaces: bool,
    /// Second character of a pair, held back until its escaped partner arrives.
    held: u8,
    /// Full-width bytes still owed by escapes already seen.
    owed: usize,
    /// A `0xFF` seen and not yet accounted for: either data, or half of the
    /// pair that introduces a command.
    signals: u8,
    /// A command's two `0xFF` bytes have arrived and its code has not.
    command: bool,
    /// Whether spaces are being put back into the line being written.
    spacing: bool,
    /// The last byte written. It decides spacing and newline collapsing, so it
    /// has to outlive the block it was written in.
    last: Option<u8>,
}

impl Decoder {
    /// Decodes one block's worth of the stream onto the end of `out`.
    pub fn feed(&mut self, source: &[u8], out: &mut Vec<u8>) {
        // A file-supplied length, so it is never allowed to wrap into a small
        // reservation on a target whose pointers are narrower than the file.
        out.reserve(source.len().saturating_mul(2));

        for &byte in source {
            if byte == SIGNAL {
                if self.signals > 0 {
                    self.command = true;
                    self.signals = 0;
                } else {
                    self.signals += 1;
                }
                continue;
            }
            if self.command {
                self.apply(byte);
                self.command = false;
                continue;
            }
            // A lone signal byte was literal data after all.
            if self.signals > 0 {
                self.receive(SIGNAL, out);
                self.signals = 0;
            }
            self.receive(byte, out);
        }
    }

    /// Writes what the stream ended holding, once its last block has been fed.
    ///
    /// A trailing signal byte is only known to be data rather than half a
    /// command when nothing follows it, and this is the last place it can be
    /// written: a decoder that returned without it would drop a byte bound for
    /// the printer while reporting success.
    pub fn finish(&mut self, out: &mut Vec<u8>) -> Result<(), String> {
        if self.command {
            return Err("packed G-code ends inside a command".into());
        }
        if self.signals > 0 {
            self.receive(SIGNAL, out);
            self.signals = 0;
        }
        Ok(())
    }

    fn apply(&mut self, code: u8) {
        match code {
            ENABLE_PACKING => self.packing = true,
            DISABLE_PACKING | RESET_ALL => self.packing = false,
            ENABLE_NO_SPACES => self.no_spaces = true,
            DISABLE_NO_SPACES => self.no_spaces = false,
            _ => {}
        }
    }

    fn character(&self, nibble: u8) -> u8 {
        match nibble {
            11 if self.no_spaces => b'E',
            nibble if nibble < 15 => PACKED[nibble as usize],
            _ => 0,
        }
    }

    fn receive(&mut self, byte: u8, out: &mut Vec<u8>) {
        if !self.packing {
            self.push(byte, out);
            return;
        }
        if self.owed > 0 {
            self.push(byte, out);
            if self.held > 0 {
                let held = self.held;
                self.push(held, out);
                self.held = 0;
            }
            self.owed -= 1;
            return;
        }

        let low = byte & 0x0F;
        let high = byte >> 4;
        match (low == 0x0F, high == 0x0F) {
            (true, true) => self.owed += 2,
            (true, false) => {
                self.owed += 1;
                self.held = self.character(high);
            }
            (false, _) => {
                let first = self.character(low);
                self.push(first, out);
                // A newline ends the line, so its partner nibble is padding.
                if first != b'\n' {
                    if high == 0x0F {
                        self.owed += 1;
                    } else {
                        let second = self.character(high);
                        self.push(second, out);
                    }
                }
            }
        }
    }

    /// Writes one character, re-inserting the spaces MeatPack drops. This
    /// matches the reference decoder, so the text is identical to what Prusa's
    /// own tooling produces.
    fn push(&mut self, character: u8, out: &mut Vec<u8>) {
        let mut opened = false;
        if character == b'G' && self.last.is_none_or(|last| last == b'\n') {
            self.spacing = true;
            opened = true;
        } else if character == b'\n' {
            self.spacing = false;
        }

        if !opened
            && self.spacing
            && self.last.is_none_or(|last| last != b' ')
            && is_word(character)
        {
            out.push(b' ');
            self.last = Some(b' ');
        }

        if character != b'\n' || self.last.is_none_or(|last| last != b'\n') {
            out.push(character);
            self.last = Some(character);
        }
    }
}

fn is_word(character: u8) -> bool {
    matches!(
        character,
        b'X' | b'Y'
            | b'Z'
            | b'E'
            | b'F'
            | b'I'
            | b'J'
            | b'R'
            | b'S'
            | b'G'
            | b'P'
            | b'W'
            | b'H'
            | b'C'
            | b'A'
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn packed(nibbles: &[u8]) -> u8 {
        nibbles[0] | (nibbles[1] << 4)
    }

    /// A whole stream, fed and then closed. The container feeds a decoder one
    /// block at a time, so closing it is a separate step.
    fn decode(source: &[u8]) -> Vec<u8> {
        let mut decoder = Decoder::default();
        let mut out = Vec::new();
        decoder.feed(source, &mut out);
        decoder.finish(&mut out).expect("the stream should close");
        out
    }

    /// A packed run with an escape, a command in the middle of it and a
    /// passthrough tail, so a cut can land anywhere interesting.
    fn run() -> Vec<u8> {
        let mut stream = vec![SIGNAL, SIGNAL, ENABLE_PACKING];
        stream.push(packed(&[13, 1]));
        stream.push(packed(&[14, 1]));
        stream.push(packed(&[0x0F, 2]));
        stream.push(b'Y');
        stream.push(packed(&[12, 0]));
        stream.extend_from_slice(&[SIGNAL, SIGNAL, DISABLE_PACKING]);
        stream.extend_from_slice(b"; kept\n");
        stream
    }

    #[test]
    fn passes_through_unpacked_bytes() {
        assert_eq!(decode(b"; a comment\n"), b"; a comment\n");
    }

    #[test]
    fn unpacks_a_command_delimited_run() {
        // Enable packing, then "1.5\n" as two packed bytes.
        let stream = [
            SIGNAL,
            SIGNAL,
            ENABLE_PACKING,
            packed(&[1, 10]),
            packed(&[5, 12]),
        ];
        assert_eq!(decode(&stream), b"1.5\n");
    }

    #[test]
    fn escapes_to_full_width_characters() {
        // 'Y' is unpackable, so the low nibble escapes and the byte follows.
        let stream = [SIGNAL, SIGNAL, ENABLE_PACKING, packed(&[0x0F, 1]), b'Y'];
        assert_eq!(decode(&stream), b"Y1");
    }

    #[test]
    fn both_nibbles_may_escape() {
        let stream = [
            SIGNAL,
            SIGNAL,
            ENABLE_PACKING,
            packed(&[0x0F, 0x0F]),
            b'M',
            b'K',
        ];
        assert_eq!(decode(&stream), b"MK");
    }

    #[test]
    fn disabling_packing_restores_raw_bytes() {
        let mut stream = vec![SIGNAL, SIGNAL, ENABLE_PACKING, packed(&[1, 12])];
        stream.extend_from_slice(&[SIGNAL, SIGNAL, DISABLE_PACKING]);
        stream.extend_from_slice(b"; kept\n");
        assert_eq!(decode(&stream), b"1\n; kept\n");
    }

    #[test]
    fn no_spaces_mode_maps_the_space_slot_to_e() {
        let stream = [
            SIGNAL,
            SIGNAL,
            ENABLE_PACKING,
            SIGNAL,
            SIGNAL,
            ENABLE_NO_SPACES,
            packed(&[11, 1]),
        ];
        assert_eq!(decode(&stream), b"E1");
    }

    #[test]
    fn reinserts_spaces_between_g_words() {
        // "G1X1Y2\n" packed: G,1 then X,1 then Y escapes, then 2,\n
        let stream = [
            SIGNAL,
            SIGNAL,
            ENABLE_PACKING,
            packed(&[13, 1]),
            packed(&[14, 1]),
            packed(&[0x0F, 2]),
            b'Y',
            packed(&[12, 0]),
        ];
        assert_eq!(decode(&stream), b"G1 X1 Y2\n");
    }

    #[test]
    fn a_lone_signal_byte_is_data() {
        assert_eq!(decode(&[SIGNAL, b'a']), [SIGNAL, b'a']);
    }

    /// A comment carrying a legacy-encoded name can end on `0xFF`, and only
    /// the end of the stream says it was data rather than half a command. A
    /// decoder that dropped it would hand the printer a line it never sliced.
    #[test]
    fn a_stream_that_ends_on_a_signal_byte_keeps_it() {
        assert_eq!(decode(b"; nom\xFF"), b"; nom\xFF");
        assert_eq!(decode(&[SIGNAL]), [SIGNAL]);
    }

    /// Two signal bytes with no code after them is a truncated command, and
    /// the bytes it would have covered are gone. Better refused than decoded
    /// into G-code that is missing them.
    #[test]
    fn a_stream_that_ends_inside_a_command_is_refused() {
        let mut decoder = Decoder::default();
        let mut out = Vec::new();
        decoder.feed(&[SIGNAL, SIGNAL], &mut out);
        assert!(decoder.finish(&mut out).is_err());
    }

    /// The container may cut a packed run at any byte, so decoding one in two
    /// pieces has to give what decoding it whole gives — including when the
    /// cut lands between a command's two signal bytes, or between an escape
    /// and the character it escapes.
    #[test]
    fn a_run_split_anywhere_decodes_the_same() {
        let stream = run();
        let whole = decode(&stream);
        assert_eq!(whole, b"G1 X1 Y2\n; kept\n");

        for cut in 0..=stream.len() {
            let mut decoder = Decoder::default();
            let mut out = Vec::new();
            decoder.feed(&stream[..cut], &mut out);
            decoder.feed(&stream[cut..], &mut out);
            decoder.finish(&mut out).expect("the stream should close");
            assert_eq!(out, whole, "cut at {cut}");
        }
    }
}
