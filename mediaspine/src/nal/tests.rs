use std::{vec, vec::Vec};

use super::*;

/// An H.264 NAL unit: its header byte, then `payload`.
fn avc(nal_ref_idc: u8, nal_unit_type: u8, payload: &[u8]) -> Vec<u8> {
  let mut nal = vec![(nal_ref_idc << 5) | nal_unit_type];
  nal.extend_from_slice(payload);
  nal
}

/// A non-reference H.264 slice.
fn b_slice() -> Vec<u8> {
  avc(0, 1, &[0x88, 0x84, 0x21])
}

/// A reference H.264 slice.
fn p_slice() -> Vec<u8> {
  avc(2, 1, &[0x9A, 0x02, 0x10])
}

/// An H.264 sequence parameter set.
fn avc_sps() -> Vec<u8> {
  avc(3, 7, &[0x64, 0x00, 0x28])
}

/// An HEVC NAL unit: its two header bytes, then `payload`.
fn hevc(nal_unit_type: u8, layer: u8, temporal_id: u8, payload: &[u8]) -> Vec<u8> {
  let mut nal = vec![
    (nal_unit_type << 1) | (layer >> 5),
    ((layer & 0x1F) << 3) | (temporal_id + 1),
  ];
  nal.extend_from_slice(payload);
  nal
}

/// An HEVC SPS declaring `max_sub_layers_minus1`.
fn hevc_sps(max_sub_layers_minus1: u8) -> Vec<u8> {
  hevc(33, 0, 0, &[(max_sub_layers_minus1 << 1) | 1, 0x01, 0x60])
}

/// An HEVC VPS declaring `max_sub_layers_minus1`.
fn hevc_vps(max_sub_layers_minus1: u8) -> Vec<u8> {
  hevc(32, 0, 0, &[0x0C, 0x01 | (max_sub_layers_minus1 << 1), 0xFF])
}

/// `rbsp` with an emulation prevention byte before every byte of 3 or
/// less that follows two zero bytes, as an encoder writes it.
fn escaped(rbsp: &[u8]) -> Vec<u8> {
  let mut out = Vec::new();
  let mut zeros = 0;
  for &byte in rbsp {
    if zeros >= 2 && byte <= 3 {
      out.push(3);
      zeros = 0;
    }
    out.push(byte);
    zeros = if byte == 0 { zeros + 1 } else { 0 };
  }
  out
}

/// One SEI message: its `payloadType` and `payloadSize`, each coded in
/// runs of `0xFF`, then `payload`.
fn sei_message(payload_type: u32, payload: &[u8]) -> Vec<u8> {
  let mut out = Vec::new();
  for mut value in [payload_type as usize, payload.len()] {
    while value >= 255 {
      out.push(0xFF);
      value -= 255;
    }
    out.push(value as u8);
  }
  out.extend_from_slice(payload);
  out
}

/// The RBSP of an SEI unit holding `messages`, its trailing byte last.
fn sei_rbsp(messages: &[Vec<u8>]) -> Vec<u8> {
  let mut rbsp: Vec<u8> = messages.concat();
  rbsp.push(0x80);
  rbsp
}

/// An H.264 SEI unit holding `messages`.
fn avc_sei(messages: &[Vec<u8>]) -> Vec<u8> {
  avc(0, 6, &escaped(&sei_rbsp(messages)))
}

/// An HEVC SEI unit of `nal_unit_type`, prefix or suffix, holding
/// `messages`.
fn hevc_sei(nal_unit_type: u8, temporal_id: u8, messages: &[Vec<u8>]) -> Vec<u8> {
  hevc(nal_unit_type, 0, temporal_id, &escaped(&sei_rbsp(messages)))
}

/// A picture timing message, three bytes of it zero.
fn picture_timing() -> Vec<u8> {
  sei_message(1, &[0x00, 0x00, 0x00])
}

/// A filler payload message.
fn filler() -> Vec<u8> {
  sei_message(3, &[0xFF, 0xFF])
}

/// A decoded picture hash message: an MD5 of zeros.
fn picture_hash() -> Vec<u8> {
  sei_message(132, &[0; 17])
}

/// `nals` each after a big-endian length of `size` bytes.
fn lengths(size: usize, nals: &[Vec<u8>]) -> Vec<u8> {
  let mut out = Vec::new();
  for nal in nals {
    let length = nal.len().to_be_bytes();
    out.extend_from_slice(&length[length.len() - size..]);
    out.extend_from_slice(nal);
  }
  out
}

/// `nals` each after a start code: four bytes for the first, three after.
fn annex_b(nals: &[Vec<u8>]) -> Vec<u8> {
  let mut out = Vec::new();
  for (index, nal) in nals.iter().enumerate() {
    out.extend_from_slice(if index == 0 {
      &[0, 0, 0, 1]
    } else {
      &[0, 0, 1]
    });
    out.extend_from_slice(nal);
  }
  out
}

/// An `avcC` of `sps` and one PPS, its `lengthSizeMinusOne` given.
fn avcc_of(length_size_minus_one: u8, sps: &[u8]) -> Vec<u8> {
  let pps = avc(3, 8, &[0xEE, 0x3C, 0x80]);
  let mut record = vec![1, 0x64, 0x00, 0x28, 0xFC | length_size_minus_one, 0xE1];
  record.extend_from_slice(&(sps.len() as u16).to_be_bytes());
  record.extend_from_slice(sps);
  record.push(1);
  record.extend_from_slice(&(pps.len() as u16).to_be_bytes());
  record.extend_from_slice(&pps);
  record
}

/// An `avcC` of one SPS and one PPS, its `lengthSizeMinusOne` given.
fn avcc(length_size_minus_one: u8) -> Vec<u8> {
  avcc_of(length_size_minus_one, &avc_sps())
}

/// An `hvcC` of one array holding `nals`, its `numTemporalLayers` and
/// `lengthSizeMinusOne` given.
fn hvcc(num_temporal_layers: u8, length_size_minus_one: u8, nals: &[Vec<u8>]) -> Vec<u8> {
  let mut record = vec![0; 23];
  record[0] = 1;
  record[21] = (num_temporal_layers << 3) | length_size_minus_one;
  record[22] = 1;
  record.push(0x80 | 33);
  record.extend_from_slice(&(nals.len() as u16).to_be_bytes());
  for nal in nals {
    record.extend_from_slice(&(nal.len() as u16).to_be_bytes());
    record.extend_from_slice(nal);
  }
  record
}

fn kept(reason: KeptReason) -> Verdict {
  Verdict::Kept(reason)
}

#[test]
fn an_avc_record_frames_lengths_of_one_to_four_bytes() {
  // ISO/IEC 14496-15 allows 1, 2 and 4; FFmpeg reads 3 as it reads the
  // others.
  for (minus_one, size) in [(0, 1), (1, 2), (2, 3), (3, 4)] {
    let mut stream = NalStream::new(NalCodec::H264, &avcc(minus_one));
    let framing = match size {
      1 => Framing::Length(LengthSize::One),
      2 => Framing::Length(LengthSize::Two),
      3 => Framing::Length(LengthSize::Three),
      _ => Framing::Length(LengthSize::Four),
    };
    assert_eq!(stream.framing(), Some(framing), "{size}");
    assert_eq!(
      stream.read(&lengths(size, &[b_slice()])),
      Verdict::Disposable,
      "{size}"
    );
    assert_eq!(
      stream.read(&lengths(size, &[p_slice()])),
      kept(KeptReason::Referenced),
      "{size}"
    );
  }
  // A record that ends before its parameter sets do sets lengths and no
  // size: FFmpeg splits nothing.
  let record = avcc(3);
  let mut short = NalStream::new(NalCodec::H264, &record[..record.len() - 1]);
  assert_eq!(short.framing(), None);
  assert_eq!(
    short.read(&lengths(4, &[b_slice()])),
    kept(KeptReason::Unframed)
  );
  assert_eq!(NalStream::new(NalCodec::H264, &record[..6]).framing(), None);
}

#[test]
fn annex_b_reads_three_and_four_byte_start_codes_and_leading_and_trailing_zeros() {
  // No extradata, or extradata that is no `avcC`, is Annex B.
  for extradata in [&[][..], &annex_b(&[avc_sps()])[..]] {
    let mut stream = NalStream::new(NalCodec::H264, extradata);
    assert_eq!(stream.framing(), Some(Framing::AnnexB));
    let aud = avc(0, 9, &[0xF0]);
    let mut packet = vec![0, 0];
    packet.extend_from_slice(&annex_b(&[aud, b_slice(), b_slice()]));
    packet.extend_from_slice(&[0, 0, 0]);
    assert_eq!(stream.read(&packet), Verdict::Disposable);
    assert_eq!(
      stream.read(&annex_b(&[b_slice(), p_slice()])),
      kept(KeptReason::Referenced)
    );
  }
  let mut stream = NalStream::new(NalCodec::H264, &[]);
  // A byte other than zero before the first start code, no start code at
  // all, and a start code with nothing after it.
  assert_eq!(
    stream.read(&[7, 0, 0, 1, 0x01]),
    kept(KeptReason::Malformed)
  );
  assert_eq!(stream.read(&[0x01, 0x88]), kept(KeptReason::Malformed));
  assert_eq!(
    stream.read(&[0, 0, 1, 0x01, 0x88, 0, 0, 1]),
    kept(KeptReason::Malformed)
  );
}

#[test]
fn an_h264_packet_is_disposable_only_where_every_slice_is_non_reference_and_none_is_idr() {
  let mut stream = NalStream::new(NalCodec::H264, &avcc(3));
  let read = |stream: &mut NalStream, nals: &[Vec<u8>]| stream.read(&lengths(4, nals));
  // An access unit delimiter, filler data and an SEI unit of picture
  // timing ride beside the slices.
  let sei = avc_sei(&[picture_timing()]);
  let aud = avc(0, 9, &[0xF0]);
  let filler_data = avc(0, 12, &[0xFF, 0x80]);
  assert_eq!(
    read(
      &mut stream,
      &[aud.clone(), sei.clone(), b_slice(), filler_data]
    ),
    Verdict::Disposable
  );
  // A field pair: both fields of `nal_ref_idc` 0, or one a reference.
  assert_eq!(
    read(&mut stream, &[b_slice(), b_slice()]),
    Verdict::Disposable
  );
  assert_eq!(
    read(&mut stream, &[b_slice(), p_slice()]),
    kept(KeptReason::Referenced)
  );
  // Slice data partitions are slices too.
  assert_eq!(
    read(&mut stream, &[avc(0, 2, &[0x88]), avc(0, 3, &[0x88])]),
    Verdict::Disposable
  );
  assert_eq!(
    read(&mut stream, &[avc(0, 2, &[0x88]), avc(1, 4, &[0x88])]),
    kept(KeptReason::Referenced)
  );
  // An IDR slice, whatever its `nal_ref_idc`.
  assert_eq!(
    read(&mut stream, &[avc(3, 5, &[0x88])]),
    kept(KeptReason::RandomAccess)
  );
  assert_eq!(
    read(&mut stream, &[avc(0, 5, &[0x88])]),
    kept(KeptReason::RandomAccess)
  );
  // No slice at all.
  assert_eq!(read(&mut stream, &[aud, sei]), kept(KeptReason::NoPicture));
}

#[test]
fn a_parameter_set_an_sei_unit_or_an_end_beside_a_non_reference_slice_keeps_the_packet() {
  let mut stream = NalStream::new(NalCodec::H264, &avcc(3));
  for nal_unit_type in [7, 8, 6, 10, 11, 13, 15, 16, 19, 0, 23, 31] {
    assert_eq!(
      stream.read(&lengths(4, &[avc(3, nal_unit_type, &[0x42]), b_slice()])),
      kept(KeptReason::Companion(nal_unit_type)),
      "{nal_unit_type}"
    );
  }
  // The scalable, multiview and 3-D extensions.
  for nal_unit_type in [14, 20, 21] {
    assert_eq!(
      stream.read(&lengths(4, &[avc(0, nal_unit_type, &[0x42]), b_slice()])),
      kept(KeptReason::Extension),
      "{nal_unit_type}"
    );
  }
}

#[test]
fn an_h264_sei_unit_rides_only_where_every_message_is_picture_timing_or_filler() {
  let mut stream = NalStream::new(NalCodec::H264, &avcc(3));
  let mut read = |sei: Vec<u8>| stream.read(&lengths(4, &[sei, b_slice()]));
  // The picture timing message holds `00 00 00`, so the unit holds an
  // emulation prevention byte the reading drops.
  assert!(
    avc_sei(&[picture_timing()])
      .windows(3)
      .any(|w| w == [0, 0, 3])
  );
  assert_eq!(read(avc_sei(&[picture_timing()])), Verdict::Disposable);
  assert_eq!(
    read(avc_sei(&[picture_timing(), filler()])),
    Verdict::Disposable
  );
  // Every other message keeps the packet: a buffering period, user data
  // registered and unregistered, a recovery point, film grain, frame
  // packing, mastering display, content light — and types past 255,
  // coded in runs of `0xFF`.
  for payload_type in [0, 2, 4, 5, 6, 19, 45, 137, 144, 255, 256, 300] {
    assert_eq!(
      read(avc_sei(&[
        picture_timing(),
        sei_message(payload_type, &[0x42])
      ])),
      kept(KeptReason::Companion(6)),
      "{payload_type}"
    );
  }
  // A unit that does not read as FFmpeg reads one: no message, a message
  // past the unit's end, no trailing byte.
  assert_eq!(read(avc(0, 6, &[0x80])), kept(KeptReason::Companion(6)));
  let whole = avc_sei(&[sei_message(1, &[0x42, 0x42])]);
  let mut short = whole.clone();
  short.remove(whole.len() - 2);
  assert_eq!(read(short), kept(KeptReason::Companion(6)));
  let mut untrailed = whole;
  untrailed.pop();
  assert_eq!(read(untrailed), kept(KeptReason::Companion(6)));
}

#[test]
fn a_packet_that_does_not_parse_is_kept() {
  let mut stream = NalStream::new(NalCodec::H264, &avcc(3));
  // A length of 0, one past the end, and bytes too few for a length.
  assert_eq!(
    stream.read(&[0, 0, 0, 0, 0x01, 0x88]),
    kept(KeptReason::Malformed)
  );
  assert_eq!(
    stream.read(&[0, 0, 0, 9, 0x01, 0x88]),
    kept(KeptReason::Malformed)
  );
  let mut packet = lengths(4, &[b_slice()]);
  packet.extend_from_slice(&[0, 0]);
  assert_eq!(stream.read(&packet), kept(KeptReason::Malformed));
  // The forbidden bit.
  assert_eq!(
    stream.read(&lengths(4, &[vec![0x81, 0x88]])),
    kept(KeptReason::Malformed)
  );
  // A malformed unit after a keeping one is still malformed: the packet
  // does not parse.
  assert_eq!(
    stream.read(&lengths(4, &[p_slice(), vec![0x81, 0x88]])),
    kept(KeptReason::Malformed)
  );
  // A non-reference slice whose bytes hold `00 00 01 41`: FFmpeg's
  // splitter reads a reference slice after the start code.
  assert_eq!(
    stream.read(&lengths(4, &[avc(0, 1, &[0x88, 0, 0, 1, 0x41, 0x99])])),
    kept(KeptReason::Malformed)
  );
  // An empty packet holds no picture.
  assert_eq!(stream.read(&[]), kept(KeptReason::NoPicture));
}

#[test]
fn a_four_byte_length_track_switches_its_framing_where_ffmpeg_does_and_keeps_the_switch() {
  // `h264dec.c` 602–607: a packet that begins `00 00 00 01` whose next four
  // bytes are no length that fits is Annex B, and so are the packets after
  // it until one begins with a length that fits. A packet that switches
  // the framing is kept; the packets after it are read in the new one.
  let mut stream = NalStream::new(NalCodec::H264, &avcc(3));
  let annex = annex_b(&[avc(0, 9, &[0xF0]), b_slice()]);
  assert_eq!(stream.read(&annex), kept(KeptReason::FramingChange));
  assert_eq!(stream.framing(), Some(Framing::AnnexB));
  assert_eq!(stream.read(&annex), Verdict::Disposable);
  assert_eq!(
    stream.read(&lengths(4, &[b_slice()])),
    kept(KeptReason::FramingChange)
  );
  assert_eq!(stream.framing(), Some(Framing::Length(LengthSize::Four)));
  assert_eq!(stream.read(&lengths(4, &[b_slice()])), Verdict::Disposable);
  // Two-byte lengths are read as declared.
  let mut two = NalStream::new(NalCodec::H264, &avcc(1));
  assert_eq!(two.read(&annex), kept(KeptReason::Malformed));
}

#[test]
fn the_four_byte_size_outlives_an_annex_b_record_and_a_length_of_257_is_read_as_one() {
  // An Annex B record makes the stream one of start codes and keeps the
  // four-byte size (`h264_parse.c` 517–519), so the probe still runs.
  let mut stream = NalStream::new(NalCodec::H264, &avcc(3));
  stream.reconfigure(&annex_b(&[avc_sps()]));
  assert_eq!(stream.framing(), Some(Framing::AnnexB));
  // `00 00 01 01 41 …`: as Annex B, a start code and a non-reference
  // slice; as FFmpeg reads it, a four-byte length of 257 and a reference
  // slice.
  let mut reference = avc(2, 1, &[]);
  reference.resize(257, 0x88);
  let packet = lengths(4, &[reference]);
  assert_eq!(&packet[..5], &[0, 0, 1, 1, 0x41]);
  assert_eq!(stream.read(&packet), kept(KeptReason::FramingChange));
  assert_eq!(stream.framing(), Some(Framing::Length(LengthSize::Four)));
  assert_eq!(stream.read(&packet), kept(KeptReason::Referenced));
  // The same bytes on a track no record gave a size: start codes, and a
  // non-reference slice — what the four-byte track must not read.
  let mut annex = NalStream::new(NalCodec::H264, &[]);
  assert_eq!(annex.read(&packet), Verdict::Disposable);
}

#[test]
fn a_record_moves_only_what_ffmpeg_moves() {
  let mut stream = NalStream::new(NalCodec::H264, &avcc(3));
  stream.reconfigure(&avcc(1));
  assert_eq!(stream.framing(), Some(Framing::Length(LengthSize::Two)));
  // An empty record moves nothing (`h264_parse.c` 472–473).
  stream.reconfigure(&[]);
  assert_eq!(stream.framing(), Some(Framing::Length(LengthSize::Two)));
  // An `avcC` that fails part way: lengths, the size kept.
  let record = avcc(3);
  stream.reconfigure(&record[..record.len() - 1]);
  assert_eq!(stream.framing(), Some(Framing::Length(LengthSize::Two)));
  // An Annex B record: start codes, the size kept — two, so no probe.
  stream.reconfigure(&annex_b(&[avc_sps()]));
  assert_eq!(stream.framing(), Some(Framing::AnnexB));
  assert_eq!(stream.read(&annex_b(&[b_slice()])), Verdict::Disposable);
  // A failing `avcC` after it reads lengths again, of the size kept.
  stream.reconfigure(&record[..6]);
  assert_eq!(stream.framing(), Some(Framing::Length(LengthSize::Two)));
  assert_eq!(stream.read(&lengths(2, &[b_slice()])), Verdict::Disposable);

  // HEVC: an empty record moves nothing; one of start codes keeps the
  // size; an `hvcC` that fails part way stops at two.
  let mut hevc_stream = NalStream::new(NalCodec::Hevc, &hvcc(0, 3, &[hevc_sps(0)]));
  hevc_stream.reconfigure(&[]);
  assert_eq!(
    hevc_stream.framing(),
    Some(Framing::Length(LengthSize::Four))
  );
  hevc_stream.reconfigure(&annex_b(&[hevc_sps(0)]));
  assert_eq!(hevc_stream.framing(), Some(Framing::AnnexB));
  let whole = hvcc(0, 3, &[hevc_sps(0)]);
  hevc_stream.reconfigure(&whole[..whole.len() - 1]);
  assert_eq!(
    hevc_stream.framing(),
    Some(Framing::Length(LengthSize::Two))
  );
}

#[test]
fn a_packet_that_is_an_avcc_configures_the_track_and_is_kept() {
  // `h264dec.c` 1045–1050: while lengths are read, a packet that is an
  // `avcC` is read as one, and holds no picture.
  let mut stream = NalStream::new(NalCodec::H264, &avcc(3));
  assert_eq!(stream.read(&avcc(1)), kept(KeptReason::Configuration));
  assert_eq!(stream.framing(), Some(Framing::Length(LengthSize::Two)));
  assert_eq!(stream.read(&lengths(2, &[b_slice()])), Verdict::Disposable);
  // While start codes are read, the same bytes are no record.
  let mut annex = NalStream::new(NalCodec::H264, &[]);
  assert_eq!(annex.read(&avcc(1)), kept(KeptReason::Malformed));
  assert_eq!(annex.framing(), Some(Framing::AnnexB));
}

#[test]
fn a_record_on_a_packet_of_no_bytes_is_read_by_hevc_and_not_by_h264() {
  // FFmpeg's H.264 decoder reads a packet of no bytes as the stream's end,
  // before the record it carries (`h264dec.c` 1034–1036).
  let mut stream = NalStream::new(NalCodec::H264, &avcc(3));
  assert_eq!(
    stream.read_with_record(&[], Some(&avcc(1))),
    kept(KeptReason::NoPicture)
  );
  assert_eq!(stream.framing(), Some(Framing::Length(LengthSize::Four)));
  // On a packet of bytes the record comes first.
  assert_eq!(
    stream.read_with_record(&lengths(2, &[b_slice()]), Some(&avcc(1))),
    Verdict::Disposable
  );
  assert_eq!(stream.framing(), Some(Framing::Length(LengthSize::Two)));
  // HEVC's reads the record whatever the packet (`hevcdec.c` 3854–3860).
  let mut hevc_stream = NalStream::new(NalCodec::Hevc, &hvcc(0, 3, &[hevc_sps(0)]));
  let _ = hevc_stream.read_with_record(&[], Some(&annex_b(&[hevc_sps(0)])));
  assert_eq!(hevc_stream.framing(), Some(Framing::AnnexB));
}

#[test]
fn a_seek_after_the_framing_moved_marks_nothing_again() {
  // Nothing moved: a seek changes nothing.
  let mut stream = NalStream::new(NalCodec::H264, &avcc(3));
  stream.seeked();
  assert_eq!(stream.read(&lengths(4, &[b_slice()])), Verdict::Disposable);
  // Moved, then sought: a decoder that kept its framing and one opened
  // afresh read the next packet otherwise.
  stream.reconfigure(&annex_b(&[avc_sps()]));
  stream.seeked();
  assert_eq!(stream.framing(), None);
  assert_eq!(
    stream.read(&lengths(4, &[b_slice()])),
    kept(KeptReason::FramingUnknown)
  );
  assert_eq!(
    stream.read(&annex_b(&[b_slice()])),
    kept(KeptReason::FramingUnknown)
  );
  // A record after it does not bring it back.
  stream.reconfigure(&avcc(3));
  assert_eq!(
    stream.read(&lengths(4, &[b_slice()])),
    kept(KeptReason::FramingUnknown)
  );
  // Moved and moved back before the seek: the framing is the one the
  // stream opened in, which both decoders read.
  let mut back = NalStream::new(NalCodec::H264, &avcc(3));
  back.reconfigure(&annex_b(&[avc_sps()]));
  back.reconfigure(&avcc(3));
  back.seeked();
  assert_eq!(back.read(&lengths(4, &[b_slice()])), Verdict::Disposable);
}

#[test]
fn a_parameter_set_of_21802_bytes_or_more_leaves_the_size_unknown() {
  // `h264_parse.c` 436–437: such a parameter set that does not decode
  // ends the record with `ERANGE`, the size where it was.
  let big = |length: usize| {
    let mut sps = avc_sps();
    sps.resize(length, 0x11);
    sps
  };
  // 21,800 bytes and its two-byte length: 21,802.
  let mut stream = NalStream::new(NalCodec::H264, &avcc_of(3, &big(21_800)));
  assert_eq!(stream.framing(), None);
  assert_eq!(
    stream.read(&lengths(4, &[b_slice()])),
    kept(KeptReason::FramingUnknown)
  );
  // A record read whole sets it again.
  stream.reconfigure(&avcc(3));
  assert_eq!(stream.read(&lengths(4, &[b_slice()])), Verdict::Disposable);
  // A size it would set and already is stays known.
  stream.reconfigure(&avcc_of(3, &big(21_800)));
  assert_eq!(stream.framing(), Some(Framing::Length(LengthSize::Four)));
  // One byte less is read whole.
  let whole = NalStream::new(NalCodec::H264, &avcc_of(3, &big(21_799)));
  assert_eq!(whole.framing(), Some(Framing::Length(LengthSize::Four)));
}

#[test]
fn a_mid_stream_hvcc_of_other_than_two_byte_lengths_marks_nothing_again() {
  // At the opening an `hvcC` is read whole or leaves no decoder.
  let mut stream = NalStream::new(NalCodec::Hevc, &hvcc(0, 3, &[hevc_sps(0)]));
  assert_eq!(stream.framing(), Some(Framing::Length(LengthSize::Four)));
  // Mid-stream, one of two-byte lengths is two whether or not its
  // parameter sets decode.
  stream.reconfigure(&hvcc(0, 1, &[hevc_sps(0)]));
  assert_eq!(stream.framing(), Some(Framing::Length(LengthSize::Two)));
  assert_eq!(
    stream.read(&lengths(2, &[hevc(0, 0, 0, &[0xAF])])),
    Verdict::Disposable
  );
  // One of four is two under `AV_EF_EXPLODE` if a parameter set does not
  // decode (`hevc/parse.c` 72–76, 121–129).
  stream.reconfigure(&hvcc(0, 3, &[hevc_sps(0)]));
  assert_eq!(stream.framing(), None);
  assert_eq!(
    stream.read(&lengths(4, &[hevc(0, 0, 0, &[0xAF])])),
    kept(KeptReason::FramingUnknown)
  );
  stream.reconfigure(&annex_b(&[hevc_sps(0)]));
  assert_eq!(stream.framing(), None);
}

#[test]
fn an_hevc_slnr_picture_is_disposable_only_at_the_highest_sub_layer() {
  // Three sub-layers, from the SPS in the record.
  let mut stream = NalStream::new(NalCodec::Hevc, &hvcc(0, 3, &[hevc_sps(2)]));
  assert_eq!(stream.highest_sub_layer(), Some(2));
  let read = |stream: &mut NalStream, nals: &[Vec<u8>]| stream.read(&lengths(4, nals));
  let aud = hevc(35, 0, 2, &[0x50]);
  let sei = hevc_sei(39, 2, &[filler()]);
  for nal_unit_type in [0, 2, 4, 6, 8, 10, 12, 14] {
    assert_eq!(
      read(
        &mut stream,
        &[aud.clone(), sei.clone(), hevc(nal_unit_type, 0, 2, &[0xAF])]
      ),
      Verdict::Disposable,
      "{nal_unit_type}"
    );
    // Below it, a higher sub-layer may reference the picture.
    for below in [0, 1] {
      assert_eq!(
        read(&mut stream, &[hevc(nal_unit_type, 0, below, &[0xAF])]),
        kept(KeptReason::BelowHighestSubLayer),
        "{nal_unit_type} at {below}"
      );
    }
  }
  // The sub-layer reference types, and IRAP.
  for nal_unit_type in [1, 3, 5, 7, 9, 11, 13, 15] {
    assert_eq!(
      read(&mut stream, &[hevc(nal_unit_type, 0, 2, &[0xAF])]),
      kept(KeptReason::Referenced),
      "{nal_unit_type}"
    );
  }
  for nal_unit_type in 16..=23 {
    assert_eq!(
      read(&mut stream, &[hevc(nal_unit_type, 0, 0, &[0xAF])]),
      kept(KeptReason::RandomAccess),
      "{nal_unit_type}"
    );
  }
  // A TemporalId of -1, and one past every declared sub-layer — which
  // raises the highest to it.
  assert_eq!(
    read(&mut stream, &[vec![0x00, 0x00, 0xAF]]),
    kept(KeptReason::Malformed)
  );
  assert_eq!(
    read(&mut stream, &[hevc(0, 0, 3, &[0xAF])]),
    kept(KeptReason::Malformed)
  );
  assert_eq!(stream.highest_sub_layer(), Some(3));
  assert_eq!(
    read(&mut stream, &[hevc(0, 0, 2, &[0xAF])]),
    kept(KeptReason::BelowHighestSubLayer)
  );
}

#[test]
fn a_unit_of_a_sub_layer_no_parameter_set_declared_raises_the_highest() {
  let mut stream = NalStream::new(NalCodec::Hevc, &hvcc(0, 3, &[hevc_sps(0)]));
  assert_eq!(stream.highest_sub_layer(), Some(0));
  // A reference slice at TemporalId 2: kept as one, and learned from.
  assert_eq!(
    stream.read(&lengths(4, &[hevc(1, 0, 2, &[0xAF])])),
    kept(KeptReason::Referenced)
  );
  assert_eq!(stream.highest_sub_layer(), Some(2));
  assert_eq!(
    stream.read(&lengths(4, &[hevc(0, 0, 0, &[0xAF])])),
    kept(KeptReason::BelowHighestSubLayer)
  );
}

#[test]
fn a_single_sub_layer_stream_marks_its_slnr_pictures() {
  let mut stream = NalStream::new(NalCodec::Hevc, &hvcc(1, 3, &[hevc_sps(0)]));
  assert_eq!(stream.highest_sub_layer(), Some(0));
  assert_eq!(
    stream.read(&lengths(4, &[hevc(0, 0, 0, &[0xAF])])),
    Verdict::Disposable
  );
}

#[test]
fn the_highest_sub_layer_is_learned_from_every_parameter_set_and_only_rises() {
  // No SPS yet: nothing is disposable, whatever the record's
  // `numTemporalLayers` says.
  let mut stream = NalStream::new(NalCodec::Hevc, &hvcc(1, 3, &[]));
  assert_eq!(stream.highest_sub_layer(), None);
  assert_eq!(
    stream.read(&lengths(4, &[hevc(0, 0, 0, &[0xAF])])),
    kept(KeptReason::SubLayersUnknown)
  );
  // An in-band SPS of one sub-layer: the packet carrying it is kept, the
  // next is read.
  assert_eq!(
    stream.read(&lengths(4, &[hevc_sps(0), hevc(0, 0, 0, &[0xAF])])),
    kept(KeptReason::Companion(33))
  );
  assert_eq!(stream.highest_sub_layer(), Some(0));
  assert_eq!(
    stream.read(&lengths(4, &[hevc(0, 0, 0, &[0xAF])])),
    Verdict::Disposable
  );
  // A VPS of three raises it; a later SPS of one does not lower it.
  let _ = stream.read(&lengths(4, &[hevc_vps(2)]));
  assert_eq!(stream.highest_sub_layer(), Some(2));
  let _ = stream.read(&lengths(4, &[hevc_sps(0)]));
  assert_eq!(stream.highest_sub_layer(), Some(2));
  assert_eq!(
    stream.read(&lengths(4, &[hevc(0, 0, 0, &[0xAF])])),
    kept(KeptReason::BelowHighestSubLayer)
  );
  // The record's `numTemporalLayers` raises it too, once an SPS is known.
  let mut record = NalStream::new(NalCodec::Hevc, &hvcc(4, 3, &[hevc_sps(0)]));
  assert_eq!(record.highest_sub_layer(), Some(3));
  assert_eq!(
    record.read(&lengths(4, &[hevc(0, 0, 0, &[0xAF])])),
    kept(KeptReason::BelowHighestSubLayer)
  );
  // A field past 6 does not parse, and teaches nothing.
  assert_eq!(
    stream.read(&lengths(4, &[hevc_sps(7)])),
    kept(KeptReason::Malformed)
  );
  assert_eq!(stream.highest_sub_layer(), Some(2));
}

#[test]
fn every_parameter_set_ffmpegs_splitter_may_read_is_learned() {
  // A malformed unit before a VPS of three: the packet does not parse, and
  // the VPS is read, as FFmpeg's splitter passes over the unit
  // (`h2645_parse.c` 646–660).
  let mut stream = NalStream::new(NalCodec::Hevc, &hvcc(0, 3, &[hevc_sps(0)]));
  assert_eq!(
    stream.read(&lengths(4, &[vec![0x80, 0x01], hevc_vps(2)])),
    kept(KeptReason::Malformed)
  );
  assert_eq!(stream.highest_sub_layer(), Some(2));
  // A start code within a length-framed unit: the splitter reads the
  // bytes after it as a unit of its own (`h2645_parse.c` 44–52, 559–584).
  let mut within = NalStream::new(NalCodec::Hevc, &hvcc(0, 3, &[hevc_sps(0)]));
  let mut unit = hevc(0, 0, 0, &[0xAF, 0, 0, 1]);
  unit.extend_from_slice(&hevc_sps(2));
  assert_eq!(
    within.read(&lengths(4, &[unit])),
    kept(KeptReason::Malformed)
  );
  assert_eq!(within.highest_sub_layer(), Some(2));
  // Annex B: bytes before the first start code, which the splitter passes
  // over to the units after it (`h2645_parse.c` 565–580).
  let mut annex = NalStream::new(NalCodec::Hevc, &annex_b(&[hevc_sps(0)]));
  let mut packet = vec![7];
  packet.extend_from_slice(&annex_b(&[hevc_vps(2), hevc(0, 0, 0, &[0xAF])]));
  assert_eq!(annex.read(&packet), kept(KeptReason::Malformed));
  assert_eq!(annex.highest_sub_layer(), Some(2));
  // And a record of start codes holding bytes that frame nothing.
  let mut extradata = vec![9, 9];
  extradata.extend_from_slice(&annex_b(&[hevc_sps(1)]));
  let record = NalStream::new(NalCodec::Hevc, &extradata);
  assert_eq!(record.highest_sub_layer(), Some(1));
}

#[test]
fn hevc_annex_b_extradata_and_in_band_sets_are_read() {
  let extradata = annex_b(&[hevc_vps(0), hevc_sps(1), hevc(34, 0, 0, &[0xC1])]);
  let mut stream = NalStream::new(NalCodec::Hevc, &extradata);
  assert_eq!(stream.framing(), Some(Framing::AnnexB));
  assert_eq!(stream.highest_sub_layer(), Some(1));
  assert_eq!(
    stream.read(&annex_b(&[hevc(35, 0, 1, &[0x50]), hevc(0, 0, 1, &[0xAF])])),
    Verdict::Disposable
  );
}

#[test]
fn an_hevc_layer_above_0_or_a_companion_unit_keeps_the_packet() {
  let mut stream = NalStream::new(NalCodec::Hevc, &hvcc(0, 3, &[hevc_sps(0)]));
  let slnr = hevc(0, 0, 0, &[0xAF]);
  assert_eq!(
    stream.read(&lengths(4, &[slnr.clone(), hevc(0, 1, 0, &[0xAF])])),
    kept(KeptReason::Extension)
  );
  for nal_unit_type in [32, 33, 34, 36, 37, 39, 40, 24, 31, 41, 47, 48, 62, 63] {
    let unit = match nal_unit_type {
      32 => hevc_vps(0),
      33 => hevc_sps(0),
      other => hevc(other, 0, 0, &[0x42, 0x42]),
    };
    assert_eq!(
      stream.read(&lengths(4, &[slnr.clone(), unit])),
      kept(KeptReason::Companion(nal_unit_type)),
      "{nal_unit_type}"
    );
  }
  for beside in [35, 38] {
    assert_eq!(
      stream.read(&lengths(4, &[hevc(beside, 0, 0, &[0x50]), slnr.clone()])),
      Verdict::Disposable,
      "{beside}"
    );
  }
}

#[test]
fn an_hevc_sei_unit_rides_only_where_every_message_is_filler_or_a_picture_hash() {
  let mut stream = NalStream::new(NalCodec::Hevc, &hvcc(0, 3, &[hevc_sps(0)]));
  let slnr = hevc(0, 0, 0, &[0xAF]);
  // A prefix unit of filler before the picture, a suffix unit of its hash
  // after it — sixteen zero bytes, so emulation prevention bytes in it.
  assert!(
    hevc_sei(40, 0, &[picture_hash()])
      .windows(3)
      .any(|w| w == [0, 0, 3])
  );
  assert_eq!(
    stream.read(&lengths(
      4,
      &[
        hevc_sei(39, 0, &[filler()]),
        slnr.clone(),
        hevc_sei(40, 0, &[picture_hash(), filler()])
      ]
    )),
    Verdict::Disposable
  );
  // Every other message keeps the packet — picture timing among them, whose
  // field structure FFmpeg keeps for later pictures (`hevc/refs.c`
  // 183–187).
  for payload_type in [0, 1, 4, 5, 6, 129, 136, 137, 144, 147] {
    for nal_unit_type in [39, 40] {
      assert_eq!(
        stream.read(&lengths(
          4,
          &[
            slnr.clone(),
            hevc_sei(
              nal_unit_type,
              0,
              &[filler(), sei_message(payload_type, &[0x42])]
            )
          ]
        )),
        kept(KeptReason::Companion(nal_unit_type)),
        "{payload_type} in {nal_unit_type}"
      );
    }
  }
}

#[test]
fn an_hevc_record_is_detected_as_ffmpeg_detects_it() {
  // At least 23 bytes, its first byte 1 — or 0, with a nonzero second byte
  // or a third above 1 — else Annex B.
  let sps = [hevc_sps(0)];
  assert_eq!(
    NalStream::new(NalCodec::Hevc, &hvcc(0, 1, &sps)).framing(),
    Some(Framing::Length(LengthSize::Two))
  );
  let mut legacy = hvcc(0, 0, &sps);
  legacy[0] = 0;
  legacy[1] = 1;
  assert_eq!(
    NalStream::new(NalCodec::Hevc, &legacy).framing(),
    Some(Framing::Length(LengthSize::One))
  );
  legacy[1] = 0;
  legacy[2] = 1;
  assert_eq!(
    NalStream::new(NalCodec::Hevc, &legacy).framing(),
    Some(Framing::AnnexB)
  );
  // A length of three bytes, read as FFmpeg reads it; and a record that
  // ends inside an array, which stops at two (the decoder then does not
  // open, `hevcdec.c` 4169–4172).
  assert_eq!(
    NalStream::new(NalCodec::Hevc, &hvcc(0, 2, &sps)).framing(),
    Some(Framing::Length(LengthSize::Three))
  );
  let whole = hvcc(0, 3, &sps);
  assert_eq!(
    NalStream::new(NalCodec::Hevc, &whole[..whole.len() - 1]).framing(),
    Some(Framing::Length(LengthSize::Two))
  );
}

/// SplitMix64, for the property below: fixed seeds, the same draws on
/// every platform.
struct Seeded(u64);

impl Seeded {
  fn next(&mut self) -> u64 {
    self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = self.0;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
  }

  fn below(&mut self, bound: u64) -> u64 {
    self.next() % bound
  }
}

/// A draw that stands for an SEI unit of messages its picture alone takes
/// up, rather than a NAL unit type.
const RIDING_SEI: u8 = u8::MAX;

#[test]
fn a_packet_holding_anything_that_may_be_needed_is_never_marked() {
  // Random H.264 and HEVC packets, length-framed and Annex B: whenever one
  // holds a reference slice, a random-access slice, a parameter set or any
  // unit but a non-reference slice at the highest sub-layer, a delimiter,
  // filler or an SEI unit of messages its picture alone takes up — or no
  // slice — it is kept; else it is disposable.
  let mut marked = 0;
  let mut kept_ = 0;
  for seed in 0..512 {
    let mut draw = Seeded(seed);
    let hevc_track = draw.below(2) == 1;
    let annex = draw.below(2) == 1;
    let mut nals = Vec::new();
    let mut needed = false;
    let mut pictures = 0;
    for _ in 0..1 + draw.below(5) {
      // Most units are drawn from those that may ride in a disposable
      // packet, so both sides are drawn often; the rest from every type,
      // an SEI unit among them, of a message that keeps it.
      let friendly = draw.below(5) != 0;
      if hevc_track {
        const RIDE: [u8; 11] = [0, 2, 4, 6, 8, 10, 12, 14, 35, 38, RIDING_SEI];
        let nal_unit_type = if friendly {
          RIDE[draw.below(RIDE.len() as u64) as usize]
        } else {
          draw.below(64) as u8
        };
        let temporal_id = if friendly { 2 } else { draw.below(3) as u8 };
        let layer = if !friendly && draw.below(4) == 0 {
          1
        } else {
          0
        };
        let slnr = matches!(nal_unit_type, 0 | 2 | 4 | 6 | 8 | 10 | 12 | 14);
        let beside = matches!(nal_unit_type, 35 | 38 | RIDING_SEI);
        if layer != 0 || !(slnr && temporal_id == 2 || beside) {
          needed = true;
        }
        if slnr {
          pictures += 1;
        }
        // The units read here never include a parameter set that lowers
        // or raises the stream's three sub-layers.
        let nal = match nal_unit_type {
          RIDING_SEI => hevc_sei(39 + draw.below(2) as u8, 2, &[filler()]),
          32 => hevc_vps(2),
          33 => hevc_sps(2),
          other => hevc(other, layer, temporal_id, &[0xAF, 0x13]),
        };
        nals.push(nal);
      } else {
        const RIDE: [u8; 7] = [1, 2, 3, 4, 9, 12, RIDING_SEI];
        let nal_unit_type = if friendly {
          RIDE[draw.below(RIDE.len() as u64) as usize]
        } else {
          draw.below(32) as u8
        };
        let nal_ref_idc = if friendly { 0 } else { draw.below(4) as u8 };
        let slice = matches!(nal_unit_type, 1..=4);
        let beside = matches!(nal_unit_type, 9 | 12 | RIDING_SEI);
        if !(slice && nal_ref_idc == 0 || beside) {
          needed = true;
        }
        if slice {
          pictures += 1;
        }
        let nal = match nal_unit_type {
          RIDING_SEI => avc_sei(&[picture_timing()]),
          other => avc(nal_ref_idc, other, &[0x88, 0x13]),
        };
        nals.push(nal);
      }
    }
    if pictures == 0 {
      needed = true;
    }
    let (extradata, packet) = match (hevc_track, annex) {
      (false, false) => (avcc(3), lengths(4, &nals)),
      (false, true) => (Vec::new(), annex_b(&nals)),
      (true, false) => (hvcc(0, 3, &[hevc_sps(2)]), lengths(4, &nals)),
      (true, true) => (annex_b(&[hevc_sps(2)]), annex_b(&nals)),
    };
    let codec = if hevc_track {
      NalCodec::Hevc
    } else {
      NalCodec::H264
    };
    let verdict = NalStream::new(codec, &extradata).read(&packet);
    assert_eq!(
      verdict.is_disposable(),
      !needed,
      "seed {seed}: {nals:?} read {verdict:?}"
    );
    if verdict.is_disposable() {
      marked += 1;
    } else {
      kept_ += 1;
    }
  }
  // Both sides drawn.
  assert!(marked > 16 && kept_ > 16, "{marked} marked, {kept_} kept");
}

#[test]
fn a_kept_reason_says_why() {
  let shown = |reason: KeptReason| std::string::ToString::to_string(&reason);
  assert_eq!(
    shown(KeptReason::Companion(8)),
    "a NAL unit of type 8 a later picture may need"
  );
  assert_eq!(
    shown(KeptReason::BelowHighestSubLayer),
    "a picture below the highest temporal sub-layer"
  );
  assert_eq!(
    shown(KeptReason::FramingUnknown),
    "a framing a decoder may read otherwise"
  );
}
