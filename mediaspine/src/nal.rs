//! What a packet's H.264 or HEVC bitstream says about its picture: whether
//! anything references it, so that dropping the packet loses that picture
//! and no other.
//!
//! [`NalStream::read`] reads one packet and answers [`Verdict::Disposable`]
//! only where the bitstream itself proves that nothing references the
//! packet's picture and that nothing else the packet carries is needed
//! later. Anything it cannot prove keeps the packet ([`Verdict::Kept`]),
//! with the reason. Where a demuxer reads a track, the verdict decides
//! [`PacketFlags::DISPOSABLE`](crate::packet::PacketFlags::DISPOSABLE): it
//! is set on a disposable packet alone, and the container's own mark is
//! cleared on every other verdict, since the container cannot see what the
//! bitstream holds — a picture nothing references may still take up what
//! an earlier packet left ([`KeptReason::CarriedOver`]). A track the
//! demuxer does not read carries the container's mark alone.
//!
//! It reads NAL unit headers, the first bytes of an HEVC parameter set, the
//! message headers of an SEI unit, and the bytes of each NAL unit of a
//! length-framed packet, for a start code. It allocates nothing.
//!
//! # What the mark promises
//!
//! **Pixels, and nothing more.** Dropping a disposable packet changes no
//! later picture's pixels. It does not promise a later frame's best-effort
//! timestamp. FFmpeg guesses one for every frame it outputs, choosing the
//! frame's pts or its packet's dts by counts it keeps across every frame
//! output before (`guess_correct_pts`, `libavcodec/decode.c` 296–319 in
//! FFmpeg 9.0.1, its counts at 88–91; called for each frame at 694–696, and
//! reset only at the opening and at a flush, 2087–2090 and 2373–2376). A
//! frame dropped before the decoder leaves the counts, so it can move the
//! guess for a frame after it: with kept frames at (pts, dts) (10, 0) and
//! (100, 2), and between them a dropped one at (10, 1), the last frame's
//! guess is 2 with the dropped one decoded and 100 without. A packet
//! FFmpeg's own `sdtp` marks (`AV_PKT_FLAG_DISPOSABLE`, "frames that can be
//! discarded by the decoder", `libavcodec/packet.h` 665–669, set at
//! `libavformat/mov.c` 11719–11723) has the same property, since the guess
//! reads no flag. A frame's own pts — the pts of the packet its picture
//! came in — moves for no other packet: that is the timestamp a consumer
//! reads to be exact.
//!
//! # The decoder
//!
//! The question is FFmpeg's, asked of its own H.264 and HEVC decoders as
//! libavcodec opens them: fed a track's packets in decoding order — every
//! packet the reading reads but those it marks — as a demuxer delivers
//! them, whole access units and never in chunks (`AV_CODEC_FLAG2_CHUNKS`),
//! with the H.264 decoder's private `is_avc` and `nal_length_size` options
//! left to the configuration record (`libavcodec/h264dec.c` 1093–1094). A
//! decoder opened afresh at a seek is answered for too
//! ([`NalStream::seeked`]). A decoder's frame threads hand its framing on
//! from packet to packet (`libavcodec/h264_slice.c` 419–421,
//! `libavcodec/hevc/hevcdec.c` 4051–4052), so a threaded decoder reads as
//! one thread does.
//!
//! # H.264
//!
//! A non-reference picture — one coded with `nal_ref_idc` equal to 0 — "is
//! not used for inter prediction of any other pictures" (H.264, 3.104), and
//! a slice or slice data partition of `nal_ref_idc` 0 is part of one
//! (7.4.1). An IDR slice's `nal_ref_idc` is never 0, nor a parameter set's
//! (7.4.1). So a packet is disposable iff it holds at least one coded slice
//! (NAL unit types 1–5, Table 7-1), every slice has `nal_ref_idc` equal to
//! 0, and none is an IDR slice (type 5). Every slice is checked, so a
//! packet that carries a field pair is disposable only where both fields
//! are.
//!
//! **A field's pair may be in the next packet.** FFmpeg's decoder joins
//! the two fields of a frame across packets, holding the first, its frame
//! and its SEI until the second arrives (`h264_slice.c` 1397–1500), so
//! dropping one field of a pair costs the other its frame. A packet is
//! therefore disposable only where every sequence parameter set the
//! decoder may read codes frames only (`frame_mbs_only_flag` 1,
//! 7.4.2.1.1): before one has been read, and from the first that lets a
//! picture be a field for good, nothing is
//! ([`KeptReason::MaybeField`]). Proving each slice a frame from its own
//! header, which would mark interlaced streams too, is not done.
//!
//! Beside the slices, only access unit delimiters (9), filler data
//! (12) and an SEI unit whose every message its own picture alone takes up
//! ([SEI](#sei)) may ride. A parameter set (7, 8, 13, 15), any other SEI
//! unit, an end of sequence or of stream (10, 11), and any type this module
//! does not read keep the packet, because a later picture, or what is
//! output after it, may need them. A NAL unit of the scalable, multiview or
//! 3-D extensions (14, 20, 21) keeps the packet too, because a multiview
//! view component of `nal_ref_idc` 0 whose `inter_view_flag` is 1 "may be
//! used for inter-view prediction by other view components in the current
//! access unit" (G.7.4.1.1). This decides for the whole packet, the unit a
//! consumer drops.
//!
//! # HEVC
//!
//! A sub-layer non-reference (SLNR) picture — NAL unit types TRAIL_N, TSA_N,
//! STSA_N, RADL_N and RASL_N, which FFmpeg's decoder decodes (H.265,
//! 7.4.2.2; `hevc/hevcdec.c` 3587–3680) — holds samples no later picture
//! **of the same sub-layer** uses, but a
//! picture of a higher sub-layer may (3.162 and its note). A picture never
//! references one of a higher sub-layer, nor an SLNR picture of its own
//! (8.3.2). So an SLNR picture is disposable at the **highest** temporal
//! sub-layer the stream declares, and only there: every slice an SLNR type
//! of layer 0 at a TemporalId equal to that highest sub-layer.
//!
//! The highest sub-layer is the most any parameter set declares:
//! `sps_max_sub_layers_minus1` (7.4.3.2.1), `vps_max_sub_layers_minus1`,
//! which bounds it, and a configuration record's `numTemporalLayers` less
//! one where it is not 0. It only ever rises, so a reading that is off can
//! only fail to mark ([what the reading learns](#what-the-reading-learns)).
//! Nothing is disposable before a sequence parameter set has been seen, in
//! the configuration record or in the stream.
//!
//! Beside the slices, only access unit delimiters (35), filler data (38)
//! and an SEI unit whose every message its own picture alone takes up
//! ([SEI](#sei)) may ride. A parameter set (32–34), any other prefix or
//! suffix SEI unit (39, 40), an end of sequence or of bitstream (36, 37), a
//! reserved or unspecified type, and any NAL unit of a layer above 0 keep
//! the packet. An IRAP picture is never disposable, and nor is one of the
//! reserved SLNR types RSV_VCL_N10, N12 and N14, which FFmpeg's decoder
//! skips without a picture, so that what rides beside one is carried over
//! to the next picture.
//!
//! **FFmpeg's own `AVDISCARD_NONREF` has no temporal guard**: it skips
//! every SLNR NAL unit at every sub-layer (`libavcodec/hevc/hevcdec.c`
//! 3766–3767, the eight types of `ff_hevc_nal_is_nonref`,
//! `libavcodec/hevc/hevcdec.h` 653–667). On a temporally scalable stream
//! it can drop a picture a higher sub-layer references; this module does
//! not.
//!
//! # SEI
//!
//! An SEI unit rides beside a disposable picture only where every message
//! in it is one FFmpeg's decoder takes up for that picture alone, leaving
//! nothing behind for a later one. The unit is read as FFmpeg reads it
//! (`libavcodec/h264_sei.c` 229–300, `libavcodec/hevc/sei.c` 264–317): the
//! bytes after its header, emulation prevention bytes dropped, are
//! messages, each a `payloadType` and a `payloadSize` coded in runs of
//! `0xFF` and then that many bytes, up to the unit's `rbsp_trailing_bits`,
//! a last byte of `0x80`. A unit that does not read so keeps the packet.
//!
//! | codec | `payloadType` (`libavcodec/sei.h`) | in a packet | why |
//! |---|---|---|---|
//! | H.264 | 1, picture timing | rides beside | FFmpeg clears it before every access unit (`h264dec.c` 593–598, `h264_sei.c` 48–61) and applies it to its own picture (`h264_slice.c` 1185–1235) |
//! | H.264, HEVC | 3, filler payload | rides beside | no decoder reads it: it falls to `ff_h2645_sei_message_decode`, which has no case for it (`h2645_sei.c` 286–317; `h264_sei.c` 282–286, `hevc/sei.c` 242–248, 251–262) |
//! | HEVC | 132, decoded picture hash | keeps the packet | FFmpeg clears it only when a frame ends (`hevc/hevcdec.c` 3505–3512), and its decoder skips decodable pictures too — every SLNR picture under `skip_frame = AVDISCARD_NONREF` (3766–3767), a RASL picture after the CRA it starts at — as well as the reserved types: beside a picture skipped so, the hash carries over to the next picture's check |
//!
//! Every other message keeps the packet: a persistent one — film grain,
//! tone mapping, mastering display, a recovery point, user data whose
//! content FFmpeg keeps (an x264 version, `h2645_sei.c` 92–99) — and every
//! type this module does not read. HEVC's picture timing is among them:
//! FFmpeg keeps its field structure for every later picture that carries
//! none (`hevc/sei.c` 52–75, `hevc/refs.c` 183–187), clearing it only at
//! the decoder's opening (`hevc/hevcdec.c` 4160). So is HEVC's decoded
//! picture hash, as the table says. FFmpeg's own
//! `skip_frame = AVDISCARD_NONREF` never skips an H.264 SEI unit, for the
//! reason this table exists (`h264dec.c` 626–628).
//!
//! # Framing
//!
//! A track's packets carry their NAL units either each after a big-endian
//! length or each after a start code (Annex B). The framing is FFmpeg's
//! decoder's own state, two fields kept apart: whether lengths are read
//! (`is_avc` for H.264, `is_nalff` for HEVC) and the length's size
//! (`nal_length_size`), 0 until a record sets it. [`NalStream`] holds both
//! and moves them where FFmpeg does:
//!
//! | event | H.264 (`h264_parse.c`, `h264dec.c`) | HEVC (`hevc/parse.c`, `hevc/hevcdec.c`) |
//! |---|---|---|
//! | no record, or an empty one | nothing moves (`h264_parse.c` 472–473, `h264dec.c` 399) | nothing moves (`hevcdec.c` 3856, 4168) |
//! | a record whose first byte is 1 (H.264), or of 23 bytes or more whose first is 1, or 0 with a nonzero second or a third above 1 (HEVC) | lengths read, before anything is checked (`h264_parse.c` 475–479) | lengths read (`parse.c` 93–97), of two bytes while the record's arrays are read (103–105) |
//! | that record read whole | the size is its `lengthSizeMinusOne` plus one (515–516) | the same (134–136) |
//! | that record failing part way: too short, or a count past its end | the size stays (481–514) | the size stays two (112–119) |
//! | any other record | start codes read, the size kept (517–522) | start codes read, the size kept (137–143) |
//! | a record on a packet of no bytes | not read: the decoder takes the packet for the stream's end (`h264dec.c` 1034–1036) | read (`hevcdec.c` 3854–3860) |
//! | a packet that is an `avcC` itself, while lengths are read | read as a record (`h264dec.c` 1045–1050) | — |
//! | a packet while the size is four | start codes for one that begins with a four-byte start code whose next four bytes are a length past it, lengths for one whose first four bytes are a length that fits (`h264dec.c` 602–607) | — |
//!
//! A size of 0 while lengths are read frames nothing FFmpeg splits
//! (`libavcodec/h2645_parse.h` 130–148), and every packet is kept
//! ([`KeptReason::Unframed`]). A size of three is read as FFmpeg reads it,
//! though ISO/IEC 14496-15 allows only 1, 2 and 4. A packet that is an
//! `avcC` is kept ([`KeptReason::Configuration`]), and so is one that
//! switches the framing ([`KeptReason::FramingChange`]), so that a decoder
//! which drops every packet marked disposable still sees each packet that
//! moves its framing.
//!
//! **Where the record's reading turns on its parameter sets.** FFmpeg
//! decodes a record's parameter sets as well; this module does not. Where
//! whether one decodes moves the size, the size is unknown, and no packet
//! is marked ([`KeptReason::FramingUnknown`]):
//!
//! - H.264 retries a parameter set that does not decode, escaped, and
//!   refuses there one of 21,802 bytes or more (`h264_parse.c` 436–437),
//!   ending the record: an `avcC` holding one moves the size only if it
//!   decodes. The size is known again when a record sets it.
//! - Under `AV_EF_EXPLODE`, an HEVC record a parameter set of which does
//!   not decode stops with a size of two (`hevc/parse.c` 72–76, 121–129).
//!   The reading cannot know the decoder's `err_recognition`, so after a
//!   mid-stream `hvcC` of any other size nothing is marked again. At the
//!   decoder's opening the same record leaves no decoder at all
//!   (`hevcdec.c` 4169–4172).
//!
//! Otherwise a record's reading is FFmpeg's, an allocation that fails
//! aside: H.264 swallows any other parameter set that does not decode
//! (`h264_parse.c` 421–464), and HEVC every one without `AV_EF_EXPLODE`
//! (`hevc/parse.c` 72–76).
//!
//! **Seeks.** A seek leaves the framing a decoder reads the next packets in
//! unknown wherever the framing differs from the one the stream opened in:
//! a decoder keeps its own across a seek, and one opened afresh starts from
//! the stream's. After such a seek nothing is marked again
//! ([`NalStream::seeked`]).
//!
//! # The NAL unit types
//!
//! | codec | `nal_unit_type` | in a packet | why (clause) |
//! |---|---|---|---|
//! | H.264 | 1–4, `nal_ref_idc` 0, every SPS coding frames only | a picture nothing references | 3.104, 7.4.1, 7.4.2.1.1 |
//! | H.264 | 1–4, `nal_ref_idc` 0, before any SPS or after one that lets fields in | kept: [`KeptReason::MaybeField`] | 7.4.2.1.1; `h264_slice.c` 1397–1500 |
//! | H.264 | 1–4, `nal_ref_idc` above 0 | kept: [`KeptReason::Referenced`] | 7.4.1 |
//! | H.264 | 5, IDR | kept: [`KeptReason::RandomAccess`] | 7.4.1 |
//! | H.264 | 6, SEI of picture timing and filler payload alone | rides beside | [SEI](#sei) |
//! | H.264 | 6, any other SEI | kept: [`KeptReason::Companion`] | Annex D; [SEI](#sei) |
//! | H.264 | 9, 12: AUD, filler | rides beside | Table 7-1 |
//! | H.264 | 7, 8, 13, 15: parameter sets | kept: [`KeptReason::Companion`] | 7.4.1 |
//! | H.264 | 10, 11: end of sequence, of stream | kept: [`KeptReason::Companion`] | Table 7-1 |
//! | H.264 | 14, 20, 21: extensions | kept: [`KeptReason::Extension`] | G.7.4.1.1 |
//! | H.264 | 0, 16–19, 22–31: auxiliary, reserved, unspecified | kept: [`KeptReason::Companion`] | Table 7-1 |
//! | HEVC | 0, 2, 4, 6, 8: SLNR, at the highest sub-layer | a picture nothing references | 3.162, 7.4.2.2, 8.3.2 |
//! | HEVC | 10, 12, 14: reserved SLNR, which FFmpeg skips | kept: [`KeptReason::Companion`] | 7.4.2.2; `hevc/hevcdec.c` 3587–3680 |
//! | HEVC | the same, below the highest sub-layer | kept: [`KeptReason::BelowHighestSubLayer`] | 3.162, note |
//! | HEVC | the same, before any SPS | kept: [`KeptReason::SubLayersUnknown`] | 7.4.3.2.1 |
//! | HEVC | 1, 3, 5, 7, 9, 11, 13, 15: sub-layer reference | kept: [`KeptReason::Referenced`] | 7.4.2.2 |
//! | HEVC | 16–23: IRAP | kept: [`KeptReason::RandomAccess`] | 7.4.2.2 |
//! | HEVC | 32–34: parameter sets | kept: [`KeptReason::Companion`] | Table 7-1 |
//! | HEVC | 35, 38: AUD, filler | rides beside | Table 7-1 |
//! | HEVC | 36, 37: end of sequence, of bitstream | kept: [`KeptReason::Companion`] | Table 7-1 |
//! | HEVC | 39, 40: SEI of filler payload alone | rides beside | [SEI](#sei) |
//! | HEVC | 39, 40: any other SEI | kept: [`KeptReason::Companion`] | Annex D; [SEI](#sei) |
//! | HEVC | 24–31, 41–63: reserved, unspecified | kept: [`KeptReason::Companion`] | Table 7-1 |
//! | HEVC | any type of a layer above 0 | kept: [`KeptReason::Extension`] | Annex F |
//!
//! # What the reading learns
//!
//! An HEVC picture is disposable only at the highest sub-layer, and an
//! H.264 one only where every sequence parameter set codes frames only, so
//! the reading must have read every parameter set the decoder may have. It
//! reads more than FFmpeg's splitter does, never less: every NAL unit of a
//! packet the framing frames, a malformed packet's too; in Annex B, the
//! bytes after every start code, past bytes that frame nothing, where
//! FFmpeg's splitter takes up the stream again (`h2645_parse.c` 559–584);
//! and within a length-framed NAL unit, the bytes after each start code in
//! it, which the splitter reads as units of their own (`h2645_parse.c`
//! 44–52, 559–584). A packet the reading cannot frame, which a decoder
//! still reads in a framing of its own, is read in every framing one may
//! read it in: Annex B, and lengths of one to four bytes. A unit of a
//! sub-layer no parameter set has declared raises the highest sub-layer to
//! its own. An H.264 sequence parameter set is read through its VUI's
//! `bitstream_restriction_flag`, as FFmpeg's decoder reads one, in each of
//! the three readings it may adopt: as its RBSP, as its bytes, and as the
//! bytes FFmpeg writes when it escapes an `avcC` entry to read it again;
//! where its RBSP reads whole, FFmpeg adopts that reading alone. What an
//! adopted reading does not prove — frames only, no gaps in `frame_num`, a
//! reorder depth fixed at 15 or less — stays unproven for good. All of it
//! only raises what later packets are held to, so reading more can only
//! fail to mark.
//!
//! # What a non-reference picture writes
//!
//! A packet is disposable only where dropping it changes no later
//! picture's pixels ([what the mark promises](#what-the-mark-promises)).
//! The census of the state FFmpeg
//! 9.0.1 writes while it decodes a picture nothing references, under any
//! threading and `skip_frame` (at `nonref` and above FFmpeg skips such a
//! picture itself, `h264dec.c` 626–628, `hevc/hevcdec.c` 3766–3767):
//!
//! | state | written at | dropping the picture changes a later picture | the rule |
//! |---|---|---|---|
//! | H.264 `prev_frame_num`, `prev_frame_num_offset` | every picture, `h264_picture.c` 202–203 | through the frame-number offset (POC types 1, 2) and the gap fill, `h264_slice.c` 1432–1530: not where `frame_num` cannot jump, a non-reference picture's then being the next picture's (7.4.3) | every SPS: no gaps ([`KeptReason::FrameNumGaps`]) |
//! | H.264 `prev_poc_lsb`, where a flush left it unset | the first picture after the decoder opens or is flushed, `h264_parse.c` 293–294 | the picture order count of every later picture, the film grain seed among its uses | a slice of a reference picture the decoder decodes, NAL unit type 1 or 5, read since the opening or the last seek: it passes over slice data partitions, `h264dec.c` 682–686 ([`KeptReason::AwaitingReference`]) |
//! | H.264 `prev_poc_msb` / `prev_poc_lsb` otherwise | reference pictures only, `h264_picture.c` 197–201 | no | — |
//! | H.264 `poc_offset` | an intra-only picture under frame threading, `h264_slice.c` 1427–1430 (`picture_intra_only`, `h264dec.c` 501–551) | the H.274 film grain seed, `h264_slice.c` 1258–1260 | not intra only ([`KeptReason::IntraOnly`]) |
//! | H.264 `has_b_frames`, `last_pocs` | every picture, `h264_slice.c` 1305–1333 | output order: with no `bitstream_restriction_flag` FFmpeg grows the reorder depth from the pictures it sees, and a later picture can be output too late and dropped; and a picture that follows 16 it precedes in output order resets the record (1310–1328), which a `max_num_reorder_frames` of 16 allows | every SPS: `bitstream_restriction_flag` with a `max_num_reorder_frames` of 15 at most ([`KeptReason::ReorderUnfixed`]); the depth is then the SPS's (1305–1307, 1411–1414) and the output stays in picture order |
//! | H.264 `delayed_pic`, `next_outputed_poc`, `next_output_pic` | every picture, `h264_slice.c` 1345–1390 | with the depth fixed, only when each picture comes out, not its order | — |
//! | H.264 `mmco_reset`, which the picture after an MMCO 5 picture or a flush takes as an output barrier | `h264_slice.c` 1302–1303 | no: the barrier falls on the next picture decoded, whichever it is, and the output on either side of it stays in picture order | — |
//! | H.264 `non_gray` | any intra slice, `h264_slice.c` 2032–2034; cleared by a flush, `h264dec.c` 492 | whether a later picture that references a gray frame filling a gap uses another, `h264_refs.c` 410–419 | where a slice may be intra, an intra slice in a packet kept since the opening or the last seek ([`KeptReason::AwaitingIntraSlice`]) |
//! | H.264 `frame_recovered`, `recovery_frame` | `h264_slice.c` 1645–1677, 1374–1376 | no: only a reference picture or IDR moves them, and a recovery point SEI keeps its packet | — |
//! | H.264 field pairing: `first_field`, `droppable`, `picture_structure` | `h264_slice.c` 1397–1520 | the second field's frame | every SPS: frames only ([`KeptReason::MaybeField`]) |
//! | H.264 `prev_interlaced_frame` | `h264_slice.c` 1196–1235 | a later frame's interlaced flag, not its pixels | — |
//! | H.264 SEI state | reset per access unit, `h264dec.c` 593–598 | no: a riding SEI is one the picture alone takes up ([SEI](#sei)) | — |
//! | HEVC `poc_tid0` | TemporalId-0 pictures but SLNR, RASL, RADL, `hevc/hevcdec.c` 3297–3306 | no | — |
//! | HEVC `no_rasl_output_flag`, which skips RASL pictures | IRAP pictures, `hevc/hevcdec.c` 3283–3287, 3545–3546 | no | — |
//! | HEVC `recovery_poc` | any picture of the recovery's count, `hevc/hevcdec.c` 3289–3294 | whether the pictures decoded before it are output, `hevc/refs.c` 239–242 | no recovery point SEI — of any layer, a packet's or a record's — since the last IRAP picture the decoder decodes, of types 16–21 ([`KeptReason::Recovering`]) |
//! | HEVC reference marking | every picture's RPS, `hevc/refs.c` | no: a later picture's RPS holds no picture an earlier one dropped (8.3.2) | — |
//! | HEVC output and bumping | `hevc/refs.c`, by the SPS's reorder and DPB sizes | only when each picture comes out | — |
//! | HEVC SEI state and the Dolby Vision RPU that a picture takes up: captions, AFD, LCEVC, time codes, film grain that does not persist, user data, the decoded picture hash, the RPU | the next picture decoded, `hevc/hevcdec.c` 3068–3151, 3505–3512, 3723–3760; `h2645_sei.c` 518–695 | that picture's side data, its film grain and its hash check, where a packet whose picture the decoder does not decode — a RASL picture it skips, a reserved type, none at all — left them, or a configuration record did, whose SEI the decoder reads before the packet that carries it ([the census](#one-census-every-road)) | nothing pending: since such a packet or record, a picture of a type the decoder always decodes has been read ([`KeptReason::CarriedOver`]); beside a picture, only filler payload rides ([SEI](#sei)) |
//! | HEVC `eos`, `last_eos` | set by an EOS or EOB unit, a flush and the decoder's opening, and taken up by the next packet read, `hevc/hevcdec.c` 3687–3720, 4161, 4194 | whether the next CRA picture starts a sequence, its RASL pictures skipped (3283–3287, 786–787) | not the first packet after the opening, a seek, or a packet that may hold an EOS or EOB unit ([`KeptReason::SequenceStart`]) |
//! | the framing, parameter sets | [Framing](#framing); VPS, SPS, PPS | yes: their packets are kept | — |
//!
//! # One census, every road
//!
//! What is pending for later pictures — the SEI state and the RPU the next
//! picture decoded takes up, a recovery point, the `eos` the next packet
//! takes up, and what H.264 awaits after a flush — is moved by one census of
//! the NAL units the decoder reads, whatever road they come by, H.264's and
//! HEVC's alike:
//!
//! | road | the units the census takes up | what the decoder takes up of them |
//! |---|---|---|
//! | the record the decoder opens with ([`NalStream::new`]); again at a seek, where a decoder opened afresh reads it ([`NalStream::seeked`]) | every unit its splitter may read in the record | H.264: the parameter sets alone (`h264_parse.c` 380–409). HEVC: the parameter sets, and the SEI units, prefix and suffix, of any layer, which wait for the first picture it decodes (`hevc/parse.c` 39–67, `hevc/hevcdec.c` 4161–4174) |
//! | a record mid-stream ([`NalStream::reconfigure`]): a packet's `AV_PKT_DATA_NEW_EXTRADATA` ([`NalStream::read_with_record`]), an H.264 packet that is an `avcC` | the same | the same, before the packet that carries it (`h264dec.c` 1038–1050, `hevc/hevcdec.c` 3855–3860) |
//! | a packet the reading reads whole | its units, in order | each as its type says |
//! | a packet it does not: one that does not parse, that switches the framing, or that comes while the framing is unknown, unset or lost | every unit a decoder's splitter may read in it, in each framing one may read it in | anything: SEI state, an end of sequence or a recovery point may be left behind, and nothing the packet would take up or prove counts |
//!
//! An HEVC record mid-stream that fails — an array that runs past the
//! record's end (`hevc/parse.c` 115–118), or under `AV_EF_EXPLODE` a
//! parameter set or SEI unit that does not decode (71–74) — returns before
//! the decoder reads the packet that carries it (`hevc/hevcdec.c`
//! 3855–3860). The reading cannot know the decoder's `err_recognition`, so
//! whatever was pending before a packet that follows an HEVC record stays
//! pending after it.
//!
//! # The rule
//!
//! From that census, an H.264 packet is marked only where all of these
//! hold, and kept otherwise:
//!
//! - the framing is known, and the packet reads whole as it frames it;
//! - it holds a slice, every slice of `nal_ref_idc` 0 and none an IDR, and
//!   nothing beside them but access unit delimiters, filler data and SEI
//!   units of picture timing and filler payload alone;
//! - some slice is not intra, as FFmpeg reads a slice's type, or none is of
//!   a type the decoder decodes (1, 5), so that it starts no picture;
//! - a sequence parameter set has been read, and every one the decoder may
//!   read codes frames only, lets `frame_num` jump nowhere and fixes the
//!   reorder depth at 15 or less, under every reading of it FFmpeg may
//!   adopt;
//! - a slice of a reference picture, of NAL unit type 1 or 5, has been
//!   read since the reading opened or was sought;
//! - where some slice may be intra, a packet kept since then has held an
//!   intra slice.
//!
//! An HEVC packet is marked only where all of these hold:
//!
//! - the framing is known, and the packet reads whole as it frames it;
//! - every slice is TRAIL_N, TSA_N, STSA_N, RADL_N or RASL_N, of layer 0,
//!   at the highest sub-layer any parameter set or unit has declared, a
//!   sequence parameter set having been read, and nothing rides beside
//!   them but delimiters, filler data and SEI units of filler payload
//!   alone;
//! - it is not the first packet after the reading opened or was sought,
//!   nor the first after a packet that may hold an end of sequence or of
//!   bitstream, or that the decoder may not have read;
//! - no recovery point SEI, of any layer, a packet's or a record's, has
//!   been read since the last IRAP picture of layer 0 the decoder decodes
//!   (types 16–21) in a packet it reads for certain;
//! - nothing is pending for it to take up: after every SEI unit other than
//!   one of filler payload alone — a prefix unit of a packet, any unit of a
//!   record the decoder has read, and a suffix unit or an RPU of a packet
//!   whose own picture did not take it up — a picture of a type the
//!   decoder always decodes (TRAIL, TSA, STSA, RADL, IRAP) has begun in a
//!   packet it reads for certain.
//!
//! `mediadecode-ffmpeg`'s demuxer adds two: no side data that persists
//! past the packet, and no `CORRUPT` flag.
//!
//! # What is never marked
//!
//! A codec other than H.264 and HEVC is never read: its packets carry the
//! container's mark alone. A packet that does not parse is kept as
//! [`KeptReason::Malformed`]: a NAL unit whose forbidden bit is set, a
//! length of 0 or one past the packet's end (which FFmpeg refuses too,
//! `h2645_parse.h` 130–148), a length-framed NAL unit holding a start code,
//! bytes that form no NAL unit, an HEVC TemporalId of −1 or past the highest
//! sub-layer, a parameter set too short to read. A packet this module
//! cannot read is one it cannot prove disposable.
//!
//! The citations are FFmpeg 9.0.1's sources, and Rec. ITU-T H.264 (V16,
//! 06/2026) and H.265 (V11, 01/2026) by clause.

use core::fmt;

/// The codecs whose bitstream [`NalStream`] reads.
///
/// Every other codec is unclassified: whether its packets are disposable
/// is the container's to say, and a demuxer marks them from the container
/// alone.
///
/// Marked `#[non_exhaustive]`: a later codec joins as a variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum NalCodec {
  /// H.264 / AVC.
  H264,
  /// H.265 / HEVC.
  Hevc,
}

/// How a track's packets carry their NAL units.
///
/// Marked `#[non_exhaustive]`: a later framing joins as a variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Framing {
  /// Each NAL unit after a big-endian length of this many bytes, as an
  /// `avcC` or `hvcC` configuration record declares.
  Length(LengthSize),
  /// Each NAL unit after a start code, `00 00 01` (Annex B).
  AnnexB,
}

/// The size of the length before each NAL unit: a configuration record's
/// `lengthSizeMinusOne` plus one. ISO/IEC 14496-15 allows one, two and
/// four bytes; FFmpeg reads three as it reads the others
/// (`libavcodec/h2645_parse.h` 130–148), and so does this module.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LengthSize {
  /// One byte.
  One,
  /// Two bytes.
  Two,
  /// Three bytes.
  Three,
  /// Four bytes.
  Four,
}

impl LengthSize {
  /// The size, in bytes.
  pub const fn bytes(self) -> usize {
    match self {
      Self::One => 1,
      Self::Two => 2,
      Self::Three => 3,
      Self::Four => 4,
    }
  }

  /// From FFmpeg's `nal_length_size`; `None` for 0, a size no record has
  /// set, and for anything past four.
  const fn from_bytes(bytes: u8) -> Option<Self> {
    match bytes {
      1 => Some(Self::One),
      2 => Some(Self::Two),
      3 => Some(Self::Three),
      4 => Some(Self::Four),
      _ => None,
    }
  }
}

/// What [`NalStream::read`] answers for one packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Verdict {
  /// Nothing references the packet's picture, and nothing else in it is
  /// needed later: dropping it loses that picture and no other.
  Disposable,
  /// The packet is kept, for this reason.
  Kept(KeptReason),
}

impl Verdict {
  /// Whether the packet is disposable.
  pub const fn is_disposable(self) -> bool {
    matches!(self, Self::Disposable)
  }
}

/// Why [`NalStream::read`] keeps a packet: the first NAL unit, in the
/// packet's order, that keeps it, or what about the packet does.
///
/// Marked `#[non_exhaustive]`: a later reason joins as a variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum KeptReason {
  /// The packet holds no picture: no coded slice.
  NoPicture,
  /// A picture something may reference: an H.264 slice whose
  /// `nal_ref_idc` is not 0, or an HEVC sub-layer reference picture.
  Referenced,
  /// A random-access picture: an H.264 IDR slice, or an HEVC IRAP one.
  RandomAccess,
  /// A NAL unit beside the picture that a later picture, or what is
  /// output after it, may need, of this `nal_unit_type`: a parameter set,
  /// an SEI unit holding a message its picture alone does not take up, an
  /// end of sequence or of stream, a reserved type, or any other than an
  /// access unit delimiter or filler data.
  Companion(u8),
  /// A NAL unit of an extension: H.264's scalable, multiview or 3-D slices
  /// and prefixes (14, 20, 21), or any HEVC NAL unit of a layer above 0.
  Extension,
  /// An HEVC sub-layer non-reference picture below the highest temporal
  /// sub-layer, which a picture of a higher sub-layer may reference.
  BelowHighestSubLayer,
  /// An HEVC picture before any sequence parameter set: the stream's
  /// highest temporal sub-layer is not yet known.
  SubLayersUnknown,
  /// The packet does not parse as its track's framing says.
  Malformed,
  /// The track's NAL units follow lengths of a size no record has set:
  /// FFmpeg splits none of its packets.
  Unframed,
  /// An H.264 packet that is an `avcC` itself, which configures the decoder
  /// and holds no picture.
  Configuration,
  /// An H.264 packet whose first bytes switch the framing FFmpeg reads the
  /// stream in: a decoder that never saw it would read the packets after it
  /// otherwise.
  FramingChange,
  /// A packet the framing of which a decoder may read otherwise than the
  /// reading can know: after a seek that followed a change of framing, or
  /// after a record whose reading turns on whether its parameter sets
  /// decode.
  FramingUnknown,
  /// An H.264 picture of `nal_ref_idc` 0 that may be one field of a pair,
  /// which FFmpeg's decoder joins with its other field across packets: no
  /// sequence parameter set has been read, or one lets a picture be a field
  /// (`frame_mbs_only_flag` 0).
  MaybeField,
  /// An H.264 picture of `nal_ref_idc` 0 whose every slice is intra, which
  /// FFmpeg's decoder under frame threading reads as `picture_intra_only`
  /// and lets reset the `poc_offset` later pictures' film grain is seeded
  /// from (`h264dec.c` 501–551, `h264_slice.c` 1427–1430, 1258–1260).
  IntraOnly,
  /// An H.264 picture where `frame_num` may jump: a sequence parameter set
  /// the decoder may read allows gaps, and FFmpeg fills a gap counting from
  /// the picture before, reference or not (`h264_slice.c` 1432–1530,
  /// `h264_picture.c` 202–203).
  FrameNumGaps,
  /// An H.264 picture where the decoder's reorder depth is not fixed: a
  /// sequence parameter set the decoder may read sets no
  /// `bitstream_restriction_flag`, or a `max_num_reorder_frames` of 16.
  /// FFmpeg grows the depth from the pictures it sees in the first case,
  /// and may reset its record of them in the second, so that dropping one
  /// can cost a later picture its output (`h264_slice.c` 1305–1333).
  ReorderUnfixed,
  /// An H.264 picture before the first reference picture since the decoder
  /// opened or was sought: FFmpeg's decoder takes its picture order count
  /// up from the first picture it decodes after a flush (`h264_parse.c`
  /// 293–294; `h264dec.c` 421, 438–467).
  AwaitingReference,
  /// An H.264 picture holding an intra slice before any packet kept since
  /// the decoder opened or was sought has held one: FFmpeg's first intra
  /// slice after a flush stops later pictures referencing the gray frames
  /// it fills gaps with (`non_gray`, `h264_slice.c` 2032–2034;
  /// `h264_refs.c` 410–419; `h264dec.c` 492).
  AwaitingIntraSlice,
  /// An HEVC picture while a recovery point may be pending: FFmpeg ends the
  /// recovery at the first picture of its count, which may be this one, and
  /// holds back the pictures decoded before it ends (`hevc/hevcdec.c`
  /// 3289–3294, `hevc/refs.c` 239–242).
  Recovering,
  /// An HEVC packet the decoder reads first after it opens or is flushed,
  /// or after a packet holding an end of sequence or of bitstream: FFmpeg
  /// takes its `eos` flag up with whatever packet comes next, and the flag
  /// decides whether the CRA picture it meets starts a sequence of its own
  /// (`hevc/hevcdec.c` 3687–3720, 3283–3287, 786–787, 4161, 4194).
  SequenceStart,
  /// An HEVC picture that may take up SEI state or a Dolby Vision RPU an
  /// earlier packet left, its own picture skipped or absent: FFmpeg hands
  /// such state to the next picture it decodes, as side data, film grain
  /// or a picture hash to check (`hevc/hevcdec.c` 3068–3151, 3505–3512,
  /// 3723–3760; `h2645_sei.c` 518–695).
  CarriedOver,
}

impl fmt::Display for KeptReason {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::NoPicture => f.write_str("no picture"),
      Self::Referenced => f.write_str("a picture something may reference"),
      Self::RandomAccess => f.write_str("a random-access picture"),
      Self::Companion(nal_unit_type) => {
        write!(
          f,
          "a NAL unit of type {nal_unit_type} a later picture may need"
        )
      }
      Self::Extension => f.write_str("a NAL unit of an extension"),
      Self::BelowHighestSubLayer => f.write_str("a picture below the highest temporal sub-layer"),
      Self::SubLayersUnknown => f.write_str("a picture before any sequence parameter set"),
      Self::Malformed => f.write_str("a packet that does not parse"),
      Self::Unframed => f.write_str("lengths of a size no record has set"),
      Self::Configuration => f.write_str("a configuration record"),
      Self::FramingChange => f.write_str("a packet that switches the framing"),
      Self::FramingUnknown => f.write_str("a framing a decoder may read otherwise"),
      Self::MaybeField => f.write_str("a picture that may be one field of a pair"),
      Self::IntraOnly => f.write_str("a picture of intra slices alone"),
      Self::FrameNumGaps => f.write_str("a picture where frame_num may jump"),
      Self::ReorderUnfixed => f.write_str("a picture where the reorder depth is not fixed"),
      Self::AwaitingReference => f.write_str("a picture before the first reference picture"),
      Self::AwaitingIntraSlice => f.write_str("an intra slice before the first one kept"),
      Self::Recovering => f.write_str("a picture while a recovery point may be pending"),
      Self::SequenceStart => f.write_str("the first packet a sequence may start at"),
      Self::CarriedOver => f.write_str("a picture that may take up what an earlier packet left"),
    }
  }
}

/// One track's reading of its H.264 or HEVC bitstream: its codec, the
/// framing FFmpeg's decoder reads it in, and for HEVC the highest temporal
/// sub-layer any parameter set has declared so far.
///
/// The framing is FFmpeg's own state, held as its decoder holds it: whether
/// each NAL unit follows a length (`is_avc` in `libavcodec/h264dec.h`,
/// `is_nalff` in `hevc/hevcdec.h`) and the length's size
/// (`nal_length_size`), the two kept apart. A configuration record moves
/// them exactly as `ff_h264_decode_extradata` (`h264_parse.c` 466–524) and
/// `ff_hevc_decode_extradata` (`hevc/parse.c` 79–146) move them: the first
/// byte decides whether lengths are read, and the size moves only when the
/// record is read whole, so a four-byte size outlives a record of start
/// codes. An empty record changes nothing.
///
/// Built from the track's configuration record ([`Self::new`]), moved where
/// the configuration changes mid-stream ([`Self::reconfigure`],
/// [`Self::read_with_record`]), told of every seek ([`Self::seeked`]), and
/// fed each of the track's packets in decoding order ([`Self::read`]),
/// which learns from every parameter set it reads.
///
/// **It reads plaintext, and only a track's whole plaintext.** What it is
/// fed it takes for the bitstream's own bytes, and every verdict stands on
/// every packet before it. So a caller feeds it no encrypted payload, and
/// none of the track's packets after one: the plaintext it never saw may
/// move the framing and the parameter sets they are read by.
/// `mediadecode-ffmpeg`'s demuxer drops a track's reading at its first
/// packet that carries encryption side data, builds none for a track
/// FFmpeg names encrypted at the open, and reads no Matroska or WebM track
/// at all: FFmpeg 9.0.1 passes a Matroska encryption without a key id
/// through undecrypted, with nothing public to say so.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NalStream {
  codec: NalCodec,
  /// FFmpeg's `is_avc` or `is_nalff`: whether each NAL unit follows a
  /// length; a start code otherwise.
  lengths: bool,
  /// FFmpeg's `nal_length_size`: the bytes of each length, 0 before a
  /// record has set it; `None` where a record's reading turns on whether
  /// its parameter sets decode.
  length_size: Option<u8>,
  /// The framing the stream opened in, `(lengths, length_size)`.
  opened: (bool, Option<u8>),
  /// Whether the framing a decoder reads the next packets in is no longer
  /// known: nothing is marked again.
  lost: bool,
  /// The most any parameter set, configuration record or NAL unit has
  /// declared.
  highest_sub_layer: u8,
  /// Whether a sequence parameter set has been read.
  sps_seen: bool,
  /// For H.264: whether a sequence parameter set the decoder may read lets
  /// a picture be a field — `frame_mbs_only_flag` 0, or one this module
  /// cannot read so. It never clears.
  fields_possible: bool,
  /// For H.264: whether a sequence parameter set the decoder may read lets
  /// `frame_num` jump (`gaps_in_frame_num_value_allowed_flag` 1, or not
  /// read so). It never clears.
  gaps_possible: bool,
  /// For H.264: whether a sequence parameter set the decoder may read
  /// leaves the reorder depth to FFmpeg's guess (no
  /// `bitstream_restriction_flag`, or not read so). It never clears.
  reorder_unfixed: bool,
  /// What is pending for later pictures, as the census of every NAL unit
  /// read so far leaves it ([`Census`]).
  pending: Pending,
  /// For HEVC: whether the record the stream opened with leaves SEI state
  /// pending for the first picture a decoder decodes, which a decoder
  /// opened afresh at a seek reads again.
  opened_carried: bool,
  /// For HEVC: whether the decoder may not read the next packet: a record
  /// read mid-stream may fail, and the decoder then returns before the
  /// packet that carries it (`hevc/hevcdec.c` 3855–3860).
  record_may_fail: bool,
}

impl NalStream {
  /// The reading of a track of `codec` whose configuration record — its
  /// extradata — is `extradata`, empty for none, as the decoder reads it
  /// when it opens.
  pub fn new(codec: NalCodec, extradata: &[u8]) -> Self {
    let mut stream = Self {
      codec,
      lengths: false,
      length_size: Some(0),
      opened: (false, Some(0)),
      lost: false,
      highest_sub_layer: 0,
      sps_seen: false,
      fields_possible: false,
      gaps_possible: false,
      reorder_unfixed: false,
      pending: Pending::OPENING,
      opened_carried: false,
      record_may_fail: false,
    };
    stream.record(extradata, true);
    stream.opened = (stream.lengths, stream.length_size);
    stream.opened_carried = stream.pending.carried;
    stream
  }

  /// Reads a new configuration record — a mid-stream change of the
  /// track's extradata, as `AV_PKT_DATA_NEW_EXTRADATA` brings one — as the
  /// decoder reads it, and learns from its parameter sets. What earlier
  /// parameter sets declared is kept: the highest sub-layer only rises.
  /// What the record leaves pending — HEVC's SEI, which the decoder reads
  /// into the state the next picture it decodes takes up — is pending for
  /// the next packet read, and for HEVC that packet may go unread: a record
  /// that fails returns before the decoder reads the packet that carries it
  /// (`hevc/hevcdec.c` 3855–3860), so what is pending before that packet
  /// stays pending after it.
  ///
  /// [`Self::read_with_record`] reads a packet together with the record it
  /// carries, as the decoder takes the two.
  pub fn reconfigure(&mut self, extradata: &[u8]) {
    self.record(extradata, false);
  }

  /// Reads one packet that carries `record` — its first
  /// `AV_PKT_DATA_NEW_EXTRADATA` entry, as `av_packet_get_side_data`
  /// answers, or `None` — as the decoder takes the two: the record first
  /// ([`Self::reconfigure`]), then the packet ([`Self::read`]). FFmpeg's
  /// H.264 decoder takes no record from a packet of no bytes, which it
  /// reads as the stream's end (`libavcodec/h264dec.c` 1034–1036); HEVC's
  /// takes it from any (`libavcodec/hevc/hevcdec.c` 3854–3860).
  pub fn read_with_record(&mut self, packet: &[u8], record: Option<&[u8]>) -> Verdict {
    if let Some(record) = record
      && (self.codec == NalCodec::Hevc || !packet.is_empty())
    {
      self.reconfigure(record);
    }
    self.read(packet)
  }

  /// Tells the reading that the track was sought, or that a seek was
  /// attempted. Where the framing differs from the one the stream opened
  /// in, the framing a decoder reads the next packets in is no longer
  /// known — a decoder keeps its own across a seek, and one opened afresh
  /// starts from the stream's — so nothing is marked again
  /// ([`KeptReason::FramingUnknown`]).
  pub fn seeked(&mut self) {
    // A decoder flushed for the seek takes its picture order count up again
    // from the first picture it decodes (`h264dec.c` 438–473), forgets its
    // intra slices (492), and, for HEVC, sets `eos` (`hevc/hevcdec.c` 4194)
    // and keeps what its flush leaves of its SEI state, a picture hash
    // among it (4191; `ff_hevc_reset_sei`, `hevc/sei.h` 128–133). One opened
    // afresh reads the record the stream opened with again, its SEI pending
    // for the first picture it decodes.
    self.pending.reference_seen = false;
    self.pending.intra_seen = false;
    self.pending.sequence_start = true;
    self.pending.carried |= self.opened_carried;
    if (self.lengths, self.length_size) != self.opened {
      self.lost = true;
    }
  }

  /// The codec read.
  pub const fn codec(&self) -> NalCodec {
    self.codec
  }

  /// The framing the next packet is read in; `None` where the reading
  /// cannot read one — lengths of a size no record has set, or a framing
  /// a decoder may read otherwise ([`KeptReason::FramingUnknown`]).
  pub const fn framing(&self) -> Option<Framing> {
    if self.lost {
      return None;
    }
    match (self.lengths, self.length_size) {
      // H.264 reads its four-byte probe whatever `is_avc` says
      // (`h264dec.c` 602–607): a size the reading cannot know leaves even
      // its start codes unknown.
      (false, None) => match self.codec {
        NalCodec::H264 => None,
        NalCodec::Hevc => Some(Framing::AnnexB),
      },
      (false, Some(_)) => Some(Framing::AnnexB),
      (true, Some(bytes)) => match LengthSize::from_bytes(bytes) {
        Some(size) => Some(Framing::Length(size)),
        None => None,
      },
      (true, None) => None,
    }
  }

  /// For HEVC, the highest temporal sub-layer any parameter set,
  /// configuration record or NAL unit has declared so far; `None` before
  /// a sequence parameter set has been read, and for H.264.
  pub const fn highest_sub_layer(&self) -> Option<u8> {
    match self.codec {
      NalCodec::Hevc if self.sps_seen => Some(self.highest_sub_layer),
      _ => None,
    }
  }

  /// For H.264, whether every sequence parameter set read so far codes
  /// frames only (`frame_mbs_only_flag` 1); `None` before one has been
  /// read, and for HEVC.
  pub const fn frames_only(&self) -> Option<bool> {
    match self.codec {
      NalCodec::H264 if self.sps_seen => Some(!self.fields_possible),
      _ => None,
    }
  }

  /// Reads one packet: whether nothing references its picture. Packets
  /// are read in decoding order; every parameter set read raises what
  /// later packets are held to, and a packet that moves the framing
  /// moves it for the packets after.
  pub fn read(&mut self, packet: &[u8]) -> Verdict {
    // After an HEVC record that may fail, the decoder may read nothing of
    // this packet: what is pending before it stays pending after it.
    let unread = core::mem::take(&mut self.record_may_fail).then_some(self.pending);
    let verdict = self.read_packet(packet);
    if let Some(unread) = unread {
      self.pending = self.pending.either(unread);
    }
    verdict
  }

  /// [`Self::read`], the packet read by the decoder.
  fn read_packet(&mut self, packet: &[u8]) -> Verdict {
    if self.lost {
      self.unread(packet, None);
      return Verdict::Kept(KeptReason::FramingUnknown);
    }
    if self.codec == NalCodec::H264 {
      // A packet that is an `avcC` itself configures the decoder and holds
      // no picture (`h264dec.c` 1045–1050).
      if self.lengths && is_avcc_packet(packet) {
        self.record(packet, false);
        return Verdict::Kept(KeptReason::Configuration);
      }
      match self.length_size {
        None => {
          self.unread(packet, None);
          self.learn_unframed(packet);
          return Verdict::Kept(KeptReason::FramingUnknown);
        }
        // With four-byte lengths a packet's first bytes may switch the
        // framing (`h264dec.c` 602–607), whatever the record said: a packet
        // that switches it is kept, so a decoder that sees it reads every
        // later packet in the framing the reading does.
        Some(4) => {
          let lengths = h264_packet_lengths(packet, self.lengths);
          if lengths != self.lengths {
            self.lengths = lengths;
            // The decoder reads this packet in the framing it switches to.
            let framing = self.framing();
            self.unread(packet, framing);
            if let Some(framing) = framing {
              self.learn(packet, framing);
            }
            return Verdict::Kept(KeptReason::FramingChange);
          }
        }
        Some(_) => {}
      }
    }
    let Some(framing) = self.framing() else {
      self.unread(packet, None);
      self.learn_unframed(packet);
      return Verdict::Kept(KeptReason::Unframed);
    };
    let judged = self.judge(packet, framing);
    self.pending = judged.pending;
    self.learn(packet, framing);
    judged.verdict
  }

  /// The census of a packet the reading does not read whole
  /// ([`Census::unread`]), in `framing` or, for `None`, in every framing a
  /// decoder may read it in.
  fn unread(&mut self, packet: &[u8], framing: Option<Framing>) {
    self.pending = Census::new(self.codec, Road::Unread, self.pending).unread(packet, framing);
  }

  /// One packet's verdict, read in `framing` against what earlier packets
  /// taught, and the census of its units; it learns nothing.
  fn judge(&self, packet: &[u8], framing: Framing) -> Judged {
    // A packet that does not parse proves nothing: whatever it holds may be
    // left pending.
    let malformed = |census: Census| Judged {
      verdict: Verdict::Kept(KeptReason::Malformed),
      pending: census.unread(packet, Some(framing)),
    };
    let mut census = Census::new(self.codec, Road::Packet, self.pending);
    let mut kept = None;
    let mut pictures = 0_usize;
    // H.264: FFmpeg's `picture_intra_only`, every slice NAL unit (1, 2, 5)
    // intra; whether the packet holds a slice the decoder decodes (1, 5),
    // the only kind it starts a picture at; and whether such a slice may
    // be intra.
    let mut intra_only = true;
    let mut decoded_slice = false;
    let mut some_intra = false;
    let mut units = Units::new(packet, framing);
    loop {
      match units.next() {
        Unit::End => break,
        Unit::Broken => return malformed(census),
        Unit::Nal(nal) => {
          census.unit(nal);
          let read = match self.codec {
            NalCodec::H264 => {
              if let Some(&header) = nal.first() {
                let nal_unit_type = header & 0x1F;
                if matches!(nal_unit_type, 1 | 2 | 5) {
                  let parsed = slice_type(&nal[1..]);
                  intra_only &= parsed.is_none_or(is_intra);
                  decoded_slice |= nal_unit_type != 2;
                  some_intra |= nal_unit_type == 1 && parsed.is_none_or(is_intra);
                }
              }
              self.h264_unit(nal)
            }
            NalCodec::Hevc => self.hevc_unit(nal),
          };
          match read {
            Read::Picture => pictures += 1,
            Read::Beside => {}
            Read::Keeps(KeptReason::Malformed) => return malformed(census),
            Read::Keeps(reason) => {
              kept.get_or_insert(reason);
            }
          }
        }
      }
    }
    let verdict = match kept {
      Some(reason) => Verdict::Kept(reason),
      None if pictures == 0 => Verdict::Kept(KeptReason::NoPicture),
      // Under frame threading FFmpeg resets `poc_offset` at such a picture
      // (`h264dec.c` 501–551, `h264_slice.c` 1427–1430).
      None if self.codec == NalCodec::H264 && intra_only && decoded_slice => {
        Verdict::Kept(KeptReason::IntraOnly)
      }
      // The first intra slice decoded after a flush sets `non_gray`
      // (`h264_slice.c` 2032–2034).
      None if self.codec == NalCodec::H264 && some_intra && !self.pending.intra_seen => {
        Verdict::Kept(KeptReason::AwaitingIntraSlice)
      }
      None => Verdict::Disposable,
    };
    Judged {
      verdict,
      // A packet marked may not reach the decoder.
      pending: census.close(packet.is_empty(), !verdict.is_disposable()),
    }
  }

  /// Learns from every NAL unit FFmpeg's splitter may read in `packet`
  /// framed by `framing`, and more ([`each_unit`]).
  fn learn(&mut self, packet: &[u8], framing: Framing) {
    each_unit(packet, framing, &mut |nal| self.learn_unit(nal));
  }

  /// Learns from a packet the reading cannot frame, which a decoder still
  /// reads in a framing of its own: in every framing one may read it in
  /// ([`each_unit_in_any_framing`]).
  fn learn_unframed(&mut self, packet: &[u8]) {
    each_unit_in_any_framing(packet, &mut |nal| self.learn_unit(nal));
  }

  /// A configuration record, read as the codec's decoder reads one, when
  /// it opens or mid-stream: every unit of it learned, and taken into the
  /// census, before the packet that carries it is read.
  fn record(&mut self, extradata: &[u8], opening: bool) {
    let mut census = Census::new(self.codec, Road::Record, self.pending);
    match self.codec {
      NalCodec::H264 => self.h264_record(extradata, &mut census),
      NalCodec::Hevc => self.hevc_record(extradata, opening, &mut census),
    }
    self.pending = census.pending;
    // A record the HEVC decoder reads mid-stream may fail, and it then
    // returns before the packet (`hevc/hevcdec.c` 3855–3860); it reads no
    // record of no bytes (3856), and one it opens with that fails leaves no
    // decoder at all (4169–4172).
    if self.codec == NalCodec::Hevc && !opening && !extradata.is_empty() {
      self.record_may_fail = true;
    }
  }

  /// One NAL unit of a configuration record: learned, and taken into the
  /// record's census.
  fn record_unit(&mut self, nal: &[u8], census: &mut Census) {
    self.learn_unit(nal);
    census.unit(nal);
  }

  /// `ff_h264_decode_extradata` (`h264_parse.c` 466–524): no record
  /// changes nothing; a first byte of 1 makes the stream one of lengths
  /// before anything else is checked, and the size moves only when the
  /// `avcC` is read whole — or becomes unknown where reading it whole
  /// turns on whether a parameter set decodes; any other record makes it
  /// one of start codes, its size kept. Either way every unit the decoder
  /// may read in it is learned and taken into `census`.
  fn h264_record(&mut self, extradata: &[u8], census: &mut Census) {
    let Some(&first) = extradata.first() else {
      return;
    };
    if first != 1 {
      self.lengths = false;
      // `decode_extradata_ps` splits the record as Annex B
      // (`h264_parse.c` 517–519).
      for nal in AfterStartCodes::new(extradata) {
        self.record_unit(nal, census);
      }
      return;
    }
    self.lengths = true;
    // Every entry the record holds, its sequence parameter sets and then its
    // picture parameter sets, up to the first that does not fit: FFmpeg
    // reads each as `decode_extradata_ps_mp4` splits it, and a unit of
    // either array as its own type says (`h264_parse.c` 486–513, 367–413).
    let mut at = 6;
    let sps_count = extradata.get(5).map_or(0, |&count| count & 0x1F);
    if self.avcc_entries(extradata, &mut at, sps_count, census)
      && let Some(&pps_count) = extradata.get(at)
    {
      at += 1;
      self.avcc_entries(extradata, &mut at, pps_count, census);
    }
    match read_avcc(extradata) {
      AvccRead::Whole(size) => self.length_size = Some(size),
      AvccRead::Fails => {}
      // The size moves to the record's only if the parameter set
      // decodes: a size that is the record's already is known either way.
      AvccRead::Undecided(size) => {
        if self.length_size != Some(size) {
          self.length_size = None;
        }
      }
    }
  }

  /// Reads `count` entries of an `avcC` from `*at`, each a two-byte length
  /// and its bytes, split as `decode_extradata_ps_mp4` splits one, every
  /// unit learned and taken into `census`; whether all `count` fit.
  fn avcc_entries(
    &mut self,
    record: &[u8],
    at: &mut usize,
    count: u8,
    census: &mut Census,
  ) -> bool {
    for _ in 0..count {
      let (Some(&high), Some(&low)) = (record.get(*at), record.get(*at + 1)) else {
        return false;
      };
      let size = usize::from(u16::from_be_bytes([high, low])) + 2;
      let Some(entry) = record.get(*at..*at + size) else {
        return false;
      };
      each_after_lengths(entry, 2, &mut |nal| self.record_unit(nal, census));
      *at += size;
    }
    true
  }

  /// `ff_hevc_decode_extradata` (`hevc/parse.c` 79–146): an `hvcC` — 23
  /// bytes or more, its first byte 1, or 0 with a nonzero second byte or a
  /// third above 1 — makes the stream one of lengths, of two bytes while
  /// its arrays are read and of the record's size once every unit has been
  /// read; any other record makes it one of start codes, its size kept.
  /// No record — the decoder reads one only of at least a byte
  /// (`hevc/hevcdec.c` 3856) — changes nothing. Either way the parameter
  /// sets declare the stream's sub-layers, and every unit the decoder may
  /// read in it is taken into `census`.
  ///
  /// Mid-stream, an `hvcC` of a size other than two leaves the framing
  /// unknown for good: under `AV_EF_EXPLODE` a parameter set that does not
  /// decode stops it at two (`hevc/parse.c` 72–76, 121–129).
  fn hevc_record(&mut self, extradata: &[u8], opening: bool, census: &mut Census) {
    if extradata.is_empty() {
      return;
    }
    let hvcc = extradata.len() >= 23
      && (extradata[0] == 1 || (extradata[0] == 0 && (extradata[1] != 0 || extradata[2] > 1)));
    if !hvcc {
      self.lengths = false;
      for nal in AfterStartCodes::new(extradata) {
        self.record_unit(nal, census);
      }
      return;
    }
    self.lengths = true;
    // "nal units in the hvcC always have length coded with 2 bytes, so put
    // a fake nal_length_size = 2 while parsing them" (`hevc/parse.c`
    // 103–105): a record that fails part way leaves it so.
    self.length_size = Some(2);
    // Read as FFmpeg's `GetByteContext` reads (`bytestream.h`): past the
    // end a read gives 0.
    let mut record = Bytes::new(extradata);
    record.skip(21);
    // Byte 21: `constantFrameRate` (2), `numTemporalLayers` (3),
    // `temporalIdNested` (1), `lengthSizeMinusOne` (2)
    // (`libavformat/hevc.c` 1159–1167).
    let byte21 = record.byte();
    let arrays = record.byte();
    for _ in 0..arrays {
      let _type = record.byte();
      let count = record.be16();
      for _ in 0..count {
        // "+2 for the nal size field" (`hevc/parse.c` 113–119).
        let size = usize::from(record.peek_be16()) + 2;
        if record.left() < size {
          return;
        }
        // The decoder splits each unit after its two-byte length, as a
        // packet (`hevc/parse.c` 121–123, 24–37).
        each_after_lengths(record.take(size), 2, &mut |nal| {
          self.record_unit(nal, census)
        });
      }
    }
    let size = (byte21 & 3) + 1;
    if opening || size == 2 {
      self.length_size = Some(size);
    } else {
      self.lost = true;
    }
    let layers = (byte21 >> 3) & 7;
    if layers > 0 {
      self.raise(layers - 1);
    }
  }

  /// Raises the highest sub-layer to `declared`; it never falls.
  fn raise(&mut self, declared: u8) {
    if declared > self.highest_sub_layer {
      self.highest_sub_layer = declared;
    }
  }

  /// One HEVC NAL unit, read against what earlier packets taught.
  fn hevc_unit(&self, nal: &[u8]) -> Read {
    let &[first, second, ..] = nal else {
      return Read::Keeps(KeptReason::Malformed);
    };
    if first & 0x80 != 0 {
      return Read::Keeps(KeptReason::Malformed);
    }
    let nal_unit_type = (first >> 1) & 0x3F;
    let layer = ((first & 1) << 5) | (second >> 3);
    let Some(temporal_id) = (second & 7).checked_sub(1) else {
      return Read::Keeps(KeptReason::Malformed);
    };
    if layer != 0 {
      return Read::Keeps(KeptReason::Extension);
    }
    match nal_unit_type {
      // The SLNR types FFmpeg's decoder decodes: TRAIL_N, TSA_N, STSA_N,
      // RADL_N and RASL_N (7.4.2.2; `hevc/hevcdec.c` 3587–3680).
      0 | 2 | 4 | 6 | 8 => {
        if !self.sps_seen {
          Read::Keeps(KeptReason::SubLayersUnknown)
        } else if temporal_id < self.highest_sub_layer {
          Read::Keeps(KeptReason::BelowHighestSubLayer)
        } else if temporal_id > self.highest_sub_layer {
          // No parameter set declares the sub-layer: not as the stream
          // says it is.
          Read::Keeps(KeptReason::Malformed)
        } else if self.pending.sequence_start {
          Read::Keeps(KeptReason::SequenceStart)
        } else if self.pending.recovering {
          Read::Keeps(KeptReason::Recovering)
        } else if self.pending.carried {
          Read::Keeps(KeptReason::CarriedOver)
        } else {
          Read::Picture
        }
      }
      // The sub-layer reference types (TRAIL_R … RSV_VCL_R15).
      1 | 3 | 5 | 7 | 9 | 11 | 13 | 15 => Read::Keeps(KeptReason::Referenced),
      // The reserved SLNR types RSV_VCL_N10, N12 and N14, which FFmpeg's
      // decoder skips without a picture (`hevc/hevcdec.c` 3587–3680): what
      // rides beside them, a picture hash among it, would carry over to the
      // next picture.
      10 | 12 | 14 => Read::Keeps(KeptReason::Companion(nal_unit_type)),
      // IRAP: BLA, IDR, CRA, and the reserved IRAP types 22 and 23.
      16..=23 => Read::Keeps(KeptReason::RandomAccess),
      // A VPS or SPS whose `max_sub_layers_minus1` is past 6 does not
      // parse (7.4.3.1, 7.4.3.2.1).
      32 | 33 => match hevc_declared(nal_unit_type, nal) {
        Some(declared) if declared <= 6 => Read::Keeps(KeptReason::Companion(nal_unit_type)),
        _ => Read::Keeps(KeptReason::Malformed),
      },
      // AUD, FD.
      35 | 38 => Read::Beside,
      // Prefix and suffix SEI: beside the picture only where every message
      // its picture alone takes up.
      39 | 40 if sei_rides(&nal[2..], hevc_sei_rides) => Read::Beside,
      // PPS, EOS, EOB, any other SEI, the reserved VCL types 24–31, the
      // reserved non-VCL types 41–47 and the unspecified 48–63.
      other => Read::Keeps(KeptReason::Companion(other)),
    }
  }

  /// Learns from one NAL unit of the stream's codec.
  fn learn_unit(&mut self, nal: &[u8]) {
    match self.codec {
      NalCodec::H264 => self.h264_learn(nal),
      NalCodec::Hevc => self.hevc_learn(nal),
    }
  }

  /// Learns from one H.264 NAL unit: what a sequence parameter set proves
  /// under every reading FFmpeg may make of it ([`sps_proof`]). What one
  /// does not prove stays unproven for good. A unit whose forbidden bit is
  /// set, which FFmpeg's splitter passes over (`h2645_parse.c` 646–660),
  /// teaches nothing.
  fn h264_learn(&mut self, nal: &[u8]) {
    let Some(&header) = nal.first() else {
      return;
    };
    if header & 0x80 != 0 || header & 0x1F != 7 {
      return;
    }
    self.sps_seen = true;
    let proof = sps_proof(&nal[1..]);
    if !proof.frames_only {
      self.fields_possible = true;
    }
    if !proof.no_gaps {
      self.gaps_possible = true;
    }
    if !proof.reorder_fixed {
      self.reorder_unfixed = true;
    }
  }

  /// One H.264 NAL unit, read against what earlier packets taught.
  fn h264_unit(&self, nal: &[u8]) -> Read {
    match h264_unit(nal) {
      // A picture of `nal_ref_idc` 0 may be one field of a pair, which
      // FFmpeg's decoder joins with its other field across packets
      // (`h264_slice.c` 1397–1500): it is disposable only where every
      // sequence parameter set the decoder may read codes frames only.
      Read::Picture if !self.sps_seen || self.fields_possible => {
        Read::Keeps(KeptReason::MaybeField)
      }
      Read::Picture if self.gaps_possible => Read::Keeps(KeptReason::FrameNumGaps),
      Read::Picture if self.reorder_unfixed => Read::Keeps(KeptReason::ReorderUnfixed),
      Read::Picture if !self.pending.reference_seen => Read::Keeps(KeptReason::AwaitingReference),
      read => read,
    }
  }

  /// Learns from one HEVC NAL unit: a parameter set's
  /// `max_sub_layers_minus1`, and the sub-layer of any unit of layer 0. A
  /// unit FFmpeg's splitter passes over — its forbidden bit set, or a
  /// TemporalId of −1 (`h2645_parse.c` 646–660) — teaches nothing.
  fn hevc_learn(&mut self, nal: &[u8]) {
    let &[first, second, ..] = nal else {
      return;
    };
    if first & 0x80 != 0 {
      return;
    }
    let nal_unit_type = (first >> 1) & 0x3F;
    let layer = ((first & 1) << 5) | (second >> 3);
    let Some(temporal_id) = (second & 7).checked_sub(1) else {
      return;
    };
    if layer != 0 {
      return;
    }
    // A unit of a sub-layer no parameter set has declared: one the reading
    // has not read declares it.
    self.raise(temporal_id);
    if let Some(declared) = hevc_declared(nal_unit_type, nal)
      && declared <= 6
    {
      self.raise(declared);
      if nal_unit_type == 33 {
        self.sps_seen = true;
      }
    }
  }
}

/// A VPS's or an SPS's `max_sub_layers_minus1`: three bits of the second
/// byte after a VPS's header, of the first after an SPS's (7.3.2.1,
/// 7.3.2.2.1). No emulation prevention byte can come before either, the
/// header's two bytes being nonzero. `None` for any other unit, and for
/// one too short to hold it.
fn hevc_declared(nal_unit_type: u8, nal: &[u8]) -> Option<u8> {
  let at = match nal_unit_type {
    32 => 3,
    33 => 2,
    _ => return None,
  };
  nal.get(at).map(|&byte| (byte >> 1) & 7)
}

/// What an H.264 sequence parameter set proves, under every reading of it
/// FFmpeg's decoder may adopt: each `true` only where every reading reads
/// it so.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SpsProof {
  /// `frame_mbs_only_flag` 1: no picture is a field (7.4.2.1.1).
  frames_only: bool,
  /// `gaps_in_frame_num_value_allowed_flag` 0: `frame_num` never jumps
  /// (7.4.3).
  no_gaps: bool,
  /// The VUI's `bitstream_restriction_flag` 1, its `max_num_reorder_frames`
  /// read and 15 at most: FFmpeg's reorder depth is the stream's (E.2.1;
  /// `h264_slice.c` 1305–1307, 1411–1414), not a guess grown from the
  /// pictures it sees, and no picture follows enough pictures it precedes
  /// in output order to reset its record of them (1310–1328).
  reorder_fixed: bool,
}

/// What an H.264 sequence parameter set — `payload`, its bytes after the
/// header — proves under every reading of it FFmpeg's decoder may adopt
/// (`ff_h264_decode_seq_parameter_set`, `h264_ps.c` 284–590):
///
/// | reading | where FFmpeg makes it |
/// |---|---|
/// | [`Reading::Rbsp`] | first, in-band and in a record; again, past a truncation it then lets stand (`h264dec.c` 699–716; `h264_parse.c` 383–398) |
/// | [`Reading::Bytes`] | where the first fails (the same lines), and as the RBSP of an `avcC` entry it has escaped (`h264_parse.c` 421–464) |
/// | [`Reading::Reescaped`] | as the bytes of that escaped entry, where the RBSP of it fails |
///
/// Each reading is read only up to the first `00 00 00`, `00 00 01` or
/// `00 00 02`, at or before where FFmpeg's splitter ends the unit
/// (`h2645_parse.c` 44–52, 101–132), and within that only up to its stop
/// bit, where FFmpeg's own reading of the RBSP ends (`h2645_parse.c`
/// 348–376): so it reads no more of the unit than FFmpeg does.
///
/// Where the first reading, of the RBSP, reads the whole parameter set as
/// FFmpeg's strict reading does — every value in range and nothing past
/// its end — FFmpeg adopts that reading and makes no other (`h264dec.c`
/// 707–708, `h264_parse.c` 385–387), so its facts alone prove. Otherwise
/// every reading must prove a fact.
///
/// A reading that does not reach a fact, or that FFmpeg refuses on the way
/// — a value outside the range it accepts, separate colour planes, a
/// picture or cropping it rejects — proves nothing.
fn sps_proof(payload: &[u8]) -> SpsProof {
  let end = payload
    .windows(3)
    .position(|window| window[0] == 0 && window[1] == 0 && window[2] <= 2)
    .unwrap_or(payload.len());
  let unit = &payload[..end];
  let proves = |facts: &SpsFacts| SpsProof {
    frames_only: facts.frames_only == Some(true),
    no_gaps: facts.gaps == Some(false),
    reorder_fixed: facts.reorder_fixed == Some(true),
  };
  let rbsp = sps_facts(BitReader::new(unit, Reading::Rbsp));
  let mut proof = proves(&rbsp);
  if rbsp.whole {
    return proof;
  }
  for reading in [Reading::Bytes, Reading::Reescaped] {
    let other = proves(&sps_facts(BitReader::new(unit, reading)));
    proof.frames_only &= other.frames_only;
    proof.no_gaps &= other.no_gaps;
    proof.reorder_fixed &= other.reorder_fixed;
  }
  proof
}

/// What one reading of a sequence parameter set says: `None` where it does
/// not reach the fact, or FFmpeg refuses it first.
#[derive(Default)]
struct SpsFacts {
  gaps: Option<bool>,
  frames_only: Option<bool>,
  reorder_fixed: Option<bool>,
  /// Whether the reading reads the whole parameter set, every value one
  /// FFmpeg accepts and nothing past its end: FFmpeg's strict reading of
  /// it succeeds.
  whole: bool,
}

/// A sequence parameter set read as FFmpeg reads it (`h264_ps.c` 284–590,
/// VUI 104–196; `h2645_vui.c` 37–98), with the bounds it refuses on, as
/// far as it reads.
fn sps_facts(mut bits: BitReader<'_>) -> SpsFacts {
  let mut facts = SpsFacts::default();
  facts.whole = sps_read(&mut bits, &mut facts).is_some();
  facts
}

fn sps_read(bits: &mut BitReader<'_>, facts: &mut SpsFacts) -> Option<()> {
  /// What `get_ue_golomb_31` reads where FFmpeg refuses past 30.
  fn ue30(bits: &mut BitReader<'_>) -> Option<u32> {
    bits.ue().filter(|&value| value <= 30)
  }
  let profile_idc = bits.bits(8)?;
  // The constraint flags and `reserved_zero_2bits`, then `level_idc`.
  bits.bits(16)?;
  // `seq_parameter_set_id`, below `MAX_SPS_COUNT` (32).
  ue30(bits)?;
  let mut chroma_format_idc = 1;
  // FFmpeg's own list of the profiles that code chroma format, bit depths
  // and scaling matrices (`h264_ps.c` 343–354).
  if matches!(
    profile_idc,
    100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128 | 138 | 144
  ) {
    chroma_format_idc = ue30(bits).filter(|&value| value <= 3)?;
    // FFmpeg refuses separate colour planes.
    if chroma_format_idc == 3 && bits.bits(1)? != 0 {
      return None;
    }
    let luma = ue30(bits)?;
    let chroma = ue30(bits)?;
    // Equal depths, of 8 to 14 bits.
    if luma != chroma || luma > 6 {
      return None;
    }
    // `qpprime_y_zero_transform_bypass_flag`.
    bits.bits(1)?;
    // `seq_scaling_matrix_present_flag`, then six 4x4 lists and two 8x8,
    // and four more 8x8 at 4:4:4 (`h264_ps.c` 231–266).
    if bits.bits(1)? == 1 {
      let lists = if chroma_format_idc == 3 { 12 } else { 8 };
      for list in 0..lists {
        if bits.bits(1)? == 1 {
          scaling_list(bits, if list < 6 { 16 } else { 64 })?;
        }
      }
    }
  }
  // `log2_max_frame_num_minus4`, 0 to 12.
  ue30(bits).filter(|&value| value <= 12)?;
  match ue30(bits)? {
    // `log2_max_pic_order_cnt_lsb_minus4`, to 12.
    0 => {
      ue30(bits).filter(|&value| value <= 12)?;
    }
    1 => {
      // `delta_pic_order_always_zero_flag`, `offset_for_non_ref_pic`,
      // `offset_for_top_to_bottom_field`, then a cycle of fewer than 256
      // `offset_for_ref_frame`.
      bits.bits(1)?;
      bits.se()?;
      bits.se()?;
      let cycle = bits.ue().filter(|&value| value < 256)?;
      for _ in 0..cycle {
        bits.se()?;
      }
    }
    2 => {}
    _ => return None,
  }
  // `max_num_ref_frames`, to `H264_MAX_DPB_FRAMES` (16).
  ue30(bits).filter(|&value| value <= 16)?;
  facts.gaps = Some(bits.bits(1)? == 1);
  // `pic_width_in_mbs_minus1`, `pic_height_in_map_units_minus1`.
  let width = bits.ue()?;
  let height = bits.ue()?;
  let frames_only = bits.bits(1)? == 1;
  facts.frames_only = Some(frames_only);
  if !frames_only {
    // `mb_adaptive_frame_field_flag`.
    bits.bits(1)?;
  }
  // FFmpeg reads the two by `get_ue_golomb`, an error past 8190, and
  // refuses a picture `av_image_check_size` refuses: (8w + 1024)(h + 128)
  // at or past `INT_MAX` (`h264_ps.c` 446–470; `imgutils.c`).
  if width > 8190 || height > 8190 {
    return None;
  }
  let picture_width = 16 * (u64::from(width) + 1);
  let picture_height = 16 * (u64::from(height) + 1) * if frames_only { 1 } else { 2 };
  if (8 * picture_width + 1024) * (picture_height + 128) >= i32::MAX as u64 {
    return None;
  }
  // `direct_8x8_inference_flag`, then the cropping offsets, read by
  // `get_ue_golomb` and refused where they leave no picture (`h264_ps.c`
  // 474–512).
  bits.bits(1)?;
  if bits.bits(1)? == 1 {
    let mut crop = [0_u64; 4];
    for offset in &mut crop {
      *offset = u64::from(bits.ue().filter(|&value| value <= 8190)?);
    }
    let step_x = if matches!(chroma_format_idc, 1 | 2) {
      2
    } else {
      1
    };
    let step_y = if frames_only { 1 } else { 2 } << u64::from(chroma_format_idc == 1);
    if (crop[0] + crop[1]) * step_x >= picture_width
      || (crop[2] + crop[3]) * step_y >= picture_height
    {
      return None;
    }
  }
  facts.reorder_fixed = Some(bits.bits(1)? == 1 && vui_fixes_reorder(bits)?);
  Some(())
}

/// Whether a VUI sets `bitstream_restriction_flag` with a
/// `max_num_reorder_frames` of 15 at most, as `decode_vui_parameters` reads
/// it (`h264_ps.c` 129–196; `h2645_vui.c` 37–98). `false` where FFmpeg
/// leaves it unset or the depth is 16, `None` where FFmpeg refuses the
/// parameter set first.
fn vui_fixes_reorder(bits: &mut BitReader<'_>) -> Option<bool> {
  // `aspect_ratio_info_present_flag`, `aspect_ratio_idc`, an extended SAR.
  if bits.bits(1)? == 1 && bits.bits(8)? == 255 {
    bits.bits(32)?;
  }
  // `overscan_info_present_flag`, `overscan_appropriate_flag`.
  if bits.bits(1)? == 1 {
    bits.bits(1)?;
  }
  // `video_signal_type_present_flag`: the format, the range, and the
  // colour description.
  if bits.bits(1)? == 1 {
    bits.bits(4)?;
    if bits.bits(1)? == 1 {
      bits.bits(24)?;
    }
  }
  // `chroma_loc_info_present_flag`, its two sites.
  if bits.bits(1)? == 1 {
    bits.ue_31()?;
    bits.ue_31()?;
  }
  // "Truncated VUI": FFmpeg reads no further.
  if bits.peek()? == 1 && bits.left() < 10 {
    return Some(false);
  }
  // `timing_info_present_flag`: `num_units_in_tick`, `time_scale`,
  // `fixed_frame_rate_flag`.
  if bits.bits(1)? == 1 {
    bits.bits(32)?;
    bits.bits(32)?;
    bits.bits(1)?;
  }
  let nal_hrd = bits.bits(1)? == 1;
  if nal_hrd {
    hrd_parameters(bits)?;
  }
  let vcl_hrd = bits.bits(1)? == 1;
  if vcl_hrd {
    hrd_parameters(bits)?;
  }
  if nal_hrd || vcl_hrd {
    // `low_delay_hrd_flag`.
    bits.bits(1)?;
  }
  // `pic_struct_present_flag`; with nothing after it FFmpeg reads no
  // further.
  bits.bits(1)?;
  if bits.left() == 0 || bits.bits(1)? == 0 {
    return Some(false);
  }
  // `motion_vectors_over_pic_boundaries_flag`, `max_bytes_per_pic_denom`,
  // `max_bits_per_mb_denom` and the two motion vector lengths.
  bits.bits(1)?;
  for _ in 0..4 {
    bits.ue_31()?;
  }
  // `max_num_reorder_frames`, refused past 16, and
  // `max_dec_frame_buffering`. The reading ends at FFmpeg's end, so a
  // restriction it reads whole is one FFmpeg keeps (`h264_ps.c` 182–185).
  let reorder = bits.ue_31()?;
  bits.ue_31()?;
  if reorder > 16 {
    return None;
  }
  // At 16, a picture may follow 16 pictures it precedes in output order,
  // and FFmpeg then resets its record of them (`last_pocs`, `h264_slice.c`
  // 1310–1328), which a picture dropped from them changes.
  Some(reorder <= 15)
}

/// `hrd_parameters` (E.1.2), as `decode_hrd_parameters` reads it
/// (`h264_ps.c` 104–127).
fn hrd_parameters(bits: &mut BitReader<'_>) -> Option<()> {
  let cpb_count = bits.ue_31()? + 1;
  if cpb_count > 32 {
    return None;
  }
  // `bit_rate_scale`, `cpb_size_scale`.
  bits.bits(8)?;
  for _ in 0..cpb_count {
    bits.ue()?;
    bits.ue()?;
    bits.bits(1)?;
  }
  // The three delay lengths and `time_offset_length`.
  bits.bits(20)?;
  Some(())
}

/// The `slice_type` FFmpeg reads of an H.264 slice NAL unit — `payload`,
/// its bytes after the header — as `get_last_needed_nal` and the slice
/// header's parse both read it (`h264dec.c` 501–551, `h264_slice.c`
/// 1709–1727): `first_mb_in_slice` by `get_ue_golomb_long`, then
/// `slice_type` by `get_ue_golomb_31`. `None` where the header does not
/// read so; each caller then assumes what keeps its packet.
fn slice_type(payload: &[u8]) -> Option<u32> {
  let mut bits = BitReader::whole(payload, Reading::Rbsp);
  bits.ue()?;
  bits.ue_31()
}

/// Whether FFmpeg reads `slice_type` as an intra slice, I or SI: 2 or 4,
/// or 7 or 9, its value for every slice of the picture. A value past 9 is
/// P to `get_last_needed_nal` (`h264dec.c` 537–543) and refused by the
/// slice header's parse (`h264_slice.c` 1711–1717).
const fn is_intra(slice_type: u32) -> bool {
  matches!(slice_type, 2 | 4 | 7 | 9)
}

/// One scaling list of `size` deltas (7.3.2.1.1.1), each in `-128..=127`
/// as FFmpeg holds it (`h264_ps.c` 201–229).
fn scaling_list(bits: &mut BitReader<'_>, size: usize) -> Option<()> {
  let (mut last, mut next) = (8_i64, 8_i64);
  for index in 0..size {
    if next != 0 {
      let delta = bits.se().filter(|delta| (-128..=127).contains(delta))?;
      next = (last + delta) & 0xFF;
      if index == 0 && next == 0 {
        // The list is not written: the default stands.
        return Some(());
      }
    }
    if next != 0 {
      last = next;
    }
  }
  Some(())
}

/// Which bytes a [`BitReader`] reads of a NAL unit's payload.
#[derive(Clone, Copy)]
enum Reading {
  /// The RBSP: every `03` after two zero bytes dropped, as FFmpeg's
  /// splitter extracts it (`h2645_parse.c` 101–132).
  Rbsp,
  /// The bytes as they stand.
  Bytes,
  /// The bytes as `decode_extradata_ps_mp4` escapes them before it reads a
  /// parameter set again (`h264_parse.c` 438–455): a `03` written after
  /// every two zero bytes that come before a byte of 3 or less.
  Reescaped,
}

/// Bits of a NAL unit's payload, most significant first, in one
/// [`Reading`].
#[derive(Clone)]
struct BitReader<'a> {
  reading: Reading,
  rbsp: Rbsp<'a>,
  raw: &'a [u8],
  /// For [`Reading::Reescaped`], the rest of an escape being written.
  pending: &'static [u8],
  byte: u8,
  left: u32,
  /// The bits read.
  position: u64,
  /// The bits there are to read.
  end: u64,
}

impl<'a> BitReader<'a> {
  /// The reading's bits up to its stop bit: the lowest set bit of its last
  /// byte that is not zero, where FFmpeg's reading of an RBSP ends
  /// (`h2645_parse.c` 348–376).
  fn new(bytes: &'a [u8], reading: Reading) -> Self {
    let mut probe = Self::whole(bytes, reading);
    let (mut index, mut last) = (0_u64, None);
    while let Some(byte) = probe.next_byte() {
      if byte != 0 {
        last = Some((index, byte));
      }
      index += 1;
    }
    let mut reader = Self::whole(bytes, reading);
    reader.end = last.map_or(0, |(at, byte)| {
      at * 8 + 7 - u64::from(byte.trailing_zeros())
    });
    reader
  }

  /// The reading's every bit.
  const fn whole(bytes: &'a [u8], reading: Reading) -> Self {
    Self {
      reading,
      rbsp: Rbsp::new(bytes),
      raw: bytes,
      pending: &[],
      byte: 0,
      left: 0,
      position: 0,
      end: u64::MAX,
    }
  }

  /// The next byte of the reading.
  fn next_byte(&mut self) -> Option<u8> {
    match self.reading {
      Reading::Rbsp => self.rbsp.byte(),
      Reading::Bytes => {
        let (&byte, rest) = self.raw.split_first()?;
        self.raw = rest;
        Some(byte)
      }
      Reading::Reescaped => {
        if let Some((&byte, rest)) = self.pending.split_first() {
          self.pending = rest;
          return Some(byte);
        }
        // Where three bytes are left and they read `00 00 0x`, x at most
        // 3, `00 00 03` is written for the two zeros, and x is read next.
        if let [0, 0, rest @ ..] = self.raw
          && rest.first().is_some_and(|&next| next <= 3)
        {
          self.raw = rest;
          self.pending = &[0, 3];
          return Some(0);
        }
        let (&byte, rest) = self.raw.split_first()?;
        self.raw = rest;
        Some(byte)
      }
    }
  }

  fn bit(&mut self) -> Option<u32> {
    if self.position >= self.end {
      return None;
    }
    if self.left == 0 {
      self.byte = self.next_byte()?;
      self.left = 8;
    }
    self.left -= 1;
    self.position += 1;
    Some(u32::from(self.byte >> self.left) & 1)
  }

  /// The next bit, not read.
  fn peek(&self) -> Option<u32> {
    self.clone().bit()
  }

  /// The bits left before the end.
  const fn left(&self) -> u64 {
    self.end.saturating_sub(self.position)
  }

  /// `n` bits, at most 32.
  fn bits(&mut self, n: u32) -> Option<u32> {
    (0..n).try_fold(0_u32, |value, _| Some((value << 1) | self.bit()?))
  }

  /// An unsigned Exp-Golomb value, `ue(v)` (9.1): at most 31 leading zero
  /// bits, as FFmpeg's longest reader takes.
  fn ue(&mut self) -> Option<u32> {
    let mut zeros = 0;
    while self.bit()? == 0 {
      zeros += 1;
      if zeros > 31 {
        return None;
      }
    }
    let rest = self.bits(zeros)?;
    Some(((1_u64 << zeros) - 1 + u64::from(rest)) as u32)
  }

  /// A signed Exp-Golomb value, `se(v)` (9.1.1).
  fn se(&mut self) -> Option<i64> {
    let k = i64::from(self.ue()?);
    Some(if k % 2 == 1 { (k + 1) / 2 } else { -(k / 2) })
  }

  /// What `get_ue_golomb_31` reads, value and bits alike (`golomb.h`
  /// 120–141, its tables in `golomb.c`): a code of up to four leading zeros
  /// its value; five, 31 where its next three bits are zero and else 32; six
  /// to eight, 32; nine or more, 32, having taken 19 bits.
  fn ue_31(&mut self) -> Option<u32> {
    let mut zeros = 0;
    while zeros < 9 {
      if self.bit()? == 1 {
        break;
      }
      zeros += 1;
    }
    if zeros == 9 {
      self.bits(10)?;
      return Some(32);
    }
    let info = self.bits(zeros)?;
    Some(match zeros {
      0..=4 => (1 << zeros) - 1 + info,
      5 if info >> 2 == 0 => 31,
      _ => 32,
    })
  }
}

/// The size of the parameter set — its two-byte length counted — from
/// which `decode_extradata_ps_mp4` refuses one that does not decode, with
/// `ERANGE`, ending the record: `buf_size / 2 >= (INT16_MAX -
/// AV_INPUT_BUFFER_PADDING_SIZE) / 3` (`h264_parse.c` 436–437).
const ESCAPED_LIMIT: usize = 2 * ((i16::MAX as usize - 64) / 3);

/// How `ff_h264_decode_extradata` ends on an `avcC`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AvccRead {
  /// Read whole: the size is the record's, in bytes.
  Whole(u8),
  /// Refused part way: the size stays as it was.
  Fails,
  /// Read whole only if a parameter set of [`ESCAPED_LIMIT`] bytes or
  /// more decodes; the record's size, in bytes, if it does.
  Undecided(u8),
}

/// How FFmpeg reads `record`, an `avcC` (`h264_parse.c` 475–516): at least
/// seven bytes, then each count's parameter sets within the record — a
/// count FFmpeg reads past the record's end reads as 0, the zero padding
/// every FFmpeg buffer carries.
fn read_avcc(record: &[u8]) -> AvccRead {
  if record.len() < 7 {
    return AvccRead::Fails;
  }
  let mut at = 6;
  let mut undecided = false;
  if !avcc_sets(
    record,
    usize::from(record[5] & 0x1F),
    &mut at,
    &mut undecided,
  ) {
    return AvccRead::Fails;
  }
  let count = record.get(at).copied().unwrap_or(0);
  at += 1;
  if !avcc_sets(record, usize::from(count), &mut at, &mut undecided) {
    return AvccRead::Fails;
  }
  let size = (record[4] & 3) + 1;
  if undecided {
    AvccRead::Undecided(size)
  } else {
    AvccRead::Whole(size)
  }
}

/// `count` parameter sets of an `avcC` from `at`, each a two-byte size and
/// that many bytes within the record; whether all are, and whether any is
/// one of [`ESCAPED_LIMIT`] bytes or more.
fn avcc_sets(record: &[u8], count: usize, at: &mut usize, undecided: &mut bool) -> bool {
  for _ in 0..count {
    let (Some(&high), Some(&low)) = (record.get(*at), record.get(*at + 1)) else {
      return false;
    };
    let size = usize::from(u16::from_be_bytes([high, low])) + 2;
    if size > record.len() - *at {
      return false;
    }
    if size >= ESCAPED_LIMIT {
      *undecided = true;
    }
    *at += size;
  }
  true
}

/// Whether FFmpeg's H.264 decoder reads `packet` as an `avcC` of its own
/// (`h264dec.c` 1045–1046 and `is_avcc_extradata`, 899–921): at least nine
/// bytes, a first byte of 1, a third of 0 and a fifth whose high six bits
/// are set, then one or more sequence parameter sets and one or more
/// picture parameter sets, each within the packet.
fn is_avcc_packet(packet: &[u8]) -> bool {
  if packet.len() < 9 || packet[0] != 1 || packet[2] != 0 || packet[4] & 0xFC != 0xFC {
    return false;
  }
  // FFmpeg's padding: a byte past the packet reads as 0.
  let at_or_zero = |at: usize| packet.get(at).copied().unwrap_or(0);
  let sets = |count: usize, kind: u8, at: &mut usize| -> bool {
    if count == 0 {
      return false;
    }
    for _ in 0..count {
      let size = usize::from(u16::from_be_bytes([at_or_zero(*at), at_or_zero(*at + 1)])) + 2;
      if size > packet.len().saturating_sub(*at) || at_or_zero(*at + 2) & 0x9F != kind {
        return false;
      }
      *at += size;
    }
    true
  };
  let mut at = 6;
  if !sets(usize::from(packet[5] & 0x1F), 7, &mut at) {
    return false;
  }
  let count = at_or_zero(at);
  at += 1;
  sets(usize::from(count), 8, &mut at)
}

/// Whether FFmpeg's H.264 decoder reads `packet` after lengths where its
/// `nal_length_size` is 4 (`h264dec.c` 602–607): start codes for a packet
/// that begins with a four-byte start code whose next four bytes are a
/// length past the packet; lengths for one whose first four bytes are a
/// length that fits; else as the packet before it.
fn h264_packet_lengths(packet: &[u8], lengths: bool) -> bool {
  let word = |at: usize| -> Option<u32> {
    let bytes = packet.get(at..at + 4)?;
    Some(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
  };
  let size = packet.len();
  if size > 8 && word(0) == Some(1) && word(5).is_some_and(|next| next as usize > size) {
    false
  } else if size > 3 && word(0).is_some_and(|first| first > 1 && first as usize <= size) {
    true
  } else {
    lengths
  }
}

/// A record read as FFmpeg's `GetByteContext` reads one (`bytestream.h`):
/// a read past the end gives 0 and leaves the reader at the end.
struct Bytes<'a> {
  rest: &'a [u8],
}

impl<'a> Bytes<'a> {
  const fn new(bytes: &'a [u8]) -> Self {
    Self { rest: bytes }
  }

  const fn left(&self) -> usize {
    self.rest.len()
  }

  fn skip(&mut self, count: usize) {
    self.rest = &self.rest[count.min(self.rest.len())..];
  }

  fn byte(&mut self) -> u8 {
    match self.rest.split_first() {
      Some((&byte, rest)) => {
        self.rest = rest;
        byte
      }
      None => 0,
    }
  }

  fn be16(&mut self) -> u16 {
    match self.rest.split_first_chunk::<2>() {
      Some((&bytes, rest)) => {
        self.rest = rest;
        u16::from_be_bytes(bytes)
      }
      None => {
        self.rest = &self.rest[self.rest.len()..];
        0
      }
    }
  }

  fn peek_be16(&self) -> u16 {
    match self.rest.first_chunk::<2>() {
      Some(&bytes) => u16::from_be_bytes(bytes),
      None => 0,
    }
  }

  /// The next `count` bytes, which the caller has checked are left.
  fn take(&mut self, count: usize) -> &'a [u8] {
    let (taken, rest) = self.rest.split_at(count);
    self.rest = rest;
    taken
  }
}

/// One H.264 NAL unit, read.
fn h264_unit(nal: &[u8]) -> Read {
  let Some(&header) = nal.first() else {
    return Read::Keeps(KeptReason::Malformed);
  };
  if header & 0x80 != 0 {
    return Read::Keeps(KeptReason::Malformed);
  }
  let nal_ref_idc = (header >> 5) & 3;
  match header & 0x1F {
    // An IDR slice.
    5 => Read::Keeps(KeptReason::RandomAccess),
    // A slice, or a slice data partition A, B or C.
    1..=4 if nal_ref_idc != 0 => Read::Keeps(KeptReason::Referenced),
    1..=4 => Read::Picture,
    // An access unit delimiter, filler data.
    9 | 12 => Read::Beside,
    // SEI whose every message its picture alone takes up.
    6 if sei_rides(&nal[1..], h264_sei_rides) => Read::Beside,
    // A prefix NAL unit, a coded slice extension (scalable or multiview),
    // a 3-D AVC slice extension.
    14 | 20 | 21 => Read::Keeps(KeptReason::Extension),
    // SPS, PPS, any other SEI, the end of a sequence or of the stream, an
    // SPS extension, a subset SPS, a depth parameter set, an auxiliary
    // slice, and every reserved or unspecified type.
    other => Read::Keeps(KeptReason::Companion(other)),
  }
}

/// The H.264 SEI messages a picture alone takes up ([SEI](self#sei)):
/// picture timing, filler payload.
const fn h264_sei_rides(payload_type: u32) -> bool {
  matches!(payload_type, 1 | 3)
}

/// The HEVC SEI messages a picture alone takes up ([SEI](self#sei)):
/// filler payload.
const fn hevc_sei_rides(payload_type: u32) -> bool {
  matches!(payload_type, 3)
}

/// Whether an SEI unit — `payload`, its bytes after the header — holds one
/// message or more, every one of a type `rides` admits, read as FFmpeg
/// reads them (`h264_sei.c` 229–300, `hevc/sei.c` 264–317): emulation
/// prevention bytes dropped, each message a `payloadType` and a
/// `payloadSize` coded in runs of `0xFF`, then that many bytes, up to the
/// unit's last byte, `rbsp_trailing_bits`, which is `0x80`. Trailing zero
/// bytes are dropped first, as FFmpeg drops them (`h2645_parse.c`
/// 348–376). A unit that does not read so rides nowhere.
fn sei_rides(payload: &[u8], rides: fn(u32) -> bool) -> bool {
  let Some(last) = payload.iter().rposition(|&byte| byte != 0) else {
    return false;
  };
  if payload[last] != 0x80 {
    return false;
  }
  let mut rbsp = Rbsp::new(&payload[..last]);
  let mut messages = 0_usize;
  while let Some(first) = rbsp.byte() {
    let Some(payload_type) = rbsp.coded(first) else {
      return false;
    };
    let Some(size) = rbsp.byte().and_then(|first| rbsp.coded(first)) else {
      return false;
    };
    if !rides(payload_type) || !rbsp.skip(size) {
      return false;
    }
    messages += 1;
  }
  messages > 0
}

/// Whether an SEI unit — `payload`, its bytes after the header — may hold a
/// message of `payload_type`: one of its messages is, or it does not read
/// as [`sei_rides`] reads one.
fn sei_may_hold(payload: &[u8], payload_type: u32) -> bool {
  let Some(last) = payload.iter().rposition(|&byte| byte != 0) else {
    return true;
  };
  if payload[last] != 0x80 {
    return true;
  }
  let mut rbsp = Rbsp::new(&payload[..last]);
  while let Some(first) = rbsp.byte() {
    let Some(found) = rbsp.coded(first) else {
      return true;
    };
    let Some(size) = rbsp.byte().and_then(|first| rbsp.coded(first)) else {
      return true;
    };
    if found == payload_type || !rbsp.skip(size) {
      return true;
    }
  }
  false
}

/// A NAL unit's bytes as its RBSP: each emulation prevention byte — a 3
/// after two zero bytes — dropped, as `ff_h2645_extract_rbsp` drops it
/// (`h2645_parse.c` 37–147).
#[derive(Clone)]
struct Rbsp<'a> {
  raw: &'a [u8],
  zeros: u8,
}

impl<'a> Rbsp<'a> {
  const fn new(raw: &'a [u8]) -> Self {
    Self { raw, zeros: 0 }
  }

  fn byte(&mut self) -> Option<u8> {
    loop {
      let (&byte, rest) = self.raw.split_first()?;
      self.raw = rest;
      if self.zeros >= 2 && byte == 3 {
        self.zeros = 0;
        continue;
      }
      self.zeros = if byte == 0 {
        self.zeros.saturating_add(1)
      } else {
        0
      };
      return Some(byte);
    }
  }

  /// A value coded as SEI codes `payloadType` and `payloadSize`: each
  /// `0xFF` byte adds 255 and one more, `first` among them, ends it
  /// (7.3.2.3.1 in H.264, 7.3.5 in H.265).
  fn coded(&mut self, first: u8) -> Option<u32> {
    let mut value = u32::from(first);
    let mut byte = first;
    while byte == 0xFF {
      byte = self.byte()?;
      value = value.checked_add(u32::from(byte))?;
    }
    Some(value)
  }

  /// Passes over `count` bytes; whether there were that many.
  fn skip(&mut self, count: u32) -> bool {
    (0..count).all(|_| self.byte().is_some())
  }
}

/// A packet's verdict, with what is pending after it.
struct Judged {
  verdict: Verdict,
  /// The census of the packet's units ([`Census::close`]).
  pending: Pending,
}

/// What FFmpeg's decoder holds for later pictures that the packets and
/// records around a picture move ([the census](self#one-census-every-road)).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct Pending {
  /// For H.264: whether a reference picture has been read since the
  /// reading opened or was sought.
  reference_seen: bool,
  /// For H.264: whether a packet kept since the reading opened or was
  /// sought has held an intra slice.
  intra_seen: bool,
  /// For HEVC: whether a recovery point SEI may have been read since the
  /// last IRAP picture.
  recovering: bool,
  /// For HEVC: whether the next packet is the first the decoder reads
  /// after it opens or is flushed, or after an end of sequence or of
  /// bitstream.
  sequence_start: bool,
  /// For HEVC: whether SEI state or a Dolby Vision RPU may be pending for
  /// the next picture the decoder decodes.
  carried: bool,
}

impl Pending {
  /// The decoder as it opens: FFmpeg's HEVC decoder sets `eos`
  /// (`hevc/hevcdec.c` 4161), and nothing is pending or seen.
  const OPENING: Self = Self {
    reference_seen: false,
    intra_seen: false,
    recovering: false,
    sequence_start: true,
    carried: false,
  };

  /// What is pending after a packet the decoder may have read, leaving
  /// `self`, or may not have, leaving `unread`: whatever either leaves
  /// pending, and as seen only what both have seen.
  const fn either(self, unread: Self) -> Self {
    Self {
      reference_seen: self.reference_seen && unread.reference_seen,
      intra_seen: self.intra_seen && unread.intra_seen,
      recovering: self.recovering || unread.recovering,
      sequence_start: self.sequence_start || unread.sequence_start,
      carried: self.carried || unread.carried,
    }
  }
}

/// The road a NAL unit comes to the decoder by, which decides what the
/// decoder takes up of it ([the census](self#one-census-every-road)).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Road {
  /// A unit of a packet the reading reads whole, in the packet's order:
  /// the decoder takes it up as its type says.
  Packet,
  /// A unit of a configuration record, which the decoder reads before the
  /// packet that carries it: H.264 takes up its parameter sets alone
  /// (`h264_parse.c` 380–409), HEVC its parameter sets and its SEI units,
  /// prefix and suffix, of any layer (`hevc/parse.c` 39–67).
  Record,
  /// A unit the decoder's splitter may read in a packet the reading does
  /// not read whole: it may leave state behind, and takes none up.
  Unread,
}

/// The census of one run of NAL units the decoder reads together — a
/// packet's, or a record's — taken up unit by unit ([`Self::unit`]) against
/// what was pending before them. Every NAL unit the reading reads, on every
/// road, H.264's and HEVC's alike, goes through [`Self::unit`], and nothing
/// else moves what is pending but a seek ([`NalStream::seeked`]) and a
/// record that may fail ([`NalStream::read`]).
#[derive(Debug, Clone, Copy)]
struct Census {
  codec: NalCodec,
  road: Road,
  /// What is pending, as the units so far leave it.
  pending: Pending,
  /// HEVC: a suffix SEI unit or an RPU of the packet, which only a picture
  /// of the packet takes up.
  tail: bool,
  /// HEVC: a picture the decoder decodes whatever came before has begun in
  /// the packet.
  decodes: bool,
  /// HEVC: the packet holds an IRAP picture of layer 0 the decoder decodes
  /// (types 16–21).
  irap: bool,
  /// HEVC: the packet may hold an end of sequence or of bitstream.
  ends: bool,
  /// H.264: the packet holds a slice of a reference picture the decoder
  /// decodes (types 1 and 5).
  reference: bool,
  /// H.264: the packet holds a slice the decoder decodes that reads as
  /// intra.
  intra: bool,
}

impl Census {
  /// The census of a run of units that come by `road`, against `pending`.
  const fn new(codec: NalCodec, road: Road, pending: Pending) -> Self {
    Self {
      codec,
      road,
      pending,
      tail: false,
      decodes: false,
      irap: false,
      ends: false,
      reference: false,
      intra: false,
    }
  }

  /// Takes one NAL unit up, as FFmpeg's decoder takes it up on the
  /// census's road: what it leaves pending for a later picture, and what
  /// pending state it takes up.
  fn unit(&mut self, nal: &[u8]) {
    match self.codec {
      NalCodec::H264 => {
        // The decoder takes nothing pending from a record, whose parameter
        // sets alone it reads (`h264_parse.c` 380–409), and its SEI state
        // lasts one access unit (`h264dec.c` 593–598): what H.264 awaits is
        // met only in a packet read whole.
        if self.road != Road::Packet {
          return;
        }
        let Some(&header) = nal.first() else {
          return;
        };
        let nal_unit_type = header & 0x1F;
        // The decoder decodes slices of types 1 and 5 alone, passing over
        // slice data partitions (`h264dec.c` 636–685).
        if nal_unit_type == 5 || (nal_unit_type == 1 && header & 0x60 != 0) {
          self.reference = true;
        }
        if matches!(nal_unit_type, 1 | 5) && slice_type(&nal[1..]).is_some_and(is_intra) {
          self.intra = true;
        }
      }
      NalCodec::Hevc => {
        let &[first, second, ..] = nal else {
          return;
        };
        let nal_unit_type = (first >> 1) & 0x3F;
        // The decoder reads an SEI unit of any layer, a packet's or a
        // record's, into the state the pictures after it take up
        // (`hevc/hevcdec.c` 3626–3631, `hevc/parse.c` 59–64).
        if matches!(nal_unit_type, 39 | 40) {
          // A recovery point (D.3.8), or a unit that does not read so.
          if sei_may_hold(&nal[2..], 6) {
            self.pending.recovering = true;
          }
          if !sei_rides(&nal[2..], hevc_sei_rides) {
            // A packet's suffix unit waits for a picture of its own packet,
            // after it; any other — a prefix unit, or a record's, read
            // before the packet — for the next picture decoded.
            if nal_unit_type == 40 && self.road == Road::Packet {
              self.tail = true;
            } else {
              self.pending.carried = true;
            }
          }
        }
        // Of a record the decoder reads nothing else that a picture takes up
        // (`hevc/parse.c` 65–67), and of a packet not read whole nothing
        // counts but what it may leave behind.
        if self.road != Road::Packet {
          return;
        }
        let base = first & 1 == 0 && second >> 3 == 0;
        // The IRAP types the decoder decodes; it skips RSV_IRAP_VCL22 and 23.
        self.irap |= base && (16..=21).contains(&nal_unit_type);
        self.ends |= matches!(nal_unit_type, 36 | 37);
        match nal_unit_type {
          // The first slice segment of a picture the decoder decodes
          // whatever came before (every type but RASL and the reserved
          // ones): it takes what is pending up.
          0..=7 | 16..=21 if base && nal.get(2).is_some_and(|&byte| byte & 0x80 != 0) => {
            self.pending.carried = false;
            self.decodes = true;
          }
          // An RPU, which the decoder takes from the packet's last units
          // before any other (`hevc/hevcdec.c` 3723–3760).
          62 => self.tail = true,
          _ => {}
        }
      }
    }
  }

  /// What is pending after the packet whose units the census has taken up:
  /// `empty` for a packet of no bytes, `kept` where the packet is kept —
  /// one marked may not reach the decoder.
  fn close(mut self, empty: bool, kept: bool) -> Pending {
    // A suffix SEI unit or an RPU no picture of its own packet took up
    // waits for the next picture.
    if self.tail && !self.decodes {
      self.pending.carried = true;
    }
    // An IRAP picture ends any recovery, one a recovery point in its own
    // packet would start among them: FFmpeg's decoder starts none after it
    // before an end of sequence or a flush (`hevc/hevcdec.c` 3283–3294,
    // 3675–3692).
    if self.irap {
      self.pending.recovering = false;
    }
    // FFmpeg's HEVC decoder takes `eos` up with every packet it reads
    // (`hevc/hevcdec.c` 3687–3720), and a packet of no bytes is none.
    if !empty {
      self.pending.sequence_start = self.ends;
    }
    if self.reference {
      self.pending.reference_seen = true;
    }
    if self.intra && kept {
      self.pending.intra_seen = true;
    }
    self.pending
  }

  /// What is pending after `packet`, which the reading does not read whole
  /// — framed by `framing`, or for `None` by any framing a decoder may read
  /// it in — with whatever units of it the census has taken up already:
  /// SEI state, an end of sequence or a recovery point may be left behind,
  /// in any unit the decoder's splitter may read, and nothing the packet
  /// would take up or prove counts.
  fn unread(mut self, packet: &[u8], framing: Option<Framing>) -> Pending {
    self.road = Road::Unread;
    self.pending.carried = true;
    self.ends = true;
    self.tail = false;
    self.decodes = false;
    self.irap = false;
    self.reference = false;
    self.intra = false;
    match framing {
      Some(framing) => each_unit(packet, framing, &mut |nal| self.unit(nal)),
      None => each_unit_in_any_framing(packet, &mut |nal| self.unit(nal)),
    }
    self.close(packet.is_empty(), true)
  }
}

/// Every NAL unit FFmpeg's splitter may read in `packet` framed by
/// `framing` (`h2645_parse.c` 527–664), and more: in Annex B the bytes after
/// each start code, past bytes that frame nothing; after lengths each unit,
/// up to a length that does not fit — where the splitter refuses the packet
/// whole — and the bytes after each start code within a unit.
fn each_unit(packet: &[u8], framing: Framing, visit: &mut impl FnMut(&[u8])) {
  match framing {
    Framing::AnnexB => {
      for nal in AfterStartCodes::new(packet) {
        visit(nal);
      }
    }
    Framing::Length(size) => each_after_lengths(packet, size.bytes(), visit),
  }
}

/// [`each_unit`] in every framing a decoder may read `packet` in: Annex B,
/// and lengths of one to four bytes (FFmpeg's `nal_length_size`, set from a
/// record's two bits plus one).
fn each_unit_in_any_framing(packet: &[u8], visit: &mut impl FnMut(&[u8])) {
  each_unit(packet, Framing::AnnexB, visit);
  for size in 1..=4 {
    each_after_lengths(packet, size, visit);
  }
}

/// [`each_unit`] for NAL units after lengths of `size` bytes.
fn each_after_lengths(mut bytes: &[u8], size: usize, visit: &mut impl FnMut(&[u8])) {
  while let Some((prefix, rest)) = bytes.split_at_checked(size) {
    let length = prefix
      .iter()
      .fold(0_usize, |length, &byte| (length << 8) | usize::from(byte));
    if length == 0 {
      return;
    }
    let Some((nal, rest)) = rest.split_at_checked(length) else {
      return;
    };
    // The splitter reads a unit to its first start code, and then the bytes
    // after each start code in it as units of their own (`h2645_parse.c`
    // 44–52, 559–584).
    let first = find_start_code(nal).unwrap_or(nal.len());
    visit(trim_trailing_zeros(&nal[..first]));
    for piece in AfterStartCodes::new(nal) {
      visit(piece);
    }
    bytes = rest;
  }
}

/// What one NAL unit says about its packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Read {
  /// A slice of a picture nothing references.
  Picture,
  /// A NAL unit that may ride beside such a picture.
  Beside,
  /// A NAL unit that keeps the packet.
  Keeps(KeptReason),
}

/// The NAL units of a packet, one at a time, read strictly: any byte the
/// framing does not account for is [`Unit::Broken`].
struct Units<'a> {
  rest: &'a [u8],
  framing: Framing,
  /// Annex B: whether the first start code has been read.
  started: bool,
  /// Annex B: whether the last bytes read were a start code, which a NAL
  /// unit must follow.
  after_start_code: bool,
}

/// What [`Units::next`] yields.
enum Unit<'a> {
  /// A NAL unit, its header first.
  Nal(&'a [u8]),
  /// The packet's end.
  End,
  /// Bytes the framing does not account for.
  Broken,
}

impl<'a> Units<'a> {
  const fn new(packet: &'a [u8], framing: Framing) -> Self {
    Self {
      rest: packet,
      framing,
      started: false,
      after_start_code: false,
    }
  }

  fn next(&mut self) -> Unit<'a> {
    match self.framing {
      Framing::Length(size) => self.next_after_length(size.bytes()),
      Framing::AnnexB => self.next_after_start_code(),
    }
  }

  /// The next NAL unit after its length: a length of 0, one past the
  /// packet, bytes too few for a length, and a unit holding a start code
  /// — which FFmpeg's splitter reads as more than one unit
  /// (`h2645_parse.c` 44–52, 559–584) — are broken.
  fn next_after_length(&mut self, size: usize) -> Unit<'a> {
    if self.rest.is_empty() {
      return Unit::End;
    }
    let Some((prefix, rest)) = self.rest.split_at_checked(size) else {
      return Unit::Broken;
    };
    let length = prefix
      .iter()
      .fold(0_usize, |length, &byte| (length << 8) | usize::from(byte));
    if length == 0 {
      return Unit::Broken;
    }
    let Some((nal, rest)) = rest.split_at_checked(length) else {
      return Unit::Broken;
    };
    if find_start_code(nal).is_some() {
      return Unit::Broken;
    }
    self.rest = rest;
    Unit::Nal(nal)
  }

  /// The next NAL unit after its start code, to the next start code or the
  /// packet's end, its trailing zero bytes dropped. Zero bytes may lead the
  /// first start code; any other byte before it is broken, and so is a
  /// packet with no start code, and a start code no NAL unit follows.
  fn next_after_start_code(&mut self) -> Unit<'a> {
    if !self.started {
      let lead = self.rest.iter().take_while(|&&byte| byte == 0).count();
      if lead < 2 || self.rest.get(lead) != Some(&1) {
        return Unit::Broken;
      }
      self.rest = &self.rest[lead + 1..];
      self.started = true;
    } else if self.rest.is_empty() {
      return if self.after_start_code {
        Unit::Broken
      } else {
        Unit::End
      };
    }
    // The unit runs to the next `00 00 01`; the zeros before it are the
    // next start code's, or trailing zero bytes.
    let (unit, rest) = match find_start_code(self.rest) {
      Some(at) => {
        self.after_start_code = true;
        (&self.rest[..at], &self.rest[at + 3..])
      }
      None => {
        self.after_start_code = false;
        (self.rest, &self.rest[self.rest.len()..])
      }
    };
    self.rest = rest;
    let nal = trim_trailing_zeros(unit);
    if nal.is_empty() {
      return Unit::Broken;
    }
    Unit::Nal(nal)
  }
}

/// The bytes after each start code in a run of bytes that one byte or more
/// follows, each to the next start code or the end, its trailing zero
/// bytes dropped: every NAL unit FFmpeg's splitter may find there by
/// searching for start codes (`find_next_start_code`, `h2645_parse.c`
/// 465–478), whatever comes between.
struct AfterStartCodes<'a> {
  rest: &'a [u8],
}

impl<'a> AfterStartCodes<'a> {
  const fn new(bytes: &'a [u8]) -> Self {
    Self { rest: bytes }
  }
}

impl<'a> Iterator for AfterStartCodes<'a> {
  type Item = &'a [u8];

  fn next(&mut self) -> Option<&'a [u8]> {
    loop {
      let at = find_start_code(self.rest)?;
      let after = &self.rest[at + 3..];
      let end = find_start_code(after).unwrap_or(after.len());
      self.rest = &after[end..];
      let unit = trim_trailing_zeros(&after[..end]);
      if !unit.is_empty() {
        return Some(unit);
      }
    }
  }
}

/// `bytes` without its trailing zero bytes.
fn trim_trailing_zeros(bytes: &[u8]) -> &[u8] {
  let trailing = bytes.iter().rev().take_while(|&&byte| byte == 0).count();
  &bytes[..bytes.len() - trailing]
}

/// Where the next `00 00 01` begins in `bytes`.
fn find_start_code(bytes: &[u8]) -> Option<usize> {
  bytes.windows(3).position(|window| window == [0, 0, 1])
}

// The laws build packets in a `Vec`, so they need an allocator.
#[cfg(all(test, any(feature = "std", feature = "alloc")))]
mod tests;
