//! The FFmpeg muxer: `mediaencode`'s [`Muxer`] face over libavformat.
//!
//! [`FfmpegMuxer`] writes a container — any libavformat names, by its
//! short name ([`ContainerName`]) — into a caller's `Write + Seek`, from
//! the track table a remux collects: one
//! [`TrackDescription`] per track, each carrying the [`CodecTicket`] its
//! codec parameters are rebuilt from, an attachment track carrying its
//! one file. Packets are copied into libavformat, never lent
//! ([`mediaio_ffmpeg::output`]).
//!
//! **This module holds no `unsafe`.** It reaches FFmpeg through the base
//! alone: the muxing context ([`mediaio_ffmpeg::output::Output`], whose
//! documentation carries the census — what it owns and frees on every
//! road, every `unsafe` read and write, every FFmpeg call and whose its
//! failures are), the codec ids' descriptor reads (`CodecId::intra_only`,
//! `CodecId::bits_per_sample`), and the side-data names
//! ([`packet_side_data_name`](mediaio_ffmpeg::extras::packet_side_data_name)).
//! What it refuses before any of them is reached is its own, by name
//! ([`FfmpegMuxRefusal`], [`MuxOpenError`]).
//!
//! # Opening: refusals by name, losses in a report
//!
//! [`CarrierMuxer::open`] judges the whole table before it writes a byte.
//! What the container cannot take at all is refused by name, and nothing
//! is written ([`MuxOpenError`]): a codec the muxer says it cannot store,
//! an attachment Matroska could not label, a chapter whose span runs
//! backwards. What the container can take only by dropping something is
//! written, and what it drops is **reported** ([`MuxReport`]): an
//! attachment MP4 has no place for, a language MP4 cannot spell, a
//! disposition flag the container has no field for.
//!
//! The report follows each track's **road**, as the roads keep different
//! fields: a timed stream keeps its language and the flags its container
//! has fields for; an attachment Matroska writes keeps its file, filename,
//! MIME type and language, but no flag; MP4's cover keeps its picture
//! alone, and reads back as an attached picture — its filename, MIME
//! type, language and other flags are each reported
//! ([`Uncarried::Filename`], [`Uncarried::MimeType`]).
//!
//! The report covers what this release knows of each container — the
//! constants on [`ContainerName`]: Matroska, WebM and MP4. A container is
//! known by the muxer libavformat resolves its name to, never by the
//! name's spelling: libavformat matches a name case-insensitively, so
//! `MP4` opens MP4's muxer and meets every rule MP4's does. Any other
//! muxer opens the same way and libavformat decides what it writes; an
//! attachment is not handed to it (and is reported), and nothing else is
//! claimed about what it keeps but the bitstream filters it inserts, each
//! named for its track, and what those this muxer mirrors convert or drop
//! (table B).
//!
//! # Writing: the face's refusals, then the container's
//!
//! Every write keeps the face's rules — the track exists and is of the
//! packet's kind, it carries the decode time its track is ordered by,
//! which does not go back ([`check_decode_order`]), and it is not
//! presented before it is decoded — each refused as a [`MuxRefusal`]
//! with the packet handed back. The presentation rule is judged as
//! libavformat judges it, on the counts it is handed in the stream's own
//! time base ([`check_presentation_order`] over those counts), so two
//! times a hair apart that count to one tick are written, as a stream
//! copy writes them; its refusal names the times the packet carries.
//! The counts come first: a time the stream cannot count — past `i64`,
//! or landing on libavformat's no-timestamp value — is refused by the
//! muxer's own name ([`FfmpegMuxRefusal::Unrepresentable`], in
//! [`MuxError::Backend`]). Then the container's own rule: a container
//! without `AVFMT_TS_NONSTRICT` needs a video or audio stream's decode
//! times strictly increasing, which libavformat would refuse only after
//! taking the packet; this muxer refuses the repeat first, by its own
//! name ([`FfmpegMuxRefusal::RepeatedDts`]), and hands the packet back.
//!
//! # The packet-rule table
//!
//! libavformat interleaves the packets it is handed before its muxer sees
//! them, so a muxer that refuses a packet does so after taking it, and
//! often while writing another. This muxer accounts for every rule
//! libavformat applies on its packet path in the containers it knows —
//! the generic checks and timestamp computation (`libavformat/mux.c`),
//! MP4's (`movenc.c`) and Matroska's and WebM's (`matroskaenc.c`), at
//! FFmpeg 9.0.1 — and maps each to exactly one of three:
//!
//! - **refused by name**, before libavformat is handed anything, the
//!   packet untouched: a timing rule as a [`FfmpegMuxRefusal`] variant,
//!   a rule over the packet's bytes or side data as
//!   [`FfmpegMuxRefusal::Payload`] and its [`PayloadRule`], and a track
//!   the container cannot write at all as a [`MuxOpenError`] at open;
//! - **reported**, where the container writes a track's packets in its
//!   own form ([`Uncarried::Rewritten`] and its [`Rewrite`]): at open, or,
//!   for what only the packets show — MP4's durations, where they do not
//!   abut — as it is learned ([`CarrierMuxer::report`]);
//! - **passed through**, written and read back as handed over.
//!
//! Another container is opened the same way and keeps the generic rules —
//! duration, key frame and decode order, and libavformat's shift of early
//! times — and the rules of every filter it inserts that this muxer
//! mirrors (table B); nothing else is claimed of it, nor of its side data
//! beyond what such a filter drops.
//!
//! # The automatic filters
//!
//! libavformat's automatic bitstream filters are on, as libavformat and
//! `ffmpeg -c copy` have them (`AVFMT_FLAG_AUTO_BSF`,
//! `libavformat/options_table.h` 42): container-level conversions that lose
//! nothing of what a decoder reads, each named for its track
//! ([`Uncarried::Filtered`], [`AutomaticFilter`]) — at open, where open
//! decides it, else at the write of the packet that inserts it —
//!
//! - MP4 and Matroska strip AAC's ADTS headers where the track states no
//!   configuration, making it from the first frame's header, and FLV and
//!   LATM where the track's first packet is ADTS;
//! - MP4, Matroska, WebM, MOV's other modes and IVF join VP9's hidden
//!   frames, with the next frame the stream shows, into superframes;
//! - Matroska joins PGS segments into display sets;
//! - DAUD, GXF and MXF cut PCM into packets of their counts of samples;
//!   IVF and OBU put temporal delimiters before AV1's temporal units; MCC
//!   and MXF rewrite EIA-608 as SMPTE 436M; FLV makes a configuration from
//!   the parameter sets of H.264, HEVC, VVC, AV1 or MPEG-4 that states
//!   none; MPEG-TS, MXF and the raw muxers rewrite length-prefixed H.264,
//!   HEVC or VVC as Annex B.
//!
//! **The first packet decides.** libavformat inserts most filters when it
//! writes a track's first packet, and never later: every muxer's
//! `check_bitstream` answers 1 or the insertion's answer, which settles the
//! track (`check_bitstream`, `mux.c` 1056–1073; table B's greps): VP9's and
//! PGS's always, AAC's only where that packet opens with an ADTS header
//! (`movenc.c` 9138–9151; `matroskaenc.c` 3630–3646), the Annex B
//! conversions only where it is length-prefixed. This muxer admits on an
//! AAC track with no configuration in MP4 and Matroska only what the filter
//! converts, one ADTS frame a packet ([`PayloadRule::AdtsFrame`]), so the
//! report at open stays true; on a configured one, and in another
//! container, the first packet decides as it does for libavformat — an
//! ADTS first packet inserts the filter, a raw one none, after which MP4
//! and Matroska refuse an ADTS packet ([`PayloadRule::AdtsHeader`]) — and
//! the filter it inserts is named at its write, once. What a
//! filter would refuse after taking it is refused first, by name
//! ([`PayloadRule::Superframe`], [`PayloadRule::PgsSegments`],
//! [`PayloadRule::AdtsFrame`]), every packet it puts out is sized before any
//! of it is handed over (table B), and every mirror of libavformat's timing
//! and the container's moves on what a filter puts out now, in the times
//! each packet it puts out carries — never on what it takes ([`Emits`];
//! the mirrors, table B): a packet a filter holds meets none of the rules
//! of its times until a packet put out carries them — PGS's display set, at
//! its end segment's packet, in its presentation segment's packet's times
//! — and a hidden VP9 frame's are dropped. PGS's open display sets this
//! muxer holds itself, and hands to libavformat whole at the packet that
//! completes them, so that nothing reaches FFmpeg's filter that a later
//! refusal would leave there ([`CarrierMuxer::take_held`]).
//!
//! A caller turns them off ([`MuxOptions::set_automatic_filters`]), and no
//! filter is then inserted at a first packet: AAC with no configuration is
//! refused at open ([`MuxOpenError::ConfigurationMissing`]), an ADTS packet
//! anywhere ([`PayloadRule::AdtsHeader`]). A filter a muxer inserts at its
//! start runs either way — IVF's VP9 superframes (`ivf_init`, `ivfenc.c`
//! 42) — and is mirrored and reported either way.
//!
//! # The side-data table
//!
//! libavformat's generic packet path reads no packet side data
//! (`libavformat/mux.c` in FFmpeg 9.0.1); each container writes the kinds
//! it has a place for, and no other. A kind a container does not write so
//! that it reads back as itself is reported for its track, by name, when
//! the first packet carrying it is written ([`Uncarried::SideData`],
//! [`SideDataFate::Dropped`]); a kind whose fate the source does not settle
//! — where it depends on a later packet, or on where the packet lies — is
//! reported as possibly dropped ([`SideDataFate::PossiblyDropped`]). An
//! entry that says nothing — empty, or skip samples of zeros — loses
//! nothing.
//!
//! | kind | MP4 (`movenc.c`) | Matroska, WebM (`matroskaenc.c`) |
//! |---|---|---|
//! | new extradata | kept: a sample description from the packet on, or the configuration of a track that had none (7044–7074, 7412–7429); AC-3's and E-AC-3's dropped: their sample entry's configuration is MP4's own parse of the frames, the bytes never written (`mov_write_ac3_tag`, `mov_write_eac3_tag`, 418–462, 612–666; 1084–1087, 1507–1510) | AAC, FLAC (Matroska): possibly dropped — it rewrites the track's one configuration, which a later packet's replaces (3065–3097); any other: dropped, unless it is the track's own (3098–3115) |
//! | skip samples | possibly dropped: the end skip trims the track's end in the edit list from the last packet alone, the start skip and the reasons are not read; dropped where the codec fixes no frame's samples (7311–7319, 7333–7342) | an audio track's end skip, as the block's discard padding (2922–2934), and its first packet's start skip where it is the stream's initial padding, written as the codec delay (2089–2104), which the reader puts back (`matroskadec.c` 2896–2901): kept; else dropped |
//! | producer reference time | dropped: written only into fragments, with `write_prft` set (6034–6102, 7365–7369) | dropped |
//! | WebVTT identifier, settings | dropped | a WebVTT cue's: kept, on lines of their own (2828–2855, 3591–3593); else dropped |
//! | Matroska block additional | dropped | kept where its id — its first eight bytes, big-endian — is 1 (2936–2946); else dropped |
//! | HDR10+, SMPTE 2094 APP5, LCEVC | dropped | a video track's: kept, as ITU-T T.35 block additions (2947–3006) the trailer maps (3418–3443), which the reader turns back into their kinds (`matroskadec.c` 4038–4130); else dropped |
//! | any other | dropped | dropped |
//!
//! A packet whose own side data an automatic filter drops — a hidden VP9
//! frame's, a PGS packet's that holds no presentation segment — has all of
//! it dropped (`vp9_superframe.c` 169; `pgs_frame_merge.c` 132–136), and the first
//! frame AAC's filter converts has its new extradata replaced by the
//! configuration the filter makes (`aac_adtstoasc.c` 98–103).
//!
//! # The preflight census
//!
//! **The rule:** for every byte this muxer hands libavformat, it
//! establishes before the handoff every precondition FFmpeg 9.0.1's
//! consuming code relies on. A check libavformat makes only after it has
//! taken a packet — in a muxer's write, in a bitstream filter, at the
//! trailer — is made here first, and refused by name with the packet
//! untouched; where the check needs the whole stream, the refusal comes at
//! the first packet that decides it. Every change a container makes that
//! the caller did not ask for is reported. The tables below are that
//! census, each row from the source; `movenc.c` and `matroskaenc.c` are
//! `libavformat/`'s, the filters `libavcodec/bsf/`'s.
//!
//! ## A. Packet side data, and what each writer assumes of it
//!
//! Every reader of packet side data in libavformat's writers, by kind; what
//! each assumes of the entry's bytes; and what is held to it here. Every
//! entry reaches libavformat as libavcodec allocates it, its size and 64
//! zeroed bytes of padding behind it (`av_packet_new_side_data`,
//! `libavcodec/packet.c` 231–250), one entry a kind (197–227). The report
//! of what each container keeps is the side-data table above.
//!
//! | kind | read by, and what it assumes | here |
//! |---|---|---|
//! | HDR10+ | Matroska and WebM, a video block's: cast to `AVDynamicHDRPlus` without asking the size (`matroskaenc.c` 2947–2969), then serialized by a function that trusts every count, divides by every denominator, multiplies in `int` and asserts its size (`hdr_dynamic_metadata.c` 237–395); the reader fails the packet over a payload it cannot parse (`matroskadec.c` 4057–4077) | checked exactly — the linked libavutil's size, every count, divisor, product, bit width, unit and flag; the bytes the serializer declares are the bytes it writes, a byte past them leaving Matroska's stack buffer unwritten; and the reader, run over what the writer's exact bit schedule writes, reads back every field ([`mediaio_ffmpeg::hdr_metadata`]) — nothing stricter: the laws hold it to libavutil's own round trip; refused by name, the packet untouched ([`PayloadRule::DynamicHdrPlus`]) |
//! | SMPTE 2094 APP5 | Matroska and WebM, a video block's: cast to `AVDynamicHDRSmpte2094App5` (2970–2990), serialized into an 855-byte buffer by a function that bounds one image's curve count of four (`hdr_dynamic_metadata.c` 563–706) | checked exactly, each image's count among it ([`PayloadRule::DynamicHdrApp5`]) |
//! | LCEVC | Matroska and WebM, a video block's: its size, plus four bytes, copied (2991–3006) | bytes, any size |
//! | Matroska block additional | Matroska and WebM: read at a size of 8 or more (2936–2946) | bytes |
//! | skip samples | Matroska: at 10 or more (2922–2934); MP4: at 10 or more, where the codec fixes a frame's samples (`movenc.c` 7311–7320), refused there below nothing after the packet is taken | bytes; MP4's refusal made first ([`PayloadRule::SkipSamples`]) |
//! | WebVTT identifier, settings | Matroska and WebM, a WebVTT block's: copied by size, their sum with the cue's against `INT_MAX` (2828–2853) | bytes; the cue rule ([`PayloadRule::WebVttCue`]); the sum within the cluster's bound (table B) |
//! | new extradata | MP4: a sample description from the packet on, written by the codec's writer at the trailer (`movenc.c` 7044–7074, 7406–7420), AC-3's and E-AC-3's of none of its bytes (418–462, 612–666); Matroska: AAC's parsed and fitted to the space its header reserved, FLAC's of the track's length (3065–3097) | table C |
//! | producer reference time | MP4: copied only at its struct's exact size (7365–7369) | bytes |
//! | palette | MOV's raw video alone (7695–7700; `rawutils.c` 82–92, its size checked) — not MP4's | bytes |
//! | IAMF parameters | cast to `AVIAMFParamDefinition` (`iamf_writer.c` 1176–1184), reached only for a stream group's track (`movenc.c` 7594–7603); the muxing context adds no stream group | never reached |
//! | any other | read by no writer of these three containers | bytes |
//! | any, in another container | every reader in libavformat's byte-stream writers bounds its read by the entry's size, or reads inside its zeroed padding: `adtsenc.c` 182, `apngenc.c` 120, `flacenc.c` 291, `flvenc.c` 1256, `framecrcenc.c` 77–161 (each struct only at its exact size), `hashenc.c` 296–309, `latmenc.c` 186, `mp3enc.c` 366, `mpegtsenc.c` 1883 (one byte, inside the padding) and 2087, `nutenc.c` 866–915, `oggenc.c` 665–691, `rawutils.c` 82, `rtpenc.c` 629, `srtenc.c` 63, `webvttenc.c` 68–82; the muxers that write no byte stream of their own (DASH, HLS, segment, tee, FIFO, WHIP) are refused at open; HDR10+ and APP5 are read by none | forwarded as given |
//! | two entries of one kind | libavcodec keeps one: the later's bytes in the earlier's place (`packet.c` 197–227), the earlier dropped unsaid | refused in every container ([`PayloadRule::SideDataRepeated`]) |
//! | the stream's own, coded side data | the ticket's, mirrored from a demuxer's parameters ([`CodecTicket::mirror`] is its only constructor): libavformat's and libavcodec's structs at their own sizes; MP4 casts Dolby Vision's only under the strict compliance "unofficial" (`movenc.c` 2999–3006), which the context never sets; MP4's frame cropping is table C's | — |
//!
//! ## B. The bitstream filters, and the sizes libavformat counts
//!
//! **The filters**, found by the two mechanisms that insert one — every
//! `ff_stream_add_bitstream_filter` call in libavformat, by the function it
//! is in, and every muxer's `check_bitstream` (#82's R5 greps them, with
//! their output): in a muxer's `init` or header, whatever
//! `AVFMT_FLAG_AUTO_BSF` says; in its `check_bitstream`, at a track's first
//! packet and only with it set (`check_bitstream`, `mux.c` 1056–1073), one
//! filter a stream (1301). A filter's own refusals after it takes a packet
//! are silent: libavformat logs them and goes on (1132–1140). The muxers
//! that hand their packets on to others — DASH, segment, tee, WHIP, and
//! every other that writes no byte stream of its own — are refused at open;
//! the muxers nested in a byte stream's — MPEG-TS's ADTS and LATM for raw
//! AAC, RTP MPEG-TS's MPEG-TS, image2pipe's GIF, FITS and AVIF, MP4's TTML
//! writer — insert none this table does not name. Each filter is mirrored
//! by the muxer libavformat resolved ([`Family::of`] names it), and every
//! packet it puts out is sized before any it is made of is handed over.
//!
//! | filter | inserted by | when, for | what it puts out, and its arithmetic | here |
//! |---|---|---|---|---|
//! | `vp9_superframe` | IVF (`ivf_init`, `ivfenc.c` 42) | its start, whatever the option; VP9 | a superframe of the frames it holds — at most six hidden and the one shown — and its index of their sizes, summed in `unsigned` and allocated by the sum (`vp9_superframe.c` 34–98, 151–155): past 4 GiB the sum wraps and the copy runs past the allocation, below it `av_new_packet` refuses after taking the frames | mirrored with the filters on or off: the run counted wide, a hidden frame refused where no frame shown after it could join the run within the bound, a frame shown where its superframe passes it ([`PayloadRule::SuperframeSize`]); the filter's own refusals ([`PayloadRule::Superframe`]); reported ([`AutomaticFilter::Vp9Superframe`]) |
//! | `vp9_superframe` | Matroska, WebM, the MOV family — MOV, 3GP, MP4, PSP, 3G2, iPod, ISMV, F4V, every muxer whose `check_bitstream` is MOV's (`mkv_check_bitstream`, `matroskaenc.c` 3639; `mov_check_bitstream`, `movenc.c` 9147) | the first packet, `AUTO_BSF`; VP9 | as above | as above, with the filters on |
//! | `aac_adtstoasc` | Matroska's video muxer, the MOV family, FLV, LATM (`matroskaenc.c` 3634–3637; `movenc.c` 9144–9145; `flvenc.c` 1491–1493; `latmenc.c` 255–257) | the first packet, `AUTO_BSF`, where it opens with an ADTS header of more than two bytes; AAC | each frame less its header and CRC; the first converted, of channel configuration 0, less the program config element opening its payload too, consumed with no check that it fits, so that a packet the element runs past is put out at a negative size (`aac_adtstoasc.c` 54–96); raw packets passed on a configured track (51–52); its start refuses a configuration libavcodec does not parse (130–144) | MP4 and Matroska: ADTS frames alone, whole, on a track with no configuration (table F), named at open; on a configured one the first packet decides, the frames converted whole, and Matroska's configuration the filter makes within the space its header reserved ([`PayloadRule::NewExtradata`]); another container: the first packet decides; each filter named at its write, its refusals and its output's size held to ([`PayloadRule::AdtsFrame`]) |
//! | `pgs_frame_merge` | Matroska's video muxer (`matroskaenc.c` 3640–3642) | the first packet, `AUTO_BSF`; PGS | display sets — the segments it holds and the packet's, several from one packet — grown by `av_grow_packet`, which refuses past the bound after taking them; each put out at the packet that ends it, in the times of the packet that held its presentation segment; what it holds at the end, written marked corrupt (`pgs_frame_merge.c` 77–83, 93–157) | mirrored: each set counted, with room for its end segment, refused at the packet past which none fits ([`PayloadRule::DisplaySetSize`]); an open set held by this muxer, not FFmpeg's filter, and handed to libavformat whole at the packet that completes it, judged then in the times it is put out in; one no packet completes, handed back on asking ([`CarrierMuxer::take_held`]), else not written, reported ([`Uncarried::Held`]) |
//! | `pcm_rechunk` | DAUD (`daud_init`, `daudenc.c` 44), GXF (`gxf_write_header`, `gxfenc.c` 724), MXF, MXF D-10, MXF OP-Atom (`mxf_init`, `mxfenc.c` 3081) | the start; an audio track — of PCM alone, which libavcodec admits to it (`av_bsf_init`, `libavcodec/bsf.c` 154–170) | packets of a fixed count of samples (`pcm_rechunk.c` 123–186); its start multiplies the channels by the bits per sample in `int`, and refuses a size past `INT_MAX` over it (49–58) | the product held at open ([`MuxOpenError::SampleSize`]); its packets are its options'; named at open ([`AutomaticFilter::PcmRechunk`]) |
//! | `av1_metadata`, `td=insert` | IVF (`ivfenc.c` 46); OBU (`rawenc.c` 572) | IVF's start, whatever the option; OBU's first packet, `AUTO_BSF`; AV1 | the OBUs read and written back by CBS, each unit's write buffer at most `INT_MAX / 8` (`cbs.c` 342–372), the fragment summed in `size_t` (`cbs_av1.c` 1245–1265) and handed on as an `int` (`cbs.c` 472–474), which only shrinks it | nothing to hold; its refusals are the container's own; named at open ([`AutomaticFilter::Av1Metadata`]) |
//! | `eia608_to_smpte436m` | MCC (`mcc_init`, `mccenc.c` 434), MXF (`mxf_init`, `mxfenc.c` 3104) | the start; EIA-608 | one ANC packet of at most 255 bytes, of at most 80 triples (`eia608_to_smpte436m.c` 168–209, 227) | nothing to hold; named at open ([`AutomaticFilter::Eia608ToSmpte436m`]) |
//! | `extract_extradata` | FLV (`flvenc.c` 1501) | the first packet, `AUTO_BSF`, no configuration; H.264, HEVC, VVC, AV1, MPEG-4 | the packet less its parameter sets, each sum within the packet's or against `INT_MAX` (`extract_extradata.c` 102–116, 412–419) | nothing to hold; named at open, which its configuration decides ([`AutomaticFilter::ExtractExtradata`]) |
//! | `h264_mp4toannexb`, `hevc_mp4toannexb`, `vvc_mp4toannexb` | MPEG-TS (`mpegtsenc.c` 2331) and RTP MPEG-TS's MPEG-TS, nested in a context of its own whose option stays on (`rtpenc_mpegts.c` 67); MXF and MXF OP-Atom, not MXF D-10 (`mxfenc.c` 3636, 3707–3758); the raw H.264, VVC and HEVC muxers (`rawenc.c` 391, 416, 441) | the first packet, `AUTO_BSF` — the nested context's own — of five bytes or more opening on no start code, MPEG-TS's on a 3-byte one where the configuration is length-prefixed; H.264, HEVC, VVC — MXF's H.264 alone | start codes for lengths, parameter sets before an IDR, counted in 64 bits against `INT_MAX` (`h264_mp4toannexb.c` 291, 420–424; `hevc_mp4toannexb.c` 199–206; `vvc_mp4toannexb.c` 287–294) | nothing to hold; named at the write of the packet that inserts it ([`AutomaticFilter::H264Mp4ToAnnexB`], [`HevcMp4ToAnnexB`](AutomaticFilter::HevcMp4ToAnnexB), [`VvcMp4ToAnnexB`](AutomaticFilter::VvcMp4ToAnnexB)) |
//!
//! **What moves each mirror** (grep G20 of #82): every mirror this muxer
//! keeps of libavformat's timing and of the container's moves on what the
//! track's filter puts out now, in the times each packet put out carries
//! ([`Emits`]) — what `write_packets_from_bsfs` hands on from the filter to
//! `write_packet_common` (`mux.c` 1094–1149) — and never on a packet the
//! filter takes and holds.
//!
//! | mirror | moved by | why |
//! |---|---|---|
//! | libavformat's equal-decode-time rule ([`FfmpegMuxRefusal::RepeatedDts`]) | each packet put out, its decode time | `compute_muxer_pkt_fields` runs on each packet a filter puts out (`mux.c` 557–566, 1094–1149) |
//! | the move of early times and the interleaving's first write ([`Shift`]) | each packet put out, in its times | `handle_avoid_negative_ts` and the interleaving see only what the filter puts out (636–717, 939–1003) |
//! | MP4's `check_pkt` reference (the last sample written, table C″) | the last packet put out, or the joined E-AC-3 sample | `check_pkt` judges each packet MP4 is handed (`movenc.c` 6897–6930) |
//! | the latest decode time handed beside a `mov_text` cue ([`Neighbours`]) | each packet put out | the interleaving's (`mux.c` 939–1003) |
//! | the container's first packet of a track | the first put out | Matroska's start skip (`matroskaenc.c` 2089–2104); MP4's AC-3 configuration (`movenc.c` 424–429) |
//! | the rules of a packet's times — MP4's 32-bit fields, a missing presentation time, a negative duration, a time before the earliest | each packet put out, in its times | the container judges what it is handed |
//! | Matroska's block bound (table B) | each packet put out, and what the filter holds at the least it can come to | the cluster's |
//! | a packet's flags' and side data's fates (the side-data table) | the packet whose own they are, put out now or later | the filter carries or drops them (`vp9_superframe.c` 169; `pgs_frame_merge.c` 132–136) |
//! | the face's decode order | every packet offered, held or not | the face's own rule, not libavformat's |
//!
//! A filter that holds packets inside FFmpeg can lose none to a later
//! refusal: VP9's superframe takes the times of the frame that completes it
//! (`vp9_superframe.c` 163–175), so no rule judged then depends on a held
//! frame's, each held frame's size is judged at its own packet against the
//! least a completion comes to, and the filter drops, never writes, what it
//! holds at the end (198–207, reported); MP4's E-AC-3 join is MP4's own,
//! judged at the run's first packet against a reference no other packet
//! moves (table C″), and freed at the trailer, reported at open. PGS's
//! filter writes what it holds at the end marked corrupt, in the times of a
//! packet a later refusal may leave before the earliest: so this muxer
//! holds PGS's open sets itself.
//!
//! **The sizes libavformat counts**, every size the packet path computes in
//! 32 bits, with the bound held here — libavcodec's largest packet
//! ([`PACKET_BYTES_MAX`], `INT_MAX` less the padding, less one) unless
//! another is named. Each bound is counted in `u64` before anything is
//! handed over.
//!
//! | what | where, and its arithmetic | here |
//! |---|---|---|
//! | the Annex B rewrite | MP4 (H.264, HEVC, VVC, LCEVC) and Matroska and WebM (H.264, HEVC, VVC) over an Annex B configuration: each start code becomes a 4-byte length, the size summed in an `int` (`nal_parse_units`, `nal.c` 74–111); MP4's HEVC under `hvc1` through a dynamic buffer of at most `INT_MAX` (`aviobuf.c` 1281–1311) | its upper bound — the packet and a byte for each start code — held to the bound ([`PayloadRule::RewrittenSize`]) |
//! | Matroska's cluster | written into a dynamic buffer of at most `INT_MAX`, refused past it and the cluster written cut; a new one once the last passes 5 MiB, two blocks at most written after that check (`matroskaenc.c` 2746–2749, 3213–3271) | each block — the packet as written, or each one its filter puts out, VP9's superframe and PGS's display set with the side data they take, and what the filter holds at the least it can come to — its payload and side data at most half of what is left beside 5 MiB and 64 KiB, less its own elements (`MATROSKA_BLOCK_BYTES_MAX`, [`PayloadRule::ClusterSize`]) |
//! | WebVTT's lines | Matroska and WebM: the cue, its identifier and its settings summed, refused past `INT_MAX` (`matroskaenc.c` 2828–2853) | within the cluster's bound |
//! | MP4's joined TTML | libavcodec's TTML encoder's paragraphs, joined at the trailer into one document in a dynamic buffer and then one packet (`movenc_ttml.c` 236–317) | the document's bytes counted — each paragraph and its element, and the header its configuration and language make — and refused at the paragraph that would pass the bound ([`PayloadRule::DocumentSize`]) |
//! | the payload and side data copied in | the copy road's builder refuses a packet marked trusted, side data past 64 entries or 256 KiB, a payload past the muxer's budget ([`PacketLimits`]), and a side-data kind libavcodec does not name — the base's one judgment of what a packet alone decides (`mediaio_ffmpeg::boundary::check_packet_build`) — and `av_new_packet` and `av_packet_new_side_data` refuse past the bound (`libavcodec/packet.c` 83–95, 231–250) | each refused by name with the packet in hand ([`PayloadRule::Trusted`], [`PayloadRule::SideDataBudget`], [`PayloadRule::PayloadTooLarge`], [`PayloadRule::SideDataUnnamed`]); MP4's cover, built after the header, at open ([`MuxOpenError::Cover`]); what is left — an allocation's failure — a fault ([`OutputError::Build`]) |
//!
//! ## C. Each container's checks of each codec
//!
//! Every check MP4's and Matroska's writers make of a track's packets or
//! configuration, by where it is made. A check in a header is made before
//! any packet is taken: where this muxer does not refuse its input first,
//! it is libavformat's fault at open ([`MuxOpenError::Fault`] at
//! [`MuxStage::Header`]), and nothing of the caller's packets is lost. A
//! check on the packet path, or in a trailer — MP4 writes its `moov`
//! there, every codec's configuration box with it — is refused here
//! first, by name; where it needs the whole stream, at the first packet
//! that decides it. A configuration is held to the shape the writer
//! checks; whether it parses as its codec's — an `avcC`, an `hvcC`, a
//! sequence header — is the source's, a demuxer's own, which this muxer
//! does not judge.
//!
//! **MP4, the packet path** (`movenc.c`):
//!
//! | check | source | here |
//! |---|---|---|
//! | a duration or a decode gap past `i32::MAX` ticks — the gap from the last sample MP4 wrote on the track, none before the first; a subtitle's negative duration, which libavformat hands on as it is — another track's it writes as zero, or its own guess (`mux.c` 605–633), reported ([`Rewrite::Durations`]); a composition offset past `i32` | `check_pkt` 6897–6928; 6955–6959 | [`FfmpegMuxRefusal::DurationTooLong`], [`DtsGap`](FfmpegMuxRefusal::DtsGap), [`NegativeDuration`](FfmpegMuxRefusal::NegativeDuration), [`PresentationOffset`](FfmpegMuxRefusal::PresentationOffset); the last sample written, table C″ |
//! | AMR-NB: one frame a packet; ADPCM MS and IMA WAV: a frame size | 6983–7002 | not reached: MP4's tag table holds neither (`codec_mp4_tags`, 9230–9290), refused at open ([`MuxOpenError::CodecNotCarried`]); MOV's other modes reach it |
//! | no samples: less than a frame of a sized codec | 7006–7010 | [`PayloadRule::PartialSample`], [`PayloadRule::Empty`] |
//! | the first packet taken as the configuration of H.264, HEVC, VVC, EVC, LCEVC, VP9, TrueHD or DNxHD where none is stated | 7024–7041 | H.264, HEVC, VVC, LCEVC and EVC refused at open ([`MuxOpenError::ConfigurationMissing`]); TrueHD refused at open; VP9's and DNxHD's boxes made from it, written |
//! | new extradata: a sample description from the packet on, each written by its codec's writer at the trailer — AC-3's and E-AC-3's, `dac3` and `dec3`, from MP4's parse of the frames, of none of its bytes; MPEG-4 ALS, AAC, AV1 and FLAC's overwriting a configuration stated as none | 7044–7074, 7406–7420; 418–462, 612–666 | AC-3's and E-AC-3's reported dropped (the side-data table); FLAC's of 34 bytes ([`PayloadRule::FlacStreamInfo`]); Opus's an OpusHead `dOps` takes, H.264's, HEVC's, VVC's and LCEVC's of the track's form — Annex B or not — as the rewrite reads it ([`PayloadRule::NewExtradata`]); a configuration stated as none refused at open, so none is overwritten |
//! | AAC framed in ADTS | 7076–7086 | [`PayloadRule::AdtsHeader`], [`PayloadRule::AdtsFrame`] |
//! | the Annex B rewrite | 7087–7158 | reported ([`Rewrite::LengthPrefixed`]); its size, table B |
//! | AV1's OBUs | 7159–7174 | [`PayloadRule::Obus`]; reported ([`Rewrite::ObusFiltered`]) |
//! | AC-3 and E-AC-3: a frame header that parses, dependent frames that tile a first packet, blocks joined into samples of six | 7175–7181; `handle_eac3` 464–610 | [`PayloadRule::Ac3Header`], [`Ac3Substreams`](PayloadRule::Ac3Substreams) (at most 15, `dec3`'s four bits), [`Ac3Blocks`](PayloadRule::Ac3Blocks), [`Ac3SampleSize`](PayloadRule::Ac3SampleSize); the joined samples, table C″; reported ([`Rewrite::Ac3Parsed`]) |
//! | EIA-608's pairs into `cdat` | 7182–7197 | not reached: not in MP4's tag table |
//! | APV's packets read for `apvC` | 7198–7203 | written as given |
//! | skip samples that leave less than nothing | 7311–7320 | [`PayloadRule::SkipSamples`] — a joined E-AC-3 sample's at its last packet, table C″ |
//! | VC-1's and TrueHD's sync read from the bitstream | 7344–7348 | reported ([`Rewrite::SyncFromBitstream`]) |
//! | an empty packet, dropped | 7431–7441 | [`PayloadRule::Empty`] |
//! | a cover's one picture, a second ignored | 7606–7618 | the cover road writes one |
//! | `mov_text`'s cue ends | 7625–7656 | [`FfmpegMuxRefusal::CueOrder`]; the samples MP4 makes around the cues, tables C′ and C″; reported ([`Rewrite::CueEnds`]) |
//! | libavcodec's TTML paragraphs, held for one document | 7658–7684 | the one sample MP4 joins them into, table C′; its size, table B; reported ([`Rewrite::ParagraphsJoined`]) |
//! | IAMF's packets; MOV's raw video and palette | 7594–7603; 7688–7720 | not reached: no stream group; MOV's mode alone |
//!
//! **MP4, the trailer** — the `moov`, written at the end (`mov_write_trailer`, 9013–9140):
//!
//! | check | source | here |
//! |---|---|---|
//! | `dac3`: a frame parsed; a bit stream id of 8 or less; a bit rate code | `mov_write_ac3_tag` 424–445 | a track that took no frame refused at `finish` ([`FfmpegMuxRefusal::NoFrame`]); a first frame past 8 refused there ([`PayloadRule::Ac3BitStreamId`]); every AC-3 frame's code is its frame size code's (`ac3_parser.c` 324) |
//! | `dec3`: a frame parsed | 619–623 | [`FfmpegMuxRefusal::NoFrame`] |
//! | `dfLa`: one 34-byte STREAMINFO | 857–876 | none: [`MuxOpenError::ConfigurationMissing`]; another: [`MuxOpenError::ConfigurationInvalid`]; new extradata: [`PayloadRule::FlacStreamInfo`] |
//! | `dOps`: an OpusHead, its mapping table where its family names one | 877–910 | [`MuxOpenError::ConfigurationMissing`], [`ConfigurationInvalid`](MuxOpenError::ConfigurationInvalid), [`PayloadRule::NewExtradata`] |
//! | `dmlp`, TrueHD | 911–939 | refused at open: MP4 writes TrueHD only as an experiment ([`MuxOpenError::CodecNotCarried`]) |
//! | `chnl`, PCM's layout | 1299–1340 | [`MuxOpenError::ChannelLayout`] |
//! | `SA3D` | 940–997 | not reached: read from stream metadata the context never sets |
//! | the video configuration boxes — `avcC`, `hvcC`, `lhvC`, `vvcC`, `evcC`, `lvcC`, `apvC`, `vpcC`, `av1C`, `dvc1` | 2880–2930 | their writers' failures are ignored, the box left as far as it was written: AV1's absence refused at open ([`MuxOpenError::ConfigurationMissing`]), ALAC's and MPEG-4 ALS's likewise (672–676, 765–816); the rest is the configuration's own |
//! | frame cropping that leaves no picture | 3024–3036 | [`MuxOpenError::Cropping`] |
//! | TTML's codec tag | 2259–2280 | not reached: the header refuses `dfxp` outside ISMV first (8603–8609), a fault at open |
//! | libavcodec's TTML joined into one document: its header strings, its size | `movenc_ttml.c` 236–317; `ttmlenc.c` 82–122 | [`MuxOpenError::ConfigurationInvalid`]; [`PayloadRule::DocumentSize`] |
//! | the cover: an empty picture skipped | 4823 | table E |
//! | the chapters: a text track and a Nero list | 7727–7815; 5040–5064 | the track's samples, table C′; the report, table E |
//!
//! **MP4, the packets it makes itself** (table C′) — every place MP4's
//! writer makes or hands on a packet of its own, by grep (#82's R5 lists
//! each `ff_mov_write_packet`, `mov_write_single_packet` and
//! `mov_write_subtitle_end_packet` call, every `av_new_packet`,
//! `av_packet_alloc` and `av_packet_ref` in `movenc.c` and `movenc_ttml.c`,
//! by the function it is in). Each meets `check_pkt` as a caller's packet
//! does (6897–6930), and is held here to it, or reported:
//!
//! | packet | made by, and when | its times, and the rules it meets | here |
//! |---|---|---|---|
//! | a chapter's sample | `mov_create_chapter_track`, at the header (7727–7815, from 8851–8853) | each titled chapter's start and end counted in the movie timescale — `av_gcd_q` of the streams' time bases, at least 1000 (8427–8446) — its duration their distance; the first counted from zero, the track's start being 0 (8448–8474), each later one from the last; `ff_mov_write_packet`'s answer ignored (7807): a duration past `INT_MAX` loses the sample, a start before the last or `INT_MAX` or more after it is moved | refused at open ([`MuxOpenError::ChapterSample`]), the movie timescale mirrored |
//! | an empty `mov_text` sample | `mov_write_subtitle_end_packet` (7479–7497), from `mov_write_packet` before a packet past the track's end (7646–7653) — at zero before the first cue — and from the trailer after the last (9025–9031) | `track_duration`, the last cue's end counted from the track's first sample, its decode time; `check_pkt` counts its gap from that cue's decode time, and the next cue's from it | table C″; reported ([`Rewrite::CueEnds`]) |
//! | TTML's joined sample | `mov_write_squashed_packet` at the trailer (6579–6615, from 9036–9037), its bytes `ff_mov_generate_squashed_ttml_packet`'s document (`movenc_ttml.c` 236–317) | from zero to the latest paragraph's end (108–110, 178, 212–213, 305–306); no paragraph meets `check_pkt` itself; refused past `INT_MAX` ticks, failing the trailer and the file | a paragraph ending past `INT_MAX` refused ([`FfmpegMuxRefusal::DocumentSpan`]); its bytes, table B; reported ([`Rewrite::ParagraphsJoined`]) |
//! | a timecode track's sample | `mov_create_timecode_track` (7827–7870), at the header, for a video stream with `timecode` metadata, or the context's (8339–8369, 8865–8885) | — | never made: the context and its streams carry no `timecode` ([`Output`] writes a stream's language, filename and MIME type alone) |
//! | E-AC-3's joined samples | `handle_eac3` (577–606) | table C″ | table C″ |
//! | the cover's picture | held by `mov_write_packet` (7606–7618), written by `mov_write_covr` at the trailer (4815–4839) | — | the cover road writes one |
//! | an RTP hint packet | `ff_mov_add_hinted_packet` (`movenchint.c` 405–429), with `-movflags rtphint` alone | — | never made: no flag is set |
//!
//! Matroska makes no packet of its own.
//!
//! **MP4 and Matroska, what they write of what they take** (table C″) —
//! every road on which a writer takes a packet without writing it at once
//! as a sample of its own, by grep (#82's R6 lists every `goto end`,
//! `return 0`, `av_packet_ref`, `av_grow_packet`, `av_packet_move_ref`,
//! `ff_packet_list_put` and `mov_write_subtitle_end_packet` on MP4's packet
//! path, and every `cur_audio_pkt` on Matroska's, by the function it is
//! in). The writer's own checks run against what it has written:
//! `check_pkt` counts a packet's gap from the last sample MP4 wrote on the
//! track, none before the first (6897–6930), and that sample moves only
//! where MP4 writes one. Each check here is made against the same, and
//! moves only there.
//!
//! | road | the mechanism | what is written | here | law |
//! |---|---|---|---|---|
//! | E-AC-3's frames of fewer than six blocks | `handle_eac3` (577–606): held in `info->pkt`, joined by `av_grow_packet` until six, then written as one sample; nothing written before | one sample: the first packet's decode and presentation times, flags and side data (588); the bytes joined (593–595); the durations summed (597); the last packet's own duration for its skip samples and the edit list's end (6940, 7311–7342) | each packet's gap counted from the last sample written, none before the first ([`FfmpegMuxRefusal::DtsGap`]); libavformat's own rules on every packet as taken, which meet it before the hold ([`FfmpegMuxRefusal::RepeatedDts`]); the joined bytes within `av_grow_packet`'s bound ([`PayloadRule::Ac3SampleSize`]); the run's skip samples judged at its last packet, against that packet's duration, a held packet's never ([`PayloadRule::SkipSamples`]) — where a frame size is stated, which no demuxer's AC-3 or E-AC-3 ticket states (`demux.c` 2076–2087); a joined packet's side data reported dropped, and its new extradata, which MP4 reads before holding it (7044–7074) into a sample entry made from its parse (612–666); the durations counted between the samples written ([`Rewrite::Durations`]) | L57, L58; the unit laws |
//! | the last sample, joined | the trailer reads its duration from the sum and asserts it within `INT_MAX` (`get_cluster_duration`, 1248–1256: `av_assert0` ends the process), and ends the edit list at its last packet's duration (7329–7342) | — | refused at `finish` past `INT_MAX` ([`FfmpegMuxRefusal::LastSampleTooLong`]); its end reported at `finish` ([`Rewrite::Durations`]) | L59, L60 |
//! | E-AC-3's blocks short of six at the end | `mov_free` frees the run (7951–7955) | dropped | reported at open ([`Rewrite::Ac3Parsed`]) | L61 |
//! | an AC-3 frame that does not parse | `handle_eac3` (481–493): dropped before the track's first sample, refused after | — | refused ([`PayloadRule::Ac3Header`]) | L25 |
//! | `mov_text`'s empty samples | `mov_write_packet` (7646–7653): before a packet of any track whose decode time, in its own ticks, passes the cue track's `track_duration`, where the track's last sample is a cue or it has none; the trailer's after the last (9025–9031) | an empty sample at `track_duration` — the last cue's end counted from the track's first sample, read as a decode time — the one at zero first; none before a cue at or before it, so that the last cue is shortened to it ([`Cues::take`]) | each cue's gap counted from the sample before it, the empty one where MP4 writes it first ([`FfmpegMuxRefusal::DtsGap`]); a cue's duration within `INT_MAX` of its empty sample's ([`FfmpegMuxRefusal::DurationTooLong`]); a cue before the end MP4 marks taken as MP4 writes it, the last cue's shortening reported ([`Rewrite::Durations`]) — but where another track's packet can make MP4 mark the end first, one counted in finer ticks or one handed more than libavformat's 10 s of interleaving past the end; and a first cue before zero, which MP4's reader drops, counting the samples after it from the edit list's start (`mov_fix_index`, `mov.c` 4374–4682): refused ([`FfmpegMuxRefusal::CueOrder`]) | L15, L53, L65, L66; the unit law |
//! | TTML's paragraphs | `mov_write_packet` (7658–7684): queued by `ff_packet_list_put`, joined at the trailer | table C′ | table C′ | L54 |
//! | the cover's picture | `mov_write_packet` (7606–7619): held by `av_packet_ref`, a second ignored | its one picture, at the trailer (4815–4839) | the cover road writes one | L10 |
//! | an empty packet | `mov_write_single_packet` (7431–7441) | dropped | refused ([`PayloadRule::Empty`]) | L17 |
//! | a timed ID3 packet | `mov_write_packet` (7586–7589) | an `emsg` box, no sample | not reached: MP4's tag tables hold no timed ID3, refused at open ([`MuxOpenError::CodecNotCarried`]) | — |
//! | IAMF's packets; a fragment's | `mov_build_iamf_packet` (7594–7603); `mov_flush_fragment` | — | not reached: no stream group, no fragmenting flag | — |
//! | Matroska's audio packet | `mkv_write_packet` (3254–3271): held by `av_packet_ref` until the next packet, or the trailer (3300–3306) | the packet as taken, one packet later, its cluster's checks made then (3122–3200) | the rules its own write met, its block's bound counting the block before it (table B) | L1, L49 |
//! | libavformat's interleaving | `ff_interleave_packet_per_dts` (`mux.c` 940–1003): every packet queued, written in decode-time order across the streams, each stream's own in its order | every packet as taken | a track's rules are its own; across tracks, MP4's `mov_text` empty samples, above | L66 |
//! | the automatic filters | table B | table B | table B | table B's |
//!
//! **MP4, the header** (`mov_init`, 8129–8700; `mov_write_header`): refused
//! by name first — TrueHD's experiment (8580–8587); libavformat's own
//! faults at open otherwise.
//!
//! **Matroska and WebM, the header** (`mkv_init`, 3513–3560;
//! `mkv_write_header`, 2662 on, each codec's private data assembled,
//! 1150–1300): refused by name first — what Matroska does not yet write
//! (3518–3530, [`MuxOpenError::CodecNotCarried`]), an attachment without a
//! filename or a MIME type (2538–2543, 3606–3613), a chapter's span
//! (2445–2451, [`MuxOpenError::ChapterSpan`]); a configuration its private
//! data's parser refuses, frame cropping its track entry refuses
//! (1857–1870), is libavformat's fault at open.
//!
//! **Matroska and WebM, the packet path** (`matroskaenc.c`):
//!
//! | check | source | here |
//! |---|---|---|
//! | new extradata: AAC's an AudioSpecificConfig libavcodec parses, within the space reserved for it — the configuration's length, or `MAX_PCE_SIZE` where there was none | `mkv_check_new_extra_data` 3065–3082; `mkv_update_codecprivate` 1334–1365 | [`PayloadRule::NewExtradata`] — on the first frame AAC's filter converts, the configuration the filter makes in its place, held to the same space |
//! | new extradata: FLAC's of the configuration's length | 3084–3097 | [`PayloadRule::FlacStreamInfo`] |
//! | AV1 or AAC with no configuration: every packet refused | 3080–3083, 3098–3109 | refused at open ([`MuxOpenError::ConfigurationMissing`]), AAC's only with the filters off |
//! | a packet with no timestamp | 3140–3143 | a reordering stream's refused ([`FfmpegMuxRefusal::MissingPts`]); libavformat fills it otherwise |
//! | a time before the earliest written, which libavformat's first write moves every stream's times past (`mux.c` 636–717) | 2089–2104, 3538–3541 | mirrored, reported ([`Uncarried::TimesShifted`]); a packet after that write the move leaves before it refused ([`FfmpegMuxRefusal::BeforeZero`]) — every one where a video track of no native id makes the times judged uncertain (2025–2081) |
//! | the Annex B rewrite | 3574–3582 | reported ([`Rewrite::LengthPrefixed`]); its size, table B |
//! | WavPack's blocks | 2775–2816 | [`PayloadRule::WavPackBlocks`]; reported ([`Rewrite::WavPackHeaders`]) |
//! | ProRes's atom | 3584–3587 | [`PayloadRule::ProResAtom`] |
//! | AV1's OBUs | 3589–3590 | [`PayloadRule::Obus`] |
//! | WebVTT's lines | 2828–2853 | [`PayloadRule::WebVttCue`]; their size within the cluster's |
//! | an empty audio packet, dropped | 3262–3268 | [`PayloadRule::Empty`] |
//! | a subtitle's negative duration, written as zero | 3134 | reported ([`Rewrite::Durations`]) |
//! | the block's side data | 2864–3010 | table A |
//! | the cluster's buffer | 2746–2749, 3213–3271 | table B |
//!
//! **Matroska and WebM, the trailer** (3292 on): the audio packet held back
//! is written (3300–3306) under the rules its own write met; the cues, the
//! seek head and the reserved track entries are patched in place — a seek
//! the sink fails is named ([`OutputError::SinkSeek`]); nothing of the
//! caller's is judged there.
//!
//! **Another container:** its checks are its own, and libavformat's fault
//! where it refuses after taking a packet; what is claimed of it is
//! memory's — tables A and B.
//!
//! ## D. A track's kind and its media type
//!
//! Judged before any stream is added ([`MuxOpenError::MediaTypeMismatch`]):
//! a timed track's stream is made of its ticket's media type, and its
//! packets are judged by the track's kind, so the two must agree, and the
//! codec's own descriptor with them (`avcodec_get_type`).
//!
//! | kind | the ticket's media type | the codec's descriptor |
//! |---|---|---|
//! | video | video | video, or none in this build |
//! | audio | audio | audio, or none |
//! | subtitle | subtitle | subtitle, or none |
//! | data | data | data, or none |
//! | attachment | any: its road makes its stream's media type itself — an attachment stream, or a cover's video stream ([`StreamMedia::Attachment`], [`StreamMedia::Cover`]); libavformat's demuxers deliver a font as an attachment stream and cover art as a video one (`ff_add_attached_pic`, `demux_utils.c` 107–140) | any: the codec names the file |
//! | unknown | refused by its own name ([`MuxOpenError::UnknownTrackKind`]) | — |
//!
//! ## E. The report: what a container adds, reinterprets or drops
//!
//! What reads back otherwise than the table and the packets state, beyond
//! the report table, the packet-rule table's reported rows and the
//! side-data table above — each from the writer's source and its reader's.
//!
//! | what | where | here |
//! |---|---|---|
//! | the default flag MP4 adds | MP4 enables the first video, audio and subtitle track of each type where none of the type declares the default flag, a cover excepted (`enable_tracks`, `movenc.c` 7883–7923); its reader reads the enabled flag as the default | reported at open ([`Uncarried::DispositionAdded`]) |
//! | the attached picture a cover reads back as | MP4's cover, read back as an attached picture (`mov_write_covr`, 4815–4839); a Matroska attachment of a GIF, JPEG, PNG or TIFF MIME type, which its reader turns into one (`matroskadec.c` 817–824, 3509–3533) | the flag kept where declared, reported where added ([`Uncarried::DispositionAdded`]) |
//! | MP4's paired roles | one DASH role, `caption`, for the hearing impaired or captions, and one, `description`, for the visual impaired or descriptions — written for either flag, read back as both (`mov_write_track_kinds`, `movenc.c` 4338–4356; `isom.c` 450–462; `mov.c` 8994–9004) | the partner reported where added ([`Uncarried::DispositionAdded`]) |
//! | Matroska's default flag | written as declared: its `default_mode` is `passthrough` (`matroskaenc.c` 3684) | — |
//! | an attachment of no bytes | Matroska's reader drops it as incomplete (`matroskadec.c` 3510–3512); MP4 skips the cover (`movenc.c` 4823) | not written, and reported at open ([`Uncarried::Attachment`]) |
//! | a chapter's id | MP4 keeps none: its reader numbers chapters by position (`mov.c`'s `chpl` and chapter track); Matroska renumbers every chapter from 1 where one's id is 0 or repeats (`matroskaenc.c` 2399–2442) | reported at open ([`Uncarried::ChapterId`]) |
//! | a chapter's span | MP4 counts it in its movie timescale (table C′) and its reader ends each chapter where the next starts (`movenc.c` 7727–7815); Matroska counts it in nanoseconds (2439–2440), each rounded to the nearest | reported at open where an instant moves ([`Uncarried::ChapterSpan`]) |
//! | a chapter without a title, a title past 65535 bytes | MP4 writes every chapter into its Nero list but a titled one alone into its chapter track, which its reader lays over the list by position; a title with a 16-bit length (`movenc.c` 7791–7807) | refused at open ([`MuxOpenError::ChapterWithoutTitle`], [`ChapterTitleTooLong`](MuxOpenError::ChapterTitleTooLong)) |
//! | VP9's hidden frames, still held at the stream's end | dropped by the filter, which no later frame shows (`vp9_superframe.c` 198–207) | reported at `finish` ([`Uncarried::Held`], with the report [`CarrierMuxer::finish_into_sink`] hands back): the muxer cannot know at a held frame that none will follow, and does not refuse the file for it |
//! | PGS segments, still held at the stream's end | written as one packet marked corrupt (`pgs_frame_merge.c` 77–83) | held by this muxer, not FFmpeg's filter: handed back on asking ([`CarrierMuxer::take_held`]), else not written, reported at `finish` ([`Uncarried::Held`]) |
//! | MP4's durations, where packets do not abut | kept only as the distance to the next decode time (`movenc.c` 1241–1259), counted between the samples MP4 writes (table C″) | reported as learned ([`Rewrite::Durations`]) |
//! | an E-AC-3 track's last sample, joined | the edit list ends it at its last packet's duration (`movenc.c` 6940, 7329–7342), short of the frames it joins | reported at `finish` ([`Rewrite::Durations`]) |
//!
//! The report covers these fields and those of the tables above; a codec's
//! parameters beyond its configuration — its color properties, its field
//! order, its bit rate — are each container's track entry's to write, and
//! are not claimed of.
//!
//! ## F. AAC's program config element
//!
//! MP4's and Matroska's AAC the automatic filter converts — with no
//! configuration, or a configured track whose first packet is ADTS:
//! `aac_adtstoasc` makes the track's configuration from its first frame, and where that frame's channel configuration is 0, from the
//! program config element opening its payload — which it consumes with
//! no check that it fits (`aac_adtstoasc.c` 80–95; `mpeg4audio_copy_pce.h`
//! 38–67). Its reader reads zeros past the packet, inside the padding
//! every packet carries, so nothing is read out of bounds; what it leaves
//! is the packet's size less the span it read.
//!
//! | what the filter reads | what it assumes | here ([`PayloadRule::AdtsFrame`]) |
//! |---|---|---|
//! | the element id, 3 bits | 5, a program config element | refused otherwise |
//! | the element — its tag, object type and sampling index, the front, side, back, LFE, data and coupling counts, the three mixdowns, 5 bits for each front, side, back and coupling element and 4 for each LFE and data one — then the byte alignment, the comment's size and its bytes | that the span fits the payload: past it, the packet is left a negative size, written so | the span bound-parsed as the filter reads it ([`pce_span`]), refused where it runs past the payload |
//! | the raw data after it | that some is left: a span that ends the payload leaves an empty packet | refused where nothing is left |
//!
//! ## G. Each refusal, against what FFmpeg does without it
//!
//! A refusal stands only where FFmpeg 9.0.1, handed the packet or the
//! track, would refuse it after taking it, lose it, corrupt it, end the
//! process, or change it in a way the report has no name for; a change the
//! report names is written and reported. Every refusal rounds 3 to 6 of
//! #82 added or changed, and round 2's, judged again, with its cause:
//!
//! | refusal | without it, FFmpeg | so |
//! |---|---|---|
//! | [`PayloadRule::AdtsFrame`] | `aac_adtstoasc` refuses a frame it cannot parse after taking it, and libavformat drops it unsaid (`mux.c` 1132–1140); of a packet longer than its frame, it strips the first header alone; a program config element past the payload leaves a negative size (`aac_adtstoasc.c` 39–128) | loses, or corrupts |
//! | [`PayloadRule::AdtsHeader`], on a configured MP4 or Matroska track | where no filter converts it: MP4 refuses the first after taking it and writes the rest with their headers inside (`movenc.c` 7076–7086); Matroska writes them into a raw track. An ADTS first packet inserts the filter, which converts it, taken and named (round 6) | refuses after taking, or corrupts |
//! | [`PayloadRule::Superframe`] | `vp9_superframe` refuses after taking, dropped unsaid; a seventh hidden frame refuses every packet after it, and the seven are dropped at the end (`vp9_superframe.c` 111–161, 198–207) | loses |
//! | [`PayloadRule::PgsSegments`] | `pgs_frame_merge` writes a set without its presentation segment, or of segments it cannot parse, marked corrupt (`pgs_frame_merge.c` 59–68, 93–130) | corrupts |
//! | [`PayloadRule::Empty`], a PGS packet's | `pgs_frame_merge` drops an empty packet unsaid, and with it the time it marks (`pgs_frame_merge.c` 87–90) | loses |
//! | [`MuxOpenError::ConfigurationMissing`], AAC's with the filters off | MP4 refuses an ADTS first packet after taking it (`movenc.c` 7076–7086) and writes raw AAC with no configuration to read it by; Matroska refuses every packet after taking it (`matroskaenc.c` 3098–3109) | refuses after taking, or loses |
//! | [`PayloadRule::SuperframeSize`], [`PayloadRule::DisplaySetSize`], [`PayloadRule::Ac3SampleSize`], [`PayloadRule::RewrittenSize`], [`PayloadRule::ClusterSize`], [`PayloadRule::DocumentSize`], [`PayloadRule::PayloadTooLarge`] | a size past what libavcodec allocates or a buffer holds: the sum wraps and the copy runs past its allocation (`vp9_superframe.c` 52–98), `av_new_packet` or `av_grow_packet` refuses after taking the packets (`packet.c` 83–127), the rewriter counts in an `int` (`nal.c` 74–111), the cluster is written cut (`matroskaenc.c` 2746–2749), the trailer fails (`movenc_ttml.c` 236–317) | memory, loses, or refuses after taking |
//! | [`PayloadRule::DynamicHdrPlus`], [`PayloadRule::DynamicHdrApp5`] | Matroska casts the entry and serializes it trusting every count and divisor (`matroskaenc.c` 2947–2990; `hdr_dynamic_metadata.c` 237–706) | memory |
//! | [`PayloadRule::NewExtradata`] | MP4's trailer refuses an Opus configuration `dOps` cannot write (`movenc.c` 877–910); an H.264, HEVC, VVC or LCEVC one of the other form turns the length-prefix rewrite on or off for the packets after it (7087–7158); Matroska refuses an AAC one it cannot parse or fit after taking the packet — the caller's, or the one AAC's filter makes (`matroskaenc.c` 3065–3082, 1334–1365) | refuses after taking, or corrupts |
//! | [`PayloadRule::SideDataRepeated`] | libavcodec keeps one entry a kind, the earlier one's bytes replaced (`packet.c` 197–227): the copy road cannot carry both | loses |
//! | [`PayloadRule::Trusted`], [`PayloadRule::SideDataBudget`], [`PayloadRule::SideDataUnnamed`] | the base's packet builder refuses them (`mediaio_ffmpeg::boundary::check_packet_build`): the write would fail with the packet taken | refuses after taking |
//! | [`PayloadRule::Ac3BitStreamId`], [`FfmpegMuxRefusal::NoFrame`] | MP4's trailer fails without `dac3`'s or `dec3`'s frame, or over a bit stream id past 8 (`movenc.c` 424–445, 619–623) | refuses after taking: the file |
//! | [`FfmpegMuxRefusal::DocumentSpan`] | the joined TTML sample past `INT_MAX` fails `check_pkt` at the trailer (`movenc.c` 6925–6928, 9036–9037) | refuses after taking: the file |
//! | [`FfmpegMuxRefusal::LastSampleTooLong`] | the trailer's `av_assert0` on the last joined E-AC-3 sample's duration (`movenc.c` 1248–1256) | ends the process (round 6) |
//! | [`FfmpegMuxRefusal::DtsGap`], as rounds 5 and 6 count it | `check_pkt` moves the packet one tick past the last sample written and drops its presentation time (`movenc.c` 6897–6923) — a `mov_text` cue's from the empty sample before it, an E-AC-3 packet's from the last sample, not a held packet | corrupts |
//! | [`FfmpegMuxRefusal::DurationTooLong`], a `mov_text` cue's | `check_pkt` moves the empty sample marking its end where that lies `INT_MAX` or more after it, its duration and the track's start before zero, so that the cue shows one tick (`movenc.c` 6917–6923, 7321, 7652): at the next cue past it, or at the trailer, which the cue alone decides | loses |
//! | [`FfmpegMuxRefusal::CueOrder`] | an overlapping or replacing cue is written as it is, the last cue shortened (`movenc.c` 7646–7653) — taken and reported (round 6) — unless another track's packet makes MP4 mark the end first and then move the cue (6917–6923), as the `ffmpeg` CLI does beside video: "Packet duration: -1000 / dts: 2000 in stream 1 is out of range". A first cue before zero MP4's writer takes, and its reader drops: its edit list starts the track at zero (4133–4239), and `mov_fix_index` marks a sample before an edit's start discarded and counts every later one from it (`mov.c` 4374–4682) — `ffprobe` reads the CLI's cues at −30 and 20 (`-itsoffset -0.03`) back at −40, marked discarded, and 10. Judged by the round trip, not the writer alone (ruled) | corrupts, or loses, where it is refused |
//! | [`FfmpegMuxRefusal::NegativeDuration`], a subtitle's outside Matroska and WebM | libavformat hands a subtitle's on as it is (`guess_pkt_duration`, `mux.c` 605–611): MP4 refuses it after taking the packet (`check_pkt`, `movenc.c` 6925–6928), or joins a TTML paragraph into its document ending before it starts (`ttmlenc.c` 172); SRT drops the cue (`srtenc.c` 71–75); WebVTT and ASS write it ending before it starts (`webvttenc.c` 80, `assenc.c` 160). Any other track's libavformat writes as zero, or its own guess (605–633), and Matroska and WebM a subtitle's as zero (`matroskaenc.c` 3134): taken, reported ([`Rewrite::Durations`]) — L11; PLANT: refused on every track, L11's video packet refused; on none, MP4 refuses L11's cue after taking it | refuses after taking, loses, or corrupts, where it is refused |
//! | [`FfmpegMuxRefusal::KeyFrameUnmarked`] | libavformat marks every packet of a codec its descriptor counts of key frames alone a key frame (`mux.c` 820–821), overriding the packet's own word that it is no seek point: a frame can lean on the one before — an MP3 frame whose bit reservoir starts there decodes, where decoding starts at it, to silence (`mpegaudiodec_template.c` 1316–1322) — so the mark would make a seek point that does not decode. Kept: a caller that knows the packet is a key frame marks it — L16; PLANT: dropped, L16's unmarked frame taken | writes it false |
//! | [`FfmpegMuxRefusal::BeforeZero`] | libavformat moves every stream's times later at its first write, by the earliest distance before its stream's earliest of the packet it writes or of one its interleaving holds then (`handle_avoid_negative_ts`, `mux.c` 636–717): mirrored ([`Shift`]), taken, reported ([`Uncarried::TimesShifted`]). A packet after that write the move leaves before the earliest it hands the container as it is, warning it "failed to avoid negative timestamp" (692–715): FLV refuses it after taking it (`flvenc.c` 1267–1274); Matroska writes it with no cue, its block time a 16-bit count from its cluster's, which starts no earlier than zero, so that one more than 32768 ticks before zero wraps (`matroskaenc.c` 969–970, 3146–3163, 2888) — refused. Where the move is not mirrored — GXF's and MXF's own interleaving, Matroska's times uncertain beside a video track of no native id — every time before the earliest is refused, as round 2 refused it — L14, L46; PLANT: the move unmirrored, L14's early packets refused; after the first write unjudged, L14's late packets taken. Each packet is judged in the times of what its track's filter puts out, when it puts it out (table B's mirrors; Codex R6 [high]): a PGS display set at the packet that ends it, in its presentation segment's packet's times — L69; PLANT: the mirrors moved on what the filter takes, L69's held presentation segment moving every stream | refuses after taking, or corrupts, where it is refused |
//! | [`MuxOpenError::ChannelLayout`], [`MuxOpenError::ConfigurationInvalid`], [`MuxOpenError::Cropping`] | MP4's trailer refuses the box (`movenc.c` 1299–1340, 857–910, 3024–3036; `movenc_ttml.c` 236–317) | refuses after taking: the file |
//! | [`MuxOpenError::SampleSize`], [`MuxOpenError::Cover`] | `pcm_rechunk`'s start fails the header (`pcm_rechunk.c` 49–58); the cover's builder fails after the header (`packet.c` 83–95) | refuses: the file |
//! | [`MuxOpenError::MediaTypeMismatch`] | a stream of one media type whose packets this muxer would judge as another's | corrupts |
//! | [`MuxOpenError::ChapterWithoutTitle`], [`MuxOpenError::ChapterTitleTooLong`], [`MuxOpenError::ChapterSample`] | MP4 writes every chapter into its Nero list but a titled one alone into its chapter track, which its reader lays over the list by position, with a 16-bit title length (`movenc.c` 7791–7807); `ff_mov_write_packet`'s answer for a chapter's sample ignored, a sample past `INT_MAX` lost, one out of range moved (7807) | corrupts, or loses |

use core::{fmt, marker::PhantomData};
use std::io::{Seek, Write};

use ffmpeg_next::ffi::{AV_NOPTS_VALUE, AVChannel, AVChannelOrder, AVPacketSideDataType};
use mediaencode::{
  MuxError, MuxRefusal, Muxer, check_decode_order, check_presentation_order,
  mediaspine::{
    Timebase, Timestamp,
    demuxer::{
      AudioTrackPacket, Chapter, DataTrackPacket, SubtitleTrackPacket, TrackDisposition,
      TrackIndex, TrackKind, TrackParams, VideoTrackPacket,
    },
    mediatime::Rounding,
    packet::PacketFlags,
    track::TrackDescription,
  },
};
use mediaio_ffmpeg::{
  CarrierOps, ChannelLayoutTicket, CodecId, CodecTicket, CustomChannel, Ffmpeg, FfmpegCarrier,
  Owned, PacketLimits, PacketRefusal, View,
  boundary::check_packet_build,
  extras::{SideDataEntry, packet_side_data_name},
  hdr_metadata::{check_hdr10_plus, check_smpte2094_app5},
  output::{
    CodecQuery, Output, OutputError, OutputPacket, OutputText, PacketTimes, StreamCodec,
    StreamMedia, StreamSpec,
  },
};
use smol_bytes::Utf8Bytes;

/// A libavformat muxer's short name — the container a muxer writes.
///
/// An open set, as libavformat's own is: any name `ffmpeg -f` takes
/// opens, in any case — libavformat matches names case-insensitively. The
/// constants are the containers whose losses this release reports (see
/// [the module](self)), and any spelling of one is judged as that
/// container: the muxer is known by the name libavformat resolves it to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerName(Utf8Bytes);

impl ContainerName {
  /// Matroska (`.mkv`): every codec Matroska names, attachments with
  /// their filename and MIME type, chapters.
  pub const MATROSKA: Self = Self(Utf8Bytes::from_static("matroska"));
  /// WebM: the Matroska subset for VP8, VP9, AV1, Vorbis, Opus and
  /// WebVTT; no attachments.
  pub const WEBM: Self = Self(Utf8Bytes::from_static("webm"));
  /// MP4 (ISO base media): cover art, chapters; no other attachment.
  pub const MP4: Self = Self(Utf8Bytes::from_static("mp4"));

  /// The muxer libavformat names `name`.
  pub fn new(name: impl Into<Utf8Bytes>) -> Self {
    Self(name.into())
  }

  /// The short name.
  pub fn as_str(&self) -> &str {
    self.0.as_str()
  }
}

impl fmt::Display for ContainerName {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(self.as_str())
  }
}

/// The containers this release knows the losses of, from the muxers'
/// source in FFmpeg 9.0.1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Family {
  Matroska,
  WebM,
  Mp4,
  Other,
}

impl Family {
  /// What this release knows of the muxer libavformat resolved a name to,
  /// by that muxer's own name ([`Output::container`], its
  /// `AVOutputFormat.name`) — never by the caller's spelling.
  /// `av_guess_format` matches a short name case-insensitively
  /// (`av_match_name`, `libavutil/avstring.c` 346–367; `format.c` 79–115 in
  /// FFmpeg 9.0.1), so `MP4` opens the muxer named `mp4` and meets its every
  /// rule. Two muxers are named `matroska`, the video one and the audio one
  /// (`matroskaenc.c` 3715–3741, 3777–3800); both are Matroska's writer, and
  /// the name reaches the second only in a build without the first
  /// (`allformats.c` 274–275: `av_guess_format` keeps the first of equal
  /// matches).
  fn of(muxer: &str) -> Self {
    match muxer {
      "matroska" => Self::Matroska,
      "webm" => Self::WebM,
      "mp4" => Self::Mp4,
      _ => Self::Other,
    }
  }
}

/// The most bytes libavcodec allocates a packet of: `packet_alloc` refuses
/// `INT_MAX - AV_INPUT_BUFFER_PADDING_SIZE` and more (`libavcodec/packet.c`
/// 83–95 in FFmpeg 9.0.1), and `av_grow_packet` grows none more than a byte
/// past it (121–127). Every size this muxer's mirrors of libavformat's
/// arithmetic bound — a filter's every output, a rewritten packet, a joined
/// document — is held to it (table B of the census, see [the
/// module](self)).
const PACKET_BYTES_MAX: u64 = i32::MAX as u64 - 65;

/// The most bytes Matroska and WebM take of one block — its payload as
/// written and the side data written with it — so that a cluster holds
/// them. Matroska writes a cluster into a dynamic buffer of at most
/// `INT_MAX` bytes (`dyn_buf_write`, `aviobuf.c` 1281–1311), which refuses
/// a write past it and leaves the cluster cut; it starts a new cluster once
/// the last passes 5 MiB (`matroskaenc.c` 2746–2749, 3213–3245), and
/// writes up to two blocks after that check — the audio packet it held
/// back, then the packet (3253–3271). So two blocks and 5 MiB fit, with 64 KiB to
/// spare for a cluster's own elements.
const MATROSKA_BLOCK_BYTES_MAX: u64 = (i32::MAX as u64 - 5 * 1024 * 1024 - 64 * 1024) / 2;

/// What a Matroska block adds to its payload and side data, at most: its
/// group, header, duration and padding elements, a WebVTT cue's two line
/// breaks, and each block addition's own elements (`mkv_write_block`,
/// `matroskaenc.c` 2864–3010).
const MATROSKA_BLOCK_OVERHEAD: u64 = 4 * 1024;

/// What MP4's joined TTML document adds per paragraph, at most: its `<p>`
/// element and its two times (`ttml_write_packet`, `ttmlenc.c` 161–181;
/// `ttml_write_time`, 67–80).
const TTML_PARAGRAPH_OVERHEAD: u64 = 160;

/// What the document adds around its paragraphs beside the strings the
/// track's configuration and language give it: its header and footer
/// (`ttml_header_text`, `ttml_footer_text`, `ttmlenc.c` 53–65).
const TTML_DOCUMENT_OVERHEAD: u64 = 1024;

/// How a muxer opens: the bound on what it copies of each packet, and
/// whether libavformat's automatic bitstream filters run (see [the
/// module](self)).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MuxOptions {
  limits: PacketLimits,
  automatic_filters: bool,
  bounds: Bounds,
}

/// The sizes the muxer holds what libavformat makes of a packet to: a
/// filter's every output and a rewritten packet, at most libavcodec's
/// largest packet; a Matroska block's payload and side data, at most its
/// share of a cluster less the block's own elements (table B of the
/// census, see [the module](self)).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct Bounds {
  packet: u64,
  block: u64,
}

impl Bounds {
  /// libavcodec's and Matroska's own bounds.
  const REAL: Self = Self {
    packet: PACKET_BYTES_MAX,
    block: MATROSKA_BLOCK_BYTES_MAX - MATROSKA_BLOCK_OVERHEAD,
  };
}

impl Default for MuxOptions {
  fn default() -> Self {
    Self::new()
  }
}

impl MuxOptions {
  /// The defaults: [`PacketLimits::new`], and the automatic filters on —
  /// libavformat's own default (`libavformat/options_table.h` 42 in FFmpeg
  /// 9.0.1), which `ffmpeg -c copy` runs with.
  pub const fn new() -> Self {
    Self {
      limits: PacketLimits::new(),
      automatic_filters: true,
      bounds: Bounds::REAL,
    }
  }

  /// What one packet's payload may cost.
  pub const fn limits(&self) -> PacketLimits {
    self.limits
  }

  /// Whether libavformat's automatic bitstream filters run: on, a
  /// container converts the packets they convert, and each conversion is
  /// reported for its track ([`Uncarried::Filtered`]); off, no filter is
  /// inserted at a track's first packet. A filter a muxer inserts at its
  /// start runs either way — IVF's VP9 superframes, reported as on — and so
  /// does its every rule here (table B of the census, see [the
  /// module](self)).
  pub const fn automatic_filters(&self) -> bool {
    self.automatic_filters
  }

  /// Sets what one packet's payload may cost (consuming builder).
  #[must_use]
  pub const fn with_limits(mut self, limits: PacketLimits) -> Self {
    self.limits = limits;
    self
  }

  /// Turns the automatic filters on or off (consuming builder).
  #[must_use]
  pub const fn with_automatic_filters(mut self, on: bool) -> Self {
    self.automatic_filters = on;
    self
  }

  /// Sets what one packet's payload may cost, in place.
  pub const fn set_limits(&mut self, limits: PacketLimits) -> &mut Self {
    self.limits = limits;
    self
  }

  /// Turns the automatic filters on or off, in place.
  pub const fn set_automatic_filters(&mut self, on: bool) -> &mut Self {
    self.automatic_filters = on;
    self
  }

  /// Lowers the bounds the muxer holds what libavformat makes of a packet
  /// to — libavcodec's largest packet, `packet`, and the bytes of a
  /// Matroska block's payload and side data, `block` — never raising either
  /// past its real one: the laws' seam, which shows a bound without
  /// gigabytes of packets. A muxer opened with it refuses what libavformat
  /// would take.
  #[doc(hidden)]
  #[must_use]
  pub const fn with_lowered_bounds(mut self, packet: u64, block: u64) -> Self {
    let real = Bounds::REAL;
    self.bounds = Bounds {
      packet: if packet < real.packet {
        packet
      } else {
        real.packet
      },
      block: if block < real.block {
        block
      } else {
        real.block
      },
    };
    self
  }
}

/// One of libavformat's bitstream filters a muxer inserts for a track —
/// when it writes the track's first packet, or at its start
/// ([`Uncarried::Filtered`]; table B of the census, see [the
/// module](self)) — each from its source in FFmpeg 9.0.1.
///
/// Marked `#[non_exhaustive]`: a filter a later release learns of joins as
/// a variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum AutomaticFilter {
  /// `aac_adtstoasc`, for MP4's and Matroska's AAC framed in ADTS headers
  /// with no configuration (`mov_check_bitstream`, `movenc.c` 9138–9151;
  /// `mkv_check_bitstream`, `matroskaenc.c` 3630–3646): each packet's
  /// header — and its CRC — is stripped, and the track's configuration is
  /// made from the first frame's, with its program config element where
  /// the header names none (`aac_adtstoasc.c` 39–128).
  AacAdtsToAsc,
  /// `vp9_superframe`, for VP9 in MP4, Matroska, WebM and MOV's other
  /// modes, the automatic filters on, and in IVF, which inserts it at its
  /// start whatever they say: a frame the
  /// stream does not show is held, and joined with the next frame it shows
  /// into one superframe, which takes that frame's times, flags and side
  /// data — the held frames' own are dropped — and any still held when the
  /// stream ends, which no later frame shows, are dropped
  /// (`vp9_superframe.c` 101–181, 198–207).
  Vp9Superframe,
  /// `pgs_frame_merge`, for PGS subtitles in Matroska: the segments of a
  /// display set are joined into one packet at its end segment, which
  /// takes the times, flags and side data of the packet holding its
  /// presentation segment — the other packets' own are dropped — and is
  /// marked a key frame by that segment's composition state; segments
  /// still held when the stream ends are written as one packet, marked
  /// corrupt (`pgs_frame_merge.c` 59–157).
  PgsFrameMerge,
  /// `pcm_rechunk`, for PCM audio in DAUD, GXF and the MXF muxers, which
  /// insert it at their start whatever the automatic filters say
  /// (`daudenc.c` 44; `gxfenc.c` 724; `mxfenc.c` 3081): the track's
  /// samples are cut into packets of the count the muxer names, their
  /// times counted from the first (`pcm_rechunk.c` 123–186).
  PcmRechunk,
  /// `av1_metadata`, inserting a temporal delimiter before each temporal
  /// unit that opens without one — IVF's AV1 at its start, whatever the
  /// automatic filters say, and the low-overhead OBU muxer's at a track's
  /// first packet (`ivfenc.c` 46; `rawenc.c` 572): each packet's OBUs read
  /// and written back.
  Av1Metadata,
  /// `eia608_to_smpte436m`, for EIA-608 captions in MCC and the MXF
  /// muxers, at their start (`mccenc.c` 434; `mxfenc.c` 3104): each
  /// packet's byte pairs rewritten as one SMPTE 436M ANC packet
  /// (`eia608_to_smpte436m.c` 168–227).
  Eia608ToSmpte436m,
  /// `extract_extradata`, for H.264, HEVC, VVC, AV1 and MPEG-4 video with
  /// no configuration in FLV, at a track's first packet (`flvenc.c`
  /// 1495–1501): the track's configuration made from the parameter sets
  /// its packets carry, handed to FLV as new extradata, the packets kept
  /// (`extract_extradata.c` 412–419).
  ExtractExtradata,
  /// `h264_mp4toannexb`, for H.264 whose first packet is length-prefixed,
  /// in MPEG-TS, MXF, MXF OP-Atom and the raw H.264 muxer, at that packet
  /// (`mpegtsenc.c` 2310–2334; `mxfenc.c` 3631–3639; `rawenc.c` 386–393):
  /// each length rewritten as a start code, the parameter sets put before
  /// each IDR frame.
  H264Mp4ToAnnexB,
  /// `hevc_mp4toannexb`, for HEVC whose first packet is length-prefixed,
  /// in MPEG-TS and the raw HEVC muxer (`mpegtsenc.c` 2310–2334;
  /// `rawenc.c` 436–443), as for H.264.
  HevcMp4ToAnnexB,
  /// `vvc_mp4toannexb`, for VVC whose first packet is length-prefixed, in
  /// MPEG-TS and the raw VVC muxer (`mpegtsenc.c` 2310–2334; `rawenc.c`
  /// 411–418), as for H.264.
  VvcMp4ToAnnexB,
}

impl AutomaticFilter {
  /// libavcodec's name for the filter.
  pub const fn name(self) -> &'static str {
    match self {
      Self::AacAdtsToAsc => "aac_adtstoasc",
      Self::Vp9Superframe => "vp9_superframe",
      Self::PgsFrameMerge => "pgs_frame_merge",
      Self::PcmRechunk => "pcm_rechunk",
      Self::Av1Metadata => "av1_metadata",
      Self::Eia608ToSmpte436m => "eia608_to_smpte436m",
      Self::ExtractExtradata => "extract_extradata",
      Self::H264Mp4ToAnnexB => "h264_mp4toannexb",
      Self::HevcMp4ToAnnexB => "hevc_mp4toannexb",
      Self::VvcMp4ToAnnexB => "vvc_mp4toannexb",
    }
  }
}

impl fmt::Display for AutomaticFilter {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(self.name())
  }
}

/// What became of packet side data a container does not keep whole
/// ([`Uncarried::SideData`]) — by the side-data table (see [the
/// module](self)).
///
/// Marked `#[non_exhaustive]`: a fate a later release tells apart joins as
/// a variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SideDataFate {
  /// The container writes nothing of it, or nothing that reads back as it.
  Dropped,
  /// The container writes it where whether it reads back as it depends on
  /// what the muxer cannot know while writing — a later packet's side data,
  /// or where the packet lies in the track — so the source does not settle
  /// its fate.
  PossiblyDropped,
}

/// Something the container could not carry, which the muxer was opened
/// without.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Uncarried {
  /// An attachment the container has no place for — a font in MP4, any
  /// attachment in WebM. Its track is not written.
  Attachment {
    /// The track, in the table the muxer was opened with.
    track: TrackIndex,
  },
  /// An attachment's filename its road does not keep: MP4 keeps a
  /// cover's picture and the type of its image, and nothing else
  /// (`mov_write_covr`, `movenc.c` 4815–4839 in FFmpeg 9.0.1).
  Filename {
    /// The track.
    track: TrackIndex,
    /// The filename the attachment declared.
    filename: Utf8Bytes,
  },
  /// An attachment's MIME type its road does not keep — MP4's cover, as
  /// for its filename.
  MimeType {
    /// The track.
    track: TrackIndex,
    /// The MIME type the attachment declared.
    mime_type: Utf8Bytes,
  },
  /// A language the track's road does not write so that it reads back as
  /// itself — the report table's language rule. Matroska and WebM write
  /// an empty tag as `und` (`matroskaenc.c` 1967–1969 in FFmpeg 9.0.1) and
  /// read `und` back as none (`matroskadec.c` 3280–3281). MP4 keeps only a
  /// code that packs and unpacks to itself: exactly three bytes in
  /// `0x60..=0x7F`, the first not `0x60`, which packs below `0x400` and
  /// unpacks through the Macintosh table, and not all three `0x7F`, which
  /// packs to "unspecified" (`isom.c` 233–280; `movenc.c` 8455–8458).
  /// MP4's cover, which no track carries, keeps none.
  Language {
    /// The track.
    track: TrackIndex,
    /// The tag the track declared.
    tag: Utf8Bytes,
  },
  /// Disposition flags the container has no field for on the track's
  /// road. An attachment Matroska writes has no track entry, and so no
  /// flag (`matroskaenc.c` 1957–1958); MP4's cover reads back as an
  /// attached picture and nothing more.
  Disposition {
    /// The track.
    track: TrackIndex,
    /// The flags not written.
    flags: TrackDisposition,
  },
  /// Packets the container rewrites on their way into the file: what it
  /// writes is the container's form of a packet, not the bytes offered —
  /// the packet-rule table's reported rows (see [the module](self)).
  Rewritten {
    /// The track.
    track: TrackIndex,
    /// What the container does to the track's packets.
    rewrite: Rewrite,
  },
  /// Packets one of libavformat's bitstream filters converts before the
  /// container sees them — the filter, named ([`AutomaticFilter`]).
  /// Reported at open, where open decides it: a muxer inserts it at its
  /// start, or libavformat at the track's first packet where the codec and
  /// the configuration settle it, or this muxer admits on the track only the
  /// packets that filter converts; else at the write of the first packet,
  /// which inserts it — AAC's in FLV and LATM, the Annex B conversions —
  /// once ([`CarrierMuxer::report`]; see [the module](self), table B).
  Filtered {
    /// The track.
    track: TrackIndex,
    /// The filter.
    filter: AutomaticFilter,
  },
  /// Disposition flags the track reads back with though it did not
  /// declare them — table E of the census (see [the module](self)). MP4
  /// enables the first video, audio and subtitle track of each type when
  /// none of the type declares the default flag, and its reader reads the
  /// enabled flag as the default (`enable_tracks`, `movenc.c` 7883–7923;
  /// `mov.c` 5732); MP4 reads a hearing-impaired or captions track back as
  /// both, and a visual-impaired or descriptions track as both
  /// (`movenc.c` 4338–4356; `isom.c` 450–462); MP4's cover, and a Matroska
  /// attachment of an image type, read back as attached pictures
  /// (`mov_write_covr`, `movenc.c` 4815–4839; `matroskadec.c` 3509–3533).
  DispositionAdded {
    /// The track.
    track: TrackIndex,
    /// The flags added.
    flags: TrackDisposition,
  },
  /// A chapter's id the container does not keep: it reads back with
  /// another — table E of the census. MP4 keeps no chapter id, its reader
  /// numbering chapters by their position (`mov.c`'s `chpl` and chapter
  /// track); Matroska numbers every chapter from 1 where one's id is 0 or
  /// repeats another's (`mkv_new_chapter_ids_needed`, `matroskaenc.c`
  /// 2399–2442).
  ChapterId {
    /// The chapter's position in the table the muxer was opened with.
    chapter: usize,
    /// The id it declared.
    id: i64,
  },
  /// A chapter's start or end the container does not keep: it reads back
  /// at another instant — table E of the census. MP4 counts a chapter's
  /// times in its movie timescale, which its streams' time bases make
  /// (`mov_init`, `movenc.c` 8427–8446), and its reader ends each chapter
  /// where the next starts (`mov_create_chapter_track`, `movenc.c`
  /// 7727–7815); Matroska counts them in nanoseconds (2439–2440). Each
  /// rescale rounds to the nearest (`av_rescale_q`).
  ChapterSpan {
    /// The chapter's position in the table.
    chapter: usize,
  },
  /// Packets held when the muxer finished, which the stream's end decides:
  /// VP9's hidden frames, which no later frame shows, dropped by its filter
  /// (`vp9_superframe.c` 198–207, its flush), and the packets of a PGS
  /// display set no packet completed, which this muxer held and does not
  /// write — FFmpeg's filter, holding them, would write them marked corrupt
  /// (`pgs_frame_merge.c` 77–83) — unless handed back first
  /// ([`CarrierMuxer::take_held`]): table E of the census (see [the
  /// module](self)). Reported by [`CarrierMuxer::finish_into_sink`], with
  /// the report it hands back.
  Held {
    /// The track.
    track: TrackIndex,
    /// The filter.
    filter: AutomaticFilter,
    /// How many packets it held.
    packets: u64,
  },
  /// Every stream's times written later than offered — the chapters' not.
  /// libavformat moves them all where the first packet it writes — or one
  /// its interleaving holds then — lies before the earliest its stream's
  /// container writes, by that packet's distance before it, each stream's
  /// rounded up to its own tick (`handle_avoid_negative_ts`, `mux.c`
  /// 636–717 in FFmpeg 9.0.1): Matroska and WebM judge presentation times —
  /// decode times beside a video track written in VFW mode — below zero or
  /// below an audio track's codec delay before it; another container
  /// without `AVFMT_TS_NEGATIVE` decode times below zero. Reported once, at
  /// the first packet that makes it certain ([`CarrierMuxer::report`]); a
  /// packet after the first write that the move leaves before the earliest
  /// is refused ([`FfmpegMuxRefusal::BeforeZero`]).
  TimesShifted,
  /// Packet side data of a kind the container has no place for on the
  /// track, or keeps in part — the side-data table (see [the
  /// module](self)). Reported once per track, kind and fate, when the
  /// first packet carrying it is written ([`CarrierMuxer::report`]).
  SideData {
    /// The track.
    track: TrackIndex,
    /// The kind: FFmpeg's `AV_PKT_DATA_*` value, as the packet carries it.
    kind: i32,
    /// libavcodec's name for the kind, where this build names it
    /// ([`mediaio_ffmpeg::extras::packet_side_data_name`]).
    name: Option<&'static str>,
    /// What became of it.
    fate: SideDataFate,
  },
}

/// What a container does to a track's packets on their way into the
/// file, reported at open ([`Uncarried::Rewritten`]) — each from the
/// muxer's source in FFmpeg 9.0.1.
///
/// Marked `#[non_exhaustive]`: a rewrite a later release learns of joins
/// as a variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Rewrite {
  /// H.264, HEVC, VVC or LCEVC whose configuration is Annex B: each
  /// packet's start codes are rewritten as length prefixes, and MP4 drops
  /// an HEVC packet's parameter sets under `hvc1` (`movenc.c` 7087–7158;
  /// `matroskaenc.c` 3574–3582).
  LengthPrefixed,
  /// AV1: each packet's temporal delimiters, redundant frame headers,
  /// tile lists and padding are dropped (`av1.c` 33–81; `movenc.c`
  /// 7159–7174; `matroskaenc.c` 3589–3590).
  ObusFiltered,
  /// MP4 AC-3 and E-AC-3: E-AC-3 frames of fewer than six blocks are
  /// joined into samples of six, each taking its first packet's times,
  /// flags and side data, and the blocks of a set the stream leaves
  /// incomplete at its end are dropped (`handle_eac3`, `movenc.c` 577–606;
  /// `mov_free`, 7951–7955; table C″ of the census). A packet MP4 would
  /// refuse or drop is refused by name ([`PayloadRule::Ac3Header`] and its
  /// neighbours).
  Ac3Parsed,
  /// MP4 `mov_text`: an empty sample is written at the end of each cue a
  /// later packet passes, and at zero before the first, each read back as
  /// a cue of its own (`movenc.c` 7625–7656).
  CueEnds,
  /// MP4 TTML from libavcodec's TTML encoder: the track's paragraphs are
  /// joined into one sample (`movenc.c` 7658–7684, 8596–8600).
  ParagraphsJoined,
  /// MP4 VC-1 and TrueHD: a sample's sync flag is read from its bitstream,
  /// not from the packet's key flag (`movenc.c` 7344–7348).
  SyncFromBitstream,
  /// A packet's duration is not kept as given. MP4 keeps every sample's
  /// duration as the distance to the next one's decode time, the last
  /// one's from its packet (`get_cluster_duration`, `movenc.c` 1241–1259;
  /// 7321), so a track whose samples do not abut — a gap, or an overlap —
  /// loses the durations it offered there, counted between the samples MP4
  /// writes: a joined E-AC-3 sample's is its packets' sum (table C″ of the
  /// census). Reported for such a track when the first packet showing it is
  /// written ([`CarrierMuxer::report`]); samples that abut lose nothing and
  /// report nothing. `mov_text` marks each cue's end with an empty sample
  /// ([`Rewrite::CueEnds`]), but none before a cue that starts before that
  /// end, which shortens the last cue to it: reported at that cue. And MP4
  /// ends an E-AC-3 track's edit list at its last sample's last packet's
  /// duration (6940, 7329–7342): a last sample joined of several is
  /// presented short of its frames, reported at `finish`
  /// ([`CarrierMuxer::finish_into_sink`]). A negative duration libavformat
  /// writes as zero, or as its own guess (`guess_pkt_duration`, `mux.c`
  /// 605–633 in FFmpeg 9.0.1), and Matroska and WebM a subtitle's as zero
  /// (`matroskaenc.c` 3134): reported at the packet — a subtitle's another
  /// container is handed as it is, refused
  /// ([`FfmpegMuxRefusal::NegativeDuration`]). Matroska and WebM keep a
  /// video or audio packet's only where it differs from the track's default
  /// duration (`mkv_write_block`, `matroskaenc.c` 2895–2911), and a video
  /// track without a frame rate, or an audio track whose codec fixes no
  /// frame's samples, has none (1933–1940, 2058–2065, 2119–2124): reported
  /// at open.
  Durations,
  /// Matroska WavPack: each block's header is stripped to the fields
  /// Matroska keeps (`matroskaenc.c` 2775–2816, 3571–3572).
  WavPackHeaders,
}

/// What the container could not carry: what is known at open, and what
/// the muxer learns while it writes ([`CarrierMuxer::report`]).
///
/// Empty when the container keeps everything the table and the packets
/// state, as far as this release knows the container and as far as the
/// census's tables reach — a track's fields, its packets, their side data,
/// the chapters; not a codec's parameters beyond its configuration (see
/// [the module](self), table E).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MuxReport {
  uncarried: Vec<Uncarried>,
}

impl MuxReport {
  /// Each thing not carried. What is known at open comes first, in table
  /// order — and within a track, field by field: an attachment's filename,
  /// its MIME type, the language, the disposition flags dropped, then
  /// those added, then the automatic filter and the container's own
  /// rewrites; then the chapters, in table order, each its id and its
  /// span. What is learned while writing follows, in the order it was
  /// learned: a track's durations, the side data of each kind a container
  /// drops; then, from [`CarrierMuxer::finish_into_sink`], the packets an
  /// automatic filter still held at the end, track by track.
  pub fn uncarried(&self) -> &[Uncarried] {
    &self.uncarried
  }

  /// Whether everything was carried.
  pub fn is_empty(&self) -> bool {
    self.uncarried.is_empty()
  }
}

/// Where a muxer's fault happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum MuxStage {
  /// Opening the muxing context.
  Open,
  /// Adding a track's stream.
  Stream {
    /// The track.
    track: TrackIndex,
  },
  /// Adding a chapter.
  Chapter {
    /// The chapter's position in the table it was opened with.
    chapter: usize,
  },
  /// Writing the header.
  Header,
  /// Writing a track's cover picture, just after the header.
  Cover {
    /// The track.
    track: TrackIndex,
  },
  /// Writing a packet.
  Write {
    /// The track.
    track: TrackIndex,
  },
  /// Writing the trailer.
  Trailer,
  /// Freeing the context and handing the sink back.
  Finish,
}

/// The muxer's own fault: where, and what — libavformat's or the sink's
/// account of it, or a refusal the muxer made where no packet could be
/// handed back. A fault may come after libavformat took the packet, so a
/// write's fault does not hand it back.
#[derive(Debug, thiserror::Error)]
#[error("{stage:?}: {cause}")]
pub struct MuxFault {
  stage: MuxStage,
  #[source]
  cause: FaultCause,
}

/// What a [`MuxFault`] was.
#[derive(Debug, thiserror::Error)]
enum FaultCause {
  /// The muxing context's account: libavformat's, or the sink's.
  #[error(transparent)]
  Output(OutputError),
  /// A refusal at `finish`, of a file the container's trailer would fail.
  #[error(transparent)]
  Refused(FfmpegMuxRefusal),
}

impl MuxFault {
  const fn new(stage: MuxStage, error: OutputError) -> Self {
    Self {
      stage,
      cause: FaultCause::Output(error),
    }
  }

  const fn refused(stage: MuxStage, refusal: FfmpegMuxRefusal) -> Self {
    Self {
      stage,
      cause: FaultCause::Refused(refusal),
    }
  }

  /// Where it happened.
  pub const fn stage(&self) -> MuxStage {
    self.stage
  }

  /// The muxing context's account of it — libavformat's, or the sink's —
  /// where that is what it was.
  pub const fn error(&self) -> Option<&OutputError> {
    match &self.cause {
      FaultCause::Output(error) => Some(error),
      FaultCause::Refused(_) => None,
    }
  }

  /// The refusal, where the muxer refused to finish a file the
  /// container's trailer would fail ([`FfmpegMuxRefusal::NoFrame`]): no
  /// packet could be handed back, and the trailer was not written.
  pub const fn refusal(&self) -> Option<FfmpegMuxRefusal> {
    match &self.cause {
      FaultCause::Refused(refusal) => Some(*refusal),
      FaultCause::Output(_) => None,
    }
  }

  /// The panic the caller's sink raised, when that is what this was.
  pub fn sink_panic(&self) -> Option<&Utf8Bytes> {
    match &self.cause {
      FaultCause::Output(OutputError::SinkPanic { message }) => Some(message),
      _ => None,
    }
  }
}

/// A refusal of the container's own, beyond the face's — the packet
/// handed back in [`MuxError::Backend`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum FfmpegMuxRefusal {
  /// A decode time equal to the last one written on its stream, in a
  /// container that needs them strictly increasing: libavformat's rule
  /// for every muxer without `AVFMT_TS_NONSTRICT`, on every stream but a
  /// subtitle or data stream, counted in the stream's own time base
  /// (`compute_muxer_pkt_fields`, `libavformat/mux.c` 557–566 in FFmpeg
  /// 9.0.1). The face admits the repeat; this container does not.
  #[error(
    "track {} already holds a packet at decode time {}: this container needs them increasing",
    .track.get(), .dts.pts()
  )]
  RepeatedDts {
    /// The track.
    track: TrackIndex,
    /// The decode time offered.
    dts: Timestamp,
  },
  /// A timestamp the stream's time base cannot count: past `i64` once
  /// rescaled, or — a presentation or decode time — landing on
  /// `i64::MIN`, which is `AV_NOPTS_VALUE`: libavformat reads that count
  /// as no timestamp at all, and would write the packet with timing of
  /// its own making, or refuse it after taking it (`mux.c` 531–572 in
  /// FFmpeg 9.0.1). Refused before libavformat is handed anything, the
  /// packet untouched.
  #[error("track {}: {} cannot be counted in the stream's time base", .track.get(), .timestamp.pts())]
  Unrepresentable {
    /// The track.
    track: TrackIndex,
    /// The timestamp.
    timestamp: Timestamp,
  },
  /// A subtitle's negative duration, outside Matroska and WebM.
  /// libavformat hands a subtitle's on as it is (`guess_pkt_duration`,
  /// `mux.c` 605–611 in FFmpeg 9.0.1): MP4 refuses it after taking the
  /// packet (`check_pkt`, `movenc.c` 6925–6928), or joins a TTML paragraph
  /// into its document ending before it starts (`ttmlenc.c` 172); SRT drops
  /// the cue (`srtenc.c` 71–75); WebVTT and ASS write it ending before it
  /// starts (`webvttenc.c` 80, `assenc.c` 160). Any other track's
  /// libavformat writes as zero, or as its own guess (605–633), and
  /// Matroska and WebM a subtitle's as zero (`matroskaenc.c` 3134): taken,
  /// and reported ([`Rewrite::Durations`]).
  #[error("track {}: the subtitle's duration {} is negative", .track.get(), .duration.pts())]
  NegativeDuration {
    /// The track.
    track: TrackIndex,
    /// The duration offered.
    duration: Timestamp,
  },
  /// MP4: a duration of more than `i32::MAX` ticks of the stream's time
  /// base, which MP4 refuses after taking the packet (`movenc.c`
  /// 6925–6928).
  #[error("track {}: the duration {} is longer than MP4 stores", .track.get(), .duration.pts())]
  DurationTooLong {
    /// The track.
    track: TrackIndex,
    /// The duration offered.
    duration: Timestamp,
  },
  /// MP4: a decode time `i32::MAX` ticks or more after the last sample
  /// MP4 wrote on the track — a packet MP4 holds back writes none, and an
  /// empty `mov_text` sample is one (table C″ of the census): MP4 would
  /// rewrite it to one tick after that sample and drop the presentation
  /// time, and write the packet so (`check_pkt`, `movenc.c` 6897–6923).
  /// Before the track's first sample, nothing is judged.
  #[error("track {}: the decode time {} lies too far after the last for MP4", .track.get(), .dts.pts())]
  DtsGap {
    /// The track.
    track: TrackIndex,
    /// The decode time offered.
    dts: Timestamp,
  },
  /// MP4 TTML from libavcodec's TTML encoder: a paragraph ending more than
  /// `i32::MAX` ticks after zero. MP4 joins the track's paragraphs at the
  /// trailer into one sample from zero to the latest end, and refuses a
  /// sample longer than `i32::MAX` ticks there, failing its trailer and the
  /// whole file (`movenc_ttml.c` 108–110, 212–213, 305–306; `check_pkt`,
  /// `movenc.c` 6925–6928; 9036–9037). Refused at the paragraph that would
  /// pass it.
  #[error("track {}: the paragraph at {} ends too far after zero for MP4's joined TTML", .track.get(), .pts.pts())]
  DocumentSpan {
    /// The track.
    track: TrackIndex,
    /// The paragraph's presentation time.
    pts: Timestamp,
  },
  /// MP4: a presentation more than `i32::MAX` ticks after its decode,
  /// beyond MP4's 32-bit composition offset (`movenc.c` 6955–6959; the
  /// offset is an `int`, `movenc.h` 58).
  #[error(
    "track {}: presented at {}, decoded at {}: too far apart for MP4",
    .track.get(), .pts.pts(), .dts.pts()
  )]
  PresentationOffset {
    /// The track.
    track: TrackIndex,
    /// The presentation time offered.
    pts: Timestamp,
    /// The decode time offered.
    dts: Timestamp,
  },
  /// No presentation time on a stream that reorders — its parameters'
  /// `video_delay` above zero: libavformat makes none up there
  /// (`mux.c` 531–532), and MP4 then presents the packet at its decode
  /// time (`movenc.c` 7324–7327) while Matroska and WebM refuse it after
  /// taking it (`matroskaenc.c` 3137–3142).
  #[error("track {} reorders, and the packet has no presentation time", .track.get())]
  MissingPts {
    /// The track.
    track: TrackIndex,
  },
  /// A time libavformat would hand the container before the earliest it
  /// writes. At its first write libavformat moves every stream's times
  /// later, by the earliest distance before its stream's earliest of the
  /// packet it writes or of one its interleaving holds then
  /// (`handle_avoid_negative_ts`, `mux.c` 636–717 in FFmpeg 9.0.1): taken,
  /// and reported ([`Uncarried::TimesShifted`]). A packet after that write
  /// that the move leaves before the earliest it hands the container as it
  /// is, warning it "failed to avoid negative timestamp" (692–715): FLV
  /// refuses it after taking it (`flvenc.c` 1267–1274), and Matroska writes
  /// it with no cue (`matroskaenc.c` 969–970), its block time a 16-bit count
  /// from its cluster's, which starts no earlier than zero, so that one more
  /// than 32768 ticks before zero wraps (2888, 3146–3163) — refused.
  ///
  /// In Matroska and WebM the earliest is zero, or an audio track's codec
  /// delay before it (2089–2104), and the time judged a presentation time
  /// (3538–3541) — a decode time where a video track is written in VFW
  /// mode, as JPEG 2000 is (2025–2081); in another container without
  /// `AVFMT_TS_NEGATIVE`, zero, and a decode time. MP4 takes negative times
  /// as they are. Where this muxer does not mirror the move — GXF's and
  /// MXF's own interleaving (`gxfenc.c` 1030; `mxfenc.c` 3721–3756), and
  /// Matroska beside a video track of no native id, which libavformat may
  /// write in either mode — every time before the earliest is refused: in
  /// Matroska both the presentation and the decode time.
  #[error("track {}: {} lies before the earliest time the container writes", .track.get(), .timestamp.pts())]
  BeforeZero {
    /// The track.
    track: TrackIndex,
    /// The time offered — a presentation time, or a decode time.
    timestamp: Timestamp,
  },
  /// MP4 `mov_text`: a first cue before zero, or, beside another timed
  /// track, a cue that starts before the end MP4 marks the last one with,
  /// where a packet of that track can make MP4 mark the end first (table C″
  /// of the census).
  ///
  /// MP4 writes the empty sample marking a cue's end before any packet, of
  /// any track, whose decode time, counted in its own track's ticks, passes
  /// the cue track's end (`movenc.c` 7646–7653), and then moves a cue
  /// before it — its decode time past the mark, its presentation time
  /// dropped (`check_pkt`, 6917–6923). A track counted in finer ticks than
  /// the cue track's can pass the end in ticks before it does in time; one
  /// at least as coarse can only after it, and libavformat's interleaving
  /// hands such a packet on first only where it was handed over more than
  /// 10 s past the end (`mux.c` 957–1003). Taken otherwise, as MP4 writes
  /// it: the last cue shortened to it ([`Rewrite::Durations`]).
  ///
  /// A first cue before zero MP4 writes, its edit list starting the track
  /// at zero (`mov_write_edts_tag`, 4133–4239); its reader marks a sample
  /// that starts before an edit's start discarded, and counts the samples
  /// after it from the edit's start, so that the cue is dropped and every
  /// later one moved (`mov_fix_index`, `mov.c` 4374–4682).
  #[error("track {}: a cue at {} starts before zero, or before the last one's end where another track can make MP4 mark it first", .track.get(), .pts.pts())]
  CueOrder {
    /// The track.
    track: TrackIndex,
    /// The cue's time.
    pts: Timestamp,
  },
  /// A packet of a codec every frame of which libavformat counts a key
  /// frame, not marked one: libavformat would mark it (`mux.c` 820–821;
  /// [`CodecId::intra_only`]), overriding the packet's word that it is no
  /// seek point. A frame of such a codec can lean on the one before: an MP3
  /// frame whose bit reservoir starts there decodes, where decoding starts
  /// at it, to silence (`mpegaudiodec_template.c` 1316–1322 in FFmpeg
  /// 9.0.1), so the mark would make a seek point that does not decode.
  /// Refused, untouched: a caller that knows the packet is one marks it.
  /// Not in Matroska or WebM for a subtitle track, whose every block they
  /// write as a key frame regardless (`matroskaenc.c` 3132–3133).
  #[error("track {}: a packet of a codec of key frames alone is not marked one", .track.get())]
  KeyFrameUnmarked {
    /// The track.
    track: TrackIndex,
  },
  /// A packet whose bytes the container would refuse after taking it, or
  /// write otherwise than they are ([`PayloadRule`]).
  #[error("track {}: {rule}", .track.get())]
  Payload {
    /// The track.
    track: TrackIndex,
    /// What about its bytes.
    rule: PayloadRule,
  },
  /// MP4 AC-3 or E-AC-3: a track that took no frame when the muxer
  /// finished. MP4 writes the track's configuration (`dac3`, `dec3`) from
  /// the first frame it parsed, and fails its trailer — the whole file —
  /// without one (`movenc.c` 424–429, 619–623 in FFmpeg 9.0.1). Where a
  /// check needs the whole stream it is made at the first packet that
  /// decides it; this one no packet decides, so `finish` makes it, before
  /// the trailer, and names it in its fault ([`MuxFault::refusal`]).
  #[error("track {} took no frame, and MP4 writes its configuration from its first", .track.get())]
  NoFrame {
    /// The track.
    track: TrackIndex,
  },
  /// MP4 E-AC-3: the track's last sample, joined from frames of fewer than
  /// six blocks, lasting more than `i32::MAX` ticks of the stream's time
  /// base. MP4 joins such frames into one sample whose duration is the sum
  /// of its packets' (`handle_eac3`, `movenc.c` 597), each of which
  /// `check_pkt` held within `INT_MAX` alone, and its trailer reads the last
  /// sample's duration from that sum and asserts it fits `INT_MAX`
  /// (`get_cluster_duration`, 1248–1256): `av_assert0` ends the process.
  /// A later sample would take the duration from its own decode time, so
  /// no packet decides it: `finish` refuses it, before the trailer, and
  /// names it in its fault ([`MuxFault::refusal`]).
  #[error("track {}'s last sample lasts longer than MP4's trailer reads", .track.get())]
  LastSampleTooLong {
    /// The track.
    track: TrackIndex,
  },
}

/// What about a packet's bytes [`FfmpegMuxRefusal::Payload`] refuses — each
/// a rule of the muxer's in FFmpeg 9.0.1.
///
/// Marked `#[non_exhaustive]`: a rule a later release learns of joins as a
/// variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum PayloadRule {
  /// No bytes: MP4 drops an empty packet (`movenc.c` 7431–7441), Matroska
  /// and WebM an empty audio packet (`matroskaenc.c` 3262–3268), and, with
  /// the automatic filters on, Matroska's PGS filter an empty PGS packet
  /// (`pgs_frame_merge.c` 87–90).
  #[error("the packet is empty, and the container drops it")]
  Empty,
  /// AAC framed in an ADTS header on a track whose configuration is
  /// stated, into MP4 or Matroska, which take raw AAC, where no automatic
  /// filter converts it: MP4 refuses the first such packet after taking it
  /// and writes the rest with their headers inside (`movenc.c` 7076–7086),
  /// and Matroska writes them as they come into a track that declares raw
  /// AAC. The filter is inserted at the track's first packet where that
  /// packet is ADTS, whatever the configuration (`movenc.c` 9143–9145;
  /// `matroskaenc.c` 3635–3637), and then converts every ADTS packet: so
  /// this is refused with the filters off, and after a raw first packet.
  #[error("the packet is AAC in an ADTS header, and the track takes raw AAC")]
  AdtsHeader,
  /// MP4 and Matroska AAC with no configuration, the automatic filters
  /// on: a packet that is not one ADTS frame the filter converts whole —
  /// its sync word, a sample rate the table names, a frame length of at
  /// least its header and the packet's own, no CRC over several raw data
  /// blocks, bytes after the header and its CRC and, on the track's first
  /// frame, a channel configuration, or a program config element opening
  /// its payload whole with bytes after it. The filter refuses any other
  /// after taking it (`aac_adtstoasc.c` 39–128; `adts_header.c` 30–74),
  /// or, of a packet longer than its frame, strips the first header alone
  /// (70–73), or, of a program config element running past its payload,
  /// writes the packet at a negative size (94–95).
  ///
  /// In another container whose first packet inserts the filter — FLV,
  /// LATM, MOV's other modes (table B) — a packet the filter refuses after
  /// taking it, one whose program config element runs past its payload,
  /// which it would put out at a negative size, and, on a configured
  /// track, the ADTS packet that inserts it where libavcodec does not parse
  /// the configuration, which fails the filter's start
  /// (`aac_adtstoasc.c` 130–144).
  #[error("the packet is not one ADTS frame the automatic filter converts whole")]
  AdtsFrame,
  /// MP4 PCM: not a whole number of frames — channels times bytes per
  /// sample. MP4 refuses a packet holding none after taking it, and counts
  /// only the whole frames of another, losing the rest (`movenc.c`
  /// 6999–7009, 8538–8560).
  #[error("the packet is not a whole number of the codec's frames")]
  PartialSample,
  /// AV1 whose OBUs do not parse: MP4 writes a sample of a negative size,
  /// and Matroska refuses the packet after taking it (`av1.c` 33–81, over
  /// libavcodec's `parse_obu_header`, `av1_parse.h` 92–134).
  #[error("the packet's OBUs do not parse")]
  Obus,
  /// Matroska WavPack whose blocks do not parse whole: Matroska refuses
  /// the packet after taking it, and drops bytes left over after the last
  /// whole block (`matroskaenc.c` 2775–2816; `wv.c` 30–53).
  #[error("the packet's WavPack blocks do not parse whole")]
  WavPackBlocks,
  /// MP4: skip-samples side data that would shorten the packet below
  /// nothing, which MP4 refuses after taking it (`movenc.c` 7311–7320) —
  /// a joined E-AC-3 sample's, its first packet's, against the duration of
  /// the packet that completes it, refused there (table C″ of the census).
  #[error("the packet's skip-samples side data leaves less than nothing")]
  SkipSamples,
  /// Matroska FLAC: new STREAMINFO side data of another length than the
  /// track's, which Matroska refuses after taking the packet
  /// (`matroskaenc.c` 3085–3093).
  #[error("the packet's new FLAC STREAMINFO is not the length of the track's")]
  FlacStreamInfo,
  /// Matroska ProRes not opening on its frame's atom — its size, the
  /// packet's, and `icpf` — or holding a second one where the first ends:
  /// Matroska drops a packet's first 8 bytes (`matroskaenc.c` 3584–3587,
  /// 695–696), and its reader puts an atom back only where none follows
  /// (`matroskadec.c` 3870–3889, 4176–4178), so such a packet reads back
  /// otherwise than it was written.
  #[error("the ProRes packet does not open on its own frame atom")]
  ProResAtom,
  /// Matroska and WebM WebVTT: a cue whose text ends in a line break, or
  /// whose identifier or settings hold one or are given empty. They are
  /// written on lines of their own (`webm_reformat_vtt`, `matroskaenc.c`
  /// 2828–2855) and read back by line, the text's last line breaks dropped
  /// and an empty identifier or settings not given (`matroska_parse_webvtt`,
  /// `matroskadec.c` 3890–3990).
  #[error("the WebVTT cue does not read back from its lines as itself")]
  WebVttCue,
  /// MP4 AC-3 and E-AC-3: a packet whose first frame's header libavcodec's
  /// parser refuses (`ff_ac3_parse_header`, `ac3_parser.c` 288–395 in
  /// FFmpeg 9.0.1). MP4 drops it at the stream's start and refuses it,
  /// after taking it, later (`handle_eac3`, `movenc.c` 481–493).
  #[error("the packet's AC-3 frame header does not parse")]
  Ac3Header,
  /// MP4 E-AC-3: a track's first packet whose frames after its first are
  /// not dependent frames that tile it, which MP4 refuses after taking it
  /// (`movenc.c` 551–575).
  #[error("the packet's E-AC-3 frames after the first are not dependent frames that tile it")]
  Ac3Substreams,
  /// MP4 E-AC-3: a frame whose blocks would take the sample MP4 is making
  /// of frames of fewer than six blocks past six, which MP4 refuses after
  /// taking it (`movenc.c` 577–585).
  #[error("the packet's E-AC-3 blocks run past a sample of six")]
  Ac3Blocks,
  /// MP4 E-AC-3: a frame of fewer than six blocks whose bytes would take
  /// the sample MP4 joins such frames into past the largest packet
  /// `av_grow_packet` grows — `INT_MAX` less the padding — which refuses it
  /// after taking the frame (`handle_eac3`, `movenc.c` 592–595;
  /// `libavcodec/packet.c` 121–127; table C″ of the census).
  #[error("the E-AC-3 frames MP4 would join into one sample are larger than a packet holds")]
  Ac3SampleSize,
  /// MP4 AC-3: the track's first frame — the one MP4 writes the track's
  /// `dac3` from — of a bit stream id past 8, which `dac3` does not define
  /// and MP4's trailer refuses, failing the whole file (`mov_write_ac3_tag`,
  /// `movenc.c` 431–437; `handle_eac3` keeps the first frame's id, 536–548).
  /// Refused at the first packet, which decides it.
  #[error("the AC-3 frame's bit stream id is past 8, which MP4's dac3 does not define")]
  Ac3BitStreamId,
  /// New extradata the container would write as a configuration it then
  /// refuses, or one that changes how it writes the packets after it
  /// (table C of the census): MP4 Opus's not an OpusHead its `dOps` writer
  /// takes (`movenc.c` 877–910) — MP4 makes a sample description of new
  /// extradata, its writer's at the trailer (7044–7074); MP4 H.264, HEVC,
  /// VVC or LCEVC new extradata of the other form than the track's — Annex
  /// B or not — which turns the length-prefix rewrite on or off for the
  /// packets after it (7087–7158); Matroska AAC's not an AudioSpecificConfig
  /// libavcodec parses, or larger than the space Matroska's header reserved
  /// for the track's configuration (`mkv_check_new_extra_data`,
  /// `matroskaenc.c` 3065–3082; `mkv_update_codecprivate`, 1334–1365) —
  /// the caller's, or the one AAC's filter makes of the first frame it
  /// converts — each refused after taking the packet.
  #[error(
    "the packet's new extradata is a configuration the container would refuse or write otherwise"
  )]
  NewExtradata,
  /// VP9, the automatic filters on: a packet the superframe filter would
  /// refuse after taking it — an empty one, or a superframe, while it
  /// holds hidden frames (`vp9_superframe.c` 111–118, 143–147) — or a
  /// seventh hidden frame in a row, which the filter would hold only to
  /// refuse every packet after it (151–161) and drop all seven when the
  /// stream ends (198–207).
  #[error("the VP9 packet cannot follow the hidden frames the automatic filter holds")]
  Superframe,
  /// Matroska PGS, the automatic filters on: segments the filter would
  /// write marked corrupt — a display set it completes with no
  /// presentation segment, or segments that do not parse
  /// (`pgs_frame_merge.c` 59–68, 93–130).
  #[error("the PGS segments do not make a display set the automatic filter writes whole")]
  PgsSegments,
  /// Matroska and WebM video: HDR10+ side data
  /// (`AV_PKT_DATA_DYNAMIC_HDR10_PLUS`) that is not an `AVDynamicHDRPlus`
  /// the writer serializes whole and the reader reads back as itself. The
  /// writer casts the entry's bytes to the struct without asking their
  /// size, and libavutil's serializer trusts every count and divides by
  /// every denominator (`matroskaenc.c` 2947–2969;
  /// `hdr_dynamic_metadata.c` 237–395): an entry of another size than the
  /// linked libavutil's struct, a count past its array, a zero
  /// denominator, a value its bits cannot hold — each rule in
  /// [`mediaio_ffmpeg::hdr_metadata::check_hdr10_plus`], table A of the
  /// census (see [the module](self)).
  #[error("the packet's HDR10+ side data is not a struct Matroska writes whole and reads back")]
  DynamicHdrPlus,
  /// Matroska and WebM video: SMPTE ST 2094 application 5 side data
  /// (`AV_PKT_DATA_DYNAMIC_HDR_SMPTE_2094_APP5`) that is not an
  /// `AVDynamicHDRSmpte2094App5` the writer serializes whole and the reader
  /// reads back as itself (`matroskaenc.c` 2970–2990;
  /// `hdr_dynamic_metadata.c` 563–706) — each rule in
  /// [`mediaio_ffmpeg::hdr_metadata::check_smpte2094_app5`].
  #[error(
    "the packet's SMPTE 2094 APP5 side data is not a struct Matroska writes whole and reads back"
  )]
  DynamicHdrApp5,
  /// VP9, wherever the superframe filter is inserted (table B): a frame
  /// whose superframe — with the hidden frames the filter holds — would pass
  /// the largest packet libavcodec allocates. The filter sums the frames'
  /// sizes in 32 bits and copies them into what it allocated by that sum
  /// (`merge_superframe`, `vp9_superframe.c` 52–98): past 4 GiB the sum
  /// wraps and the copy runs past the allocation, and below it
  /// `av_new_packet` refuses the superframe after the frames were taken.
  /// Refused at the frame that decides it: a hidden frame no frame shown
  /// after it could join within the bound, or the frame shown that would
  /// pass it.
  #[error("the VP9 superframe the automatic filter would join is larger than a packet holds")]
  SuperframeSize,
  /// Matroska PGS, the automatic filters on: segments whose display set,
  /// with what the filter holds of it, would pass the largest packet
  /// `av_grow_packet` makes, which refuses it after the segments were
  /// taken (`pgs_frame_merge.c` 137–140; `libavcodec/packet.c` 121–127) —
  /// or would leave it no room for its end segment, so that no packet after
  /// could complete it.
  #[error("the PGS display set the automatic filter would join is larger than a packet holds")]
  DisplaySetSize,
  /// MP4, Matroska and WebM, a track whose packets are rewritten from
  /// Annex B to length prefixes ([`Rewrite::LengthPrefixed`]): a packet
  /// whose rewritten size — a 4-byte length for each start code — would
  /// pass what the rewriter counts in its `int` (`nal_parse_units`,
  /// `nal.c` 74–111) and the dynamic buffer it writes into holds
  /// (`aviobuf.c` 1281–1311).
  #[error("the packet, rewritten with length prefixes, is larger than the rewriter counts")]
  RewrittenSize,
  /// Matroska and WebM: a packet whose block — its payload as written and
  /// the side data written with it, or each block its track's filter puts
  /// out with it, VP9's superframe and PGS's display set, and what the
  /// filter holds at the least it can come to — is larger than a cluster's
  /// buffer takes beside the block before it (`MATROSKA_BLOCK_BYTES_MAX`;
  /// table B of the census): the buffer refuses the write after the
  /// filter took the packets, and the cluster is written cut.
  #[error("the packet is larger than a Matroska cluster holds")]
  ClusterSize,
  /// MP4 TTML from libavcodec's TTML encoder: a paragraph that would take
  /// the document MP4 joins the track's paragraphs into at the trailer
  /// ([`Rewrite::ParagraphsJoined`]) past the largest packet libavcodec
  /// allocates, which the trailer refuses (`movenc_ttml.c` 236–317).
  /// Refused at the paragraph that would pass it.
  #[error("the TTML paragraph would take the document MP4 joins past what a packet holds")]
  DocumentSize,
  /// Two side-data entries of one kind. libavcodec keeps one entry a kind:
  /// a later one's bytes replace the earlier one's in its place
  /// (`av_packet_add_side_data`, `libavcodec/packet.c` 197–227 in FFmpeg
  /// 9.0.1), so the earlier would be dropped unsaid. Refused in every
  /// container.
  #[error("the packet carries two side-data entries of one kind")]
  SideDataRepeated,
  /// A packet marked `AV_PKT_FLAG_TRUSTED`, whose payload libavcodec may
  /// follow pointers out of: no packet so marked is built into FFmpeg
  /// ([`mediaio_ffmpeg::TrustedPayload`]; the base's one judgment of what
  /// a packet alone decides, `mediaio_ffmpeg::boundary::check_packet_build`).
  #[error("the packet is marked trusted, which no packet into FFmpeg may be")]
  Trusted,
  /// A payload past the muxer's [`PacketLimits`], or past the largest
  /// packet libavcodec allocates (`packet_alloc`, `libavcodec/packet.c`
  /// 83–95 in FFmpeg 9.0.1), which the packet's builder refuses.
  #[error("the packet's payload is larger than the muxer's limits or than a packet holds")]
  PayloadTooLarge,
  /// Side data past the caps the read side keeps — 64 entries, 256 KiB in
  /// all — which the packet's builder refuses.
  #[error("the packet's side data passes the 64 entries or 256 KiB a packet carries")]
  SideDataBudget,
  /// A side-data kind the linked libavcodec does not name, which the
  /// packet's builder refuses rather than hand C a kind nothing downstream
  /// reads.
  #[error("the packet carries side data of a kind the linked libavcodec does not name")]
  SideDataUnnamed,
}

/// Why a muxer did not open. Nothing was written.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum MuxOpenError {
  /// libavformat has no muxer by that name.
  #[error("libavformat has no muxer named {name:?}")]
  UnknownContainer {
    /// The name.
    name: ContainerName,
  },
  /// The muxer writes no byte stream of its own, so it cannot write into
  /// a sink.
  #[error("the {name:?} muxer writes no byte stream")]
  NoByteStream {
    /// The name.
    name: ContainerName,
  },
  /// A track the demuxer could not classify; there is no stream kind to
  /// write it as.
  #[error("track {} is of no kind a muxer can write", .track.get())]
  UnknownTrackKind {
    /// The track.
    track: TrackIndex,
  },
  /// The track's ticket names another codec than its description.
  #[error("track {}'s codec parameters are for another codec", .track.get())]
  TicketMismatch {
    /// The track.
    track: TrackIndex,
  },
  /// A track whose kind is not its parameters' media type, or not its
  /// codec's — a video track over an audio stream's parameters. Its stream
  /// would be made of the ticket's media type ([`StreamMedia::Timed`]),
  /// and its packets judged by the track's kind, so it is refused before
  /// any stream is added (table D of the census, see [the module](self)).
  /// An attachment is the exception: its road makes its stream's media
  /// type itself.
  #[error("track {}'s kind is not its parameters' media type", .track.get())]
  MediaTypeMismatch {
    /// The track.
    track: TrackIndex,
  },
  /// A codec the muxer says it cannot store (`avformat_query_codec`), or
  /// one whose header it refuses though the query admits it: TrueHD in
  /// MP4, experimental there (`mov_init`, `movenc.c` 8580–8587 in FFmpeg
  /// 9.0.1), and ATRAC3, Cook, RealAudio 2.0, SIPR and RealVideo 1.0, 2.0
  /// and 3.0 in Matroska, which it does not yet write (`mkv_init`,
  /// `matroskaenc.c` 3518–3530).
  #[error("the container cannot store track {}'s codec {codec:?}", .track.get())]
  CodecNotCarried {
    /// The track.
    track: TrackIndex,
    /// The codec.
    codec: CodecId,
  },
  /// MP4 PCM of more than one channel whose layout MP4 cannot write: a
  /// channel with no position in ISO/IEC 23001-8's table — any of an
  /// unspecified layout's among them — which fails the `chnl` box at the
  /// trailer, and the whole file with it (`mov_write_chnl_tag`, `movenc.c`
  /// 1299–1323, written for `ipcm` and `fpcm` from two channels, 1521–1528;
  /// `mov_chan.c` 653–766).
  #[error("track {}'s channel layout has a channel MP4's PCM cannot place", .track.get())]
  ChannelLayout {
    /// The track.
    track: TrackIndex,
  },
  /// An audio track of a muxer that rechunks PCM at its start — DAUD, GXF
  /// and the MXF muxers insert `pcm_rechunk` (table B of the census, see
  /// [the module](self)) — whose channels times its codec's bits per sample
  /// pass C's `int`, which the filter's start multiplies them in, an
  /// overflow (`pcm_rechunk.c` 49–50 in FFmpeg 9.0.1).
  #[error("track {}'s sample size passes what PCM rechunking counts in an int", .track.get())]
  SampleSize {
    /// The track.
    track: TrackIndex,
  },
  /// MP4's cover, whose one packet its builder would refuse after the
  /// header is written ([`PayloadRule::Trusted`],
  /// [`PayloadRule::PayloadTooLarge`]): refused here, nothing written.
  #[error("cover track {}'s picture is one its packet's builder refuses: {rule}", .track.get())]
  Cover {
    /// The track.
    track: TrackIndex,
    /// What its packet breaks.
    rule: PayloadRule,
  },
  /// An attachment with no filename, which Matroska requires
  /// (`matroskaenc.c` 2538–2543 in FFmpeg 9.0.1).
  #[error("attachment track {} has no filename, which Matroska requires", .track.get())]
  AttachmentWithoutFilename {
    /// The track.
    track: TrackIndex,
  },
  /// An attachment whose MIME type is neither stated nor known for its
  /// codec, which Matroska requires (`matroskaenc.c` 3606–3613).
  #[error("attachment track {} has no MIME type, which Matroska requires", .track.get())]
  AttachmentWithoutMimeType {
    /// The track.
    track: TrackIndex,
  },
  /// A track's text holding a NUL, which a C string cannot carry.
  #[error("track {}'s {text:?} holds a NUL byte", .track.get())]
  TextWithNul {
    /// The track.
    track: TrackIndex,
    /// Which text.
    text: OutputText,
  },
  /// A codec whose configuration — its extradata — the container needs and
  /// the track does not carry: MP4 would take an H.264, HEVC, VVC, LCEVC or
  /// EVC track's first packet as it and read every packet by it (`movenc.c`
  /// 7024–7041, 7087–7158); Matroska and WebM refuse every AV1 packet of a
  /// track with none (`matroskaenc.c` 3098–3109). An AAC track's comes,
  /// with the automatic filters on, from the ADTS header of its first
  /// frame ([`AutomaticFilter::AacAdtsToAsc`]); with them off — or in a
  /// build whose Matroska muxer is its audio one alone, which inserts no
  /// AAC filter (table B) — it is refused here: Matroska refuses every AAC
  /// packet of a track with none (3080–3083), and MP4 writes its decoder
  /// configuration from the track's alone (`mov_write_esds_tag`, `movenc.c`
  /// 765–816; 7412–7429).
  #[error("track {} carries no configuration for {codec:?}, which this container needs", .track.get())]
  ConfigurationMissing {
    /// The track.
    track: TrackIndex,
    /// The codec.
    codec: CodecId,
  },
  /// A configuration the container's writer refuses at its trailer, after
  /// every packet was written — the whole file lost (table C of the
  /// census): MP4 FLAC's not one 34-byte STREAMINFO (`dfLa`, `movenc.c`
  /// 857–876), MP4 Opus's shorter than its OpusHead or than the channel
  /// mapping table it names (`dOps`, 877–910), MP4 TTML's from libavcodec's
  /// TTML encoder whose header strings are not two terminated strings
  /// (`ttml_set_header_values_from_extradata`, `ttmlenc.c` 82–122, which
  /// the trailer's joined document is written by).
  #[error("track {}'s configuration is one {codec:?} in this container refuses at its trailer", .track.get())]
  ConfigurationInvalid {
    /// The track.
    track: TrackIndex,
    /// The codec.
    codec: CodecId,
  },
  /// MP4 video whose parameters state a frame cropping that leaves no
  /// picture — as wide as the frame, or as tall — which MP4's trailer
  /// refuses, failing the whole file (`mov_write_video_tag`, `movenc.c`
  /// 3024–3036).
  #[error("track {}'s frame cropping leaves no picture, which MP4's trailer refuses", .track.get())]
  Cropping {
    /// The track.
    track: TrackIndex,
  },
  /// A chapter whose title holds a NUL.
  #[error("chapter {chapter}'s title holds a NUL byte")]
  ChapterTitleWithNul {
    /// The chapter's position in the table.
    chapter: usize,
  },
  /// MP4: a chapter without a title. MP4 writes every chapter into its
  /// Nero list and only a titled one into its chapter track
  /// (`mov_create_chapter_track`, `movenc.c` 7791–7807), and its reader
  /// lays the track's chapters over the list's by position, so the
  /// chapters after it read back as others — table E of the census.
  #[error("chapter {chapter} has no title, which MP4's chapter track needs")]
  ChapterWithoutTitle {
    /// The chapter's position in the table.
    chapter: usize,
  },
  /// MP4: a chapter title longer than the 16-bit length MP4's chapter
  /// track writes it with (`movenc.c` 7797–7801), which reads back cut.
  #[error("chapter {chapter}'s title is longer than MP4's chapter track holds")]
  ChapterTitleTooLong {
    /// The chapter's position in the table.
    chapter: usize,
  },
  /// A chapter that starts before zero or ends before it starts — a
  /// span no container writes (Matroska fails its header on one,
  /// `matroskaenc.c` 2445–2451) — or one its own time base cannot count.
  #[error("chapter {chapter}'s span runs backwards or before zero")]
  ChapterSpan {
    /// The chapter's position in the table.
    chapter: usize,
  },
  /// MP4: a chapter whose sample in the chapter track MP4 makes at its
  /// header MP4 would refuse or rewrite — table C′ of the census (see [the
  /// module](self)). MP4 counts each chapter in its movie timescale, which
  /// its streams' time bases make, and hands each sample to its packet
  /// writer, ignoring its answer (`mov_create_chapter_track`, `movenc.c`
  /// 7727–7815): a chapter longer than `i32::MAX` ticks loses its sample and
  /// lays every later title over another chapter; one starting before the
  /// last chapter, or `i32::MAX` ticks or more after it, is moved to one tick
  /// after (`check_pkt`, 6897–6930).
  #[error("chapter {chapter}'s sample in MP4's chapter track would be refused or moved")]
  ChapterSample {
    /// The chapter's position in the table.
    chapter: usize,
  },
  /// The muxer's own fault.
  #[error(transparent)]
  Fault(#[from] MuxFault),
}

/// What the muxer keeps of a track while it writes.
#[derive(Debug, Clone)]
struct TrackState {
  kind: TrackKind,
  /// The stream the track writes to, and libavformat's time base for it
  /// — `None` for a track that takes no packet.
  stream: Option<(usize, Timebase)>,
  /// Whether libavformat's equal-decode-time rule applies.
  strict: bool,
  /// The last decode time written, as an instant.
  last: Option<Timestamp>,
  /// The decode time, counted in the stream's time base, of the last
  /// packet libavformat was handed on the track: the last its automatic
  /// filter put out ([`Emits`]).
  last_ticks: Option<i64>,
  /// The decode time of that packet, as an instant.
  handed: Option<Timestamp>,
  /// PGS: what the packets this muxer holds of the track put out, once
  /// handed ([`Plan::pending`]).
  pending: Vec<Timed>,
  /// MP4: the last sample MP4 wrote on the track, which `check_pkt`
  /// counts the next packet's gap from — moved only where MP4 writes one,
  /// not by a packet it holds (table C″ of the census).
  sample: Option<Sample>,
  /// The rules the container's packet path holds this track's packets
  /// to (see [the module](self)).
  rules: PacketRules,
  /// The automatic filter libavformat inserts for the track, and what it
  /// holds.
  filter: Filter,
  /// Which side data the container writes on the track.
  sides: SideRules,
  /// Whether the track's filter has put out a packet yet: the container's
  /// first.
  written: bool,
  /// MP4 `mov_text`: the samples MP4 has written of the track.
  cues: Cues,
  /// MP4 AC-3 and E-AC-3: what MP4 has learned of the track's frames.
  ac3: Ac3State,
  /// MP4 TTML from libavcodec's TTML encoder: the bytes of the document
  /// MP4 joins its paragraphs into, so far.
  document: u64,
}

/// A sample MP4 wrote ([`TrackState::sample`]): its decode time, and its
/// duration as offered — a joined E-AC-3 sample's, the sum of its
/// packets', where each offered one — where above zero, in ticks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Sample {
  dts: i64,
  offered: Option<i128>,
}

/// MP4 `mov_text`: what MP4 has written of a track — the samples
/// `check_pkt` counts a cue's gap from, and the end it marks the last cue
/// with (`movenc.c` 7246–7322, 7646–7656; table C″ of the census).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Cues {
  /// The decode time of the track's first sample — zero, where MP4 wrote
  /// the empty one there before the first cue: `start_dts` (7288–7290).
  start: Option<i64>,
  /// `track_duration`: the last sample's decode time and duration,
  /// counted from `start` (7321) — zero before the first. MP4 writes the
  /// empty sample marking the last cue's end at this count, read as a
  /// decode time (7652, 9029).
  end: i128,
  /// The decode time of the last sample written — a cue, or the empty
  /// one after it (7252).
  last: Option<i128>,
  /// Whether the last sample is the empty one at a cue's end
  /// (`last_sample_is_subtitle_end`, 7322, 7653).
  ended: bool,
  /// The last cue written: its decode time and its duration as offered,
  /// in ticks.
  cue: Option<(i64, i64)>,
}

/// What MP4 does with a `mov_text` cue ([`Cues::take`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CueStep {
  /// The decode time `check_pkt` counts the cue's gap from: the last
  /// sample written, the empty one where MP4 writes it first — none
  /// before the track's first (6900–6909).
  reference: Option<i128>,
  /// Whether the cue starts before the end MP4 marks the last one with —
  /// zero, before the first (`end`): MP4 writes no empty sample before it,
  /// so the last cue is shortened to it; and a packet of another track
  /// whose decode time, counted in its own ticks, passes that end, which
  /// MP4 may take first (`mux.c` 957–1003), makes MP4 write the empty
  /// sample there first, and the cue's decode time is then moved past it
  /// and its presentation time dropped (6917–6923).
  early: bool,
  /// The track after the cue.
  next: Cues,
}

impl Cues {
  /// What MP4 does with the track's next cue, at `dts` of `duration` ticks,
  /// where no packet of another track wrote the empty sample first: the
  /// empty sample at `end` first, where the cue starts past it and the last
  /// sample is not already one (7649–7653); then the cue.
  fn take(self, dts: i64, duration: i64) -> CueStep {
    let at = i128::from(dts);
    let mut track = self;
    if track.end < at && (track.start.is_none() || !track.ended) {
      // `mov_write_subtitle_end_packet` (7479–7497): a sample of its own at
      // `end`, its duration zero, which the cue follows at once. Before any,
      // `end` is zero, and the track starts there.
      track.start.get_or_insert(0);
      track.last = Some(track.end);
    }
    let reference = track.last;
    let start = *track.start.get_or_insert(dts);
    let next = Self {
      start: Some(start),
      end: at - i128::from(start) + i128::from(duration),
      last: Some(at),
      ended: false,
      cue: Some((dts, duration)),
    };
    CueStep {
      reference,
      early: at < self.end,
      next,
    }
  }
}

/// What the other timed tracks of a file can make MP4 write before a
/// `mov_text` cue ([`CarrierMuxer::neighbours`]): MP4 writes the empty
/// sample marking the last cue's end before a packet, of any track, whose
/// decode time — counted in that track's own ticks — passes the cue
/// track's `track_duration`, counted in its own (`movenc.c` 7646–7653), in
/// the order libavformat's interleaving hands the packets on (`mux.c`
/// 940–1003; table C″ of the census).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Neighbours {
  /// Whether another track counts in finer ticks than the cue track's: a
  /// packet of it before the end in time may pass it in ticks.
  finer: bool,
  /// The latest decode time handed on another track.
  latest: Option<Timestamp>,
}

/// libavformat's `max_interleave_delta`, its default — the muxing context
/// keeps it — in microseconds: past it, the interleaving hands on the
/// earliest packet it holds before every stream holds one, the times it
/// compares each rounded to the microsecond (`mux.c` 970–1003;
/// `options_table.h` 87).
const INTERLEAVE_DELTA_MICROSECONDS: i128 = 10_000_000;

impl Neighbours {
  /// Whether no packet of another track can make MP4 mark the last cue's
  /// end — at `end` ticks of `time_base`, `track_duration` read as a decode
  /// time — before a cue that starts before it. None counts in finer ticks,
  /// so one past the end in ticks lies past it in time, and sorts after the
  /// cue (`interleave_compare_dts`, `mux.c` 909–937); and none handed lies
  /// past the end by more than the interleaving's delay, the one road on
  /// which libavformat hands on a packet before an earlier one it has not
  /// yet been handed (957–1003): a packet is handed on before every stream
  /// holds one only where the ones it holds span more than the delay.
  fn leave(self, end: i128, time_base: Timebase) -> bool {
    // Microseconds of `ticks` of `base`, rounded down: a count of `i64` or
    // of `track_duration`, times a numerator of `i32` and 10⁶, fits `i128`.
    let micros = |ticks: i128, base: Timebase| {
      (ticks * i128::from(base.num()) * 1_000_000).div_euclid(i128::from(base.den().get()))
    };
    // The latest within the end and the delay, two microseconds short of it
    // for the two roundings down and libavformat's own to the nearest.
    !self.finer
      && self.latest.is_none_or(|latest| {
        micros(i128::from(latest.pts()), latest.timebase()) + 2
          <= micros(end, time_base) + INTERLEAVE_DELTA_MICROSECONDS
      })
  }
}

/// libavformat's shift of every stream's times (`handle_avoid_negative_ts`,
/// `mux.c` 636–717; table G of the census): at the first packet it writes,
/// where that packet, or one its interleaving holds then, lies before the
/// earliest its stream's container writes, it moves every stream's times
/// later by the earliest one's distance before it, each stream's distance
/// rounded up to its own tick. A packet after that write that the shift
/// leaves before the earliest is written there ("failed to avoid negative
/// timestamp", 692–715).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Shift {
  /// The times judged: presentation times in Matroska and WebM (3538–3541),
  /// decode times in another container and where Matroska writes a video
  /// track in VFW mode (`matroskaenc.c` 2077–2081).
  by_pts: bool,
  /// Whether libavformat's interleaving hands every packet on as it comes:
  /// fewer than two streams it interleaves (`mux.c` 327–335).
  passthrough: bool,
  /// The earliest time handed before the first write, less its stream's
  /// earliest, where it lies before it: what the shift moves every stream
  /// by.
  earliest: Option<Timestamp>,
  /// Whether libavformat has made its first write, which settles the shift.
  written: bool,
  /// What the interleaving holds before its first write, by track — none
  /// for a track whose stream takes no packet.
  held: Vec<Option<Held>>,
  /// The earliest decode time it holds, in microseconds: its queue's head.
  head_us: Option<i128>,
}

/// What libavformat's interleaving holds of a stream before its first
/// write ([`Shift`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Held {
  /// Whether libavformat counts the stream among those it interleaves:
  /// any but SMPTE 2038 data (`mux.c` 327–329). A stream it does not count
  /// still counts among those that hold a packet (957–958).
  interleaved: bool,
  /// Whether it writes before every stream holds a packet, without one of
  /// this — any stream's but VP8's, VP9's and SMPTE 2038 data's (959–963).
  may_lack: bool,
  /// A subtitle stream, which the interleaving's delay leaves aside (988).
  subtitle: bool,
  /// The decode time of the last packet of it held, in microseconds.
  last_us: Option<i128>,
}

/// `ticks` of `time_base` in microseconds, as `av_rescale_q` rounds: to
/// the nearest, half away from zero.
fn micros(ticks: i64, time_base: Timebase) -> i128 {
  let numerator = i128::from(ticks) * i128::from(time_base.num()) * 1_000_000;
  let den = i128::from(time_base.den().get());
  let away = if numerator < 0 { -(den / 2) } else { den / 2 };
  (numerator + away) / den
}

impl Shift {
  /// The shift `family`'s muxer, named `muxer`, makes over `tracks` — each
  /// track's kind, codec, and whether it has a stream — where this muxer
  /// mirrors it; `None` where libavformat makes none (MP4's
  /// `AVFMT_TS_NEGATIVE`), or where this muxer does not mirror it: a muxer
  /// that interleaves its own way (GXF, the MXF muxers: `gxfenc.c` 1030,
  /// `mxfenc.c` 3721–3756), and Matroska with a video track of no native
  /// Matroska id that is not certainly in VFW mode, which may make
  /// libavformat judge decode times or presentation times (`matroskaenc.c`
  /// 2025–2081). There the times before the earliest stay refused.
  fn of(
    family: Family,
    muxer: &str,
    shifts: bool,
    tracks: &[(TrackKind, CodecId, bool)],
  ) -> Option<Self> {
    let by_pts = match family {
      Family::Mp4 => return None,
      Family::Matroska | Family::WebM => {
        let mut by_pts = true;
        for &(kind, codec, _) in tracks {
          if kind != TrackKind::Video || MATROSKA_NATIVE_VIDEO.contains(&codec) {
            continue;
          }
          // JPEG 2000 is written in VFW mode whatever the table says
          // (2027–2030); any other codec of no native id in VFW or in
          // QuickTime mode, by tables this muxer does not mirror.
          if codec == CodecId::JPEG2000 {
            by_pts = false;
          } else {
            return None;
          }
        }
        by_pts
      }
      Family::Other if shifts && !matches!(muxer, "gxf" | "mxf" | "mxf_d10" | "mxf_opatom") => {
        false
      }
      Family::Other => return None,
    };
    let held: Vec<_> = tracks
      .iter()
      .map(|&(kind, codec, stream)| {
        stream.then_some(Held {
          interleaved: codec != SMPTE_2038,
          may_lack: codec != CodecId::VP8 && codec != CodecId::VP9 && codec != SMPTE_2038,
          subtitle: kind == TrackKind::Subtitle,
          last_us: None,
        })
      })
      .collect();
    let interleaved = held
      .iter()
      .flatten()
      .filter(|held| held.interleaved)
      .count();
    Some(Self {
      by_pts,
      passthrough: interleaved <= 1,
      earliest: None,
      written: false,
      held,
      head_us: None,
    })
  }

  /// What libavformat makes of a packet of `track` — its time judged `at`,
  /// its decode time `dts`, its stream's earliest `lowest`, each in ticks of
  /// `time_base`: the shift after it, or `None` where libavformat writes it
  /// before the earliest, which only a packet after the first write can
  /// be: one before it the shift lifts.
  fn take(
    &self,
    track: usize,
    (at, dts, lowest): (i64, i64, i64),
    time_base: Timebase,
  ) -> Option<Self> {
    let below = i128::from(at) - i128::from(lowest);
    if self.written {
      // Each stream's distance, rounded up to its own tick (`mux.c`
      // 676–680).
      let lift = self.earliest.map_or(0, |earliest| {
        let from = earliest.timebase();
        let numerator =
          -i128::from(earliest.pts()) * i128::from(from.num()) * i128::from(time_base.den().get());
        let den = i128::from(from.den().get()) * i128::from(time_base.num());
        if den <= 0 {
          0
        } else {
          (numerator + den - 1).div_euclid(den)
        }
      });
      return (below + lift >= 0).then(|| self.clone());
    }
    let mut next = self.clone();
    if below < 0 {
      let instant = Timestamp::new(i64::try_from(below).unwrap_or(i64::MIN), time_base);
      next.earliest = Some(
        next
          .earliest
          .map_or(instant, |earliest| earliest.min(instant)),
      );
    }
    let us = micros(dts, time_base);
    if let Some(Some(held)) = next.held.get_mut(track) {
      held.last_us = Some(us);
    }
    next.head_us = Some(next.head_us.map_or(us, |head| head.min(us)));
    next.written = next.passthrough || next.flushes();
    Some(next)
  }

  /// Whether the interleaving, holding what it holds, writes its first
  /// packet (`ff_interleave_packet_per_dts`, `mux.c` 939–1003): as many
  /// streams hold one as it interleaves; or each that holds none may be
  /// lacked, and the packets held span more than `max_interleave_delta` —
  /// its default, the context keeping it — the subtitles' left aside.
  fn flushes(&self) -> bool {
    let streams = self.held.iter().flatten();
    let holding = streams
      .clone()
      .filter(|held| held.last_us.is_some())
      .count();
    let interleaved = streams.clone().filter(|held| held.interleaved).count();
    if holding == interleaved {
      return true;
    }
    let lacking = streams
      .clone()
      .filter(|held| held.last_us.is_none() && held.may_lack)
      .count();
    let Some(head) = self.head_us else {
      return false;
    };
    interleaved == holding + lacking
      && streams
        .filter(|held| !held.subtitle)
        .filter_map(|held| held.last_us)
        .any(|last| last - head > INTERLEAVE_DELTA_MICROSECONDS)
  }
}

/// The video codecs Matroska writes with ids of its own
/// (`ff_mkv_codec_tags`, `matroska.c` 80–105), JPEG 2000 but written in VFW
/// mode (`matroskaenc.c` 2027–2030); RealVideo 1.0 to 3.0 are refused at open.
const MATROSKA_NATIVE_VIDEO: [CodecId; 23] = [
  CodecId::AV1,
  CodecId::from_raw(ffmpeg_next::ffi::AVCodecID::AV_CODEC_ID_AVS2 as i32),
  CodecId::from_raw(ffmpeg_next::ffi::AVCodecID::AV_CODEC_ID_AVS3 as i32),
  CodecId::from_raw(ffmpeg_next::ffi::AVCodecID::AV_CODEC_ID_DIRAC as i32),
  CodecId::FFV1,
  CodecId::MJPEG,
  CodecId::from_raw(ffmpeg_next::ffi::AVCodecID::AV_CODEC_ID_MPEG1VIDEO as i32),
  CodecId::MPEG2VIDEO,
  CodecId::MPEG4,
  CodecId::H264,
  CodecId::HEVC,
  CodecId::VVC,
  CodecId::from_raw(ffmpeg_next::ffi::AVCodecID::AV_CODEC_ID_MSMPEG4V3 as i32),
  CodecId::PRORES,
  CodecId::RV10,
  CodecId::RV20,
  CodecId::RV30,
  CodecId::from_raw(ffmpeg_next::ffi::AVCodecID::AV_CODEC_ID_RV40 as i32),
  CodecId::from_raw(ffmpeg_next::ffi::AVCodecID::AV_CODEC_ID_SNOW as i32),
  CodecId::from_raw(ffmpeg_next::ffi::AVCodecID::AV_CODEC_ID_THEORA as i32),
  CodecId::from_raw(ffmpeg_next::ffi::AVCodecID::AV_CODEC_ID_RAWVIDEO as i32),
  CodecId::VP8,
  CodecId::VP9,
];

/// `AV_CODEC_ID_SMPTE_2038`, data libavformat's interleaving neither counts
/// nor waits on (`mux.c` 327–329, 959–963).
const SMPTE_2038: CodecId =
  CodecId::from_raw(ffmpeg_next::ffi::AVCodecID::AV_CODEC_ID_SMPTE_2038 as i32);

/// The rows of the packet-rule table one track's packets meet in its
/// container, settled at open from the container and the stream's own
/// parameters ([`PacketRules::settle`]); each is refused by name, the
/// packet untouched, before libavformat is handed anything.
#[derive(Debug, Clone, Copy, Default)]
struct PacketRules {
  /// MP4's `check_pkt` and composition-offset rules: a decode time at
  /// least `i32::MAX` ticks after the last, a duration past `i32::MAX`,
  /// a presentation more than `i32::MAX` after its decode.
  mp4: bool,
  /// The earliest time the container writes without moving every stream.
  earliest: Option<Earliest>,
  /// Whether a time before it is refused here, where this muxer does not
  /// mirror libavformat's shift ([`Shift`]) — and whether a decode time is
  /// judged too, where the shift may judge either.
  before_zero: Option<BeforeZero>,
  /// A negative duration is refused: a subtitle's outside Matroska and
  /// WebM, which libavformat hands the container as it is
  /// ([`FfmpegMuxRefusal::NegativeDuration`]); any other libavformat or
  /// the container writes as zero, or as libavformat's guess: reported.
  negative_duration: bool,
  /// An empty packet is dropped.
  no_empty: bool,
  /// The stream reorders, and the container makes up or refuses a
  /// missing presentation time.
  needs_pts: bool,
  /// AAC whose configuration is stated must be raw: no ADTS header.
  raw_aac: bool,
  /// Every packet of the codec is a key frame.
  intra_only: bool,
  /// MP4 PCM: the bytes of one frame.
  frame_bytes: Option<u32>,
  /// AV1: the OBUs are parsed.
  obus: bool,
  /// Matroska WavPack: the blocks are parsed.
  wavpack: bool,
  /// MP4 `mov_text`: the empty samples MP4 writes around the cues
  /// (table C″).
  mov_text: bool,
  /// MP4: skip-samples side data is applied, with the codec's frame size
  /// and sample rate.
  skip_samples: Option<(i32, i32)>,
  /// Matroska FLAC: the STREAMINFO length new extradata must keep.
  streaminfo_len: Option<usize>,
  /// Matroska ProRes: each packet opens on its frame's atom.
  prores: bool,
  /// Matroska and WebM WebVTT: each cue reads back from its lines.
  webvtt: bool,
  /// MP4 AC-3 and E-AC-3: each packet is one MP4's AC-3 handling writes,
  /// and how it takes them.
  ac3: Option<Ac3Rules>,
  /// Matroska and WebM video: HDR10+ and APP5 side data is a struct the
  /// writer serializes whole (table A of the census).
  hdr_metadata: bool,
  /// MP4, Matroska and WebM: the packets are rewritten from Annex B to
  /// length prefixes, which the rewriter counts in an `int`.
  length_prefixed: bool,
  /// Matroska and WebM: each block fits a cluster's buffer.
  matroska_block: bool,
  /// MP4 TTML from libavcodec's TTML encoder: the bytes the joined
  /// document takes beside its paragraphs.
  ttml_document: Option<u64>,
  /// What new extradata the container takes on the track (table C).
  new_extradata: ExtradataRule,
}

/// What a container takes of new extradata on a track, beside what the
/// side-data table reports of it (table C of the census, see [the
/// module](self)).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum ExtradataRule {
  /// Whatever it is.
  #[default]
  Any,
  /// MP4 Opus: an OpusHead its `dOps` writer takes.
  OpusHead,
  /// MP4 H.264, HEVC, VVC or LCEVC: of the track's form — Annex B, which
  /// the packets after it are rewritten from, or not.
  Form { codec: CodecId, annex_b: bool },
  /// Matroska AAC: an AudioSpecificConfig libavcodec parses, within the
  /// space Matroska's header reserved.
  AudioSpecificConfig { reserved: usize },
}

/// MP4 AC-3 and E-AC-3: how MP4's handling takes a track's packets
/// ([`ac3_packet`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Ac3Rules {
  /// E-AC-3, whose frames of fewer than six blocks MP4 joins; else AC-3.
  eac3: bool,
  /// The duration, in ticks, libavformat gives a packet that states none
  /// before MP4 is handed it (`guess_pkt_duration`, `mux.c` 604–633):
  /// AC-3's 1536 samples, or an E-AC-3 track's frame size where above one
  /// (`get_audio_frame_duration`, `libavcodec/utils.c` 578–800), counted at
  /// its sample rate as `av_rescale_q` rounds; else none.
  guessed: i64,
  /// Whether MP4's trailer reads each sample's duration from its sample
  /// table: a frame size other than one makes the track's audio of
  /// variable rate (`mov_init`, `movenc.c` 8539–8553), whose every sample's
  /// duration `mov_write_stts_tag` reads (3247–3270) — the last one's from
  /// the sum `handle_eac3` made, asserted within `INT_MAX` (1248–1256).
  durations_read: bool,
}

impl Ac3Rules {
  /// The rules of an MP4 AC-3 or E-AC-3 track whose stream holds `codec`,
  /// counted in `time_base`.
  fn of(codec: &StreamCodec, time_base: Timebase) -> Self {
    let eac3 = codec.codec() == CodecId::EAC3;
    let frame_size = codec.frame_size();
    let samples = if eac3 {
      if frame_size > 1 { frame_size } else { 0 }
    } else {
      1536
    };
    let sample_rate = codec.sample_rate();
    let guessed = if samples != 0 && sample_rate != 0 {
      rescale_nearest(
        i128::from(samples),
        i128::from(time_base.den().get()),
        i128::from(sample_rate) * i128::from(time_base.num()),
      )
    } else {
      0
    };
    Self {
      eac3,
      guessed,
      durations_read: frame_size != 1,
    }
  }

  /// A packet's duration as MP4 is handed it: as offered where above zero,
  /// else libavformat's guess.
  const fn handed(self, offered: i64) -> i64 {
    if offered > 0 { offered } else { self.guessed }
  }
}

/// A time before the earliest a container writes, where this muxer does
/// not mirror libavformat's shift ([`Shift`]): refused, as the time judged,
/// or both times where the shift may judge either.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BeforeZero {
  Refused,
  RefusedEither,
}

/// The earliest time a container writes without moving every stream, in
/// the stream's ticks, and which of a packet's times it judges.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Earliest {
  /// Matroska and WebM judge the presentation time (`matroskaenc.c`
  /// 3538–3541) — the decode time where a packet states none, as
  /// libavformat fills it (`mux.c` 531–532).
  Presentation(i64),
  /// Another container judges the decode time (`mux.c` 647).
  Decode(i64),
}

impl PacketRules {
  /// The rules `family`'s packet path applies to a timed track of `kind`
  /// whose stream holds `codec`, counted in `time_base`; `shifts` says
  /// whether libavformat moves early times in this container
  /// ([`Output::shifts_early_times`]).
  fn settle(
    family: Family,
    kind: TrackKind,
    codec: &StreamCodec,
    time_base: Timebase,
    shifts: bool,
  ) -> Self {
    let id = codec.codec();
    let mp4 = family == Family::Mp4;
    let matroska = family == Family::Matroska;
    let either_matroska = matroska || family == Family::WebM;
    let earliest = if either_matroska {
      Some(Earliest::Presentation(-matroska_codec_delay(
        kind, codec, time_base,
      )))
    } else if family == Family::Other && shifts {
      Some(Earliest::Decode(0))
    } else {
      None
    };
    Self {
      mp4,
      earliest,
      before_zero: None,
      negative_duration: kind == TrackKind::Subtitle && !either_matroska,
      no_empty: mp4 || (either_matroska && kind == TrackKind::Audio),
      needs_pts: family != Family::Other && codec.video_delay() > 0,
      raw_aac: (mp4 || matroska) && id == CodecId::AAC && !codec.extradata().is_empty(),
      // Matroska and WebM write every subtitle block as a key frame
      // (`matroskaenc.c` 3132–3133): the flag a subtitle packet offers is
      // not kept there either way.
      intra_only: id.intra_only() && !(either_matroska && kind == TrackKind::Subtitle),
      frame_bytes: if mp4 {
        mp4_frame_bytes(kind, codec)
      } else {
        None
      },
      obus: (mp4 || either_matroska) && id == CodecId::AV1,
      wavpack: matroska && id == CodecId::WAVPACK,
      mov_text: mp4 && id == CodecId::MOV_TEXT,
      skip_samples: (mp4 && codec.frame_size() != 0 && codec.sample_rate() > 0)
        .then(|| (codec.frame_size(), codec.sample_rate())),
      streaminfo_len: ((matroska || mp4) && id == CodecId::FLAC).then(|| codec.extradata().len()),
      prores: matroska && id == CodecId::PRORES,
      webvtt: either_matroska && id == CodecId::WEBVTT,
      ac3: (mp4 && (id == CodecId::AC3 || id == CodecId::EAC3))
        .then(|| Ac3Rules::of(codec, time_base)),
      hdr_metadata: either_matroska && kind == TrackKind::Video,
      length_prefixed: length_prefixed(family, id, codec.extradata()),
      matroska_block: either_matroska,
      ttml_document: None,
      new_extradata: if mp4 && id == CodecId::OPUS {
        ExtradataRule::OpusHead
      } else if mp4 && [CodecId::H264, CodecId::HEVC, CodecId::VVC, CodecId::LCEVC].contains(&id) {
        ExtradataRule::Form {
          codec: id,
          annex_b: length_prefixed(family, id, codec.extradata()),
        }
      } else if matroska && id == CodecId::AAC {
        // `mkv_assemble_native_codecprivate` reserves `MAX_PCE_SIZE` for a
        // track with no configuration, and the configuration's own length
        // for one with it (`matroskaenc.c` 1192–1196, 2174–2183).
        ExtradataRule::AudioSpecificConfig {
          reserved: if codec.extradata().is_empty() {
            MAX_PCE_SIZE
          } else {
            codec.extradata().len()
          },
        }
      } else {
        ExtradataRule::Any
      },
    }
  }
}

/// `MAX_PCE_SIZE` (`libavcodec/mpeg4audio.h`): the most bytes of a program
/// config element, which Matroska reserves for an AAC track's
/// configuration where the track states none.
const MAX_PCE_SIZE: usize = 320;

/// An audio track's codec delay in Matroska, in its stream's ticks: its
/// initial padding counted at its sample rate and rounded to the nearest
/// tick, as `av_rescale_q` rounds (`matroskaenc.c` 2089–2104) — the
/// distance before zero its earliest presentation may lie. Zero for any
/// other track.
fn matroska_codec_delay(kind: TrackKind, codec: &StreamCodec, time_base: Timebase) -> i64 {
  let padding = codec.initial_padding();
  if kind != TrackKind::Audio || padding <= 0 || codec.sample_rate() <= 0 {
    return 0;
  }
  // `padding / sample_rate` seconds in ticks of `num / den` seconds.
  rescale_nearest(
    i128::from(padding),
    i128::from(time_base.den().get()),
    i128::from(codec.sample_rate()) * i128::from(time_base.num()),
  )
}

/// `a × b / c` rounded to the nearest, half away from zero — `av_rescale`'s
/// `AV_ROUND_NEAR_INF` — for `a ≥ 0` and `b, c > 0`; saturated at `i64`.
fn rescale_nearest(a: i128, b: i128, c: i128) -> i64 {
  if c <= 0 {
    return 0;
  }
  let product = a.saturating_mul(b);
  i64::try_from((product.saturating_add(c / 2)) / c).unwrap_or(i64::MAX)
}

/// MP4's bytes per frame for a PCM-like audio codec — `bits per sample /
/// 8 × channels`, which `size / sample_size` counts a packet's samples by
/// — or `None` where MP4 counts samples otherwise (`movenc.c` 8538–8560):
/// a codec with frames of more than one sample, or of no fixed size.
fn mp4_frame_bytes(kind: TrackKind, codec: &StreamCodec) -> Option<u32> {
  if kind != TrackKind::Audio {
    return None;
  }
  let bits = codec.codec().bits_per_sample();
  let frame_size = codec.frame_size();
  if (frame_size == 0 && bits == 0) || frame_size > 1 {
    return None;
  }
  let bytes = (bits >> 3).checked_mul(u32::try_from(codec.channels()).ok()?)?;
  (bytes > 0).then_some(bytes)
}

/// What the packet rules read of a packet beside its times.
#[derive(Clone, Copy)]
struct Payload<'a> {
  bytes: &'a [u8],
  key: bool,
  flags: PacketFlags,
  side_data: &'a [SideDataEntry],
}

/// Whether `bytes` opens with an ADTS header — its first twelve bits set —
/// as MP4's and Matroska's checks read one (`movenc.c` 7076–7077;
/// `matroskaenc.c` 3636–3637).
fn adts_header(bytes: &[u8]) -> bool {
  bytes.len() > 2 && (u16::from_be_bytes([bytes[0], bytes[1]]) & 0xfff0) == 0xfff0
}

/// Whether `bytes` is a run of OBUs libavformat's AV1 filter parses whole
/// (`av1_filter_obus`, `av1.c` 33–81).
fn obus_parse(bytes: &[u8]) -> bool {
  let mut rest = bytes;
  while !rest.is_empty() {
    match obu_len(rest) {
      Some(len) => rest = &rest[len..],
      None => return false,
    }
  }
  true
}

/// The length of the OBU `buf` starts with — its header and its payload —
/// as libavcodec's `parse_obu_header` reads it (`av1_parse.h` 92–134 in
/// FFmpeg 9.0.1), or `None` where it refuses one: the forbidden bit set, a
/// header running past the ten bytes it reads or past the packet, or a
/// payload running past the packet. A header is a byte, an extension byte
/// where flagged, and a LEB128 size of up to eight bytes where flagged
/// (`leb.h` 57–68); without one the OBU runs to the packet's end.
fn obu_len(buf: &[u8]) -> Option<usize> {
  let window = &buf[..buf.len().min(10)];
  let first = *window.first()?;
  if first & 0x80 != 0 {
    return None;
  }
  let extension = first & 0x04 != 0;
  let has_size = first & 0x02 != 0;
  let mut header = 1;
  if extension {
    window.get(header)?;
    header += 1;
  }
  let payload = if has_size {
    let mut size: u64 = 0;
    for shift in 0..8 {
      let byte = *window.get(header)?;
      header += 1;
      size |= u64::from(byte & 0x7f) << (7 * shift);
      if byte & 0x80 == 0 {
        break;
      }
    }
    size
  } else {
    // The rest of the packet, as `buf_size - 1 - extension_flag` counts it.
    (buf.len() - header) as u64
  };
  let len = payload.checked_add(header as u64)?;
  if len > buf.len() as u64 {
    return None;
  }
  usize::try_from(len).ok()
}

/// Whether `bytes` is a run of WavPack blocks Matroska's reformatter
/// parses whole (`mkv_reformat_wavpack`, `matroskaenc.c` 2775–2816, over
/// `ff_wv_parse_header`, `wv.c` 30–53): each a 32-byte header tagged
/// `wvpk` whose block size lies in 24..=2^20, then the rest of its block,
/// and nothing left over.
fn wavpack_parse(bytes: &[u8]) -> bool {
  const HEADER: usize = 32;
  const BLOCK_LIMIT: u32 = 1 << 20;
  let mut rest = bytes;
  while !rest.is_empty() {
    if rest.len() < HEADER || &rest[..4] != b"wvpk" {
      return false;
    }
    let size = u32::from_le_bytes([rest[4], rest[5], rest[6], rest[7]]);
    if !(24..=BLOCK_LIMIT).contains(&size) {
      return false;
    }
    let block = (size - 24) as usize;
    match rest[HEADER..].get(block..) {
      Some(after) => rest = after,
      None => return false,
    }
  }
  true
}

/// The payload of `side_data`'s entry of `kind`, if the packet carries one.
fn side_data(side_data: &[SideDataEntry], kind: AVPacketSideDataType) -> Option<&[u8]> {
  side_data
    .iter()
    .find(|entry| entry.kind() == kind as i32)
    .map(SideDataEntry::data)
}

/// The samples MP4 drops from a packet's end by its skip-samples side
/// data, where the entry is one MP4 reads — of 10 bytes or more
/// (`movenc.c` 7311–7312): its second 32-bit field.
fn skip_end(side: &[SideDataEntry]) -> Option<u32> {
  let skip = side_data(side, AVPacketSideDataType::AV_PKT_DATA_SKIP_SAMPLES)?;
  (skip.len() >= 10).then(|| u32::from_le_bytes([skip[4], skip[5], skip[6], skip[7]]))
}

/// Whether MP4 would shorten the packet below nothing by its
/// skip-samples side data (`movenc.c` 7311–7320): its duration, at least
/// a frame of `frame_size` samples, less the samples to drop from its end,
/// each counted in `time_base` from `sample_rate` as `av_rescale_q`
/// rounds.
fn skips_below_nothing(
  side: &[SideDataEntry],
  duration: i64,
  skip: (i32, i32),
  time_base: Timebase,
) -> bool {
  skip_end(side).is_some_and(|end| end_below_nothing(end, duration, skip, time_base))
}

/// Whether MP4 would shorten a sample below nothing by skip samples that
/// drop `end` samples from its end, its end counted with `duration` — the
/// packet's own, or a joined E-AC-3 sample's last packet's (6940) — at
/// least a frame of `frame_size` samples, each counted in `time_base` from
/// `sample_rate` as `av_rescale_q` rounds (7311–7320).
fn end_below_nothing(
  end: u32,
  duration: i64,
  (frame_size, sample_rate): (i32, i32),
  time_base: Timebase,
) -> bool {
  let ticks = |samples: i128| {
    rescale_nearest(
      samples,
      i128::from(time_base.den().get()),
      i128::from(sample_rate) * i128::from(time_base.num()),
    )
  };
  let longest = ticks(i128::from(frame_size)).max(duration);
  i128::from(longest) - i128::from(ticks(i128::from(end))) < 0
}

/// Whether a ProRes packet reads back from Matroska as itself: it opens on
/// its frame's atom — its size, the packet's, and `icpf` — which Matroska
/// drops (`matroskaenc.c` 3584–3587, 695–696) and its reader puts back,
/// unless the bytes after the atom open on one of their own
/// (`matroskadec.c` 3870–3889, 4176–4178).
fn prores_atom(bytes: &[u8]) -> bool {
  bytes.len() >= 8
    && usize::try_from(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
      .is_ok_and(|size| size == bytes.len())
    && &bytes[4..8] == b"icpf"
    && bytes.get(12..16) != Some(b"icpf".as_slice())
}

/// Whether a WebVTT cue reads back from Matroska's and WebM's lines as
/// itself (`webm_reformat_vtt`, `matroskaenc.c` 2828–2855;
/// `matroska_parse_webvtt`, `matroskadec.c` 3890–3990): its identifier
/// and settings each absent or a line of their own, not empty, and its
/// text not ending in a line break.
fn webvtt_cue(bytes: &[u8], side: &[SideDataEntry]) -> bool {
  let line = |kind| match side_data(side, kind) {
    None => true,
    Some(text) => !text.is_empty() && !text.iter().any(|byte| matches!(byte, b'\r' | b'\n')),
  };
  line(AVPacketSideDataType::AV_PKT_DATA_WEBVTT_IDENTIFIER)
    && line(AVPacketSideDataType::AV_PKT_DATA_WEBVTT_SETTINGS)
    && !matches!(bytes.last(), Some(b'\r' | b'\n'))
}

/// The bytes of an AC-3 frame, by its frame size code and sample rate
/// code, in 16-bit words (`ff_ac3_frame_size_tab`, `ac3tab.c` 36–75 in
/// FFmpeg 9.0.1).
const AC3_FRAME_WORDS: [[u16; 3]; 38] = [
  [64, 69, 96],
  [64, 70, 96],
  [80, 87, 120],
  [80, 88, 120],
  [96, 104, 144],
  [96, 105, 144],
  [112, 121, 168],
  [112, 122, 168],
  [128, 139, 192],
  [128, 140, 192],
  [160, 174, 240],
  [160, 175, 240],
  [192, 208, 288],
  [192, 209, 288],
  [224, 243, 336],
  [224, 244, 336],
  [256, 278, 384],
  [256, 279, 384],
  [320, 348, 480],
  [320, 349, 480],
  [384, 417, 576],
  [384, 418, 576],
  [448, 487, 672],
  [448, 488, 672],
  [512, 557, 768],
  [512, 558, 768],
  [640, 696, 960],
  [640, 697, 960],
  [768, 835, 1152],
  [768, 836, 1152],
  [896, 975, 1344],
  [896, 976, 1344],
  [1024, 1114, 1536],
  [1024, 1115, 1536],
  [1152, 1253, 1728],
  [1152, 1254, 1728],
  [1280, 1393, 1920],
  [1280, 1394, 1920],
];

/// The channels each bit of an E-AC-3 dependent frame's channel map adds,
/// from its most significant (`ff_eac3_custom_channel_map_locations`,
/// `ac3tab.c` 164–181): a pair where the location is one.
const EAC3_MAP_CHANNELS: [u32; 16] = [1, 1, 1, 1, 1, 2, 2, 1, 1, 2, 2, 2, 1, 2, 1, 1];

/// An AC-3 or E-AC-3 frame's header, as much of it as MP4's handling
/// reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Ac3Header {
  /// The bit stream id.
  bsid: u8,
  /// `EAC3_FRAME_TYPE_*`: independent `0`, dependent `1`, an AC-3 frame
  /// `2`.
  frame_type: u8,
  /// The frame's bytes, as its header counts them.
  size: usize,
  /// The audio blocks it holds.
  blocks: u8,
}

/// Bits read from the front, as libavcodec's checked reader reads them:
/// past the end, zeros.
struct Bits<'a> {
  bytes: &'a [u8],
  at: usize,
}

impl Bits<'_> {
  fn read(&mut self, count: u32) -> u32 {
    let mut value = 0;
    for _ in 0..count {
      let byte = self.bytes.get(self.at / 8).copied().unwrap_or(0);
      value = (value << 1) | u32::from((byte >> (7 - self.at % 8)) & 1);
      self.at += 1;
    }
    value
  }

  fn skip(&mut self, count: usize) {
    self.at += count;
  }
}

/// The header of the AC-3 or E-AC-3 frame `bytes` opens with, or `None`
/// where libavcodec's parser refuses it (`ff_ac3_parse_header`,
/// `ac3_parser.c` 288–395, and `eac3_parse_header`, 130–288, in FFmpeg
/// 9.0.1): no sync word; a bit stream id past 16; for AC-3 a reserved
/// sample rate or a frame size code past 37; for E-AC-3 a reserved frame
/// type, a frame of fewer than 7 bytes, a reserved sample rate, a
/// substream other than the first, or a dependent frame's channel map of
/// more than 16 channels.
fn ac3_header(bytes: &[u8]) -> Option<Ac3Header> {
  let mut bits = Bits { bytes, at: 0 };
  if bits.read(16) != 0x0B77 {
    return None;
  }
  // The bit stream id sits at bits 40–44 in both.
  let bsid = u8::try_from(Bits { bytes, at: 40 }.read(5)).ok()?;
  if bsid > 16 {
    return None;
  }
  if bsid <= 10 {
    bits.skip(16);
    let sample_rate = bits.read(2);
    if sample_rate == 3 {
      return None;
    }
    let frame_size = usize::try_from(bits.read(6)).ok()?;
    let words = AC3_FRAME_WORDS.get(frame_size)?[usize::try_from(sample_rate).ok()?];
    return Some(Ac3Header {
      bsid,
      frame_type: 2,
      size: usize::from(words) * 2,
      blocks: 6,
    });
  }
  let frame_type = u8::try_from(bits.read(2)).ok()?;
  if frame_type == 3 {
    return None;
  }
  let substream = bits.read(3);
  let size = (usize::try_from(bits.read(11)).ok()? + 1) << 1;
  if size < 7 {
    return None;
  }
  let blocks = if bits.read(2) == 3 {
    if bits.read(2) == 3 {
      return None;
    }
    6
  } else {
    [1, 2, 3, 6][usize::try_from(bits.read(2)).ok()?]
  };
  let channel_mode = bits.read(3);
  bits.skip(1);
  if substream != 0 {
    return None;
  }
  // The bit stream id, then the volume fields — twice for dual mono —
  // before a dependent frame's channel map.
  bits.skip(5);
  for _ in 0..if channel_mode == 0 { 2 } else { 1 } {
    bits.skip(5);
    if bits.read(1) == 1 {
      bits.skip(8);
    }
  }
  if frame_type == 1 && bits.read(1) == 1 {
    let map = bits.read(16);
    let channels: u32 = (0..16)
      .filter(|bit| map & (1 << (15 - bit)) != 0)
      .map(|bit| EAC3_MAP_CHANNELS[bit])
      .sum();
    if channels > 16 {
      return None;
    }
  }
  Some(Ac3Header {
    bsid,
    frame_type,
    size,
    blocks,
  })
}

/// What MP4's AC-3 handling has learned of a track (`handle_eac3`,
/// `movenc.c` 464–610): whether it has read the stream's first frame set,
/// the bit stream id of its first frame, the run of E-AC-3 frames of fewer
/// than six blocks it holds back to make up a sample of six, and the last
/// sample it wrote, which the trailer reads.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Ac3State {
  done: bool,
  bsid: u8,
  run: Option<Ac3Run>,
  last: Option<Ac3Sample>,
}

/// The sample MP4 is joining E-AC-3 frames of fewer than six blocks into,
/// while it holds any (`handle_eac3`, `movenc.c` 586–603): it takes its
/// first packet's decode time, presentation time, flags and side data
/// (`av_packet_ref`, 588), the packets' bytes joined (`av_grow_packet`,
/// 593–595) and their durations summed (597).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Ac3Run {
  /// Its blocks so far, below six.
  blocks: u8,
  /// The first packet's decode time, in ticks: the sample's.
  dts: i64,
  /// The packets' durations as MP4 is handed them — libavformat's for a
  /// packet that states none ([`Ac3Rules::guessed`]) — summed: the
  /// sample's.
  duration: i128,
  /// The packets' durations as offered, summed, where each offered one
  /// above zero.
  offered: Option<i128>,
  /// The joined bytes.
  bytes: u64,
  /// The first packet's skip samples, which the sample carries: the samples
  /// to drop from its end, where the entry is one MP4 reads (7311–7312).
  skip_end: Option<u32>,
}

/// The last sample MP4 wrote of an AC-3 or E-AC-3 track, as its trailer
/// reads it: the sample's duration as MP4 was handed it — a joined
/// sample's, its packets' sum — and the duration its edit list's end is
/// counted with, the last packet's own (`movenc.c` 6940, 7329–7342).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Ac3Sample {
  duration: i128,
  closing: i128,
}

/// What MP4 does with an AC-3 or E-AC-3 packet ([`ac3_packet`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ac3Step {
  /// It writes the packet as a sample of its own: a frame of six blocks,
  /// with none held (577–580).
  Alone,
  /// It holds the packet, opening a run of frames of fewer than six blocks
  /// (587–591).
  Opens,
  /// It holds the packet, joined to the run (592–599).
  Joins,
  /// It writes the run the packet completes as one sample (600–603): the
  /// run as completed.
  Completes(Ac3Run),
}

/// An AC-3 or E-AC-3 packet as MP4's handling reads it: its bytes, its
/// decode time and its duration as MP4 is handed them, in ticks, and its
/// skip samples' end ([`skip_end`]).
#[derive(Debug, Clone, Copy)]
struct Ac3Packet<'a> {
  bytes: &'a [u8],
  dts: i64,
  duration: i64,
  offered: Option<i64>,
  skip_end: Option<u32>,
}

/// Holds an MP4 AC-3 (`eac3` false) or E-AC-3 packet to MP4's handling of
/// it, from what MP4 has learned of the track (`handle_eac3`, `movenc.c`
/// 464–610), the sample it joins held to `limit`, libavcodec's largest
/// packet, which the laws lower: answers what MP4 learns of the track and
/// what it does with the packet ([`Ac3Step`]), or the rule it breaks.
fn ac3_packet(
  state: Ac3State,
  eac3: bool,
  packet: Ac3Packet<'_>,
  limit: u64,
) -> Result<(Ac3State, Ac3Step), PayloadRule> {
  let bytes = packet.bytes;
  let first = ac3_header(bytes).ok_or(PayloadRule::Ac3Header)?;
  let mut next = state;
  // 509–575: until the first set is read, a second independent frame ends
  // it; else the frame is the set's first, and an E-AC-3 track's first
  // packet must be that frame and dependent frames tiling the rest.
  let independent = first.frame_type != 1;
  if !state.done {
    if independent && state.bsid != 0 {
      next.done = true;
    } else {
      next.bsid = first.bsid;
      if !eac3 {
        // The frame `dac3` is written from, at the trailer, which refuses
        // an id past 8 (431–437).
        if first.bsid > 8 {
          return Err(PayloadRule::Ac3BitStreamId);
        }
        next.done = true;
      } else {
        // Dependent frames tiling the rest, counted into `dec3`'s 4 bits
        // (551–575; 644).
        let mut at = first.size;
        let mut dependents = 0;
        while at != bytes.len() {
          let dependent = bytes
            .get(at..)
            .and_then(ac3_header)
            .ok_or(PayloadRule::Ac3Substreams)?;
          if dependent.frame_type != 1 || dependents == 15 {
            return Err(PayloadRule::Ac3Substreams);
          }
          dependents += 1;
          at += dependent.size;
        }
      }
    }
  }
  // 577–606: a frame of six blocks is a sample of its own; fewer are held
  // back until six, joined into one sample.
  let len = bytes.len() as u64;
  let (run, step) = match state.run {
    None if first.blocks == 6 => (None, Ac3Step::Alone),
    None => (
      Some(Ac3Run {
        blocks: first.blocks,
        dts: packet.dts,
        duration: i128::from(packet.duration),
        offered: packet.offered.map(i128::from),
        bytes: len,
        skip_end: packet.skip_end,
      }),
      Ac3Step::Opens,
    ),
    Some(run) if run.blocks + first.blocks > 6 => return Err(PayloadRule::Ac3Blocks),
    Some(run) => {
      // `av_grow_packet` grows a packet to `INT_MAX` less the padding at
      // most, a byte past libavcodec's largest (`libavcodec/packet.c`
      // 121–127), and refuses after the frame was taken.
      if run.bytes + len > limit + 1 {
        return Err(PayloadRule::Ac3SampleSize);
      }
      let joined = Ac3Run {
        blocks: run.blocks + first.blocks,
        duration: run.duration + i128::from(packet.duration),
        offered: run
          .offered
          .zip(packet.offered)
          .map(|(sum, own)| sum + i128::from(own)),
        bytes: run.bytes + len,
        ..run
      };
      if joined.blocks == 6 {
        (None, Ac3Step::Completes(joined))
      } else {
        (Some(joined), Ac3Step::Joins)
      }
    }
  };
  next.run = run;
  next.last = match step {
    Ac3Step::Alone => Some(Ac3Sample {
      duration: i128::from(packet.duration),
      closing: i128::from(packet.duration),
    }),
    Ac3Step::Completes(run) => Some(Ac3Sample {
      duration: run.duration,
      closing: i128::from(packet.duration),
    }),
    Ac3Step::Opens | Ac3Step::Joins => state.last,
  };
  Ok((next, step))
}

/// When libavformat inserts a filter for a track (table B of the census,
/// see [the module](self)): at the muxer's start — its `init`, or its
/// header — whatever `AVFMT_FLAG_AUTO_BSF` says; or at the track's first
/// packet, through the muxer's `check_bitstream`, only with it set
/// (`check_bitstream`, `mux.c` 1056–1073 in FFmpeg 9.0.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Insertion {
  Start,
  FirstPacket,
  /// At the first packet, by a context the muxer opens of its own, which
  /// keeps libavformat's default — the filters on — whatever the caller's
  /// option says: RTP MPEG-TS's MPEG-TS (`rtp_mpegts_write_header`,
  /// `rtpenc_mpegts.c` 67).
  Nested,
}

/// Where a muxer inserts a length-prefix-to-Annex-B filter, and how its
/// `check_bitstream` decides it at a track's first packet (table B).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AnnexBSite {
  /// The codecs it inserts one for: MPEG-TS's H.264, HEVC and VVC; MXF's
  /// H.264; a raw muxer's own codec.
  codecs: &'static [CodecId],
  /// MPEG-TS's rule: a packet opening on a 3-byte start code still inserts
  /// the filter where the track's configuration is of the length-prefixed
  /// form (`mpegtsenc.c` 2319–2330).
  configuration: bool,
  insertion: Insertion,
}

/// The filters the muxer libavformat resolved inserts that this muxer
/// mirrors — by the muxer's own name, and for the name two muxers share,
/// the MIME type that tells them apart (table B of the census, see [the
/// module](self)).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Sites {
  /// `vp9_superframe`: IVF's at its start (`ivf_init`, `ivfenc.c` 42);
  /// Matroska's, WebM's and the MOV family's at a VP9 track's first packet
  /// (`mkv_check_bitstream`, `matroskaenc.c` 3639; `mov_check_bitstream`,
  /// `movenc.c` 9147).
  vp9: Option<Insertion>,
  /// `aac_adtstoasc`, at an AAC track's first packet where it opens with
  /// an ADTS header: Matroska's video muxer's, the MOV family's, FLV's and
  /// LATM's (`matroskaenc.c` 3634–3637; `movenc.c` 9144–9145; `flvenc.c`
  /// 1491–1493; `latmenc.c` 255–257).
  aac: bool,
  /// `pgs_frame_merge`, at a PGS track's first packet: Matroska's video
  /// muxer's (`matroskaenc.c` 3640–3642).
  pgs: bool,
  /// `pcm_rechunk`, at the start, for an audio track: DAUD's, GXF's and
  /// the MXF muxers' (`daudenc.c` 44; `gxfenc.c` 724; `mxfenc.c` 3081).
  rechunk: bool,
  /// `av1_metadata`, `td=insert`: IVF's at its start (`ivfenc.c` 46); the
  /// OBU muxer's at the first packet (`rawenc.c` 572).
  av1: Option<Insertion>,
  /// `eia608_to_smpte436m`, at the start, for EIA-608: MCC's and the MXF
  /// muxers' (`mccenc.c` 434; `mxfenc.c` 3104).
  eia608: bool,
  /// `extract_extradata`, at the first packet of a track with no
  /// configuration: FLV's (`flvenc.c` 1495–1501).
  extract: bool,
  /// `h264_mp4toannexb` and its HEVC and VVC kin, at a length-prefixed
  /// first packet: MPEG-TS's, MXF's and MXF OP-Atom's, the raw muxers', and
  /// RTP MPEG-TS's nested MPEG-TS (`mpegtsenc.c` 2310–2334; `mxfenc.c`
  /// 3631–3639; `rawenc.c` 386–443).
  annexb: Option<AnnexBSite>,
}

impl Sites {
  /// The MOV family: every muxer whose `check_bitstream` is MOV's
  /// (`movenc.c` 9343–9512). AVIF's, the last of the file, has none.
  const MOV: [&'static str; 8] = ["mov", "3gp", "mp4", "psp", "3g2", "ipod", "ismv", "f4v"];

  /// The filters the muxer named `muxer`, of MIME type `mime`, inserts. The
  /// audio muxer named `matroska` (`audio/x-matroska`) is reached only in a
  /// build without the video one, where `CONFIG_MATROSKA_MUXER` is 0 and
  /// `mkv_check_bitstream` inserts neither AAC's filter nor PGS's
  /// (`matroskaenc.c` 3634, 3640).
  fn of(muxer: &str, mime: Option<&str>) -> Self {
    let mov = Self::MOV.contains(&muxer);
    let matroska = muxer == "matroska";
    let matroska_video = matroska && mime != Some("audio/x-matroska");
    Self {
      vp9: if muxer == "ivf" {
        Some(Insertion::Start)
      } else if matroska || muxer == "webm" || mov {
        Some(Insertion::FirstPacket)
      } else {
        None
      },
      aac: matroska_video || mov || muxer == "flv" || muxer == "latm",
      pgs: matroska_video,
      rechunk: matches!(muxer, "daud" | "gxf" | "mxf" | "mxf_d10" | "mxf_opatom"),
      av1: match muxer {
        "ivf" => Some(Insertion::Start),
        "obu" => Some(Insertion::FirstPacket),
        _ => None,
      },
      eia608: matches!(muxer, "mcc" | "mxf" | "mxf_d10" | "mxf_opatom"),
      extract: muxer == "flv",
      annexb: Self::annexb(muxer),
    }
  }

  /// The muxer's length-prefix-to-Annex-B site: MPEG-TS's and RTP
  /// MPEG-TS's nested one for H.264, HEVC and VVC, the configuration asked;
  /// MXF's and MXF OP-Atom's for H.264 — MXF D-10 asks no `check_bitstream`
  /// (`mxfenc.c` 3726–3740) — and each raw muxer's for its codec.
  fn annexb(muxer: &str) -> Option<AnnexBSite> {
    const TS: &[CodecId] = &[CodecId::H264, CodecId::HEVC, CodecId::VVC];
    let site = |codecs, configuration, insertion| {
      Some(AnnexBSite {
        codecs,
        configuration,
        insertion,
      })
    };
    match muxer {
      "mpegts" => site(TS, true, Insertion::FirstPacket),
      "rtp_mpegts" => site(TS, true, Insertion::Nested),
      "mxf" | "mxf_opatom" | "h264" => site(&[CodecId::H264], false, Insertion::FirstPacket),
      "hevc" => site(&[CodecId::HEVC], false, Insertion::FirstPacket),
      "vvc" => site(&[CodecId::VVC], false, Insertion::FirstPacket),
      _ => None,
    }
  }

  /// Whether a filter inserted so is inserted, the automatic filters `on`
  /// or off.
  const fn inserts(insertion: Option<Insertion>, on: bool) -> bool {
    match insertion {
      Some(Insertion::Start | Insertion::Nested) => true,
      Some(Insertion::FirstPacket) => on,
      None => false,
    }
  }
}

/// The automatic filter libavformat inserts for a track, and what it holds
/// between packets — mirrored, so that a packet the filter would refuse
/// after taking it is refused first, every packet it puts out is sized
/// before any it is made of is handed over, and the times, flags and side
/// data it drops are known (table B of the census, see [the module](self)).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum Filter {
  /// None: every packet reaches the container as it was handed over.
  #[default]
  None,
  /// `aac_adtstoasc` not yet inserted — on a track with a configuration, or
  /// in another container than MP4 and Matroska: the track's first packet
  /// decides — the filter is inserted where it opens with an ADTS header of
  /// more than two bytes, and never otherwise (`check_bitstream`, `mux.c`
  /// 1063–1068). Whether the track has a configuration, and whether
  /// libavcodec parses it: the filter's start refuses one it does not
  /// (`aac_adtstoasc_init`, `aac_adtstoasc.c` 130–144); and whether each
  /// packet it converts must then be one frame whole (MP4's and Matroska's
  /// rule, table F).
  AdtsUndecided {
    configured: bool,
    parses: bool,
    whole: bool,
  },
  /// `aac_adtstoasc`: whether it has converted a frame — the first it
  /// converts takes the program config element's road — whether the track
  /// has a configuration, which raw packets then pass by, and whether each
  /// packet must be one frame whole (MP4's and Matroska's rule, table F).
  Adts {
    converted: bool,
    configured: bool,
    whole: bool,
  },
  /// `vp9_superframe`, and the hidden frames it holds: how many, their
  /// bytes, and the largest's.
  Vp9 { held: u8, bytes: u64, largest: u64 },
  /// `pgs_frame_merge`, and what it holds of a display set.
  Pgs(PgsState),
  /// A filter of another container that this muxer names and does not
  /// mirror: its packets reach it as handed over, and nothing is claimed
  /// of what it does with them but its name (table B).
  Unmirrored(AutomaticFilter),
  /// A length-prefix-to-Annex-B filter not yet inserted: the track's first
  /// packet decides — the filter is inserted where it holds five bytes or
  /// more and opens on no 4-byte start code, nor on a 3-byte one unless
  /// `configured` — the track's configuration of the length-prefixed form,
  /// asked by MPEG-TS alone — and never otherwise (`check_bitstream`,
  /// `mux.c` 1063–1068; `mpegtsenc.c` 2326–2331; `mxfenc.c` 3633–3636;
  /// `rawenc.c` 389–391).
  AnnexBUndecided {
    filter: AutomaticFilter,
    configured: bool,
  },
}

/// What a track's filter does with a packet ([`Filter::step`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Step {
  /// Whether the packet's own flags and side data reach the container, in a
  /// packet the filter puts out now or later: not where the filter drops
  /// them, with the packet's times.
  owns: bool,
  /// What the filter puts out now, by whose times each packet carries: what
  /// libavformat's interleaving and the container are handed at the
  /// packet's write, which every mirror of their timing moves on alone.
  emits: Emits,
  /// The largest packet the filter puts out with this one — its payload's
  /// bytes and its side data's — or `None` where it puts out none.
  largest: Option<u64>,
  /// What the packets the filter holds after this one come to at the
  /// least, once put out — VP9's superframe of them and a frame of a byte,
  /// PGS's display set and its end segment — or `None` where it holds none.
  held: Option<u64>,
  /// The filter this packet inserts, where the track's first packet decides
  /// it: reported for the track at its write (table B).
  inserted: Option<AutomaticFilter>,
}

/// The packets a track's filter puts out with a packet, now, by whose times
/// each carries ([`Step::emits`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Emits {
  /// The display set the packet completes, held before it: it takes the
  /// times of the packet that held its presentation segment
  /// (`pgs_frame_merge.c` 132–136).
  held: Option<Timed>,
  /// Whether it puts out a packet with the packet's own times — one or
  /// more: the display sets one PGS packet holds whole share them.
  own: bool,
}

impl Emits {
  /// Nothing put out.
  const NONE: Self = Self {
    held: None,
    own: false,
  };

  /// The packet, as its own.
  const OWN: Self = Self {
    held: None,
    own: true,
  };

  /// The times of what is put out, in the order put out, the packet's own
  /// being `own`.
  fn times(self, own: Timed) -> impl Iterator<Item = Timed> {
    self.held.into_iter().chain(self.own.then_some(own))
  }
}

impl Filter {
  /// The filter the muxer libavformat resolved inserts for a timed track
  /// of `kind` and `codec` whose stream holds `extradata`, the automatic
  /// filters `on`, by `sites`, the muxer's (table B of the census). MP4 and
  /// Matroska take ADTS frames alone on an AAC track with no configuration,
  /// one a packet, so its filter is inserted at its first
  /// ([`PayloadRule::AdtsFrame`]), and raw AAC alone on one with it
  /// ([`PayloadRule::AdtsHeader`]), so it never is; another container's
  /// first packet decides.
  fn settle(
    sites: Sites,
    family: Family,
    (kind, codec): (TrackKind, CodecId),
    extradata: &[u8],
    on: bool,
  ) -> Self {
    let annexb = sites
      .annexb
      .filter(|site| site.codecs.contains(&codec) && Sites::inserts(Some(site.insertion), on));
    if codec == CodecId::VP9 && Sites::inserts(sites.vp9, on) {
      Self::Vp9 {
        held: 0,
        bytes: 0,
        largest: 0,
      }
    } else if codec == CodecId::AAC && sites.aac && on {
      match (family, extradata.is_empty()) {
        (Family::Mp4 | Family::Matroska, true) => Self::Adts {
          converted: false,
          configured: false,
          whole: true,
        },
        (family, empty) => Self::AdtsUndecided {
          configured: !empty,
          parses: audio_specific_config_parses(extradata),
          whole: matches!(family, Family::Mp4 | Family::Matroska),
        },
      }
    } else if codec == CodecId::HDMV_PGS_SUBTITLE && sites.pgs && on {
      Self::Pgs(PgsState::default())
    } else if kind == TrackKind::Audio && sites.rechunk {
      Self::Unmirrored(AutomaticFilter::PcmRechunk)
    } else if codec == CodecId::AV1 && Sites::inserts(sites.av1, on) {
      Self::Unmirrored(AutomaticFilter::Av1Metadata)
    } else if codec == EIA_608 && sites.eia608 {
      Self::Unmirrored(AutomaticFilter::Eia608ToSmpte436m)
    } else if sites.extract
      && on
      && extradata.is_empty()
      && [
        CodecId::H264,
        CodecId::HEVC,
        CodecId::VVC,
        CodecId::AV1,
        CodecId::MPEG4,
      ]
      .contains(&codec)
    {
      Self::Unmirrored(AutomaticFilter::ExtractExtradata)
    } else if let Some(site) = annexb {
      // The configuration's first byte, `configurationVersion` 1 for H.264
      // and HEVC, VVC's reserved `11111` (`mpegtsenc.c` 2319–2321).
      let (filter, mask, value) = match codec {
        CodecId::H264 => (AutomaticFilter::H264Mp4ToAnnexB, 0xff, 0x01),
        CodecId::HEVC => (AutomaticFilter::HevcMp4ToAnnexB, 0xff, 0x01),
        _ => (AutomaticFilter::VvcMp4ToAnnexB, 0xf8, 0xf8),
      };
      Self::AnnexBUndecided {
        filter,
        configured: site.configuration
          && extradata.first().is_some_and(|first| first & mask == value),
      }
    } else {
      Self::None
    }
  }

  /// The filter, named, where it is known at open.
  const fn named(self) -> Option<AutomaticFilter> {
    match self {
      Self::None | Self::AdtsUndecided { .. } | Self::AnnexBUndecided { .. } => None,
      Self::Adts { .. } => Some(AutomaticFilter::AacAdtsToAsc),
      Self::Vp9 { .. } => Some(AutomaticFilter::Vp9Superframe),
      Self::Pgs(_) => Some(AutomaticFilter::PgsFrameMerge),
      Self::Unmirrored(filter) => Some(filter),
    }
  }

  /// Holds a packet timed `timed`, of `bytes`, carrying `side` bytes of
  /// side data, to the filter, every packet it puts out held to `limit` —
  /// libavcodec's largest packet, which the laws lower: answers what the
  /// filter holds after it and what it puts out ([`Step`]), or the rule it
  /// breaks.
  fn step(
    self,
    timed: Timed,
    bytes: &[u8],
    side: u64,
    limit: u64,
  ) -> Result<(Self, Step), PayloadRule> {
    let len = bytes.len() as u64;
    let passed = Step {
      owns: true,
      emits: Emits::OWN,
      largest: Some(len + side),
      held: None,
      inserted: None,
    };
    match self {
      Self::None | Self::Unmirrored(_) => Ok((self, passed)),
      // An ADTS header of more than two bytes inserts the filter, which
      // takes the packet (`mux.c` 1063–1068); anything else, never.
      Self::AdtsUndecided {
        configured,
        parses,
        whole,
      } => {
        if bytes.len() > 2 && u16::from_be_bytes([bytes[0], bytes[1]]) & 0xfff0 == 0xfff0 {
          if configured && !parses {
            return Err(PayloadRule::AdtsFrame);
          }
          let (filter, step) = Self::Adts {
            converted: false,
            configured,
            whole,
          }
          .step(timed, bytes, side, limit)?;
          Ok((
            filter,
            Step {
              inserted: Some(AutomaticFilter::AacAdtsToAsc),
              ..step
            },
          ))
        } else {
          Ok((Self::None, passed))
        }
      }
      Self::AnnexBUndecided { filter, configured } => {
        let start_code = |at: usize| {
          bytes
            .get(..at)
            .is_some_and(|code| code[..at - 1].iter().all(|&byte| byte == 0) && code[at - 1] == 1)
        };
        let inserts = bytes.len() >= 5 && !start_code(4) && (!start_code(3) || configured);
        Ok(if inserts {
          (
            Self::Unmirrored(filter),
            Step {
              inserted: Some(filter),
              ..passed
            },
          )
        } else {
          (Self::None, passed)
        })
      }
      Self::Adts {
        converted,
        configured,
        whole,
      } => match aac_conversion(bytes, !converted, configured) {
        Some(AacConversion::Passed) => Ok((self, passed)),
        Some(AacConversion::Frame { frame, output })
          if output >= 0 && (!whole || (frame == bytes.len() && output > 0)) =>
        {
          Ok((
            Self::Adts {
              converted: true,
              configured,
              whole,
            },
            passed,
          ))
        }
        _ => Err(PayloadRule::AdtsFrame),
      },
      Self::Vp9 {
        held,
        bytes: sum,
        largest,
      } => {
        let (next, own) = vp9_superframe(held, bytes)?;
        let run = vp9_run((held, sum, largest), next, len, limit)?;
        let step = if own {
          // A frame shown alone, or a superframe, passes as it is; a frame
          // shown after hidden ones makes their superframe, which takes its
          // side data (`vp9_superframe.c` 148–150, 163–175).
          let out = if held == 0 {
            len
          } else {
            superframe_bytes(sum + len, largest.max(len), u64::from(held) + 1)
          };
          Step {
            owns: true,
            emits: Emits::OWN,
            largest: Some(out + side),
            held: None,
            inserted: None,
          }
        } else {
          Step {
            owns: false,
            emits: Emits::NONE,
            largest: None,
            held: Some(superframe_bytes(
              run.0 + 1,
              run.1.max(1),
              u64::from(next) + 1,
            )),
            inserted: None,
          }
        };
        Ok((
          Self::Vp9 {
            held: next,
            bytes: run.0,
            largest: run.1,
          },
          step,
        ))
      }
      Self::Pgs(state) => pgs_frame_merge(state, timed, bytes, side, limit)
        .map(|(state, step)| (Self::Pgs(state), step)),
    }
  }
}

/// What `aac_adtstoasc` makes of a packet it takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AacConversion {
  /// A raw packet on a configured track, passed as it is
  /// (`aac_adtstoasc.c` 51–52).
  Passed,
  /// One frame converted: its header's frame length, and the bytes put out
  /// — the payload after the header and its CRC, less, on the first frame
  /// converted with channel configuration 0, the program config element
  /// opening it, which the filter consumes with no check that it fits
  /// (80–95), so that the bytes put out may be fewer than none.
  Frame { frame: usize, output: i64 },
}

/// What `aac_adtstoasc` does with a packet of `bytes` (`aac_adtstoasc.c`
/// 39–128, over `ff_adts_header_parse`, `adts_header.c` 30–74, in FFmpeg
/// 9.0.1), the track `configured` or not, the packet the `first` it would
/// convert: [`AacConversion`], or `None` where the filter refuses it — a
/// packet shorter than a header (54–55); no sync word, a sample rate index
/// the table does not name (13 and past, `mpeg4audio_sample_rates.h`), a
/// frame length below the header's 7 bytes (`adts_header.c` 37–60); a CRC
/// over several raw data blocks (63–68); nothing after the header and its
/// CRC (70–72); on the first, a payload that does not open with a program
/// config element where the channel configuration is 0 (80–90).
fn aac_conversion(bytes: &[u8], first: bool, configured: bool) -> Option<AacConversion> {
  if configured && bytes.len() >= 2 && u16::from_be_bytes([bytes[0], bytes[1]]) >> 4 != 0xfff {
    return Some(AacConversion::Passed);
  }
  let mut bits = Bits { bytes, at: 0 };
  if bytes.len() < 7 || bits.read(12) != 0xFFF {
    return None;
  }
  // The id and the layer.
  bits.skip(3);
  let crc_absent = bits.read(1) == 1;
  // The profile.
  bits.skip(2);
  let sample_rate = bits.read(4);
  // The private bit.
  bits.skip(1);
  let channels = bits.read(3);
  // The originality, home and copyright bits.
  bits.skip(4);
  let frame = bits.read(13) as usize;
  // The buffer fullness.
  bits.skip(11);
  let blocks = bits.read(2);
  let header = if crc_absent { 7 } else { 9 };
  if sample_rate >= 13 || frame < 7 || (!crc_absent && blocks > 0) || bytes.len() <= header {
    return None;
  }
  let payload = (bytes.len() - header) as i64;
  let output = if first && channels == 0 {
    payload - pce_span(&bytes[header..])? as i64
  } else {
    payload
  };
  Some(AacConversion::Frame { frame, output })
}

/// Whether `bytes` is one ADTS frame `aac_adtstoasc` converts whole, as
/// MP4 and Matroska take AAC on a track with no configuration
/// (table F of the census, see [the module](self)): a frame the filter
/// converts ([`aac_conversion`]) whose length is the packet's own — the
/// filter strips the first header alone (`aac_adtstoasc.c` 70–73) — and
/// which leaves some bytes: on the track's `first` frame, the program
/// config element where the channel configuration names none is shorter
/// than its payload ([`pce_span`]). [`Filter::step`] holds such a track's
/// packets to it.
#[cfg(test)]
fn adts_frame(bytes: &[u8], first: bool) -> bool {
  matches!(
    aac_conversion(bytes, first, false),
    Some(AacConversion::Frame { frame, output }) if frame == bytes.len() && output > 0
  )
}

/// Whether a packet of `bytes` is the first frame AAC's `filter` converts —
/// the one whose new extradata the filter replaces with the configuration
/// it makes (`aac_adtstoasc.c` 98–103): an ADTS frame, where the filter has
/// converted none, or where the track's first packet inserts it.
fn first_conversion(filter: Filter, bytes: &[u8]) -> bool {
  match filter {
    Filter::Adts {
      converted: false,
      configured,
      ..
    } => matches!(
      aac_conversion(bytes, true, configured),
      Some(AacConversion::Frame { .. })
    ),
    Filter::AdtsUndecided { .. } => {
      bytes.len() > 2 && u16::from_be_bytes([bytes[0], bytes[1]]) & 0xfff0 == 0xfff0
    }
    _ => false,
  }
}

/// Whether `pcm_rechunk`'s start would count the sample size of a stream
/// of `channels`, its codec's samples `bits` wide, past C's `int`: the two
/// multiplied in `int` (`pcm_rechunk.c` 49–50 in FFmpeg 9.0.1). The filter
/// takes PCM alone (`av_bsf_init`, `libavcodec/bsf.c` 154–170), of a byte a
/// sample or more, so its division by that size — over 8 — never divides
/// by zero (57).
fn rechunk_overflows(channels: i32, bits: u32) -> bool {
  i64::from(channels) * i64::from(bits) > i64::from(i32::MAX)
}

/// The bytes the program config element opening `payload` spans — its
/// element id, the element and its comment, through the byte alignment
/// before the comment — read as `aac_adtstoasc` and `ff_copy_pce_data`
/// read it (`aac_adtstoasc.c` 80–95; `mpeg4audio_copy_pce.h` 38–67 in
/// FFmpeg 9.0.1), or `None` where the payload opens with another element
/// than one, element id 5. The filter consumes exactly this span and no
/// more, with no check that it fits: a span past the payload leaves the
/// packet a negative size, and one that ends it leaves it empty (94–95),
/// so a first frame is converted whole only where the span is shorter
/// than its payload.
fn pce_span(payload: &[u8]) -> Option<usize> {
  pce_read(payload).map(|(span, _)| span)
}

/// The program config element opening `payload`, as `ff_copy_pce_data`
/// reads and writes it (`mpeg4audio_copy_pce.h` 38–67): the bytes it
/// consumes from the payload ([`pce_span`]) and the bytes it writes into
/// the configuration AAC's filter makes — the element's fields without its
/// id, aligned to a byte from their own start, then the comment's size and
/// its bytes — or `None` where the payload opens with another element.
fn pce_read(payload: &[u8]) -> Option<(usize, usize)> {
  let mut bits = Bits {
    bytes: payload,
    at: 0,
  };
  if bits.read(3) != 5 {
    return None;
  }
  // The element's tag, object type and sampling index.
  bits.skip(10);
  // Front, side and back elements, then LFE, data and coupling ones: five
  // bits each but LFE's and data's, four.
  let mut five_bit = bits.read(4) + bits.read(4) + bits.read(4);
  let four_bit = bits.read(2) + bits.read(3);
  five_bit += bits.read(4);
  // The mono, stereo and matrix mixdowns, each flagged.
  for width in [4, 4, 3] {
    if bits.read(1) == 1 {
      bits.skip(width);
    }
  }
  bits.skip(usize::try_from(five_bit * 5 + four_bit * 4).ok()?);
  // The fields copied, which `align_put_bits` aligns from the writer's own
  // start (61–62).
  let fields = bits.at - 3;
  // `align_get_bits`, over the reader that opened at the payload.
  bits.at = bits.at.div_ceil(8) * 8;
  let comment = usize::try_from(bits.read(8)).ok()?;
  bits.skip(comment * 8);
  Some((bits.at / 8, fields.div_ceil(8) + 1 + comment))
}

/// The bytes of the configuration AAC's filter makes of the ADTS frame
/// `bytes` it converts first: two of the header's fields, and the program
/// config element it copies where the channel configuration is 0
/// (`aac_adtstoasc.c` 75–117) — or `None` where the frame is not one the
/// filter converts ([`aac_conversion`]).
fn made_configuration(bytes: &[u8]) -> Option<usize> {
  let Some(AacConversion::Frame { .. }) = aac_conversion(bytes, true, false) else {
    return None;
  };
  let crc_absent = bytes[1] & 1 == 1;
  let channels = ((bytes[2] & 1) << 2) | (bytes[3] >> 6);
  let header = if crc_absent { 7 } else { 9 };
  if channels == 0 {
    pce_read(&bytes[header..]).map(|(_, copied)| 2 + copied)
  } else {
    Some(2)
  }
}

/// What VP9's automatic filter does with a packet of `bytes`, given the
/// hidden frames it `held` (`vp9_superframe_filter`, `vp9_superframe.c`
/// 101–181 in FFmpeg 9.0.1): answers how many it holds after it, and
/// whether the packet's own times, flags and side data reach the container
/// — a hidden frame's do not: it is held, and joined into the superframe of
/// the next frame the stream shows, which takes that frame's — or the rule
/// the packet breaks ([`PayloadRule::Superframe`]).
fn vp9_superframe(held: u8, bytes: &[u8]) -> Result<(u8, bool), PayloadRule> {
  // 111–118: an empty packet passes where nothing is held.
  let Some(&marker) = bytes.last() else {
    return if held == 0 {
      Ok((0, true))
    } else {
      Err(PayloadRule::Superframe)
    };
  };
  // 120–126: a superframe ends on its index, whose marker opens it too.
  let superframe = marker & 0xe0 == 0xc0 && {
    let frames = 1 + usize::from(marker & 0x7);
    let width = 1 + usize::from((marker >> 3) & 0x3);
    let index = 2 + frames * width;
    bytes.len() >= index && bytes[bytes.len() - index] == marker
  };
  // 131–141: the frame marker, the profile — a reserved bit after the
  // third — then whether it shows an existing frame, else its type and
  // whether it is shown.
  let mut bits = Bits { bytes, at: 0 };
  bits.skip(2);
  let profile = bits.read(1) | (bits.read(1) << 1);
  if profile == 3 {
    bits.skip(1);
  }
  let hidden = bits.read(1) == 0 && {
    bits.skip(1);
    bits.read(1) == 0
  };
  match (superframe, hidden) {
    // 143–147: a superframe after hidden frames.
    (true, _) if held > 0 => Err(PayloadRule::Superframe),
    // 148–150: a superframe, or a frame shown, passes where nothing is
    // held; a frame shown after hidden ones makes their superframe, timed
    // by it (163–175).
    (true, _) | (false, false) => Ok((0, true)),
    // 151–161: a seventh hidden frame in a row would be held only for the
    // filter to refuse whatever follows.
    (false, true) if held >= 6 => Err(PayloadRule::Superframe),
    (false, true) => Ok((held + 1, false)),
  }
}

/// The bytes the hidden frames VP9's filter holds come to after a packet of
/// `len` bytes: `before` the frames held, their bytes and the largest's,
/// and `after` how many it holds once it has taken the packet
/// ([`vp9_superframe`]) — or the superframe the run would make passing
/// `limit` ([`PayloadRule::SuperframeSize`]). A frame held is refused where
/// no frame shown after it — one byte at the least — could join the run
/// within the limit; a frame shown, where its superframe passes it. The
/// limit is [`PACKET_BYTES_MAX`]; the laws lower it.
fn vp9_run(
  (held, bytes, largest): (u8, u64, u64),
  after: u8,
  len: u64,
  limit: u64,
) -> Result<(u64, u64), PayloadRule> {
  if after > held {
    let (bytes, largest) = (bytes + len, largest.max(len));
    if superframe_bytes(bytes + 1, largest.max(1), u64::from(after) + 1) > limit {
      return Err(PayloadRule::SuperframeSize);
    }
    return Ok((bytes, largest));
  }
  if held > 0 && superframe_bytes(bytes + len, largest.max(len), u64::from(held) + 1) > limit {
    return Err(PayloadRule::SuperframeSize);
  }
  Ok((0, 0))
}

/// The bytes `merge_superframe` allocates for `frames` frames of `sum`
/// bytes, the largest `largest` (`vp9_superframe.c` 52–65): the frames,
/// then the index — a marker, each frame's size in as many bytes as the
/// largest's bit length needs (`av_log2(max) >> 3`, plus one), the marker
/// again. Counted wide: the filter's own `unsigned` sum wraps past 4 GiB.
fn superframe_bytes(sum: u64, largest: u64, frames: u64) -> u64 {
  let size_bytes = u64::from(largest.max(1).ilog2() / 8) + 1;
  sum + 2 + size_bytes * frames
}

/// What the PGS filter holds of a display set: whether it holds any of its
/// segments, whether one was its presentation segment, their bytes, the
/// side data's bytes and the times of the packet that held the
/// presentation segment — the set is put out with that packet's times and
/// side data (`pgs_frame_merge.c` 132–136) — and how many packets'
/// segments it holds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct PgsState {
  holding: bool,
  presentation: bool,
  bytes: u64,
  side: u64,
  times: Option<Timed>,
  packets: u64,
}

/// The fewest bytes a held display set is completed with: an end segment,
/// its type and its 16-bit length of 0.
const PGS_END_SEGMENT_BYTES: u64 = 3;

/// What PGS's automatic filter does with a packet of `bytes`, carrying
/// `side` bytes of side data, given what it holds of a display set
/// (`frame_merge_filter`, `pgs_frame_merge.c` 70–157 in FFmpeg 9.0.1):
/// answers what it holds after it and what it puts out ([`Step`]) — a
/// display set takes the times, flags and side data of the packet holding
/// its presentation segment (132–136), and one packet may complete several
/// — or the rule the packet breaks: empty, which the filter drops (87–90),
/// or segments it would write marked corrupt — a display set it completes
/// with no presentation segment (59–68), or segments that do not parse
/// (93–130) — or a display set it would grow past `limit`
/// ([`PayloadRule::DisplaySetSize`]; `av_grow_packet`, 137–140), refused at
/// the packet after which no end segment fits. Each segment is a type byte
/// and a big-endian 16-bit length; a presentation segment, of type `0x16`,
/// is one per display set and at least 11 bytes; one of type `0x80` ends
/// the set. The limit is [`PACKET_BYTES_MAX`]; the laws lower it.
fn pgs_frame_merge(
  state: PgsState,
  timed: Timed,
  bytes: &[u8],
  side: u64,
  limit: u64,
) -> Result<(PgsState, Step), PayloadRule> {
  if bytes.is_empty() {
    return Err(PayloadRule::Empty);
  }
  let mut state = state;
  let mut own = false;
  // What the filter puts out of the packet, now: the set held before it,
  // which takes the times of the packet that held its presentation segment,
  // and the sets whose presentation segment it holds, which take its own.
  let mut emits = Emits::NONE;
  let mut largest: Option<u64> = None;
  let mut rest = bytes;
  // The filter takes a packet's display sets one at a time, the rest of
  // the packet after each (143–148).
  while !rest.is_empty() {
    let mut at = 0;
    let mut end = None;
    let mut presentation = false;
    while at + 3 <= rest.len() {
      let kind = rest[at];
      let len = usize::from(u16::from_be_bytes([rest[at + 1], rest[at + 2]])) + 3;
      if at + len > rest.len() {
        break;
      }
      if kind == 0x16 {
        if state.presentation || len < 11 {
          break;
        }
        state.presentation = true;
        presentation = true;
      }
      at += len;
      if kind == 0x80 {
        end = Some(at);
        break;
      }
    }
    match end {
      // 122–123: a whole display set, nothing held: put out as it is, with
      // the packet's own times and side data.
      Some(size) if !state.holding && size == rest.len() => {
        if !state.presentation {
          return Err(PayloadRule::PgsSegments);
        }
        largest = Some(largest.map_or(rest.len() as u64 + side, |most| {
          most.max(rest.len() as u64 + side)
        }));
        return Ok((
          PgsState::default(),
          Step {
            owns: true,
            emits: Emits { own: true, ..emits },
            largest,
            held: None,
            inserted: None,
          },
        ));
      }
      // 124–130: segments that stop parsing before the packet ends.
      None if at != rest.len() => return Err(PayloadRule::PgsSegments),
      _ => {}
    }
    own |= presentation;
    // The set takes the side data of the packet holding its presentation
    // segment: this one's, or the one the filter took it from.
    let set_side = if presentation { side } else { state.side };
    // The filter grows the set it holds by what it takes of the packet: the
    // segments to the set's end, or all of them (137–140).
    let held = state.bytes + end.unwrap_or(rest.len()) as u64;
    match end {
      Some(size) => {
        if held > limit {
          return Err(PayloadRule::DisplaySetSize);
        }
        if !state.presentation {
          return Err(PayloadRule::PgsSegments);
        }
        if presentation {
          emits.own = true;
        } else {
          emits.held = state.times;
        }
        largest = Some(largest.map_or(held + set_side, |most| most.max(held + set_side)));
        state = PgsState::default();
        rest = &rest[size..];
      }
      None => {
        // A set no end segment can complete within the limit is lost: the
        // packet that leaves it so decides it.
        if held + PGS_END_SEGMENT_BYTES > limit {
          return Err(PayloadRule::DisplaySetSize);
        }
        state = PgsState {
          holding: true,
          presentation: state.presentation,
          bytes: held,
          side: set_side,
          times: if presentation {
            Some(timed)
          } else {
            state.times
          },
          packets: state.packets + 1,
        };
        rest = &[];
      }
    }
  }
  let held = state
    .holding
    .then(|| state.bytes + PGS_END_SEGMENT_BYTES + if state.presentation { state.side } else { 0 });
  Ok((
    state,
    Step {
      owns: own,
      emits,
      largest,
      held,
      inserted: None,
    },
  ))
}

/// The channels ISO/IEC 23001-8 gives a position, as MP4's `chnl` box
/// writes them (`iso_channel_position`, `mov_chan.c` 652–703 in FFmpeg
/// 9.0.1): the rest of its table is reserved, positions with no channel of
/// libavutil's.
const ISO_CHANNEL_POSITIONS: [AVChannel; 32] = [
  AVChannel::AV_CHAN_FRONT_LEFT,
  AVChannel::AV_CHAN_FRONT_RIGHT,
  AVChannel::AV_CHAN_FRONT_CENTER,
  AVChannel::AV_CHAN_LOW_FREQUENCY,
  AVChannel::AV_CHAN_SIDE_LEFT,
  AVChannel::AV_CHAN_SIDE_RIGHT,
  AVChannel::AV_CHAN_FRONT_LEFT_OF_CENTER,
  AVChannel::AV_CHAN_FRONT_RIGHT_OF_CENTER,
  AVChannel::AV_CHAN_BACK_LEFT,
  AVChannel::AV_CHAN_BACK_RIGHT,
  AVChannel::AV_CHAN_BACK_CENTER,
  AVChannel::AV_CHAN_SURROUND_DIRECT_LEFT,
  AVChannel::AV_CHAN_SURROUND_DIRECT_RIGHT,
  AVChannel::AV_CHAN_SIDE_SURROUND_LEFT,
  AVChannel::AV_CHAN_SIDE_SURROUND_RIGHT,
  AVChannel::AV_CHAN_WIDE_LEFT,
  AVChannel::AV_CHAN_WIDE_RIGHT,
  AVChannel::AV_CHAN_TOP_FRONT_LEFT,
  AVChannel::AV_CHAN_TOP_FRONT_RIGHT,
  AVChannel::AV_CHAN_TOP_FRONT_CENTER,
  AVChannel::AV_CHAN_TOP_BACK_LEFT,
  AVChannel::AV_CHAN_TOP_BACK_RIGHT,
  AVChannel::AV_CHAN_TOP_BACK_CENTER,
  AVChannel::AV_CHAN_TOP_SIDE_LEFT,
  AVChannel::AV_CHAN_TOP_SIDE_RIGHT,
  AVChannel::AV_CHAN_TOP_CENTER,
  AVChannel::AV_CHAN_LOW_FREQUENCY_2,
  AVChannel::AV_CHAN_BOTTOM_FRONT_LEFT,
  AVChannel::AV_CHAN_BOTTOM_FRONT_RIGHT,
  AVChannel::AV_CHAN_BOTTOM_FRONT_CENTER,
  AVChannel::AV_CHAN_TOP_SURROUND_LEFT,
  AVChannel::AV_CHAN_TOP_SURROUND_RIGHT,
];

/// The codecs MP4 writes as ISO/IEC 23003-5 PCM, `ipcm` and `fpcm`
/// (`codec_mp4_tags`, `movenc.c` 9274–9285 in FFmpeg 9.0.1).
const MP4_PCM: [CodecId; 10] = [
  CodecId::PCM_S16BE,
  CodecId::PCM_S16LE,
  CodecId::PCM_S24BE,
  CodecId::PCM_S24LE,
  CodecId::PCM_S32BE,
  CodecId::PCM_S32LE,
  CodecId::PCM_F32BE,
  CodecId::PCM_F32LE,
  CodecId::PCM_F64BE,
  CodecId::PCM_F64LE,
];

/// What `av_channel_layout_channel_from_index` reads of a channel layout:
/// its order, its channels, its mask and its custom map.
#[derive(Debug, Clone, Copy)]
struct LayoutView<'a> {
  order: i32,
  channels: i32,
  mask: u64,
  map: &'a [CustomChannel],
}

impl<'a> LayoutView<'a> {
  /// The layout a ticket mirrors.
  fn of(layout: &'a ChannelLayoutTicket) -> Self {
    Self {
      order: layout.order(),
      channels: layout.channels(),
      mask: layout.mask(),
      map: layout.map(),
    }
  }

  /// The channel the layout names at `index`, as
  /// `av_channel_layout_channel_from_index` names it (`channel_layout.c`
  /// 673–701 in FFmpeg 9.0.1): a custom layout's from its map, an
  /// ambisonic one's ambisonic channels first, then — and a native
  /// layout's — the mask's bits from the lowest; none past the layout's
  /// channels, and none of an unspecified layout.
  fn channel_at(self, index: i32) -> Option<i32> {
    use AVChannelOrder as Order;
    if index < 0 || index >= self.channels {
      return None;
    }
    let mut index = index.unsigned_abs();
    if self.order == Order::AV_CHANNEL_ORDER_CUSTOM as i32 {
      return self.map.get(index as usize).map(CustomChannel::id);
    }
    if self.order == Order::AV_CHANNEL_ORDER_AMBISONIC as i32 {
      let ambisonic = self
        .channels
        .unsigned_abs()
        .saturating_sub(self.mask.count_ones());
      if index < ambisonic {
        return i32::try_from(index)
          .ok()
          .map(|index| AVChannel::AV_CHAN_AMBISONIC_BASE as i32 + index);
      }
      index -= ambisonic;
    } else if self.order != Order::AV_CHANNEL_ORDER_NATIVE as i32 {
      return None;
    }
    (0..64)
      .filter(|bit| self.mask & (1 << bit) != 0)
      .nth(index as usize)
  }
}

/// Whether MP4 writes PCM of `codec` laid out as `layout` whole: its
/// `chnl` box, written for `ipcm` and `fpcm` from two channels (`movenc.c`
/// 1521–1528), names one of ISO/IEC 23001-8's channel configurations or
/// each channel's position, and fails the trailer where neither is found
/// (`mov_write_chnl_tag`, 1299–1323). Every channel of a configuration has
/// a position (`iso_ch_layout_map`, `mov_chan.c` 244–259), so a layout
/// fails exactly where a channel has none (741–766).
fn mp4_pcm_layout_written(codec: CodecId, layout: LayoutView<'_>) -> bool {
  !MP4_PCM.contains(&codec)
    || layout.channels <= 1
    || (0..layout.channels).all(|index| {
      layout.channel_at(index).is_some_and(|channel| {
        ISO_CHANNEL_POSITIONS
          .iter()
          .any(|&position| position as i32 == channel)
      })
    })
}

/// Which packet side data a container writes on a track, and what it keeps
/// of it — the side-data table (see [the module](self)), from the muxers'
/// source in FFmpeg 9.0.1.
#[derive(Debug, Clone)]
struct SideRules {
  family: Family,
  kind: TrackKind,
  codec: CodecId,
  sample_rate: i32,
  frame_size: i32,
  initial_padding: i32,
  /// The stream's configuration: new extradata equal to it changes nothing.
  extradata: Vec<u8>,
}

impl SideRules {
  /// A track of which nothing is claimed: another container's, or one that
  /// takes no packet.
  const fn unclaimed() -> Self {
    Self {
      family: Family::Other,
      kind: TrackKind::Unknown,
      codec: CodecId::NONE,
      sample_rate: 0,
      frame_size: 0,
      initial_padding: 0,
      extradata: Vec::new(),
    }
  }

  /// A timed track of `kind` in `family`, its stream held as `codec`.
  fn of(family: Family, kind: TrackKind, codec: &StreamCodec) -> Self {
    Self {
      family,
      kind,
      codec: codec.codec(),
      sample_rate: codec.sample_rate(),
      frame_size: codec.frame_size(),
      initial_padding: codec.initial_padding(),
      extradata: codec.extradata().to_vec(),
    }
  }

  /// What the container makes of `entry` on a packet of the track whose own
  /// side data reaches it — `None` where it keeps it, or where the entry says
  /// nothing; `first` says whether the packet is the track's first.
  fn fate(&self, entry: &SideDataEntry, first: bool) -> Option<SideDataFate> {
    use AVPacketSideDataType as T;
    use SideDataFate::{Dropped, PossiblyDropped};
    let is = |kind: T| entry.kind() == kind as i32;
    let data = entry.data();
    let either_matroska = self.family == Family::Matroska || self.family == Family::WebM;
    if self.family == Family::Other {
      None
    } else if is(T::AV_PKT_DATA_NEW_EXTRADATA) {
      // MP4 writes new extradata as a sample description from the packet
      // on, or as the configuration of a track that had none (`movenc.c`
      // 7044–7074, 7412–7429); Matroska rewrites an AAC or FLAC track's
      // configuration with it, which a later packet's replaces in turn
      // (`matroskaenc.c` 3065–3097), and ignores any other (3098–3115).
      if data.is_empty() || data == self.extradata {
        None
      } else if self.family == Family::Mp4 {
        // MP4 writes an AC-3 or E-AC-3 sample entry's configuration, `dac3`
        // or `dec3`, from its own parse of the frames, not from the
        // extradata, whose bytes it never writes (`mov_write_ac3_tag`,
        // `mov_write_eac3_tag`, `movenc.c` 418–462, 612–666).
        (self.codec == CodecId::AC3 || self.codec == CodecId::EAC3).then_some(Dropped)
      } else if self.family == Family::Matroska
        && (self.codec == CodecId::AAC || self.codec == CodecId::FLAC)
      {
        Some(PossiblyDropped)
      } else {
        Some(Dropped)
      }
    } else if is(T::AV_PKT_DATA_SKIP_SAMPLES) {
      if data.iter().all(|&byte| byte == 0) {
        return None;
      }
      if data.len() != 10 || self.sample_rate <= 0 {
        return Some(Dropped);
      }
      if self.family == Family::Mp4 {
        // MP4 reads the end skip alone, and only to trim the track's end in
        // its edit list — which the last packet's sets — where the codec
        // fixes a frame's samples (`movenc.c` 7311–7319, 7333–7342).
        return Some(if self.frame_size == 0 {
          Dropped
        } else {
          PossiblyDropped
        });
      }
      // Matroska writes an audio block's end skip as its discard padding
      // (`matroskaenc.c` 2922–2934), and a start skip only as the stream's
      // codec delay, from its initial padding (2089–2104), which its reader
      // gives back to the track's first packet (`matroskadec.c`
      // 2896–2901); the reasons, never.
      let start = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
      let start_kept = start == 0 || (first && i64::from(start) == i64::from(self.initial_padding));
      let kept = self.kind == TrackKind::Audio && start_kept && data[8] == 0 && data[9] == 0;
      (!kept).then_some(Dropped)
    } else if either_matroska
      && (is(T::AV_PKT_DATA_WEBVTT_IDENTIFIER) || is(T::AV_PKT_DATA_WEBVTT_SETTINGS))
    {
      // A WebVTT cue's lines (`matroskaenc.c` 2828–2855, 3591–3593).
      (self.codec != CodecId::WEBVTT).then_some(Dropped)
    } else if either_matroska && is(T::AV_PKT_DATA_MATROSKA_BLOCKADDITIONAL) {
      // The codec's own block addition, id 1, alone (2936–2946).
      let id = data.first_chunk::<8>().map(|id| u64::from_be_bytes(*id));
      (id != Some(1)).then_some(Dropped)
    } else if either_matroska
      && (is(T::AV_PKT_DATA_DYNAMIC_HDR10_PLUS)
        || is(T::AV_PKT_DATA_DYNAMIC_HDR_SMPTE_2094_APP5)
        || is(T::AV_PKT_DATA_LCEVC))
    {
      // A video block's ITU-T T.35 additions (2947–3006), which the reader
      // turns back into their kinds (`matroskadec.c` 4038–4130).
      (self.kind != TrackKind::Video).then_some(Dropped)
    } else {
      Some(Dropped)
    }
  }
}

/// How a track reaches the container.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Road {
  Timed,
  Attachment,
  Cover,
  NotWritten,
}

/// The write side's two vocabularies of refusal.
enum Refusal {
  Portable(MuxRefusal),
  Container(FfmpegMuxRefusal),
}

impl Refusal {
  fn with_packet<P>(self, packet: P) -> MuxError<MuxFault, FfmpegMuxRefusal, P> {
    match self {
      Self::Portable(refusal) => MuxError::Refused { refusal, packet },
      Self::Container(refusal) => MuxError::Backend { refusal, packet },
    }
  }
}

/// A packet admitted: where it goes and what libavformat is told.
struct Plan {
  track: TrackIndex,
  stream: usize,
  /// libavformat's shift after the packet, where this muxer mirrors it.
  shift: Option<Shift>,
  dts: Timestamp,
  times: PacketTimes,
  /// What libavformat's interleaving is handed at the packet's write, by
  /// the times each carries: what the track's filter puts out ([`Emits`]).
  outputs: Vec<Timed>,
  /// Whether this muxer holds the packet, with its display set, handing
  /// libavformat nothing.
  holds: bool,
  /// What the packets this muxer holds of the track put out, once handed.
  pending: Vec<Timed>,
  /// What the track's automatic filter holds after the packet.
  filter: Filter,
  /// What the container learns of the track from the packet.
  learned: Learned,
  /// What the report learns from the packet, once it is written.
  uncarried: Vec<Uncarried>,
}

/// What a container learns of a track from a packet it writes, for the
/// next one's judgment.
#[derive(Debug, Clone, Copy, Default)]
struct Learned {
  /// MP4: the sample MP4 writes with the packet, where it writes one —
  /// not where it holds the packet (table C″ of the census).
  sample: Option<Sample>,
  /// MP4 `mov_text`: the samples MP4 has written of the track after the
  /// cue.
  cues: Option<Cues>,
  /// MP4 AC-3 and E-AC-3: what MP4 has learned of the frames.
  ac3: Option<Ac3State>,
  /// MP4 E-AC-3: a packet joined to the sample MP4 makes of the frames it
  /// holds, which takes the run's first packet's side data: the packet's
  /// own is never written, but its new extradata, which MP4 reads before
  /// holding it (`movenc.c` 7044–7074, 7175–7181).
  joined: bool,
  /// MP4 TTML from libavcodec's TTML encoder: the joined document's bytes.
  document: Option<u64>,
  /// The packet's duration as offered is not kept — a negative one, which
  /// libavformat or the container writes as zero, or as libavformat's
  /// guess; or a `mov_text` cue shortens the duration MP4 keeps of the last
  /// one (table C″).
  durations: bool,
}

/// The FFmpeg muxer, on carrier `C` — see [the module](self).
pub struct CarrierMuxer<W, C: FfmpegCarrier> {
  output: Output<W>,
  container: ContainerName,
  tracks: Vec<TrackState>,
  limits: PacketLimits,
  bounds: Bounds,
  /// What the open answered, then what writing has learned.
  report: MuxReport,
  /// libavformat's shift of every stream's times, where it makes one and
  /// this muxer mirrors it.
  shift: Option<Shift>,
  /// PGS: the packets of each display set left open, in the order offered,
  /// which no packet has completed ([`Self::take_held`]).
  held: Vec<HeldPacket<C::Buffer>>,
  _carrier: PhantomData<fn() -> C>,
}

/// The FFmpeg muxer over the **view** lane's packets — what
/// `mediadecode-ffmpeg`'s `FfmpegDemuxer` delivers.
pub type FfmpegMuxer<W> = CarrierMuxer<W, View>;

/// The FFmpeg muxer over the **owned** lane's packets.
pub type FfmpegOwnedMuxer<W> = CarrierMuxer<W, Owned>;

impl<W, C> CarrierMuxer<W, C>
where
  W: Write + Seek + Send + 'static,
  C: FfmpegCarrier + CarrierOps,
{
  /// Opens a muxer for `container` writing into `sink`, with one track
  /// per description in `tracks` — the track at `TrackIndex::new(i)` is
  /// the `i`-th — and `chapters`, each packet's payload bounded by `limits`
  /// and libavformat's automatic filters on: [`Self::open_with`] with
  /// [`MuxOptions::new`] and those limits.
  pub fn open(
    sink: W,
    container: ContainerName,
    tracks: Vec<TrackDescription<Ffmpeg, C::Buffer, CodecTicket>>,
    chapters: &[Chapter<Ffmpeg>],
    limits: PacketLimits,
  ) -> Result<(Self, MuxReport), MuxOpenError> {
    Self::open_with(
      sink,
      container,
      tracks,
      chapters,
      MuxOptions::new().with_limits(limits),
    )
  }

  /// Opens a muxer for `container` writing into `sink`, with one track
  /// per description in `tracks` — the track at `TrackIndex::new(i)` is
  /// the `i`-th — and `chapters`, as `options` say.
  ///
  /// Judges the whole table first and writes nothing on a refusal
  /// ([`MuxOpenError`]); then writes the header, an attachment Matroska
  /// keeps with it and any cover picture, and answers what the container
  /// could not carry, as far as it is known at open ([`MuxReport`]): the
  /// muxer's own report grows as it writes ([`Self::report`]). `sink` is
  /// dropped with a refusal or a fault.
  pub fn open_with(
    sink: W,
    container: ContainerName,
    tracks: Vec<TrackDescription<Ffmpeg, C::Buffer, CodecTicket>>,
    chapters: &[Chapter<Ffmpeg>],
    options: MuxOptions,
  ) -> Result<(Self, MuxReport), MuxOpenError> {
    let limits = options.limits();
    let filters = options.automatic_filters();
    let bounds = options.bounds;
    let mut output = Output::open(sink, container.as_str()).map_err(|error| match error {
      OutputError::UnknownContainer { .. } | OutputError::Nul { .. } => {
        MuxOpenError::UnknownContainer {
          name: container.clone(),
        }
      }
      OutputError::NoByteStream { .. } => MuxOpenError::NoByteStream {
        name: container.clone(),
      },
      error => MuxOpenError::Fault(MuxFault::new(MuxStage::Open, error)),
    })?;
    // The muxer libavformat resolved the name to, by its own name: the
    // caller's spelling is matched case-insensitively.
    let family = Family::of(output.container());
    // The filters that muxer inserts (table B).
    let sites = Sites::of(output.container(), output.container_mime_type());
    let mut report = MuxReport::default();
    // The automatic filters as the caller chose: on unless turned off, as
    // libavformat and `ffmpeg -c copy` have them. In a container whose
    // packet path this release accounts for, each filter is mirrored and
    // its conversion reported for its track (see the module); another
    // container's are its own.
    output
      .set_automatic_filters(filters)
      .map_err(|error| MuxFault::new(MuxStage::Open, error))?;

    // **The whole table is judged before anything is added**, so a
    // refusal leaves nothing written.
    let mut roads = Vec::with_capacity(tracks.len());
    let mut lost = Vec::with_capacity(tracks.len());
    for (index, description) in tracks.iter().enumerate() {
      let track = TrackIndex::new(index);
      let (road, losses) = judge(&output, family, sites, track, description, filters, limits)?;
      roads.push(road);
      lost.push(losses);
    }
    let mut spans = Vec::with_capacity(chapters.len());
    for (index, chapter) in chapters.iter().enumerate() {
      spans.push(chapter_span(index, chapter)?);
      if family == Family::Mp4 {
        match chapter.title() {
          None => return Err(MuxOpenError::ChapterWithoutTitle { chapter: index }),
          Some(title) if title.as_str().len() > usize::from(u16::MAX) => {
            return Err(MuxOpenError::ChapterTitleTooLong { chapter: index });
          }
          Some(_) => {}
        }
      }
    }
    // MP4's chapter track, whose samples MP4 makes at its header from the
    // chapters, counted in the movie timescale its streams' time bases make
    // (table C′ of the census).
    let movie_timescale = mp4_movie_timescale(
      tracks
        .iter()
        .zip(&roads)
        .filter(|(_, road)| !matches!(road, Road::NotWritten))
        .map(|(description, _)| description),
    );
    if family == Family::Mp4
      && let Some(chapter) = mp4_chapter_sample_refused(chapters, &spans, movie_timescale)
    {
      return Err(MuxOpenError::ChapterSample { chapter });
    }
    // MP4's default selection, which needs the whole table (table E).
    let defaults_added = mp4_defaults_added(family, &tracks, &roads);

    let strict = output.strict_decode_times();
    let mut states = Vec::with_capacity(tracks.len());
    for (index, (description, road)) in tracks.iter().zip(&roads).enumerate() {
      let track = TrackIndex::new(index);
      let kind = description.kind();
      let stream = match road {
        Road::NotWritten => None,
        road => {
          let spec = stream_spec(family, description, *road);
          let stream = output
            .add_stream(&spec)
            .map_err(|error| MuxFault::new(MuxStage::Stream { track }, error))?;
          let codec = output
            .stream_codec(stream)
            .map_err(|error| MuxFault::new(MuxStage::Stream { track }, error))?;
          // `pcm_rechunk`'s start counts an audio track's sample size in
          // `int` (table B), as the stream holds it.
          if sites.rechunk
            && kind == TrackKind::Audio
            && rechunk_overflows(codec.channels(), codec.codec().bits_per_sample())
          {
            return Err(MuxOpenError::SampleSize { track });
          }
          Some((stream, codec))
        }
      };
      // The report, track by track: what the road drops of the track's
      // fields and the flags it adds, then the automatic filter that
      // converts its packets, then what the container does to them, judged
      // from the stream as libavformat holds it.
      if defaults_added[index] {
        // One line of added flags a track: the default joins those the
        // road adds.
        match lost[index].last_mut() {
          Some(Uncarried::DispositionAdded { flags, .. }) => *flags |= TrackDisposition::DEFAULT,
          _ => lost[index].push(Uncarried::DispositionAdded {
            track,
            flags: TrackDisposition::DEFAULT,
          }),
        }
      }
      report.uncarried.append(&mut lost[index]);
      let mut filter = Filter::None;
      if let (Road::Timed, Some((_, codec))) = (road, &stream) {
        filter = Filter::settle(
          sites,
          family,
          (kind, codec.codec()),
          codec.extradata(),
          filters,
        );
        report.uncarried.extend(
          filter
            .named()
            .map(|filter| Uncarried::Filtered { track, filter }),
        );
        let frame_rate = match description.params() {
          TrackParams::Video(params) => params.frame_rate(),
          _ => None,
        };
        report.uncarried.extend(
          rewrites(
            family,
            codec.codec(),
            codec.extradata(),
            default_duration(kind, codec, frame_rate),
          )
          .map(|rewrite| Uncarried::Rewritten { track, rewrite }),
        );
      }
      states.push((kind, stream, *road, filter));
    }
    report
      .uncarried
      .extend(chapter_losses(family, chapters, &spans, movie_timescale));
    for (index, (chapter, (start, end))) in chapters.iter().zip(spans).enumerate() {
      output
        .add_chapter(
          chapter.id(),
          chapter.timebase(),
          start,
          end,
          chapter.title().map(Utf8Bytes::as_str),
        )
        .map_err(|error| MuxFault::new(MuxStage::Chapter { chapter: index }, error))?;
    }
    output
      .write_header()
      .map_err(|error| MuxFault::new(MuxStage::Header, error))?;

    let shifts = output.shifts_early_times();
    let mut track_states = Vec::with_capacity(states.len());
    for (index, (kind, stream, road, filter)) in states.into_iter().enumerate() {
      let track = TrackIndex::new(index);
      let mut rules = PacketRules::default();
      let mut sides = SideRules::unclaimed();
      let stream = match (stream, road) {
        (Some((stream, codec)), Road::Timed) => {
          let time_base = output
            .stream_time_base(stream)
            .map_err(|error| MuxFault::new(MuxStage::Header, error))?;
          rules = PacketRules::settle(family, kind, &codec, time_base, shifts);
          // MP4 joins the paragraphs of libavcodec's TTML encoder into one
          // document at the trailer, its header made of the configuration's
          // strings and the language (`ttmlenc.c` 124–158).
          if family == Family::Mp4
            && codec.codec() == CodecId::TTML
            && codec.extradata().starts_with(b"lavc-ttmlenc")
          {
            let language = tracks[index].language().map_or(0, |tag| tag.as_str().len());
            rules.ttml_document =
              Some((codec.extradata().len() + language) as u64 + TTML_DOCUMENT_OVERHEAD);
          }
          sides = SideRules::of(family, kind, &codec);
          Some((stream, time_base))
        }
        // An attachment stream takes no packet; a cover takes its one,
        // here, and nothing after it. (A description of an attachment
        // always carries its file; the guard is the type's, not ours.)
        (Some((stream, _)), Road::Cover) => {
          if let Some(file) = tracks[index].file() {
            output
              .write_packet::<C>(
                stream,
                OutputPacket::Cover(file.payload()),
                PacketTimes::new(Some(0), Some(0), 0),
                limits,
              )
              .map_err(|error| MuxFault::new(MuxStage::Cover { track }, error))?;
          }
          None
        }
        _ => None,
      };
      track_states.push(TrackState {
        kind,
        stream,
        strict: strict && !matches!(kind, TrackKind::Subtitle | TrackKind::Data),
        last: None,
        last_ticks: None,
        handed: None,
        pending: Vec::new(),
        sample: None,
        rules,
        filter,
        sides,
        written: false,
        cues: Cues::default(),
        ac3: Ac3State::default(),
        document: rules.ttml_document.unwrap_or(0),
      });
    }

    // libavformat's shift of early times, mirrored where it can be; where
    // not, a time before the earliest stays refused (table G).
    let shift = Shift::of(
      family,
      output.container(),
      shifts,
      &track_states
        .iter()
        .map(|state| (state.kind, state.sides.codec, state.stream.is_some()))
        .collect::<Vec<_>>(),
    );
    let either = family != Family::Other
      && track_states.iter().any(|state| {
        state.kind == TrackKind::Video
          && state.stream.is_some()
          && !MATROSKA_NATIVE_VIDEO.contains(&state.sides.codec)
          && state.sides.codec != CodecId::JPEG2000
      });
    for state in &mut track_states {
      if state.rules.earliest.is_some() && shift.is_none() {
        state.rules.before_zero = Some(if either {
          BeforeZero::RefusedEither
        } else {
          BeforeZero::Refused
        });
      }
    }

    Ok((
      Self {
        output,
        container,
        tracks: track_states,
        limits,
        bounds,
        report: report.clone(),
        shift,
        held: Vec::new(),
        _carrier: PhantomData,
      },
      report,
    ))
  }

  /// What the container could not carry: what the open answered, then what
  /// the muxer learned writing — a track's durations MP4 does not keep, the
  /// packet side data its container drops — in the order learned. What it
  /// learns comes from packets written, so it is whole once the last is,
  /// but for what only the end decides, which
  /// [`Self::finish_into_sink`] adds ([`Uncarried::Held`]).
  pub const fn report(&self) -> &MuxReport {
    &self.report
  }

  /// The packets of every display set this muxer holds open, handed back in
  /// the order offered, each on its track. A PGS packet that leaves its
  /// display set without its end segment is held here, with the packets of
  /// the set before it, and not handed to libavformat — whose filter would
  /// hold it out of reach, and write it at the end marked corrupt
  /// (`pgs_frame_merge.c` 77–83, 132–151 in FFmpeg 9.0.1) — until a packet
  /// completes the set: a set whose completing packet is refused stays held,
  /// and this hands it back. Each track handed back starts a new display
  /// set, its last decode time what it was before the set. What
  /// [`Self::finish_into_sink`] still finds held is not written, and is
  /// reported ([`Uncarried::Held`]).
  pub fn take_held(&mut self) -> Vec<SubtitleTrackPacket<Ffmpeg, C::Buffer>> {
    let held = core::mem::take(&mut self.held);
    // Newest first, so that each track ends at its first held packet's.
    for entry in held.iter().rev() {
      let state = &mut self.tracks[entry.packet.track().get()];
      state.last = entry.last;
      state.filter = Filter::Pgs(PgsState::default());
      state.pending.clear();
    }
    held.into_iter().map(|entry| entry.packet).collect()
  }

  /// The container this muxer writes.
  pub const fn container(&self) -> &ContainerName {
    &self.container
  }

  /// Completes the container and hands the sink back, with the whole
  /// report — [`Muxer::finish`], keeping both. The report gains what only
  /// the stream's end decides: the packets an automatic filter still held
  /// ([`Uncarried::Held`]), and where MP4 ends an E-AC-3 track's last
  /// joined sample short of its packets ([`Rewrite::Durations`]).
  pub fn finish_into_sink(mut self) -> Result<(W, MuxReport), MuxFault> {
    // A trailer the container would fail for want of a frame no packet was
    // left to decide, or would end the process over the last sample's
    // duration, which only the stream's end decides: refused by name before
    // it runs, and the context dropped as it stands (tables C and C″ of the
    // census).
    let limit = i128::from(i32::MAX);
    for (index, state) in self.tracks.iter().enumerate() {
      let Some(rules) = state.rules.ac3 else {
        continue;
      };
      let track = TrackIndex::new(index);
      if !state.written {
        return Err(MuxFault::refused(
          MuxStage::Trailer,
          FfmpegMuxRefusal::NoFrame { track },
        ));
      }
      if rules.durations_read && state.ac3.last.is_some_and(|sample| sample.duration > limit) {
        return Err(MuxFault::refused(
          MuxStage::Trailer,
          FfmpegMuxRefusal::LastSampleTooLong { track },
        ));
      }
    }
    for (index, state) in self.tracks.iter().enumerate() {
      let track = TrackIndex::new(index);
      if let Some((filter, packets)) = held_at_end(state.filter) {
        self.report.uncarried.push(Uncarried::Held {
          track,
          filter,
          packets,
        });
      }
      // PGS: a display set no packet completed, which this muxer held and
      // does not write.
      let held = self
        .held
        .iter()
        .filter(|entry| entry.packet.track() == track)
        .count() as u64;
      if held > 0 {
        self.report.uncarried.push(Uncarried::Held {
          track,
          filter: AutomaticFilter::PgsFrameMerge,
          packets: held,
        });
      }
      // MP4 ends its edit list at the last sample's decode time and its last
      // packet's own duration (`movenc.c` 6940, 7329–7342): a joined E-AC-3
      // sample is presented short of the frames it joins.
      let ended_short = Uncarried::Rewritten {
        track,
        rewrite: Rewrite::Durations,
      };
      if state
        .ac3
        .last
        .is_some_and(|sample| sample.closing < sample.duration)
        && !self.report.uncarried.contains(&ended_short)
      {
        self.report.uncarried.push(ended_short);
      }
    }
    self
      .output
      .write_trailer()
      .map_err(|error| MuxFault::new(MuxStage::Trailer, error))?;
    let Self { output, report, .. } = self;
    let sink = output
      .into_sink()
      .map_err(|error| MuxFault::new(MuxStage::Finish, error))?;
    Ok((sink, report))
  }

  /// Judges a timed packet: the face's rules — the presentation rule on
  /// the counts libavformat is handed — then the container's, in the
  /// packet-rule table's order (see [the module](self)).
  fn admit(
    &self,
    track: TrackIndex,
    offered: TrackKind,
    pts: Option<Timestamp>,
    dts: Option<Timestamp>,
    duration: Option<Timestamp>,
    payload: Payload<'_>,
  ) -> Result<Plan, Refusal> {
    let state = self
      .tracks
      .get(track.get())
      .ok_or(Refusal::Portable(MuxRefusal::UnknownTrack { track }))?;
    if state.kind != offered {
      return Err(Refusal::Portable(MuxRefusal::KindMismatch {
        track,
        expected: state.kind,
        offered,
      }));
    }
    let dts = dts.ok_or(Refusal::Portable(MuxRefusal::MissingDts { track }))?;
    check_decode_order(track, state.last, dts).map_err(Refusal::Portable)?;
    // A timed track of a known kind always has its stream: only an
    // attachment track takes none, and its kind is refused above.
    let Some((stream, time_base)) = state.stream else {
      return Err(Refusal::Portable(MuxRefusal::KindMismatch {
        track,
        expected: state.kind,
        offered,
      }));
    };
    // Each time counted in the stream's time base, as libavformat is
    // handed it — or refused by name, the packet untouched.
    let unrepresentable = |timestamp: Timestamp| {
      Refusal::Container(FfmpegMuxRefusal::Unrepresentable { track, timestamp })
    };
    let count = |timestamp: Timestamp| {
      timestamp
        .checked_rescale_with(time_base, Rounding::Nearest)
        .map(|rescaled| rescaled.pts())
        .ok_or_else(|| unrepresentable(timestamp))
    };
    // A presentation or decode time may not land on `AV_NOPTS_VALUE`, the
    // count libavformat reads as no timestamp at all. A duration has no
    // such count: libavformat zeroes a negative one (`mux.c` 607–611).
    let stamp = |timestamp: Timestamp| -> Result<i64, Refusal> {
      match count(timestamp)? {
        ticks if ticks == AV_NOPTS_VALUE => Err(unrepresentable(timestamp)),
        ticks => Ok(ticks),
      }
    };
    let dts_ticks = stamp(dts)?;
    let pts_ticks = pts.map(stamp).transpose()?;
    // The face's presentation rule, applied as the container applies it:
    // to the counts libavformat is handed, in the stream's time base
    // (`mux.c` 567–572). Two instants a hair apart may count to one tick,
    // and a packet whose presentation and decode times do is one
    // libavformat writes — as a stream copy of a file with sub-tick
    // jitter does — so it is admitted. The refusal names the times the
    // packet carries, not their counts.
    if let (Some(pts), Some(pts_ticks)) = (pts, pts_ticks) {
      let stored = |ticks: i64| Timestamp::new(ticks, time_base);
      if check_presentation_order(track, Some(stored(pts_ticks)), stored(dts_ticks)).is_err() {
        return Err(Refusal::Portable(MuxRefusal::PtsBeforeDts {
          track,
          pts,
          dts,
        }));
      }
    }
    let duration_ticks = duration.map(count).transpose()?.unwrap_or(0);
    // The packet as libavcodec builds it holds one entry a kind.
    if side_data_repeated(payload.side_data) {
      return Err(Refusal::Container(FfmpegMuxRefusal::Payload {
        track,
        rule: PayloadRule::SideDataRepeated,
      }));
    }
    // What the packet's builder refuses that the packet alone decides, and
    // the largest packet the copy road allocates (table B): refused with
    // the packet in hand, where the builder would fail the write after it
    // was taken.
    if let Some(rule) = build_refusal(
      payload.flags,
      payload.bytes.len(),
      payload.side_data,
      self.limits,
    ) {
      return Err(Refusal::Container(FfmpegMuxRefusal::Payload {
        track,
        rule,
      }));
    }
    // The track's automatic filter takes the packet before libavformat's
    // timing does (`write_packets_common`, `mux.c` 1151–1174): its own
    // refusals come first, and what libavformat's timing and the container
    // meet is what the filter puts out now ([`Emits`]) — a packet it holds
    // meets nothing until a packet it puts out carries its times, and every
    // mirror of their timing moves on what is put out alone.
    let side: u64 = payload
      .side_data
      .iter()
      .map(|entry| entry.data().len() as u64)
      .sum();
    let own = Timed {
      track,
      pts,
      dts,
      duration,
      pts_ticks,
      dts_ticks,
      duration_ticks,
    };
    let (filter, step) = state
      .filter
      .step(own, payload.bytes, side, self.bounds.packet)
      .map_err(|rule| Refusal::Container(FfmpegMuxRefusal::Payload { track, rule }))?;
    // PGS: a packet that leaves its display set open is held by this muxer,
    // with every packet of the set before it, and nothing of them reaches
    // libavformat until a packet completes the set — inside FFmpeg the
    // filter would hold them out of reach of a later refusal, and write
    // them at the end marked corrupt (`pgs_frame_merge.c` 77–83, 132–151;
    // [`Self::take_held`]). What they put out is put out when the packet
    // that completes the set hands them over.
    let holds =
      offered == TrackKind::Subtitle && matches!(filter, Filter::Pgs(next) if next.holding);
    let emitted = state.pending.iter().copied().chain(step.emits.times(own));
    let (outputs, pending): (Vec<Timed>, Vec<Timed>) = if holds {
      (Vec::new(), emitted.collect())
    } else {
      (emitted.collect(), Vec::new())
    };
    // libavformat's rule, exactly, on every packet it is handed: nothing is
    // checked while the last decode time is zero — its counter starts there
    // — and a repeat is refused after any other.
    let mut last_ticks = state.last_ticks;
    for output in &outputs {
      if state.strict && matches!(last_ticks, Some(last) if last != 0 && last >= output.dts_ticks) {
        return Err(Refusal::Container(FfmpegMuxRefusal::RepeatedDts {
          track,
          dts: output.dts,
        }));
      }
      last_ticks = Some(output.dts_ticks);
    }
    // libavformat's shift of early times, which its first write settles
    // (table G), met by every packet its interleaving is handed: one before
    // that write is lifted with every stream; one after it the shift leaves
    // before the earliest is written there.
    let shift = match &self.shift {
      Some(current) => {
        let lowest = match state.rules.earliest {
          Some(Earliest::Presentation(lowest) | Earliest::Decode(lowest)) => lowest,
          None => 0,
        };
        let mut next = current.clone();
        for output in &outputs {
          // A reordering stream's packet without the presentation time
          // judged is refused below ([`FfmpegMuxRefusal::MissingPts`]), and
          // would move nothing: libavformat judges no packet without its
          // time (`mux.c` 650–651).
          if next.by_pts && output.pts.is_none() && state.rules.needs_pts {
            continue;
          }
          let (at, timestamp) = if next.by_pts {
            (
              output.pts_ticks.unwrap_or(output.dts_ticks),
              output.pts.unwrap_or(output.dts),
            )
          } else {
            (output.dts_ticks, output.dts)
          };
          next = next
            .take(track.get(), (at, output.dts_ticks, lowest), time_base)
            .ok_or(Refusal::Container(FfmpegMuxRefusal::BeforeZero {
              track,
              timestamp,
            }))?;
        }
        Some(next)
      }
      None => None,
    };
    let neighbours = if state.rules.mov_text {
      self.neighbours(track, time_base)
    } else {
      Neighbours::default()
    };
    let learned = check_packet_rules(
      state,
      time_base,
      neighbours,
      (own, &outputs),
      payload,
      step,
      self.bounds,
    )
    .map_err(Refusal::Container)?;
    let mut uncarried = learned_losses(
      state,
      track,
      &self.report.uncarried,
      payload,
      &learned,
      step.owns,
    );
    // The filter this packet's write inserts, where the first packet
    // decides it: named for the track then, once (table B).
    if let Some(filter) = step.inserted {
      let line = Uncarried::Filtered { track, filter };
      if !self.report.uncarried.contains(&line) {
        uncarried.insert(0, line);
      }
    }
    // The shift, once a packet before the earliest makes it certain.
    if shift.as_ref().is_some_and(|shift| shift.earliest.is_some())
      && self
        .shift
        .as_ref()
        .is_some_and(|shift| shift.earliest.is_none())
    {
      uncarried.push(Uncarried::TimesShifted);
    }
    Ok(Plan {
      track,
      stream,
      shift,
      dts,
      times: PacketTimes::new(pts_ticks, Some(dts_ticks), duration_ticks),
      outputs,
      holds,
      pending,
      filter,
      learned,
      uncarried,
    })
  }

  /// What the file's other timed tracks can make MP4 write before a cue of
  /// `track`, whose stream counts in `time_base` ([`Neighbours`]).
  fn neighbours(&self, track: TrackIndex, time_base: Timebase) -> Neighbours {
    let mut neighbours = Neighbours::default();
    for (index, other) in self.tracks.iter().enumerate() {
      let Some((_, base)) = other.stream else {
        continue;
      };
      if index == track.get() {
        continue;
      }
      // A tick of `base` shorter than one of `time_base`.
      if i128::from(base.num()) * i128::from(time_base.den().get())
        < i128::from(time_base.num()) * i128::from(base.den().get())
      {
        neighbours.finer = true;
      }
      neighbours.latest = neighbours.latest.max(other.handed);
    }
    neighbours
  }

  /// Records a written packet, or names the fault.
  fn settle<P>(
    &mut self,
    plan: Plan,
    written: Result<(), OutputError>,
  ) -> Result<(), MuxError<MuxFault, FfmpegMuxRefusal, P>> {
    written.map_err(|error| {
      MuxError::Fault(MuxFault::new(MuxStage::Write { track: plan.track }, error))
    })?;
    if let Some(shift) = plan.shift {
      self.shift = Some(shift);
    }
    let state = &mut self.tracks[plan.track.get()];
    state.last = Some(plan.dts);
    state.filter = plan.filter;
    state.pending = plan.pending;
    // What libavformat was handed moves every mirror of its timing and the
    // container's: the last packet the filter put out.
    if let Some(last) = plan.outputs.last() {
      state.last_ticks = Some(last.dts_ticks);
      state.handed = Some(last.dts);
      state.written = true;
    }
    if let Some(sample) = plan.learned.sample {
      state.sample = Some(sample);
    }
    if let Some(cues) = plan.learned.cues {
      state.cues = cues;
    }
    if let Some(ac3) = plan.learned.ac3 {
      state.ac3 = ac3;
    }
    if let Some(document) = plan.learned.document {
      state.document = document;
    }
    for line in plan.uncarried {
      if !self.report.uncarried.contains(&line) {
        self.report.uncarried.push(line);
      }
    }
    Ok(())
  }

  /// Holds a PGS packet `plan` admitted, with the display set it leaves
  /// open: nothing of it reaches libavformat, and its report waits for the
  /// write that hands it over ([`Self::take_held`]).
  fn hold(&mut self, plan: Plan, packet: SubtitleTrackPacket<Ffmpeg, C::Buffer>) {
    let state = &mut self.tracks[plan.track.get()];
    let last = state.last;
    state.last = Some(plan.dts);
    state.filter = plan.filter;
    state.pending = plan.pending;
    self.held.push(HeldPacket {
      packet,
      times: plan.times,
      uncarried: plan.uncarried,
      last,
    });
  }
}

/// A PGS packet this muxer holds, with the display set it leaves open,
/// until a packet completes the set ([`CarrierMuxer::take_held`]).
struct HeldPacket<B> {
  packet: SubtitleTrackPacket<Ffmpeg, B>,
  /// What libavformat is told of it, once handed.
  times: PacketTimes,
  /// What the report learns from it, once written.
  uncarried: Vec<Uncarried>,
  /// Its track's last decode time before it, which handing it back
  /// restores.
  last: Option<Timestamp>,
}

/// What the report learns from a packet of `track` once it is written —
/// each line once ([`Uncarried`]), `reported` the report so far, `learned`
/// what the container learns of the track from it:
///
/// - MP4 keeps a duration only as the distance to the next decode time
///   ([`Rewrite::Durations`]), so a sample MP4 writes that does not abut
///   the last one it wrote loses that one's offered duration — counted
///   between the samples MP4 writes, not the packets it holds (table C″);
/// - the packet's side data the container drops, by the side-data table
///   ([`SideRules::fate`]) — all of it, where the automatic filter drops it
///   with the packet's times, or where MP4 joins it to a sample of E-AC-3 frames, which
///   takes the run's first packet's (all but its new extradata, which MP4
///   reads first), and its new extradata, on the first frame AAC's filter
///   converts, which the filter replaces with the configuration it makes
///   (`aac_adtstoasc.c` 98–103).
fn learned_losses(
  state: &TrackState,
  track: TrackIndex,
  reported: &[Uncarried],
  payload: Payload<'_>,
  learned: &Learned,
  owns: bool,
) -> Vec<Uncarried> {
  let (payload_bytes, side_data) = (payload.bytes, payload.side_data);
  let mut lines = Vec::new();
  let mut push = |line: Uncarried| {
    if !reported.contains(&line) && !lines.contains(&line) {
      lines.push(line);
    }
  };
  if learned.durations
    || matches!((learned.sample, state.sample), (Some(sample), Some(last))
      if last.offered.is_some_and(|duration| i128::from(sample.dts) - i128::from(last.dts) != duration))
  {
    push(Uncarried::Rewritten {
      track,
      rewrite: Rewrite::Durations,
    });
  }
  for entry in side_data {
    let new_extradata = entry.kind() == AVPacketSideDataType::AV_PKT_DATA_NEW_EXTRADATA as i32;
    let replaced = first_conversion(state.filter, payload_bytes) && new_extradata;
    let fate = if !owns || replaced || (learned.joined && !new_extradata) {
      Some(SideDataFate::Dropped)
    } else {
      state.sides.fate(entry, !state.written)
    };
    if let Some(fate) = fate {
      push(Uncarried::SideData {
        track,
        kind: entry.kind(),
        name: packet_side_data_name(entry.kind()),
        fate,
      });
    }
  }
  lines
}

/// A timed packet's times, as offered and as counted in its stream's time
/// base.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Timed {
  track: TrackIndex,
  pts: Option<Timestamp>,
  dts: Timestamp,
  duration: Option<Timestamp>,
  pts_ticks: Option<i64>,
  dts_ticks: i64,
  duration_ticks: i64,
}

/// What MP4's `check_pkt` judges a packet's times against (`movenc.c`
/// 6897–6930; table C″ of the census): the decode time of the last sample
/// MP4 wrote on the track — a sample, not a packet MP4 holds; for a
/// `mov_text` cue, the empty sample where MP4 writes it first — none before
/// the first; and the longest duration the packet may offer: `INT_MAX`
/// ticks, or a `mov_text` cue's, whose end MP4 marks with an empty sample
/// that `check_pkt` moves where it lies `INT_MAX` or more after it — the
/// cue's duration after it, and the track's first sample's distance before
/// zero (7321, 7652, 9029).
#[derive(Clone, Copy)]
struct Mp4Times {
  last: Option<i128>,
  longest: i128,
}

/// Holds a packet's times to the rows of the packet-rule table that judge
/// them, in the table's order: a subtitle's negative duration, where the
/// container is handed it as it is; MP4's 32-bit fields, against `mp4`,
/// what `check_pkt` judges them by — or, for TTML's paragraphs, against
/// the one sample MP4 joins them into; a missing presentation time on a
/// stream that reorders; a time before the earliest the container writes,
/// where this muxer does not mirror libavformat's move of every stream's
/// times ([`Shift`]), which judges it where it does.
fn check_times(rules: &PacketRules, mp4: Mp4Times, timed: Timed) -> Result<(), FfmpegMuxRefusal> {
  let Timed {
    track,
    pts,
    dts,
    duration,
    pts_ticks,
    dts_ticks,
    duration_ticks,
  } = timed;
  let limit = i128::from(i32::MAX);
  if let Some(duration) = duration
    && duration_ticks < 0
    && rules.negative_duration
  {
    return Err(FfmpegMuxRefusal::NegativeDuration { track, duration });
  }
  if rules.mp4 && rules.ttml_document.is_some() {
    // MP4 holds libavcodec's TTML paragraphs back, and joins them at the
    // trailer into one sample from zero to the latest end (`movenc.c`
    // 7658–7684; `movenc_ttml.c` 108–110, 178, 212–213, 305–306): no
    // paragraph meets `check_pkt`, and the joined sample meets it with that
    // end for its duration, refused past `INT_MAX` with the trailer — the
    // whole file (6925–6928, 9036–9037).
    if i128::from(pts_ticks.unwrap_or(dts_ticks)) + i128::from(duration_ticks) > limit {
      return Err(FfmpegMuxRefusal::DocumentSpan {
        track,
        pts: pts.unwrap_or(dts),
      });
    }
  } else if rules.mp4 {
    if let Some(duration) = duration
      && i128::from(duration_ticks) > mp4.longest
    {
      return Err(FfmpegMuxRefusal::DurationTooLong { track, duration });
    }
    if let Some(last) = mp4.last
      && i128::from(dts_ticks) - last >= limit
    {
      return Err(FfmpegMuxRefusal::DtsGap { track, dts });
    }
    if let (Some(pts), Some(pts_ticks)) = (pts, pts_ticks)
      && i128::from(pts_ticks) - i128::from(dts_ticks) > limit
    {
      return Err(FfmpegMuxRefusal::PresentationOffset { track, pts, dts });
    }
  }
  if rules.needs_pts && pts.is_none() {
    return Err(FfmpegMuxRefusal::MissingPts { track });
  }
  let Some(before_zero) = rules.before_zero else {
    return Ok(());
  };
  match rules.earliest {
    Some(Earliest::Presentation(earliest)) if pts_ticks.unwrap_or(dts_ticks) < earliest => {
      Err(FfmpegMuxRefusal::BeforeZero {
        track,
        timestamp: pts.unwrap_or(dts),
      })
    }
    Some(Earliest::Presentation(earliest))
      if before_zero == BeforeZero::RefusedEither && dts_ticks < earliest =>
    {
      Err(FfmpegMuxRefusal::BeforeZero {
        track,
        timestamp: dts,
      })
    }
    Some(Earliest::Decode(earliest)) if dts_ticks < earliest => Err(FfmpegMuxRefusal::BeforeZero {
      track,
      timestamp: dts,
    }),
    _ => Ok(()),
  }
}

/// Holds a packet the face admitted to the rows of the packet-rule table
/// its track meets ([`PacketRules`]), in the table's order — `step` what
/// its track's filter does with it, `bounds` the sizes held to (table B);
/// answers what the container learns of the track from it, for the next
/// one's judgment.
fn check_packet_rules(
  state: &TrackState,
  time_base: Timebase,
  neighbours: Neighbours,
  (own, outputs): (Timed, &[Timed]),
  payload: Payload<'_>,
  step: Step,
  bounds: Bounds,
) -> Result<Learned, FfmpegMuxRefusal> {
  let owns = step.owns;
  let rules = &state.rules;
  let Timed {
    track,
    dts,
    dts_ticks,
    duration_ticks,
    ..
  } = own;
  let limit = i128::from(i32::MAX);
  // MP4 `mov_text`: the samples MP4 writes around the cue (table C″).
  let cues = rules
    .mov_text
    .then(|| state.cues.take(dts_ticks, duration_ticks));
  // The rules of a packet's times, met by each packet the track's filter
  // puts out with it, in that packet's times ([`Emits`]): a packet the
  // filter holds meets none, its times reaching libavformat's timing and
  // the container only with what the filter puts out.
  let mut last_sample = state.sample.map(|sample| i128::from(sample.dts));
  for output in outputs {
    // `check_pkt` counts a packet's gap from the last sample MP4 wrote on the
    // track — not from a packet MP4 holds — and a `mov_text` cue's from the
    // empty sample where MP4 writes it first (table C″).
    let mp4 = match cues {
      Some(cue) => Mp4Times {
        last: cue.reference,
        // The empty sample after the cue lies its duration after it, and the
        // track's first sample's distance before zero (7321, 7652, 9029).
        longest: limit - 1 + i128::from(cue.next.start.unwrap_or(0)),
      },
      None => Mp4Times {
        last: last_sample,
        longest: limit,
      },
    };
    check_times(rules, mp4, *output)?;
    last_sample = Some(i128::from(output.dts_ticks));
  }
  // A cue before the end MP4 marks the last one with, which MP4 takes as
  // it is — the last cue shortened to it — unless another track's packet
  // can make MP4 mark that end first, moving the cue; and a first cue before
  // zero, which MP4 writes and its reader drops (table C″).
  if let Some(cue) = cues
    && cue.early
    && (state.cues.start.is_none() || !neighbours.leave(state.cues.end, time_base))
  {
    return Err(FfmpegMuxRefusal::CueOrder { track, pts: dts });
  }
  if rules.intra_only && !payload.key {
    return Err(FfmpegMuxRefusal::KeyFrameUnmarked { track });
  }
  let refused = |rule| Err(FfmpegMuxRefusal::Payload { track, rule });
  if rules.no_empty && payload.bytes.is_empty() {
    return refused(PayloadRule::Empty);
  }
  // AAC's filter converts the packet where it is inserted — at the track's
  // first packet, or at this one.
  let converted = matches!(state.filter, Filter::Adts { .. })
    || step.inserted == Some(AutomaticFilter::AacAdtsToAsc);
  if rules.raw_aac && adts_header(payload.bytes) && !converted {
    return refused(PayloadRule::AdtsHeader);
  }
  if let Some(frame) = rules.frame_bytes
    && !payload.bytes.len().is_multiple_of(frame as usize)
  {
    return refused(PayloadRule::PartialSample);
  }
  if rules.obus && !obus_parse(payload.bytes) {
    return refused(PayloadRule::Obus);
  }
  if rules.wavpack && !wavpack_parse(payload.bytes) {
    return refused(PayloadRule::WavPackBlocks);
  }
  // MP4's AC-3 handling, which runs before the skip samples are read
  // (`movenc.c` 7175–7181, 7311–7320): the duration MP4 is handed, and what
  // it does with the packet — a sample of its own, held, or the joined
  // sample it completes.
  let ac3 = match rules.ac3 {
    Some(ac3_rules) => {
      let duration = ac3_rules.handed(duration_ticks);
      let packet = Ac3Packet {
        bytes: payload.bytes,
        dts: dts_ticks,
        duration,
        offered: (duration_ticks > 0).then_some(duration_ticks),
        skip_end: skip_end(payload.side_data),
      };
      match ac3_packet(state.ac3, ac3_rules.eac3, packet, bounds.packet) {
        Ok((learned, ac3_step)) => Some((learned, ac3_step, duration)),
        Err(rule) => return refused(rule),
      }
    }
    None => None,
  };
  if let Some(skip) = rules.skip_samples {
    let below = match ac3 {
      // A joined sample carries its first packet's skip samples, judged at
      // the packet that completes it against that packet's own duration
      // (6940, 7311–7320); a packet MP4 holds meets no such check.
      Some((_, Ac3Step::Opens | Ac3Step::Joins, _)) => false,
      Some((_, Ac3Step::Completes(run), duration)) => run
        .skip_end
        .is_some_and(|end| end_below_nothing(end, duration, skip, time_base)),
      Some((_, Ac3Step::Alone, duration)) => {
        skips_below_nothing(payload.side_data, duration, skip, time_base)
      }
      None => skips_below_nothing(payload.side_data, duration_ticks, skip, time_base),
    };
    if below {
      return refused(PayloadRule::SkipSamples);
    }
  }
  if let Some(len) = rules.streaminfo_len
    && side_data(
      payload.side_data,
      AVPacketSideDataType::AV_PKT_DATA_NEW_EXTRADATA,
    )
    .is_some_and(|new| !new.is_empty() && new.len() != len)
  {
    return refused(PayloadRule::FlacStreamInfo);
  }
  if owns
    && let Some(new) = side_data(
      payload.side_data,
      AVPacketSideDataType::AV_PKT_DATA_NEW_EXTRADATA,
    )
    .filter(|new| !new.is_empty())
  {
    // AAC's filter replaces the first frame's with the configuration it
    // makes (`aac_adtstoasc.c` 98–103).
    let replaced = first_conversion(state.filter, payload.bytes);
    let taken = match rules.new_extradata {
      ExtradataRule::Any => true,
      ExtradataRule::OpusHead => opus_head_written(new),
      ExtradataRule::Form { codec, annex_b } => length_prefixed(Family::Mp4, codec, new) == annex_b,
      ExtradataRule::AudioSpecificConfig { reserved } => {
        replaced || (new.len() <= reserved && audio_specific_config_parses(new))
      }
    };
    if !taken {
      return refused(PayloadRule::NewExtradata);
    }
  }
  // Matroska rewrites an AAC track's configuration with the one AAC's
  // filter makes of the first frame it converts, within the space its
  // header reserved — the configuration's own length, or `MAX_PCE_SIZE`
  // where there was none — and refuses one past it after taking the packet
  // (`mkv_check_new_extra_data`, `matroskaenc.c` 3065–3082;
  // `mkv_update_codecprivate`, 1334–1365).
  if let ExtradataRule::AudioSpecificConfig { reserved } = rules.new_extradata
    && first_conversion(state.filter, payload.bytes)
    && made_configuration(payload.bytes).is_some_and(|made| made > reserved)
  {
    return refused(PayloadRule::NewExtradata);
  }
  if rules.prores && !prores_atom(payload.bytes) {
    return refused(PayloadRule::ProResAtom);
  }
  if rules.webvtt && !webvtt_cue(payload.bytes, payload.side_data) {
    return refused(PayloadRule::WebVttCue);
  }
  // The side data the container's writer casts to libavutil's structs, on
  // a packet whose side data reaches it.
  if rules.hdr_metadata && owns {
    for entry in payload.side_data {
      if let Some(rule) = hdr_metadata_rule(entry) {
        return refused(rule);
      }
    }
  }
  // The sizes the container's arithmetic holds the packet to, as written.
  let len = payload.bytes.len() as u64;
  let written = if rules.length_prefixed {
    let rewritten = length_prefixed_bytes(payload.bytes);
    if rewritten > bounds.packet {
      return refused(PayloadRule::RewrittenSize);
    }
    rewritten
  } else {
    len
  };
  if rules.matroska_block {
    // Every block the container writes of the packet — the packet as
    // written, or each packet the track's filter puts out with it — and
    // what the filter holds, at the least it can come to (table B).
    let blocks = match state.filter {
      Filter::None => {
        let side: u64 = payload
          .side_data
          .iter()
          .map(|entry| entry.data().len() as u64)
          .sum();
        [Some(written + side), None]
      }
      _ => [step.largest, step.held],
    };
    if blocks
      .into_iter()
      .flatten()
      .any(|block| block > bounds.block)
    {
      return refused(PayloadRule::ClusterSize);
    }
  }
  let document = match rules.ttml_document {
    Some(_) => {
      let document = state.document + len + TTML_PARAGRAPH_OVERHEAD;
      if document > bounds.packet {
        return refused(PayloadRule::DocumentSize);
      }
      Some(document)
    }
    None => None,
  };
  // The sample MP4 writes with the packet, where it writes one: the last
  // packet the track's filter puts out with it, and none where the filter
  // puts out none; not where MP4 holds the packet — a frame of fewer than
  // six E-AC-3 blocks — and a joined sample at the packet that completes
  // it, with its first packet's decode time (table C″). `mov_text`'s and
  // TTML's samples are their own.
  let sample = if !rules.mp4 || rules.mov_text || rules.ttml_document.is_some() {
    None
  } else {
    match ac3 {
      Some((_, Ac3Step::Opens | Ac3Step::Joins, _)) => None,
      Some((_, Ac3Step::Completes(run), _)) => Some(Sample {
        dts: run.dts,
        offered: run.offered,
      }),
      _ => outputs.last().map(|output| Sample {
        dts: output.dts_ticks,
        offered: (output.duration_ticks > 0).then(|| i128::from(output.duration_ticks)),
      }),
    }
  };
  // A negative duration libavformat or the container writes as zero, on a
  // packet put out; and `mov_text`: MP4 keeps a cue's duration as the
  // distance to the next sample — the empty one at its end, or a cue before
  // that end (table C″): a cue before the end shortens the last one.
  let durations = outputs.iter().any(|output| output.duration_ticks < 0)
    || cues.is_some_and(|cue| {
      cue.early
        && state.cues.cue.is_some_and(|(at, offered)| {
          i128::from(dts_ticks) - i128::from(at) != i128::from(offered)
        })
    });
  Ok(Learned {
    sample,
    cues: cues.map(|cue| cue.next),
    ac3: ac3.map(|(learned, _, _)| learned),
    joined: matches!(ac3, Some((_, Ac3Step::Joins | Ac3Step::Completes(_), _))),
    document,
    durations,
  })
}

/// The rule an HDR metadata entry Matroska's writer would cast breaks, if
/// it breaks one (table A of the census, see [the module](self)).
fn hdr_metadata_rule(entry: &SideDataEntry) -> Option<PayloadRule> {
  use AVPacketSideDataType as T;
  if entry.kind() == T::AV_PKT_DATA_DYNAMIC_HDR10_PLUS as i32 {
    check_hdr10_plus(entry.data())
      .is_err()
      .then_some(PayloadRule::DynamicHdrPlus)
  } else if entry.kind() == T::AV_PKT_DATA_DYNAMIC_HDR_SMPTE_2094_APP5 as i32 {
    check_smpte2094_app5(entry.data())
      .is_err()
      .then_some(PayloadRule::DynamicHdrApp5)
  } else {
    None
  }
}

/// What the copy road's packet builder would refuse of a packet with
/// `flags`, a payload of `len` bytes and `side_data`, under `limits` — the
/// base's one judgment of what the packet alone decides
/// ([`check_packet_build`]) — and the payload past the largest packet
/// `av_new_packet` allocates ([`PACKET_BYTES_MAX`]), which the copy road
/// fails on (`libavcodec/packet.c` 83–95 in FFmpeg 9.0.1).
fn build_refusal(
  flags: PacketFlags,
  len: usize,
  side_data: &[SideDataEntry],
  limits: PacketLimits,
) -> Option<PayloadRule> {
  match check_packet_build(flags, len, side_data, limits) {
    Ok(()) if len as u64 > PACKET_BYTES_MAX => Some(PayloadRule::PayloadTooLarge),
    Ok(()) => None,
    Err(PacketRefusal::TrustedPayload(_)) => Some(PayloadRule::Trusted),
    Err(PacketRefusal::SendPayloadTooLarge(_)) => Some(PayloadRule::PayloadTooLarge),
    Err(PacketRefusal::SendSideDataTooLarge(_)) => Some(PayloadRule::SideDataBudget),
    Err(PacketRefusal::UnknownSideData(_)) => Some(PayloadRule::SideDataUnnamed),
  }
}

/// Whether two of `side_data`'s entries are of one kind, which libavcodec
/// would fold into one (`av_packet_add_side_data`, `libavcodec/packet.c`
/// 197–227 in FFmpeg 9.0.1).
fn side_data_repeated(side_data: &[SideDataEntry]) -> bool {
  side_data.iter().enumerate().any(|(at, entry)| {
    side_data[..at]
      .iter()
      .any(|earlier| earlier.kind() == entry.kind())
  })
}

impl<W, C> Muxer for CarrierMuxer<W, C>
where
  W: Write + Seek + Send + 'static,
  C: FfmpegCarrier + CarrierOps,
{
  type Adapter = Ffmpeg;
  type Buffer = C::Buffer;
  type Refusal = FfmpegMuxRefusal;
  type Error = MuxFault;

  fn write_video(
    &mut self,
    packet: VideoTrackPacket<Ffmpeg, C::Buffer>,
  ) -> Result<(), MuxError<MuxFault, FfmpegMuxRefusal, VideoTrackPacket<Ffmpeg, C::Buffer>>> {
    let p = packet.packet();
    let payload = Payload {
      bytes: p.data().as_ref(),
      key: p.flags().contains(PacketFlags::KEY),
      flags: p.flags(),
      side_data: p.extra().side_data(),
    };
    let plan = match self.admit(
      packet.track(),
      TrackKind::Video,
      p.pts(),
      p.dts(),
      p.duration(),
      payload,
    ) {
      Ok(plan) => plan,
      Err(refusal) => return Err(refusal.with_packet(packet)),
    };
    let written =
      self
        .output
        .write_packet::<C>(plan.stream, OutputPacket::Video(p), plan.times, self.limits);
    self.settle(plan, written)
  }

  fn write_audio(
    &mut self,
    packet: AudioTrackPacket<Ffmpeg, C::Buffer>,
  ) -> Result<(), MuxError<MuxFault, FfmpegMuxRefusal, AudioTrackPacket<Ffmpeg, C::Buffer>>> {
    let p = packet.packet();
    let payload = Payload {
      bytes: p.data().as_ref(),
      key: p.flags().contains(PacketFlags::KEY),
      flags: p.flags(),
      side_data: p.extra().side_data(),
    };
    let plan = match self.admit(
      packet.track(),
      TrackKind::Audio,
      p.pts(),
      p.dts(),
      p.duration(),
      payload,
    ) {
      Ok(plan) => plan,
      Err(refusal) => return Err(refusal.with_packet(packet)),
    };
    let written =
      self
        .output
        .write_packet::<C>(plan.stream, OutputPacket::Audio(p), plan.times, self.limits);
    self.settle(plan, written)
  }

  // A subtitle or data packet is decoded when it is presented.
  fn write_subtitle(
    &mut self,
    packet: SubtitleTrackPacket<Ffmpeg, C::Buffer>,
  ) -> Result<(), MuxError<MuxFault, FfmpegMuxRefusal, SubtitleTrackPacket<Ffmpeg, C::Buffer>>> {
    let p = packet.packet();
    let payload = Payload {
      bytes: p.data().as_ref(),
      key: p.flags().contains(PacketFlags::KEY),
      flags: p.flags(),
      side_data: p.extra().side_data(),
    };
    let plan = match self.admit(
      packet.track(),
      TrackKind::Subtitle,
      p.pts(),
      p.pts(),
      p.duration(),
      payload,
    ) {
      Ok(plan) => plan,
      Err(refusal) => return Err(refusal.with_packet(packet)),
    };
    if plan.holds {
      self.hold(plan, packet);
      return Ok(());
    }
    // The packets held of the display set this one completes go first, in
    // the order offered: the filter puts the set out at this one.
    let track = plan.track;
    let (held, kept): (Vec<_>, Vec<_>) = core::mem::take(&mut self.held)
      .into_iter()
      .partition(|entry| entry.packet.track() == track);
    self.held = kept;
    let mut plan = plan;
    let mut lines = Vec::new();
    for entry in held {
      self
        .output
        .write_packet::<C>(
          plan.stream,
          OutputPacket::Subtitle(entry.packet.packet()),
          entry.times,
          self.limits,
        )
        .map_err(|error| MuxError::Fault(MuxFault::new(MuxStage::Write { track }, error)))?;
      lines.extend(entry.uncarried);
    }
    lines.append(&mut plan.uncarried);
    plan.uncarried = lines;
    let written = self.output.write_packet::<C>(
      plan.stream,
      OutputPacket::Subtitle(p),
      plan.times,
      self.limits,
    );
    self.settle(plan, written)
  }

  fn write_data(
    &mut self,
    packet: DataTrackPacket<Ffmpeg, C::Buffer>,
  ) -> Result<(), MuxError<MuxFault, FfmpegMuxRefusal, DataTrackPacket<Ffmpeg, C::Buffer>>> {
    let p = packet.packet();
    let payload = Payload {
      bytes: p.data().as_ref(),
      key: p.flags().contains(PacketFlags::KEY),
      flags: p.flags(),
      side_data: p.extra().side_data(),
    };
    let plan = match self.admit(
      packet.track(),
      TrackKind::Data,
      p.pts(),
      p.pts(),
      p.duration(),
      payload,
    ) {
      Ok(plan) => plan,
      Err(refusal) => return Err(refusal.with_packet(packet)),
    };
    let written =
      self
        .output
        .write_packet::<C>(plan.stream, OutputPacket::Data(p), plan.times, self.limits);
    self.settle(plan, written)
  }

  fn finish(self) -> Result<(), MuxFault> {
    self.finish_into_sink().map(drop)
  }
}

/// What `filter` still holds inside FFmpeg when the stream ends, which the
/// end decides ([`Uncarried::Held`]): the filter, and how many packets it
/// holds. PGS's display sets this muxer holds itself
/// ([`CarrierMuxer::take_held`]).
fn held_at_end(filter: Filter) -> Option<(AutomaticFilter, u64)> {
  match filter {
    Filter::Vp9 { held, .. } if held > 0 => Some((AutomaticFilter::Vp9Superframe, u64::from(held))),
    _ => None,
  }
}

impl<W, C: FfmpegCarrier> fmt::Debug for CarrierMuxer<W, C> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("CarrierMuxer")
      .field("container", &self.container)
      .field("tracks", &self.tracks.len())
      .field("output", &self.output)
      .finish_non_exhaustive()
  }
}

/// Judges one track for `family`, its automatic filters on where `filters`
/// says: refused by name, or the road it takes and what that road drops of
/// its fields.
fn judge<W, D: AsRef<[u8]>>(
  output: &Output<W>,
  family: Family,
  sites: Sites,
  track: TrackIndex,
  description: &TrackDescription<Ffmpeg, D, CodecTicket>,
  filters: bool,
  limits: PacketLimits,
) -> Result<(Road, Vec<Uncarried>), MuxOpenError> {
  let codec = description.codec();
  if codec.raw() != description.side_data().codec_id() {
    return Err(MuxOpenError::TicketMismatch { track });
  }
  if description.kind() != TrackKind::Unknown
    && !media_type_agrees(
      description.kind(),
      description.side_data().codec_type(),
      codec.media_type(),
    )
  {
    return Err(MuxOpenError::MediaTypeMismatch { track });
  }
  let nul = |text: Option<&Utf8Bytes>, which: OutputText| match text {
    Some(text) if text.as_str().contains('\0') => {
      Err(MuxOpenError::TextWithNul { track, text: which })
    }
    _ => Ok(()),
  };
  nul(description.language(), OutputText::Language)?;

  let road = match description.kind() {
    TrackKind::Unknown => return Err(MuxOpenError::UnknownTrackKind { track }),
    // A file of no bytes: Matroska's reader drops the attachment
    // (`matroskadec.c` 3510–3512), and MP4 skips the cover (4823) — not
    // written, and reported (table E of the census).
    TrackKind::Attachment
      if description
        .file()
        .is_none_or(|file| file.payload().data().as_ref().is_empty()) =>
    {
      Road::NotWritten
    }
    TrackKind::Attachment => match family {
      Family::Matroska => {
        let file = description.file();
        let filename = file.and_then(|file| file.filename());
        if filename.is_none() {
          return Err(MuxOpenError::AttachmentWithoutFilename { track });
        }
        nul(filename, OutputText::Filename)?;
        let stated = file.and_then(|file| file.mime_type());
        nul(stated, OutputText::MimeType)?;
        if stated.is_none() && matroska_mime_type(codec).is_none() {
          return Err(MuxOpenError::AttachmentWithoutMimeType { track });
        }
        Road::Attachment
      }
      Family::Mp4 if is_cover_codec(codec) => {
        // Its one packet is built after the header is written; what its
        // builder would refuse is refused here, nothing written.
        if let Some(file) = description.file()
          && let Some(rule) = build_refusal(
            file.payload().flags(),
            file.payload().data().as_ref().len(),
            &[],
            limits,
          )
        {
          return Err(MuxOpenError::Cover { track, rule });
        }
        Road::Cover
      }
      _ => Road::NotWritten,
    },
    _ => {
      if output.query_codec(codec) == CodecQuery::NotCarried || header_refuses(family, codec) {
        return Err(MuxOpenError::CodecNotCarried { track, codec });
      }
      if configuration_missing(
        family,
        codec,
        stream_extradata(description),
        sites.aac && filters,
      ) {
        return Err(MuxOpenError::ConfigurationMissing { track, codec });
      }
      if configuration_invalid(family, codec, stream_extradata(description)) {
        return Err(MuxOpenError::ConfigurationInvalid { track, codec });
      }
      if family == Family::Mp4
        && description.kind() == TrackKind::Video
        && crops_away(description.side_data())
      {
        return Err(MuxOpenError::Cropping { track });
      }
      let layout = LayoutView::of(description.side_data().ch_layout());
      if family == Family::Mp4 && !mp4_pcm_layout_written(codec, layout) {
        return Err(MuxOpenError::ChannelLayout { track });
      }
      Road::Timed
    }
  };
  Ok((road, losses(family, road, track, description)))
}

/// Whether a track of `kind` agrees with its ticket's media type
/// `ticket` and its codec's `codec` (`None` where libavcodec has no
/// descriptor for it) — table D of the census (see [the module](self)):
/// a timed track's stream is made of its ticket's media type, so its kind
/// is both. An attachment's road makes its stream's media type itself —
/// an attachment stream, or a cover's video stream
/// ([`StreamMedia::Attachment`], [`StreamMedia::Cover`]) — so its ticket's
/// is not asked: libavformat's demuxers deliver a font as an attachment
/// stream and cover art as a video one (`ff_add_attached_pic`,
/// `demux_utils.c` 107–140 in FFmpeg 9.0.1).
fn media_type_agrees(kind: TrackKind, ticket: i32, codec: Option<i32>) -> bool {
  use ffmpeg_next::ffi::AVMediaType as M;
  let media = match kind {
    TrackKind::Video => M::AVMEDIA_TYPE_VIDEO,
    TrackKind::Audio => M::AVMEDIA_TYPE_AUDIO,
    TrackKind::Subtitle => M::AVMEDIA_TYPE_SUBTITLE,
    TrackKind::Data => M::AVMEDIA_TYPE_DATA,
    TrackKind::Attachment => return true,
    _ => return false,
  } as i32;
  ticket == media && codec.is_none_or(|codec| codec == media)
}

/// The extradata a timed track's stream will hold: the description's
/// own, else its ticket's ([`stream_spec`]).
fn stream_extradata<D: AsRef<[u8]>>(
  description: &TrackDescription<Ffmpeg, D, CodecTicket>,
) -> &[u8] {
  match description.extradata() {
    Some(bytes) => bytes.as_ref(),
    None => description.side_data().extradata(),
  }
}

/// Whether `family` needs a configuration `codec`'s track does not carry
/// ([`MuxOpenError::ConfigurationMissing`]): AAC's only where its automatic
/// filter is not inserted (`aac_filtered`), as where it is its first ADTS
/// header makes one.
fn configuration_missing(
  family: Family,
  codec: CodecId,
  extradata: &[u8],
  aac_filtered: bool,
) -> bool {
  if !extradata.is_empty() {
    return false;
  }
  let aac = codec == CodecId::AAC && !aac_filtered;
  match family {
    // H.264, HEVC, VVC, LCEVC and EVC take the first packet as their
    // configuration (`movenc.c` 7024–7041); AV1's `av1C` and ALAC's atom
    // are written from the configuration alone, empty without one
    // (1573–1582, 672–676); FLAC's `dfLa` and Opus's `dOps` refuse its
    // absence at the trailer (857–910); MPEG-4 ALS's decoder configuration
    // is its `esds`'s alone (765–816).
    Family::Mp4 => {
      aac
        || [
          CodecId::H264,
          CodecId::HEVC,
          CodecId::VVC,
          CodecId::LCEVC,
          CodecId::EVC,
          CodecId::AV1,
          CodecId::FLAC,
          CodecId::OPUS,
          CodecId::ALAC,
          MP4ALS,
        ]
        .contains(&codec)
    }
    Family::Matroska => aac || codec == CodecId::AV1,
    Family::WebM => codec == CodecId::AV1,
    Family::Other => false,
  }
}

/// Whether `family`'s writer refuses `codec`'s configuration `extradata`
/// at its trailer ([`MuxOpenError::ConfigurationInvalid`]; table C of the
/// census): MP4 FLAC's not one 34-byte STREAMINFO (`dfLa`, `movenc.c`
/// 857–876), MP4 Opus's not an OpusHead `dOps` takes ([`opus_head_written`]),
/// MP4 TTML's from libavcodec's TTML encoder whose header strings do not
/// parse ([`ttml_header_strings`]). A missing configuration is
/// [`configuration_missing`]'s.
fn configuration_invalid(family: Family, codec: CodecId, extradata: &[u8]) -> bool {
  if family != Family::Mp4 || extradata.is_empty() {
    return false;
  }
  match codec {
    c if c == CodecId::FLAC => extradata.len() != FLAC_STREAMINFO_BYTES,
    c if c == CodecId::OPUS => !opus_head_written(extradata),
    c if c == CodecId::TTML => extradata
      .strip_prefix(b"lavc-ttmlenc")
      .is_some_and(|strings| !ttml_header_strings(strings)),
    _ => false,
  }
}

/// MPEG-4 ALS, which the base names no constant for.
const MP4ALS: CodecId = CodecId::from_raw(ffmpeg_next::ffi::AVCodecID::AV_CODEC_ID_MP4ALS as i32);

/// `FLAC_STREAMINFO_SIZE`, the one metadata block MP4's `dfLa` writes.
const FLAC_STREAMINFO_BYTES: usize = 34;

/// Whether MP4's `dOps` writes `config` — an Ogg OpusHead — without refusing
/// it (`mov_write_dops_tag`, `movenc.c` 877–910): its 19 bytes, and where
/// its channel mapping family (byte 18) is not 0, the table after them —
/// two bytes and one a channel (byte 9).
fn opus_head_written(config: &[u8]) -> bool {
  config.len() >= 19 && (config[18] == 0 || config.len() >= 21 + usize::from(config[9]))
}

/// Whether the header strings after the 12-byte signature of libavcodec's
/// TTML encoder's configuration parse as the TTML writer parses them
/// (`ttml_set_header_values_from_extradata`, `ttmlenc.c` 82–122): none, the
/// old form; or a terminated string followed by another.
fn ttml_header_strings(strings: &[u8]) -> bool {
  if strings.is_empty() {
    return true;
  }
  let Some(first) = strings.iter().position(|&byte| byte == 0) else {
    return false;
  };
  let rest = &strings[first + 1..];
  !rest.is_empty() && rest.contains(&0)
}

/// Whether MP4's video writer refuses the frame cropping `ticket`'s coded
/// side data states ([`crop_leaves_no_picture`]).
fn crops_away(ticket: &CodecTicket) -> bool {
  let crop = ticket
    .coded_side_data()
    .iter()
    .find(|entry| entry.kind() == AVPacketSideDataType::AV_PKT_DATA_FRAME_CROPPING as i32)
    .map(SideDataEntry::data);
  crop_leaves_no_picture(crop, ticket.width(), ticket.height())
}

/// Whether MP4's video writer refuses `crop`, frame cropping side data, on
/// a picture of `width` by `height` (`mov_write_video_tag`, `movenc.c`
/// 3024–3036): four little-endian 32-bit counts — top, bottom, left,
/// right — read from 16 bytes or more, whose left and right meet the
/// width, or top and bottom the height, compared as C compares a
/// `uint64_t` with an `int`.
fn crop_leaves_no_picture(crop: Option<&[u8]>, width: i32, height: i32) -> bool {
  let Some(crop) = crop.filter(|crop| crop.len() >= 16) else {
    return false;
  };
  let at = |offset: usize| {
    u64::from(u32::from_le_bytes([
      crop[offset],
      crop[offset + 1],
      crop[offset + 2],
      crop[offset + 3],
    ]))
  };
  let (top, bottom, left, right) = (at(0), at(4), at(8), at(12));
  left + right >= width as u64 || top + bottom >= height as u64
}

/// Whether `config` is an MPEG-4 AudioSpecificConfig libavcodec parses
/// (`ff_mpeg4audio_get_config_gb`, `mpeg4audio.c` 92–209; its entry
/// point, 212–226, in FFmpeg 9.0.1): it fails only on no bytes, on a
/// channel configuration past its table of 15, or on an ALS configuration
/// short of 112 bits, without `ALS\0`, or of no sample rate (32–57); any
/// other it reads to the end, zeros past it.
fn audio_specific_config_parses(config: &[u8]) -> bool {
  if config.is_empty() {
    return false;
  }
  let object_type = |bits: &mut Bits<'_>| match bits.read(5) {
    31 => 32 + bits.read(6),
    other => other,
  };
  let sample_rate = |bits: &mut Bits<'_>| {
    if bits.read(4) == 15 {
      bits.skip(24);
    }
  };
  let show = |at: usize, count: u32| Bits { bytes: config, at }.read(count);
  let mut bits = Bits {
    bytes: config,
    at: 0,
  };
  let mut object = object_type(&mut bits);
  sample_rate(&mut bits);
  if bits.read(4) >= 15 {
    return false;
  }
  // SBR, or PS but for the MP3onMP4 draft's signature (108–110).
  if object == 5
    || (object == 29 && !(show(bits.at, 3) & 0x03 != 0 && show(bits.at, 9) & 0x3F == 0))
  {
    sample_rate(&mut bits);
    object = object_type(&mut bits);
    if object == 22 {
      bits.skip(4);
    }
  }
  if object == 36 {
    bits.skip(5);
    if show(bits.at, 24) != 0x0041_4C53 {
      bits.skip(24);
    }
    let left = (config.len() * 8) as i64 - bits.at as i64;
    if left < 112 || bits.read(32) != 0x414C_5300 {
      return false;
    }
    // The sample rate, as the `int` it is stored in.
    if bits.read(32) as i32 <= 0 {
      return false;
    }
  }
  true
}

/// Whether `family`'s header refuses `codec`, though the muxer's query
/// admits it ([`MuxOpenError::CodecNotCarried`]): MP4 writes TrueHD only
/// as an experiment, and refuses it otherwise (`mov_init`, `movenc.c`
/// 8580–8587 in FFmpeg 9.0.1); Matroska and WebM do not yet write ATRAC3,
/// Cook, RealAudio 2.0, SIPR or RealVideo 1.0, 2.0 and 3.0 (`mkv_init`,
/// `matroskaenc.c` 3518–3530).
fn header_refuses(family: Family, codec: CodecId) -> bool {
  match family {
    Family::Mp4 => codec == CodecId::TRUEHD,
    Family::Matroska | Family::WebM => [
      CodecId::ATRAC3,
      CodecId::COOK,
      CodecId::RA_288,
      CodecId::SIPR,
      CodecId::RV10,
      CodecId::RV20,
      CodecId::RV30,
    ]
    .contains(&codec),
    Family::Other => false,
  }
}

/// Whether `extradata` is Annex B — starts with a 3- or 4-byte start code
/// — as MP4 and Matroska judge an HEVC or VVC configuration
/// (`movenc.c` 7108–7140; `matroskaenc.c` 3575–3580).
fn annex_b(extradata: &[u8]) -> bool {
  extradata.starts_with(&[0, 0, 1]) || extradata.starts_with(&[0, 0, 0, 1])
}

/// Whether `family` rewrites a track of `codec` whose configuration is
/// `extradata` from Annex B to length prefixes ([`Rewrite::LengthPrefixed`]).
/// MP4 reads an H.264 or LCEVC configuration whose first byte is not 1 —
/// an `avcC`'s version — as Annex B (`movenc.c` 7087–7089, 7141–7142), and
/// an HEVC or VVC one longer than 6 bytes opening on a start code
/// (7108–7109, 7129–7130); Matroska an H.264 one opening on a start code,
/// an HEVC one so longer than 6 bytes, a VVC one 6 or more
/// (`matroskaenc.c` 3574–3582).
fn length_prefixed(family: Family, codec: CodecId, extradata: &[u8]) -> bool {
  match family {
    Family::Mp4 => match codec {
      c if c == CodecId::H264 || c == CodecId::LCEVC => {
        extradata.first().is_some_and(|&first| first != 1)
      }
      c if c == CodecId::HEVC || c == CodecId::VVC => extradata.len() > 6 && annex_b(extradata),
      _ => false,
    },
    Family::Matroska | Family::WebM => match codec {
      c if c == CodecId::H264 => annex_b(extradata),
      c if c == CodecId::HEVC => extradata.len() > 6 && annex_b(extradata),
      c if c == CodecId::VVC => extradata.len() >= 6 && annex_b(extradata),
      _ => false,
    },
    Family::Other => false,
  }
}

/// An upper bound on the bytes the Annex B rewriter makes of `bytes`
/// (`nal_parse_units`, `nal.c` 74–111): each start code it finds becomes a
/// 4-byte length, and the bytes between them are kept or trimmed, so the
/// rewritten packet is its bytes and at most one more for each `00 00 01`.
fn length_prefixed_bytes(bytes: &[u8]) -> u64 {
  let starts = bytes
    .windows(3)
    .filter(|window| *window == [0, 0, 1])
    .count();
  (bytes.len() + starts) as u64
}

/// Whether Matroska writes a default duration for a timed track of `kind`
/// whose stream libavformat holds as `stream`, at `frame_rate`: a video
/// track's from its frame rate, an audio track's from the samples its
/// codec fixes in each frame (`matroskaenc.c` 2058–2065, 2119–2124). A
/// subtitle's duration is written with every block (2895–2898), as if it
/// had one.
fn default_duration(kind: TrackKind, stream: &StreamCodec, frame_rate: Option<Timebase>) -> bool {
  match kind {
    TrackKind::Video => frame_rate.is_some_and(|rate| rate.num() > 0),
    TrackKind::Audio => {
      stream.sample_rate() > 0 && (stream.frame_duration() != 0 || stream.frame_size() != 0)
    }
    _ => true,
  }
}

/// What `family` does to the packets of a timed track of `codec` whose
/// stream holds `extradata`, with or without a [`default_duration`] — the
/// packet-rule table's rows reported at open, in the table's order. MP4's
/// durations are reported as they are lost, while writing.
fn rewrites(
  family: Family,
  codec: CodecId,
  extradata: &[u8],
  default_duration: bool,
) -> impl Iterator<Item = Rewrite> {
  let mp4 = family == Family::Mp4;
  let matroska = family == Family::Matroska;
  let either_matroska = matroska || family == Family::WebM;
  let annex = length_prefixed(family, codec, extradata);
  let ttml_encoder = extradata.starts_with(b"lavc-ttmlenc");
  [
    (annex, Rewrite::LengthPrefixed),
    (
      (mp4 || either_matroska) && codec == CodecId::AV1,
      Rewrite::ObusFiltered,
    ),
    (
      mp4 && (codec == CodecId::AC3 || codec == CodecId::EAC3),
      Rewrite::Ac3Parsed,
    ),
    (mp4 && codec == CodecId::MOV_TEXT, Rewrite::CueEnds),
    (
      mp4 && codec == CodecId::TTML && ttml_encoder,
      Rewrite::ParagraphsJoined,
    ),
    (
      mp4 && (codec == CodecId::VC1 || codec == CodecId::TRUEHD),
      Rewrite::SyncFromBitstream,
    ),
    (either_matroska && !default_duration, Rewrite::Durations),
    (
      matroska && codec == CodecId::WAVPACK,
      Rewrite::WavPackHeaders,
    ),
  ]
  .into_iter()
  .filter_map(|(applies, rewrite)| applies.then_some(rewrite))
}

/// What `road` drops of `description` in `family`, in the report's
/// order: the whole attachment where it is not written; else, by the
/// report table ([`kept`]), an attachment's filename and MIME type, the
/// language and the disposition flags the road keeps no field for.
fn losses<D>(
  family: Family,
  road: Road,
  track: TrackIndex,
  description: &TrackDescription<Ffmpeg, D, CodecTicket>,
) -> Vec<Uncarried> {
  if road == Road::NotWritten {
    return vec![Uncarried::Attachment { track }];
  }
  let kept = kept(
    family,
    road,
    description.kind(),
    description.codec(),
    description.disposition(),
  );
  let mut lost = Vec::new();
  if let Some(file) = description.file() {
    if let Some(filename) = file.filename()
      && !kept.filename
    {
      lost.push(Uncarried::Filename {
        track,
        filename: filename.clone(),
      });
    }
    if let Some(mime_type) = file.mime_type()
      && !kept.mime_type
    {
      lost.push(Uncarried::MimeType {
        track,
        mime_type: mime_type.clone(),
      });
    }
  }
  if let Some(tag) = description.language()
    && !kept.language.keeps(tag.as_str())
  {
    lost.push(Uncarried::Language {
      track,
      tag: tag.clone(),
    });
  }
  let back = read_back(family, road, description, kept.flags);
  let flags = description.disposition().difference(kept.flags | back);
  if !flags.is_empty() {
    lost.push(Uncarried::Disposition { track, flags });
  }
  let added = back.difference(description.disposition());
  if !added.is_empty() {
    lost.push(Uncarried::DispositionAdded {
      track,
      flags: added,
    });
  }
  lost
}

/// The flags `road` reads `description` back with in `family`, whether it
/// declared them or not — table E of the census: MP4's cover reads back as
/// an attached picture (`mov_write_covr`, `movenc.c` 4815–4839), and so
/// does a Matroska attachment whose MIME type opens with an image type its
/// reader turns into one — GIF, JPEG, PNG or TIFF (`mkv_image_mime_tags`,
/// `matroskadec.c` 817–824, read at 3524–3533; `ff_add_attached_pic`,
/// `demux_utils.c` 107–140); MP4 writes one DASH role for the hearing
/// impaired and captions, and one for the visual impaired and
/// descriptions, for either flag of the pair, and its reader reads each
/// role back as both (`mov_write_track_kinds`, `movenc.c` 4338–4356;
/// `isom.c` 450–462; `mov.c` 8994–9004). `kept` is what the road writes of
/// the declared flags ([`kept`]).
fn read_back<D>(
  family: Family,
  road: Road,
  description: &TrackDescription<Ffmpeg, D, CodecTicket>,
  kept: TrackDisposition,
) -> TrackDisposition {
  use TrackDisposition as F;
  match (family, road) {
    (_, Road::Cover) => F::ATTACHED_PIC,
    (Family::Matroska, Road::Attachment)
      if description
        .file()
        .and_then(|file| file.mime_type())
        .map(Utf8Bytes::as_str)
        .or_else(|| matroska_mime_type(description.codec()))
        .is_some_and(|mime| {
          ["image/gif", "image/jpeg", "image/png", "image/tiff"]
            .iter()
            .any(|image| mime.starts_with(image))
        }) =>
    {
      F::ATTACHED_PIC
    }
    (Family::Mp4, Road::Timed) => {
      let written = description.disposition().intersection(kept);
      [
        F::HEARING_IMPAIRED | F::CAPTIONS,
        F::VISUAL_IMPAIRED | F::DESCRIPTIONS,
      ]
      .into_iter()
      .filter(|role| written.intersects(*role))
      .fold(F::empty(), |back, role| back | role)
    }
    _ => F::empty(),
  }
}

/// What a road keeps of a track's own fields in a container: one row of
/// the report table (see [the module](self)), which the report and the
/// stream's disposition both read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Kept {
  filename: bool,
  mime_type: bool,
  language: LanguageRule,
  flags: TrackDisposition,
}

/// Which languages a road writes so that they read back as themselves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LanguageRule {
  /// Any, as given.
  AsGiven,
  /// A Matroska or WebM track entry's: as given, but an empty tag is
  /// written as `und` (`matroskaenc.c` 1967–1969) and `und` reads back as
  /// none (`matroskadec.c` 3280–3281).
  Determined,
  /// MP4's packed code: exactly three bytes in `0x60..=0x7F` that pack
  /// and unpack to themselves — not a first byte of `0x60`, which packs
  /// below `0x400` and unpacks through the Macintosh table, and not
  /// `0x7F7F7F`, which packs to "unspecified" (`isom.c` 233–280;
  /// `movenc.c` 8455–8458). An empty tag is written as `und`.
  Packed,
  /// None.
  Dropped,
}

impl LanguageRule {
  /// Whether the road writes `tag` so that it reads back as `tag`.
  fn keeps(self, tag: &str) -> bool {
    match self {
      Self::AsGiven => true,
      Self::Determined => !tag.is_empty() && tag != "und",
      Self::Packed => {
        let bytes = tag.as_bytes();
        bytes.len() == 3
          && bytes.iter().all(|byte| (0x60..=0x7F).contains(byte))
          && bytes[0] != 0x60
          && bytes != [0x7F; 3]
      }
      Self::Dropped => false,
    }
  }
}

/// The report table: what `road` keeps in `family` of a track of `kind`
/// and `codec` declaring `flags` — from each muxer's source in FFmpeg
/// 9.0.1, and its demuxer's for what reads back. A container this release
/// does not know keeps everything, as far as anything is claimed.
fn kept(
  family: Family,
  road: Road,
  kind: TrackKind,
  codec: CodecId,
  flags: TrackDisposition,
) -> Kept {
  use TrackDisposition as D;
  // Every bit, an unnamed one among them.
  let everything = Kept {
    filename: true,
    mime_type: true,
    language: LanguageRule::AsGiven,
    flags: D::from_bits_retain(u32::MAX),
  };
  match (family, road) {
    (Family::Other, _) | (_, Road::NotWritten) => everything,
    // An attachment Matroska writes keeps its file, filename and MIME
    // type (`mkv_write_attachments`, `matroskaenc.c` 2513–2561) and its
    // language, among the tags written for it (`mkv_write_tags`,
    // 2377–2390), but no flag: it has no track entry (1957–1958). One of
    // an image type reads back as an attached picture ([`read_back`]),
    // which keeps that flag; `losses` reads it there.
    (_, Road::Attachment) => Kept {
      flags: D::empty(),
      ..everything
    },
    // MP4's cover keeps its picture and the type of its image
    // (`mov_write_covr`, `movenc.c` 4815–4839), and reads back as an
    // attached picture and nothing more.
    (_, Road::Cover) => Kept {
      filename: false,
      mime_type: false,
      language: LanguageRule::Dropped,
      flags: D::ATTACHED_PIC,
    },
    // `matroskaenc.c` 1967–2003, 2149–2150: the default and forced flags,
    // the commentary and impairment flags, original or dub — one of them;
    // both at once write neither — and text descriptions on a subtitle
    // track alone.
    (Family::Matroska, Road::Timed) => {
      let mut kept = D::DEFAULT | D::FORCED | D::COMMENT | D::HEARING_IMPAIRED | D::VISUAL_IMPAIRED;
      if !flags.contains(D::ORIGINAL | D::DUB) {
        kept |= D::ORIGINAL | D::DUB;
      }
      if kind == TrackKind::Subtitle {
        kept |= D::DESCRIPTIONS;
      }
      Kept {
        language: LanguageRule::Determined,
        flags: kept,
        ..everything
      }
    }
    // WebM: the default and forced flags, and a WebVTT track's kind
    // through its codec id — the first of captions, descriptions or
    // metadata it declares (`matroskaenc.c` 1974–2002).
    (Family::WebM, Road::Timed) => {
      let mut kept = D::DEFAULT | D::FORCED;
      if codec == CodecId::WEBVTT
        && let Some(first) = [D::CAPTIONS, D::DESCRIPTIONS, D::METADATA]
          .into_iter()
          .find(|kind| flags.contains(*kind))
      {
        kept |= first;
      }
      Kept {
        language: LanguageRule::Determined,
        flags: kept,
        ..everything
      }
    }
    // The track's enabled flag (`enable_tracks`, `movenc.c` 7883–7923)
    // and the DASH role `kind` boxes (4338–4356, `isom.c` 450–462).
    (Family::Mp4, Road::Timed) => Kept {
      language: LanguageRule::Packed,
      flags: D::DEFAULT
        | D::HEARING_IMPAIRED
        | D::CAPTIONS
        | D::COMMENT
        | D::VISUAL_IMPAIRED
        | D::DESCRIPTIONS
        | D::DUB
        | D::FORCED,
      ..everything
    },
  }
}

/// The stream a track becomes, on its road. The disposition handed to
/// libavformat is what the report table keeps of it ([`kept`]), so that no
/// flag the report names as dropped acts on the stream — MP4 would make a
/// timed video stream marked as an attached picture into a cover
/// (`is_cover_image`, `movenc.c` 7606–7620).
fn stream_spec<'a, D: AsRef<[u8]>>(
  family: Family,
  description: &'a TrackDescription<Ffmpeg, D, CodecTicket>,
  road: Road,
) -> StreamSpec<'a> {
  let ticket = description.side_data();
  let declared = description.disposition();
  let written = declared.intersection(
    kept(
      family,
      road,
      description.kind(),
      description.codec(),
      declared,
    )
    .flags,
  );
  let spec = StreamSpec::new(ticket, description.timebase())
    .with_disposition(written.bits() as i32)
    .with_language(description.language().map(Utf8Bytes::as_str))
    .with_extradata(description.extradata().map(|bytes| bytes.as_ref()));
  let spec = match description.params() {
    TrackParams::Video(params) => spec.with_frame_rate(params.frame_rate()),
    _ => spec,
  };
  match road {
    Road::Attachment => {
      let file = description.file();
      let mime_type = file
        .and_then(|file| file.mime_type())
        .map(Utf8Bytes::as_str)
        .or_else(|| matroska_mime_type(description.codec()));
      spec
        .with_media(StreamMedia::Attachment)
        .with_extradata(file.map(|file| file.payload().data().as_ref()))
        .with_filename(file.and_then(|file| file.filename()).map(Utf8Bytes::as_str))
        .with_mime_type(mime_type)
    }
    Road::Cover => spec.with_media(StreamMedia::Cover),
    Road::Timed | Road::NotWritten => spec,
  }
}

/// The MIME type Matroska writes for an attachment that states none: its
/// codec's, or `text/plain` for plain text (`get_mimetype`,
/// `matroskaenc.c` 2496–2511 in FFmpeg 9.0.1).
fn matroska_mime_type(codec: CodecId) -> Option<&'static str> {
  match codec.mime_type() {
    Some(mime) => Some(mime),
    None if codec == CodecId::TEXT => Some("text/plain"),
    None => None,
  }
}

/// The still-image codecs MP4 keeps as cover art (`movenc.c` 2167–2172
/// in FFmpeg 9.0.1).
fn is_cover_codec(codec: CodecId) -> bool {
  codec == CodecId::MJPEG || codec == CodecId::PNG || codec == CodecId::BMP
}

/// The timed tracks of `tracks` MP4 enables though they declare no default
/// flag, and its reader reads back as default (`enable_tracks`, `movenc.c`
/// 7883–7923): the first video, audio and subtitle track of each type
/// where none of the type declares it. A cover is not among them, nor a
/// data track. Each track's stream is of its kind's media type (table D).
fn mp4_defaults_added<D>(
  family: Family,
  tracks: &[TrackDescription<Ffmpeg, D, CodecTicket>],
  roads: &[Road],
) -> Vec<bool> {
  let mut added = vec![false; tracks.len()];
  if family != Family::Mp4 {
    return added;
  }
  for kind in [TrackKind::Video, TrackKind::Audio, TrackKind::Subtitle] {
    let mut timed = (0..tracks.len())
      .filter(|&index| roads[index] == Road::Timed && tracks[index].kind() == kind);
    let first = timed.clone().next();
    if let Some(first) = first
      && !timed.any(|index| {
        tracks[index]
          .disposition()
          .contains(TrackDisposition::DEFAULT)
      })
    {
      added[first] = true;
    }
  }
  added
}

/// Whether `count` ticks of `time_base` are a whole number of `1 / units`
/// seconds — an instant a container counting in those units keeps.
fn counts_exactly(count: i64, time_base: Timebase, units: i64) -> bool {
  (i128::from(count) * i128::from(time_base.num()) * i128::from(units))
    .checked_rem(i128::from(time_base.den().get()))
    == Some(0)
}

/// Whether `a` ticks of `a_base` and `b` ticks of `b_base` are one instant.
fn same_instant(a: i64, a_base: Timebase, b: i64, b_base: Timebase) -> bool {
  i128::from(a) * i128::from(a_base.num()) * i128::from(b_base.den().get())
    == i128::from(b) * i128::from(b_base.num()) * i128::from(a_base.den().get())
}

/// What `family` does not keep of `chapters`, each counted as `spans` in
/// its own time base — table E of the census: MP4 numbers chapters by
/// position and counts them in its movie timescale, `movie_timescale`
/// ([`mp4_movie_timescale`]), each ending where the next starts; Matroska
/// renumbers them all from 1 where an id is 0 or repeats, and counts them in
/// nanoseconds. Another container's chapters are its own.
fn chapter_losses(
  family: Family,
  chapters: &[Chapter<Ffmpeg>],
  spans: &[(i64, i64)],
  movie_timescale: i64,
) -> Vec<Uncarried> {
  let mut lost = Vec::new();
  match family {
    Family::Mp4 => {
      let movie_units = movie_timescale;
      for (index, (chapter, &(start, end))) in chapters.iter().zip(spans).enumerate() {
        if chapter.id() != index as i64 {
          lost.push(Uncarried::ChapterId {
            chapter: index,
            id: chapter.id(),
          });
        }
        let base = chapter.timebase();
        let ends_where_the_next_starts = match (chapters.get(index + 1), spans.get(index + 1)) {
          (Some(next), Some(&(next_start, _))) => {
            same_instant(end, base, next_start, next.timebase())
          }
          _ => counts_exactly(end, base, movie_units),
        };
        if !counts_exactly(start, base, movie_units) || !ends_where_the_next_starts {
          lost.push(Uncarried::ChapterSpan { chapter: index });
        }
      }
    }
    Family::Matroska | Family::WebM => {
      const NANOSECONDS: i64 = 1_000_000_000;
      let renumbered = chapters.iter().enumerate().any(|(index, chapter)| {
        chapter.id() == 0
          || chapters[..index]
            .iter()
            .any(|earlier| earlier.id() == chapter.id())
      });
      for (index, (chapter, &(start, end))) in chapters.iter().zip(spans).enumerate() {
        if renumbered && chapter.id() != index as i64 + 1 {
          lost.push(Uncarried::ChapterId {
            chapter: index,
            id: chapter.id(),
          });
        }
        let base = chapter.timebase();
        if !counts_exactly(start, base, NANOSECONDS) || !counts_exactly(end, base, NANOSECONDS) {
          lost.push(Uncarried::ChapterSpan { chapter: index });
        }
      }
    }
    Family::Other => {}
  }
  lost
}

/// `AV_CODEC_ID_EIA_608`, the captions MCC and the MXF muxers convert at
/// their start (table B).
const EIA_608: CodecId = CodecId::from_raw(ffmpeg_next::ffi::AVCodecID::AV_CODEC_ID_EIA_608 as i32);

/// `AV_CODEC_ID_TIMED_ID3`, whose stream MP4 makes no track of (`mov_init`,
/// `movenc.c` 8309–8313).
const TIMED_ID3: CodecId =
  CodecId::from_raw(ffmpeg_next::ffi::AVCodecID::AV_CODEC_ID_TIMED_ID3 as i32);

/// MP4's movie timescale for the streams `written` describes, as `mov_init`
/// makes it from their time bases before any track's own is chosen
/// (`movenc.c` 8427–8446 in FFmpeg 9.0.1): `av_gcd_q` over every stream but
/// a timed ID3 one — the least common multiple of their denominators while
/// it stays below `INT_MAX`, else none, for good (`libavutil/rational.c`
/// 188–195) — and at least 1000, `MOV_TIMESCALE`. A time base of numerator
/// 0 is libavformat's to give first: an audio stream's its sample rate's,
/// another's 90 kHz (`init_muxer`, `mux.c` 228–234). The chapter track's
/// samples, and the chapters MP4 reads back, are counted in it (7758,
/// 7788–7789).
fn mp4_movie_timescale<'a, D: AsRef<[u8]> + 'a>(
  written: impl Iterator<Item = &'a TrackDescription<Ffmpeg, D, CodecTicket>>,
) -> i64 {
  use ffmpeg_next::ffi::AVMediaType as M;
  movie_timescale(
    written
      .filter(|description| description.codec() != TIMED_ID3)
      .map(|description| {
        let ticket = description.side_data();
        let time_base = description.timebase();
        let rate = ticket.sample_rate();
        if time_base.num() != 0 {
          i64::from(time_base.den().get())
        } else if ticket.codec_type() == M::AVMEDIA_TYPE_AUDIO as i32 && rate > 0 {
          i64::from(rate)
        } else if ticket.codec_type() == M::AVMEDIA_TYPE_AUDIO as i32 && rate < 0 {
          // `avpriv_set_pts_info` ignores a rate that is not positive.
          i64::from(time_base.den().get())
        } else {
          90_000
        }
      }),
  )
}

/// `mov_init`'s fold of `av_gcd_q` over the time bases' denominators
/// `dens`, from `{0, 1}` (`movenc.c` 8427–8446): their least common
/// multiple while it stays below `INT_MAX`, else none for good
/// (`libavutil/rational.c` 188–195), and at least 1000.
fn movie_timescale(dens: impl Iterator<Item = i64>) -> i64 {
  // `av_gcd`: the positive greatest common divisor, a zero argument
  // answering the other (`libavutil/mathematics.c` 37–55).
  fn gcd(a: i64, b: i64) -> i64 {
    let (mut a, mut b) = (a.unsigned_abs(), b.unsigned_abs());
    while b != 0 {
      (a, b) = (b, a % b);
    }
    a as i64
  }
  let mut den: i64 = 1;
  for stream_den in dens {
    if den == 0 {
      break;
    }
    let lcm = (den / gcd(den, stream_den)) * stream_den;
    den = if lcm < i64::from(i32::MAX) { lcm } else { 0 };
  }
  if den == 0 {
    den = 1000;
  }
  den.max(1000)
}

/// The first of `chapters`, counted as `spans` in their own time bases,
/// whose sample in MP4's chapter track MP4 would refuse or rewrite. MP4
/// makes one sample a titled chapter at its header — its start and end each
/// counted in the movie timescale `movie_timescale`, rounded to the nearest
/// (`av_rescale_q`), its duration their distance — and hands each to
/// `ff_mov_write_packet`, ignoring its answer (`mov_create_chapter_track`,
/// `movenc.c` 7727–7815): `check_pkt` refuses a duration past `INT_MAX`,
/// which loses the sample and lays every later title over another chapter,
/// and moves a start before the last, or `INT_MAX` or more after it, to one
/// tick after (6897–6930) — the first's counted from zero, as no track past
/// the streams is given `AV_NOPTS_VALUE` for its start (8448–8474).
fn mp4_chapter_sample_refused(
  chapters: &[Chapter<Ffmpeg>],
  spans: &[(i64, i64)],
  movie_timescale: i64,
) -> Option<usize> {
  let limit = i128::from(i32::MAX);
  let mut last = 0i128;
  for (index, (chapter, &(start, end))) in chapters.iter().zip(spans).enumerate() {
    let base = chapter.timebase();
    let count = |ticks: i64| {
      i128::from(rescale_nearest(
        i128::from(ticks),
        i128::from(base.num()) * i128::from(movie_timescale),
        i128::from(base.den().get()),
      ))
    };
    let (start, end) = (count(start), count(end));
    if start < last || start - last >= limit || end - start > limit || end < start {
      return Some(index);
    }
    last = start;
  }
  None
}

/// A chapter's span in its own time base, or a refusal naming it.
fn chapter_span(index: usize, chapter: &Chapter<Ffmpeg>) -> Result<(i64, i64), MuxOpenError> {
  if chapter
    .title()
    .is_some_and(|title| title.as_str().contains('\0'))
  {
    return Err(MuxOpenError::ChapterTitleWithNul { chapter: index });
  }
  let count = |timestamp: Timestamp| {
    timestamp
      .checked_rescale_with(chapter.timebase(), Rounding::Nearest)
      .map(|rescaled| rescaled.pts())
  };
  match (count(chapter.start()), count(chapter.end())) {
    (Some(start), Some(end)) if start >= 0 && end >= start => Ok((start, end)),
    _ => Err(MuxOpenError::ChapterSpan { chapter: index }),
  }
}

#[cfg(test)]
mod tests;
