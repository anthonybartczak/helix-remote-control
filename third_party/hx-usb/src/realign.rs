//! Keeps the events channel's byte stream on message boundaries.
//!
//! Now and then the device sends an events frame with one notification's
//! first four bytes moved behind the rest of it:
//!
//! ```text
//! expected  00 00 04 00 | 0d 00 00 00 <13-byte body> | 00 00 04 00 0d ...
//! received  0d 00 00 00 <13-byte body> | 00 00 04 00 | 00 00 04 00 0d ...
//! ```
//!
//! The byte count is right, so the acknowledgement stays right, but the
//! stream reader then takes a body as a length: either "1779853698 bytes",
//! which loses the session, or 262144 bytes, which it waits for while every
//! later notification piles up behind it. Seen on an HX Effects (3.80) under
//! helix-bridge, and twice in TonePush's own HX Edit capture of an HX Stomp
//! (`captures/mac-enums-capture.log`), so it is the device, not the host.
//!
//! Every notification in those captures (about 70 000) starts `00 00 04 00`
//! (from the device, events service) and is at most 36 bytes; the reply that
//! opens the service may carry service 0. So this passes on only whole
//! messages with that prefix and a modest length, and steps over anything
//! else a byte at a time until the stream makes sense again. The cost of a
//! slip is the one notification it cut through.

/// Largest notification taken as genuine. Real ones are tens of bytes; this
/// leaves room for any not captured yet while rejecting a body read as a
/// length.
const MAX_MESSAGE: usize = 64 * 1024;

#[derive(Default)]
pub(crate) struct Realign {
    buf: Vec<u8>,
    /// Bytes stepped over to find a message boundary, over the session.
    skipped: u64,
}

impl Realign {
    /// Take bytes from the wire and return those that form whole, plausible
    /// messages, in order. A message cut across frames waits for the rest.
    pub(crate) fn push(&mut self, bytes: &[u8]) -> Vec<u8> {
        self.buf.extend_from_slice(bytes);
        let mut out = Vec::new();
        let mut pos = 0;
        while let Some(header) = self.buf.get(pos..pos + 8) {
            let originator = u16::from_le_bytes([header[0], header[1]]);
            let service = u16::from_le_bytes([header[2], header[3]]);
            let len = u32::from_le_bytes([header[4], header[5], header[6], header[7]]) as usize;
            if originator != 0 || !matches!(service, 0 | 4) || len > MAX_MESSAGE {
                pos += 1;
                self.skipped += 1;
                continue;
            }
            let end = pos + 8 + len;
            if self.buf.len() < end {
                break;
            }
            out.extend_from_slice(&self.buf[pos..end]);
            pos = end;
        }
        self.buf.drain(..pos);
        out
    }

    /// Forget a partial message, as when the channel is opened again.
    pub(crate) fn clear(&mut self) {
        self.buf.clear();
    }

    pub(crate) fn skipped(&self) -> u64 {
        self.skipped
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hx_proto::rpc::StreamReader;

    /// `event 22 {68: 10, 121: 27}`, the steady tick, as the device sends it.
    const TICK: [u8; 21] = [
        0x00, 0x00, 0x04, 0x00, 0x0d, 0x00, 0x00, 0x00, 0x82, 0x69, 0x16, 0x6a, 0x84, 0x52, 0x00,
        0x44, 0x0a, 0x79, 0x1b, 0x6a, 0xc0,
    ];

    fn ticks(n: usize) -> Vec<u8> {
        TICK.repeat(n)
    }

    /// The tick with its first four bytes moved behind the rest.
    fn rotated() -> Vec<u8> {
        [&TICK[4..], &TICK[..4]].concat()
    }

    /// How many messages a stream reader finds in what `realign` passes on.
    fn messages(realign: &mut Realign, pushes: &[Vec<u8>]) -> usize {
        let mut reader = StreamReader::new();
        for bytes in pushes {
            reader.push(&realign.push(bytes));
        }
        reader.take_messages().expect("only whole messages").len()
    }

    #[test]
    fn whole_messages_pass_unchanged() {
        let mut realign = Realign::default();
        assert_eq!(realign.push(&ticks(3)), ticks(3));
        assert_eq!(realign.skipped(), 0);
    }

    #[test]
    fn a_message_split_across_frames_waits_for_the_rest() {
        // The device splits after the first four bytes, often with a
        // keep-alive on the first frame.
        let mut realign = Realign::default();
        assert!(realign.push(&TICK[..4]).is_empty());
        assert_eq!(realign.push(&TICK[4..]), TICK.to_vec());
        assert_eq!(realign.skipped(), 0);
    }

    #[test]
    fn a_rotated_frame_costs_one_notification() {
        // HX Effects capture, events frame seq 7: one rotated tick, then four.
        let frame = [rotated(), ticks(4)].concat();
        let mut realign = Realign::default();
        assert_eq!(messages(&mut realign, &[ticks(3), frame, ticks(2)]), 9);
        assert_eq!(realign.skipped(), 21);
    }

    #[test]
    fn a_rotated_frame_after_a_split_does_not_stall_the_stream() {
        // A frame ending in a tick's first four bytes, then a rotated one:
        // unfiltered, the reader pairs them up and then waits for a
        // 262144-byte message that never comes.
        let mut realign = Realign::default();
        let pushes = [
            [ticks(2), TICK[..4].to_vec()].concat(),
            [rotated(), ticks(3)].concat(),
        ];
        assert_eq!(messages(&mut realign, &pushes), 6);
    }

    #[test]
    fn the_service_open_reply_passes() {
        // HX Edit capture: the events service answers with service 0.
        let reply = [0, 0, 0, 0, 1, 0, 0, 0, 4];
        let mut realign = Realign::default();
        assert_eq!(realign.push(&reply), reply.to_vec());
    }

    #[test]
    fn a_body_read_as_a_huge_length_is_stepped_over() {
        let mut realign = Realign::default();
        let garbage = [0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x04, 0x00];
        assert_eq!(messages(&mut realign, &[garbage.to_vec(), ticks(1)]), 1);
        assert_eq!(realign.skipped(), 8);
    }
}
