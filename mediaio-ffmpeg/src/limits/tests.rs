use super::*;

/// The defaults' own coherence — that they are finite, that 8K passes
/// and the bomb does not, that no limit is shadowed by a wider one — is
/// asserted at **compile time**, in `limits.rs` beside the constants
/// themselves. Nothing here re-checks it: a `const` block that failed
/// would have stopped the build before this file was reached.
///
/// What is left for a test run is the part that is not constant — that
/// the options structs actually carry what they are handed.
#[test]
fn frame_limits_default_to_the_consts_and_take_overrides() {
  let d = FrameLimits::default();
  assert_eq!(d, FrameLimits::new());
  assert_eq!(d.max_pixels(), DEFAULT_MAX_PIXELS);
  assert_eq!(d.max_frame_bytes(), DEFAULT_MAX_FRAME_BYTES);

  let tuned = FrameLimits::new()
    .with_max_pixels(1024)
    .with_max_frame_bytes(4096);
  assert_eq!(tuned.max_pixels(), 1024);
  assert_eq!(tuned.max_frame_bytes(), 4096);

  let mut mutated = FrameLimits::new();
  mutated.set_max_pixels(7).set_max_frame_bytes(9);
  assert_eq!((mutated.max_pixels(), mutated.max_frame_bytes()), (7, 9));
  // The builder does not mutate the value it was called on.
  assert_eq!(FrameLimits::new(), d);
}

#[test]
fn packet_limits_default_to_the_const_and_take_overrides() {
  assert_eq!(PacketLimits::default(), PacketLimits::new());
  assert_eq!(
    PacketLimits::default().max_packet_bytes(),
    DEFAULT_MAX_PACKET_BYTES,
  );
  assert_eq!(
    PacketLimits::new()
      .with_max_packet_bytes(11)
      .max_packet_bytes(),
    11,
  );
  let mut mutated = PacketLimits::new();
  mutated.set_max_packet_bytes(13);
  assert_eq!(mutated.max_packet_bytes(), 13);
}

#[test]
fn demux_limits_carry_all_three_tiers() {
  let d = DemuxLimits::default();
  assert_eq!(d, DemuxLimits::new());
  assert_eq!(d.packet(), PacketLimits::new());
  assert_eq!(d.max_attachment_bytes(), DEFAULT_MAX_ATTACHMENT_BYTES);
  assert_eq!(
    d.max_total_attachment_bytes(),
    DEFAULT_MAX_TOTAL_ATTACHMENT_BYTES,
  );

  let tuned = DemuxLimits::new()
    .with_packet(PacketLimits::new().with_max_packet_bytes(1))
    .with_max_attachment_bytes(2)
    .with_max_total_attachment_bytes(3);
  assert_eq!(tuned.packet().max_packet_bytes(), 1);
  assert_eq!(tuned.max_attachment_bytes(), 2);
  assert_eq!(tuned.max_total_attachment_bytes(), 3);

  let mut mutated = DemuxLimits::new();
  mutated
    .set_packet(PacketLimits::new().with_max_packet_bytes(4))
    .set_max_attachment_bytes(5)
    .set_max_total_attachment_bytes(6);
  assert_eq!(mutated.packet().max_packet_bytes(), 4);
  assert_eq!(mutated.max_attachment_bytes(), 5);
  assert_eq!(mutated.max_total_attachment_bytes(), 6);
}

/// The chapter limits carry their defaults and take their overrides,
/// through both mutators the house shape asks for.
///
/// The defaults themselves are the load-bearing half: both are
/// **finite**, which is the whole point of the limit — a chapter table
/// is file-controlled and libavformat has no ceiling of its own for it.
#[test]
fn demux_limits_bound_the_chapter_table() {
  let d = DemuxLimits::new();
  assert_eq!(d.max_chapters(), DEFAULT_MAX_CHAPTERS);
  assert_eq!(
    d.max_total_chapter_title_bytes(),
    DEFAULT_MAX_TOTAL_CHAPTER_TITLE_BYTES,
  );

  let tuned = DemuxLimits::new()
    .with_max_chapters(7)
    .with_max_total_chapter_title_bytes(8);
  assert_eq!(tuned.max_chapters(), 7);
  assert_eq!(tuned.max_total_chapter_title_bytes(), 8);

  let mut mutated = DemuxLimits::new();
  mutated
    .set_max_chapters(9)
    .set_max_total_chapter_title_bytes(10);
  assert_eq!(mutated.max_chapters(), 9);
  assert_eq!(mutated.max_total_chapter_title_bytes(), 10);
}
