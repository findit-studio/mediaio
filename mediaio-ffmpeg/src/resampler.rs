//! [`mediaspine::resampler::AudioResampler`] impl backed by
//! `libswresample`.
//!
//! Converts rate, sample format and channel layout between two specs
//! fixed at construction — [`FfmpegResampler::new`] takes both, because
//! neither end is discoverable and neither is a constant. The source is
//! whatever the file holds; the target is whatever the consumer wants,
//! and consumers disagree (16 kHz mono for a speech model, 48 kHz for
//! an audio-event one, from the same track at the same time).
//!
//! # Output timestamps
//!
//! `swr` is a delay line: it needs future input to produce present
//! output, so at any moment a filter's worth of samples is inside it.
//! Timestamps are therefore *counted*, not computed per call — the
//! output timeline is anchored on the first input timestamp and
//! advanced by the number of samples actually produced. The frames
//! drained after EOF continue that same line rather than restarting it,
//! and no arithmetic anywhere depends on how many samples a given
//! `swr_convert_frame` happened to yield.

use std::{
  collections::VecDeque,
  ptr::{addr_of, read_unaligned},
};

use crate::channel_layout::LayoutArm;
use derive_more::{IsVariant, TryUnwrap, Unwrap};
use ffmpeg_next::{
  ChannelLayout,
  codec::Parameters,
  ffi::{
    AV_NOPTS_VALUE, AVChannelOrder, AVMatrixEncoding, AVSampleFormat, av_channel_layout_from_mask,
    av_frame_get_buffer, swr_build_matrix2,
  },
  format::Sample,
  frame,
  software::resampling,
};
use mediaframe::audio::ChannelLayoutDescription;
use mediaspine::{
  Received, Sent, Timebase, Timestamp,
  frame::{AudioFrame, Plane},
  resampler::AudioResampler,
};

use crate::{
  Ffmpeg, error::CrossingError, extras::AudioFrameExtra, limits::FrameLimits,
  sample_format::SampleFormat,
};

/// The frame type a resampler accepts and produces, on lane `C`.
///
/// Written as a projection rather than a bounded alias so the bound
/// lives on the items that need it — `type_alias_bounds` is not
/// enforced anyway, and a bound written where it is not enforced reads
/// like a promise the compiler is keeping.
type Frame<C> = AudioFrame<
  SampleFormat,
  ChannelLayoutDescription,
  AudioFrameExtra,
  <C as crate::FfmpegCarrier>::Buffer,
>;

/// One end of a conversion: sample rate, sample format, channel layout.
///
/// Spelled in FFmpeg's own vocabulary because construction is off the
/// [`AudioResampler`] trait and this is the backend that has to be
/// handed to `swr_alloc_set_opts2`. [`FfmpegResampler`] restates the
/// source spec in the vocabulary a decoded frame carries, so the
/// mid-stream check compares like with like without the caller ever
/// seeing two dialects.
#[derive(Copy, Clone)]
pub struct ResampleSpec {
  rate: u32,
  format: Sample,
  layout: ChannelLayout,
}

/// **`Debug` that never reads undefined storage.**
///
/// The derive delegated to `ffmpeg_next::ChannelLayout`'s own
/// formatter, which prints `u.mask` unconditionally — and for an
/// `UNSPEC` order that union is undefined per FFmpeg's own header, so
/// merely *logging* a spec read storage nobody had written. The mask is
/// printed here only for the orders that define it; see [`LayoutArm`].
impl core::fmt::Debug for ResampleSpec {
  fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
    let order = layout_order(&self.layout);
    let mut out = f.debug_struct("ResampleSpec");
    out
      .field("rate", &self.rate)
      .field("format", &self.format)
      .field("order", &order)
      .field("channels", &self.layout.channels());
    match LayoutArm::of(order) {
      // SAFETY: the order defines the `mask` arm.
      LayoutArm::Mask => out.field("mask", &unsafe { self.layout.0.u.mask }),
      // The map's *address* is what a reader can use here — its
      // contents need an extent this type cannot establish.
      // SAFETY: the order defines the `map` arm; the pointer is
      // formatted, never dereferenced.
      LayoutArm::Map => out.field("map", &unsafe { self.layout.0.u.map }),
      LayoutArm::Undefined => out.field("union", &"undefined for this order"),
    };
    out.finish()
  }
}

/// **Equality that never asks FFmpeg.**
///
/// The derive that stood here was a safe `==` with a raw pointer
/// dereference behind it, and it is the worst shape this branch found.
/// `ffmpeg_next::ChannelLayout`'s own `PartialEq` is
/// `av_channel_layout_compare`, which walks `u.map[i]` for a `CUSTOM`
/// order — so safe Rust could forge a layout with a positive
/// `nb_channels` and a null or dangling map, wrap it in two specs,
/// write `a == b`, and have FFmpeg dereference it. No preflight could
/// dominate that: `==` is reachable without constructing a resampler at
/// all, before any of this crate's code runs.
///
/// So the comparison is structural and reads nothing through a pointer:
/// the raw `order` first, then the channel count, then — for every
/// order that describes its channels through the union's `mask` arm —
/// the mask itself, a `uint64_t`.
///
/// # What a `CUSTOM` order compares as, and why
///
/// **Pointer identity, never contents.** A safe function cannot
/// establish that a map is as long as the count beside it, which is the
/// same wall
/// [`channel_layout_from_ffmpeg`](crate::channel_layout::channel_layout_from_ffmpeg)
/// meets and refuses at; here the answer is the one comparison that
/// needs no extent — are these the same map? Two custom layouts
/// describing identical channels through different allocations
/// therefore compare **unequal**.
///
/// That is conservative in the only direction that matters. The single
/// product use of layout equality is [`check_pair`]'s
/// `source == target` short-circuit, where "unequal" means the
/// rematrix check *runs* rather than being skipped; an answer that was
/// wrong the other way would skip a check. And the relation is still
/// an equivalence — reflexive, symmetric, transitive — which is what
/// [`Eq`] requires of it.
impl PartialEq for ResampleSpec {
  fn eq(&self, other: &Self) -> bool {
    self.rate == other.rate
      && self.format == other.format
      && layouts_equal(&self.layout, &other.layout)
  }
}

impl Eq for ResampleSpec {}

/// Structural equality for two channel layouts, **without an FFmpeg
/// call and without following a pointer**. See [`ResampleSpec`]'s
/// [`PartialEq`] for why this exists and what a `CUSTOM` order answers.
fn layouts_equal(a: &ChannelLayout, b: &ChannelLayout) -> bool {
  let (order_a, order_b) = (layout_order(a), layout_order(b));
  if order_a != order_b || a.channels() != b.channels() {
    return false;
  }
  match LayoutArm::of(order_a) {
    // SAFETY: these two orders name the `mask` arm — a `uint64_t` —
    // and `AVChannelLayout`'s contract defines it for them.
    LayoutArm::Mask => unsafe { a.0.u.mask == b.0.u.mask },
    // SAFETY: this order names the `map` arm. The pointers are
    // **compared**, never dereferenced — which is the whole point.
    LayoutArm::Map => unsafe { core::ptr::eq(a.0.u.map, b.0.u.map) },
    // **Nothing is read.** For `UNSPEC` — and for any order this build
    // does not name, which folds here — FFmpeg's own header says the
    // union is undefined and must not be used. Two unspecified layouts
    // of the same width are the same layout, whatever bytes happen to
    // sit in storage neither of them owns.
    LayoutArm::Undefined => true,
  }
}

impl ResampleSpec {
  /// Constructs a spec from its three parts.
  ///
  /// Deliberately total and `const`: a spec is a description, and
  /// describing something `swr` cannot convert is not itself an error.
  /// [`FfmpegResampler::new`] is the choke point every construction
  /// route passes through, and it is what refuses a rate, a format or a
  /// channel layout this backend cannot honour — see
  /// [`FfmpegResampler::new`] for the roster and
  /// [`ResampleError::UnsupportedLayout`] for why a layout can be
  /// refused at all.
  #[inline]
  pub const fn new(rate: u32, format: Sample, layout: ChannelLayout) -> Self {
    Self {
      rate,
      format,
      layout,
    }
  }

  /// The spec a track *declares*, read off the owned codec ticket a
  /// `mediadecode-ffmpeg` `FfmpegDemuxer` track row carries
  /// (`track.extra().ticket()`) — the "source from `TrackInfo`" path.
  ///
  /// Reads the ticket directly rather than rebuilding an
  /// `AVCodecParameters` to read three fields out of: the fields are
  /// already owned, and a rebuild here would allocate `extradata` a
  /// resampler has no use for.
  ///
  /// Returns `None` for a non-audio track, for one whose declared
  /// sample format is `AV_SAMPLE_FMT_NONE` (a codec whose format is
  /// only known once its decoder opens), and for a custom or ambisonic
  /// channel layout — see [`Self::from_decoder`] for the first case and
  /// the note on [`unspecified_layout`] for the last.
  pub fn from_ticket(ticket: &crate::ticket::CodecTicket) -> Option<Self> {
    if !crate::boundary::media_kind_from_raw(ticket.codec_type()).is_audio() {
      return None;
    }
    let rate = ticket.sample_rate().max(0) as u32;
    if rate == 0 {
      return None;
    }
    let format = SampleFormat::from_raw(ticket.format()).to_ffmpeg()?;
    // Through the same decision [`layout_from_raw`] makes — the same
    // function, in fact, so the two roads cannot drift into admitting
    // different layouts from the same file.
    let owned = ticket.ch_layout();
    let layout = layout_from_parts(owned.order(), owned.channels(), owned.mask())?;
    Some(Self::new(rate, format, layout))
  }

  /// [`Self::from_ticket`], over a live `AVCodecParameters` — the road
  /// for a caller holding one straight off `stream.parameters()`.
  ///
  /// Returns `None` in every case [`Self::from_ticket`] does, and also
  /// for null-backed parameters: `Parameters`' safe constructors hand
  /// one back when FFmpeg's allocation failed and report nothing, so a
  /// caller can arrive here holding one without ever having been told.
  /// Parameters that were never allocated describe no audio, which
  /// this function already has a word for.
  pub fn from_parameters(parameters: &Parameters) -> Option<Self> {
    // Before `medium()`, which dereferences the pointer inside
    // ffmpeg-next. `Parameters`' safe constructors hand back a
    // null-backed value when FFmpeg's allocation failed and report
    // nothing, so a caller can arrive here holding one without ever
    // having been told. Parameters that were never allocated describe
    // no audio, which this function already has a word for.
    // SAFETY: reading the pointer without dereferencing it.
    if unsafe { parameters.as_ptr() }.is_null() {
      return None;
    }
    if !crate::boundary::media_kind_of(parameters).is_audio() {
      return None;
    }
    // SAFETY: `parameters` keeps the `AVCodecParameters` live; every
    // read below goes through the raw pointer and none of them
    // materialises a bindgen enum out of foreign memory.
    let par = unsafe { parameters.as_ptr() };
    let rate = unsafe { (*par).sample_rate }.max(0) as u32;
    if rate == 0 {
      return None;
    }
    let format = SampleFormat::from_raw(unsafe { (*par).format }).to_ffmpeg()?;
    let layout = unsafe { layout_from_raw(addr_of!((*par).ch_layout)) }?;
    Some(Self::new(rate, format, layout))
  }

  /// The spec an opened decoder will actually produce — its rate,
  /// sample format and channel layout, straight off the codec context.
  ///
  /// Reach it through
  /// `mediadecode-ffmpeg`'s `FfmpegAudioStreamDecoder::inner`.
  /// `None` on a custom or ambisonic layout, and on a context whose
  /// sample format is still unset (a decoder that has not been opened).
  pub fn from_decoder(decoder: &ffmpeg_next::decoder::Audio) -> Option<Self> {
    // SAFETY: `decoder` keeps the `AVCodecContext` live. `sample_fmt`
    // is read as the raw integer it is rather than through
    // `decoder.format()`, which would construct an `AVSampleFormat`
    // out of foreign memory.
    let ctx = unsafe { decoder.as_ptr() };
    // Same reason as `from_parameters`: `codec::Context::new()` is a
    // safe constructor over an unchecked `avcodec_alloc_context3`, so a
    // decoder can be null-backed without anyone having been told.
    if ctx.is_null() {
      return None;
    }
    let format =
      SampleFormat::from_raw(unsafe { read_unaligned(addr_of!((*ctx).sample_fmt).cast::<i32>()) })
        .to_ffmpeg()?;
    let rate = unsafe { (*ctx).sample_rate }.max(0) as u32;
    if rate == 0 {
      return None;
    }
    let layout = unsafe { layout_from_raw(addr_of!((*ctx).ch_layout)) }?;
    Some(Self::new(rate, format, layout))
  }

  /// A layout that names a channel *count* and nothing else —
  /// `AV_CHANNEL_ORDER_UNSPEC`.
  ///
  /// Not a degenerate case: a WAV file without a `WAVE_FORMAT_EXTENSIBLE`
  /// channel mask genuinely declares no layout, and FFmpeg faithfully
  /// reports it as unspecified in the codec parameters, in the codec
  /// context, and on every decoded frame. Substituting a default layout
  /// would make the source spec disagree with the frames it is supposed
  /// to describe, and every `send_frame` would be refused as a
  /// mid-stream change. `swr` accepts an unspecified layout at either
  /// end and maps the channels positionally.
  #[inline]
  pub fn unspecified_layout(channels: i32) -> ChannelLayout {
    // SAFETY: a zeroed `AVChannelLayout` is a valid value — `order`
    // reads as `AV_CHANNEL_ORDER_UNSPEC`, the zero discriminant, and
    // the union is documented as unused for that order.
    unsafe {
      let mut layout: ffmpeg_next::ffi::AVChannelLayout = std::mem::zeroed();
      layout.nb_channels = channels.max(0);
      ChannelLayout(layout)
    }
  }

  /// Sample rate in Hz.
  #[inline]
  pub const fn rate(&self) -> u32 {
    self.rate
  }
  /// Sample format.
  #[inline]
  pub const fn format(&self) -> Sample {
    self.format
  }
  /// Channel layout.
  #[inline]
  pub const fn layout(&self) -> ChannelLayout {
    self.layout
  }
  /// Channel count, from the layout.
  #[inline]
  pub fn channels(&self) -> i32 {
    self.layout.channels()
  }

  /// The timebase output frames carry — one tick per output sample.
  fn timebase(&self) -> Timebase {
    Timebase::new(
      1,
      std::num::NonZeroI32::new(self.rate.min(i32::MAX as u32) as i32).unwrap_or(
        // A zero-rate spec never reaches here: `new` is the only way in
        // and every caller of it names a real rate. Falling back to
        // one tick per second keeps the arithmetic total rather than
        // panicking on a value that cannot occur.
        std::num::NonZeroI32::new(1).expect("1 is non-zero"),
      ),
    )
  }
}

/// `mediaspine::resampler::AudioResampler` impl wrapping
/// `swresample`.
///
/// Construction is [`Self::new`], off the trait, taking both specs —
/// see the trait's own documentation for why the target can never be a
/// constant.
pub struct CarrierResampler<C: crate::FfmpegCarrier> {
  ctx: resampling::Context,
  source: ResampleSpec,
  target: ResampleSpec,
  /// The source spec restated in the vocabulary a decoded `AudioFrame`
  /// carries. The mid-stream check compares against these, not against
  /// FFmpeg's dialect, so it never has to translate a frame.
  source_format: SampleFormat,
  source_layout: ChannelLayoutDescription,
  /// The target spec in the vocabulary an output frame carries,
  /// computed once at construction. Assembling a converted frame after
  /// `swr` has run must not have to ask FFmpeg anything, because asking
  /// can fail — see [`FfmpegResampler::prepare_output`].
  target_format: SampleFormat,
  target_layout: ChannelLayoutDescription,
  /// The layouts `swr` is really configured with — see
  /// [`initialized_layout`]. Every `AVFrame` this type stages or
  /// allocates carries these, not the declared ones.
  staged_source_layout: ChannelLayout,
  staged_target_layout: ChannelLayout,
  target_timebase: Timebase,
  ready: VecDeque<Frame<C>>,
  /// Next output timestamp, in target-rate ticks. `None` until the
  /// first input frame anchors it.
  next_pts: Option<i64>,
  eof: bool,
  /// `true` once the post-EOF tail has been drained to its end.
  ///
  /// **The terminal answer has to be terminal.** Without this latch,
  /// every poll past the end re-enters the flush road — allocating an
  /// output frame and asking `swr` again — because `swr_get_delay` can
  /// keep reporting residual samples that `swr_convert_frame` will
  /// never emit (observed: 16 samples left standing after the tail is
  /// genuinely exhausted). That is harmless while allocation succeeds
  /// and wrong when it does not: a session that has already answered
  /// `Ended` would start answering with an allocation error instead,
  /// turning a settled protocol state back into a fault. The latch
  /// makes the end cheap and unconditional. `flush` clears it with the
  /// rest of the session.
  drained: bool,
  /// What one converted frame may cost. See
  /// [`Self::check_output_bytes`] for why a resampler needs a ceiling
  /// of its own even when its input already had one.
  limits: FrameLimits,
  /// The lane. Zero-sized: it selects how a produced plane is carried
  /// — shared out of the output `AVFrame` or copied out of it — and
  /// nothing else about the conversion.
  _carrier: core::marker::PhantomData<C>,
}

impl<C: crate::FfmpegCarrier + crate::CarrierOps> CarrierResampler<C> {
  /// Opens a resampler between two explicit specs.
  ///
  /// Both are required and neither is inferred. The source is what the
  /// decoder will hand over — read it off the track
  /// ([`ResampleSpec::from_parameters`]) or off the opened decoder
  /// ([`ResampleSpec::from_decoder`]). The target is the caller's, and
  /// is options: 16 kHz mono for a speech model, 48 kHz for an
  /// audio-event one, both from the same track.
  ///
  /// # The choke point
  ///
  /// [`ResampleSpec::new`] is `const` and total, so this is where both
  /// ends are checked — every construction route (`from_parameters`,
  /// `from_decoder`, the public constructor) passes through here, and
  /// nothing hazardous reaches `swr` or a staged `AVFrame` behind it:
  ///
  /// - a rate of zero, or one past `c_int`
  ///   ([`ResampleError::UnsupportedRate`]);
  /// - `AV_SAMPLE_FMT_NONE` ([`ResampleError::UnsupportedFormat`]);
  /// - a channel layout that is neither native nor unspecified, or one
  ///   naming no channels ([`ResampleError::UnsupportedLayout`]).
  ///
  /// `limits` bounds what one **converted** frame may cost.
  ///
  /// [`FrameLimits`] rather than a limit of this seam's own: the
  /// quantity is bytes of one produced audio frame, which is exactly
  /// what [`FrameLimits::max_frame_bytes`] means everywhere else in
  /// this crate, and one number for "what a frame may cost" is worth
  /// more than a second vocabulary. [`FrameLimits::max_pixels`] is
  /// unused here, as it is on the audio decode path, and for the same
  /// reason: audio has no pixels.
  pub(crate) fn new_impl(
    source: ResampleSpec,
    target: ResampleSpec,
    limits: FrameLimits,
  ) -> Result<Self, ResampleError> {
    check_spec(&source, SpecEnd::Source)?;
    check_spec(&target, SpecEnd::Target)?;
    // **Both layouts, preflighted before `swr` is handed either.**
    //
    // `swr_alloc_set_opts2` and `swr_build_matrix2` take an
    // `AVChannelLayout` and assume every invariant
    // `av_channel_layout_check` states; `ffmpeg_next::ChannelLayout` is
    // a public newtype over that public struct, so a caller can hand
    // this constructor a layout breaking any of them — a `NATIVE` count
    // that disagrees with its mask, an `AMBISONIC` layout whose
    // channels form no ambisonic order, a count large enough to take
    // FFmpeg's own `int` arithmetic somewhere it was not meant to go.
    // One preflight, shared with the demux admission pass, the codec
    // ticket and the decoder.
    preflight_layout(&source.layout, SpecEnd::Source)?;
    preflight_layout(&target.layout, SpecEnd::Target)?;

    // The layouts `swr` is really configured with, resolved *before*
    // the pair is judged — because the conversion that will run is
    // between these two, not between the two that were declared. An
    // unspecified layout becomes FFmpeg's default for its channel count
    // (twenty-four unspecified channels are 22.2), so judging the
    // declared pair let exactly the routing the explicit 22.2 refusal
    // blocks walk in through the unspecified door.
    let staged_source_layout = initialized_layout(source.layout);
    let staged_target_layout = initialized_layout(target.layout);
    check_pair(&staged_source_layout, &staged_target_layout)?;
    let ctx = open_context(&source, &target, staged_source_layout, staged_target_layout)?;

    let source_format = SampleFormat::from_ffmpeg(source.format);
    let target_format = SampleFormat::from_ffmpeg(target.format);
    // SAFETY: the layout is a live `ChannelLayout` owned by this scope.
    let target_layout =
      crate::channel_layout::channel_layout_description_from_ffmpeg(&staged_target_layout)
        .map_err(|_| {
          ResampleError::UnsupportedLayout(UnsupportedLayout::new(
            SpecEnd::Target,
            layout_order(&staged_target_layout),
            staged_target_layout.channels(),
          ))
        })?;
    // SAFETY: the layout is a live `ChannelLayout` owned by `source`
    // for the duration of this call.
    let source_layout = crate::channel_layout::channel_layout_description_from_ffmpeg(
      &source.layout,
    )
    .map_err(|_| {
      ResampleError::UnsupportedLayout(UnsupportedLayout::new(
        SpecEnd::Source,
        layout_order(&source.layout),
        source.layout.channels(),
      ))
    })?;
    let target_timebase = target.timebase();

    Ok(Self {
      ctx,
      source,
      target,
      source_format,
      source_layout,
      target_format,
      target_layout,
      staged_source_layout,
      staged_target_layout,
      target_timebase,
      ready: VecDeque::new(),
      next_pts: None,
      eof: false,
      drained: false,
      limits,
      _carrier: core::marker::PhantomData,
    })
  }

  /// The spec frames must arrive in.
  #[inline]
  pub(crate) const fn source_impl(&self) -> &ResampleSpec {
    &self.source
  }

  /// The spec frames leave in.
  #[inline]
  pub(crate) const fn target_impl(&self) -> &ResampleSpec {
    &self.target
  }

  /// Borrows the wrapped `swr` context.
  #[inline]
  pub(crate) const fn inner_impl(&self) -> &resampling::Context {
    &self.ctx
  }

  /// Samples still inside the delay line, counted at the output rate.
  #[inline]
  pub(crate) fn delay_impl(&self) -> i64 {
    self.ctx.delay().map_or(0, |d| d.output.max(0))
  }

  /// Refuses a frame whose shape is not the source spec.
  fn check_source(&self, frame: &Frame<C>) -> Result<(), ResampleError> {
    if frame.sample_rate() != self.source.rate
      || *frame.sample_format() != self.source_format
      || *frame.channel_layout() != self.source_layout
    {
      return Err(ResampleError::SourceChanged(SourceChanged::new(
        self.source.rate,
        self.source_format,
        frame.sample_rate(),
        *frame.sample_format(),
      )));
    }
    Ok(())
  }

  /// Where a frame's timestamp lands on the output timeline, or `None`
  /// when it carries none.
  ///
  /// Rescaled with the **checked** rung, and before anything is staged.
  /// `Timestamp::rescale_to` saturates, and both ends of that clamp are
  /// wrong here: a positive one reaches the counted timeline's checked
  /// addition only after `swr` has consumed the input, leaving a
  /// session no caller can retry; a negative one lands on `i64::MIN`,
  /// which *is* `AV_NOPTS_VALUE`, so the conversion back reads the
  /// frame as carrying no timestamp at all and an extreme timestamp is
  /// silently erased. A timestamp that does not fit the output timeline
  /// is refused by name, with the resampler untouched.
  fn anchor_of(&self, frame: &Frame<C>) -> Result<Option<i64>, ResampleError> {
    let Some(timestamp) = frame.pts() else {
      return Ok(None);
    };
    let ticks = timestamp.pts();
    let out_of_range = || ResampleError::TimestampOutOfRange(TimestampOutOfRange::new(ticks));
    // `AV_NOPTS_VALUE` is a sentinel, not a time. A frame carrying it
    // as a value says something contradictory, and anchoring on it
    // would produce output frames that report no timestamp.
    if ticks == AV_NOPTS_VALUE {
      return Err(out_of_range());
    }
    let rescaled = timestamp
      .timebase()
      .checked_rescale(ticks, self.target_timebase)
      .ok_or_else(out_of_range)?;
    if rescaled == AV_NOPTS_VALUE {
      return Err(out_of_range());
    }
    Ok(Some(rescaled))
  }

  /// Stages a decoded frame as an `AVFrame` swr can read.
  ///
  /// Geometry is settled **before** anything is allocated. A frame's
  /// header is a claim, not a fact: `nb_samples` comes from the same
  /// foreign memory as the planes it describes, and sizing an
  /// allocation off it first would let a forged frame with a
  /// twelve-byte plane ask for tens of gigabytes on its way to being
  /// refused.
  fn stage_input(&self, frame: &Frame<C>) -> Result<frame::Audio, ResampleError> {
    let samples = frame.nb_samples() as usize;
    let channels = self.source.channels();

    // What the *format* requires, not what the allocated frame reports
    // — the frame does not exist yet.
    let planes = if self.source.format.is_planar() {
      // `check_spec` proved this positive at construction.
      channels as usize
    } else {
      1
    };
    let found = frame.plane_count() as usize;
    if planes > found {
      return Err(ResampleError::PlaneCount(PlaneCount::new(planes, found)));
    }
    let bytes = plane_bytes(self.source.format, samples, channels)
      .ok_or(ResampleError::SampleCount(SampleCount::new(samples)))?;
    for plane in frame.planes().iter().take(planes) {
      let src = plane.data_ref().as_ref();
      if src.len() < bytes {
        return Err(ResampleError::PlaneCount(PlaneCount::new(bytes, src.len())));
      }
    }

    // Only now, with every plane proved long enough for the sample
    // count that sizes this allocation.
    let mut input = new_audio_frame(
      self.source.format,
      samples,
      self.source.rate,
      self.staged_source_layout,
    )?;
    // What the allocation really produced. `data_mut` panics past its
    // own plane count, and this crate does not put a panic on a path
    // that reads foreign geometry.
    let staged = input.planes();
    if staged < planes {
      return Err(ResampleError::PlaneCount(PlaneCount::new(planes, staged)));
    }
    for (index, plane) in frame.planes().iter().take(planes).enumerate() {
      let src = plane.data_ref().as_ref();
      let dst = input.data_mut(index);
      if dst.len() < bytes {
        return Err(ResampleError::PlaneCount(PlaneCount::new(bytes, dst.len())));
      }
      dst[..bytes].copy_from_slice(&src[..bytes]);
    }
    Ok(input)
  }

  /// The most samples the next conversion could produce: the delay
  /// line's contents plus `in_samples` of new input, rescaled to the
  /// output rate and rounded up.
  ///
  /// Separate from the allocation because it is also the preflight the
  /// output timeline is checked against — *before* `swr` consumes
  /// anything, so a refusal leaves the session where a caller can retry
  /// it.
  fn output_capacity(&self, in_samples: i64) -> Result<usize, ResampleError> {
    let delay_in = self.ctx.delay().map_or(0, |d| d.input.max(0));
    let total = delay_in.saturating_add(in_samples).max(0) as i128;
    let scaled = (total * i128::from(self.target.rate) + i128::from(self.source.rate) - 1)
      / i128::from(self.source.rate).max(1);
    // One extra sample of headroom: swr rounds its own accounting, and
    // an output frame one short would silently push the remainder into
    // the internal FIFO where the pts accounting cannot see it until
    // the next call.
    let samples = scaled + 1;
    // `av_frame_get_buffer` takes the count as a `c_int`. A request
    // past that is refused by name rather than clamped: a silently
    // shortened output frame is a stream that loses samples.
    if samples > i128::from(i32::MAX) {
      // Saturating only for a count past `usize` itself, which no
      // machine could hold either way.
      return Err(ResampleError::SampleCount(SampleCount::new(
        usize::try_from(samples).unwrap_or(usize::MAX),
      )));
    }
    Ok(samples.max(1) as usize)
  }

  /// Refuses a conversion whose output would not fit the frame ceiling.
  ///
  /// # Why a resampler needs one even though its input had one
  ///
  /// [`Self::output_capacity`] bounds the output **sample count**, and
  /// only at `i32::MAX` — the structural limit of
  /// `av_frame_get_buffer`. Nothing in it bounds the *bytes*, and the
  /// two are related by a ratio the caller does not control: the
  /// capacity is `input_samples × target_rate / source_rate`, and a
  /// source spec read off an untrusted container can say 1 Hz. One
  /// second of 1 Hz mono input converted to 48 kHz stereo `f32` is
  /// 384 KiB from 4 bytes — and the same input against a 1 Hz source
  /// claim and a 192 kHz target is multi-gigabyte. The frame that
  /// arrived was within *its* ceiling; the frame that leaves need not
  /// be, so it gets its own judgement.
  ///
  /// Both allocations are covered: `av_frame_get_buffer`'s, and the
  /// [`FfmpegBytes`] copy [`Self::finish_output`] makes from it.
  fn check_output_bytes(&self, capacity: usize, channels: i32) -> Result<(), ResampleError> {
    // **Priced as the allocator prices it, not as the samples weigh.**
    // This used to multiply the tight plane length by the plane count,
    // which is the payload's arithmetic and not
    // `av_frame_get_buffer`'s: a one-sample eight-channel planar `s16`
    // frame is 16 bytes of samples and a **768-byte** allocation,
    // because every plane is aligned and padded on its own. A 16-byte
    // ceiling admitted it.
    //
    // The overhead is under one percent on any frame big enough to
    // care about, which is exactly why this went unseen — see
    // [`crate::footprint`] for the measured table and for the rule the
    // whole crate now keeps: a judge must dominate the allocator's
    // arithmetic, not the payload's.
    let bytes = crate::footprint::audio_frame_bytes(
      ffmpeg_next::ffi::AVSampleFormat::from(self.target.format) as libc::c_int,
      capacity,
      channels.max(0) as usize,
    )
    .ok_or(ResampleError::OutputTooLarge(OutputTooLarge::new(
      usize::MAX,
      self.limits.max_frame_bytes(),
    )))?;
    if bytes > self.limits.max_frame_bytes() {
      return Err(ResampleError::OutputTooLarge(OutputTooLarge::new(
        bytes,
        self.limits.max_frame_bytes(),
      )));
    }
    Ok(())
  }

  /// Refuses a conversion whose output could not be labelled: the
  /// timeline plus everything this call might produce has to stay
  /// inside `i64`.
  ///
  /// Asked before `swr` sees a sample, like everything else that can
  /// fail. [`Self::finish_output`] performs the same addition against
  /// the count actually produced, which cannot exceed the capacity
  /// checked here — so once this passes, that one cannot fail.
  fn check_timeline(&self, anchor: Option<i64>, capacity: usize) -> Result<(), ResampleError> {
    let pts = self.next_pts.or(anchor).unwrap_or(0);
    let samples = capacity as i64;
    if pts.checked_add(samples).is_none() {
      return Err(ResampleError::TimestampOverflow(TimestampOverflow::new(
        pts, samples,
      )));
    }
    Ok(())
  }

  /// Allocates the output frame **and proves every plane the converted
  /// frame will be read out of**, before `swr` is allowed to touch a
  /// sample.
  ///
  /// This is the shape the whole seam is built around. Anything
  /// fallible that runs *after* `swr_convert_frame` has consumed input
  /// leaves a session no caller can act on: retrying feeds the same
  /// samples twice, continuing loses them, and the delay line has moved
  /// either way. The failure kept relocating — the timestamp addition,
  /// the tail drain, then the output wrapping — so the fix is not
  /// another check in another place but an ordering that leaves nothing
  /// on the far side: the frame, every plane pointer it will be read
  /// through, and the queue slot are all taken here, where failing
  /// costs nothing but an error.
  ///
  /// **What 0.9 changed, and what it did not.** Through 0.8 this
  /// function also *acquired* one `AVBufferRef` view per plane, because
  /// wrapping a plane could fail and so had to happen on this side of
  /// the conversion; [`Self::finish_output`] then narrowed each view to
  /// what `swr` produced. The amputation removes the views — the output
  /// planes are copied out afterwards instead — and with them the
  /// failure that forced the acquisition to be early. What stays early
  /// is the *proof*: every plane pointer is checked non-null and
  /// checked to address `plane_len` bytes inside one of the frame's own
  /// buffers here, so the copy on the far side has nothing left to
  /// judge and [`Self::finish_output`] remains infallible.
  fn prepare_output(&self, capacity: usize) -> Result<PreparedOutput<C>, ResampleError> {
    let channels = self.target.channels();
    let plane_count = if self.target.format.is_planar() {
      // `check_spec` proved this positive at construction.
      channels as usize
    } else {
      1
    };
    let plane_len = plane_bytes(self.target.format, capacity, channels)
      .ok_or(ResampleError::SampleCount(SampleCount::new(capacity)))?;
    // Linear in the sample count, which is what lets the post-run
    // trim be a multiplication rather than another fallible call.
    let per_sample = plane_bytes(self.target.format, 1, channels)
      .ok_or(ResampleError::SampleCount(SampleCount::new(1)))?;

    // **The byte ceiling, before `av_frame_get_buffer` and before the
    // carrier copy that follows it.** Measured first, allocated second:
    // the three quantities above are arithmetic over the target spec
    // and cost nothing, so there is no reason for the frame to exist
    // before the answer does.
    self.check_output_bytes(capacity, channels)?;

    let frame = new_audio_frame(
      self.target.format,
      capacity,
      self.target.rate,
      self.staged_target_layout,
    )?;
    if frame.planes() < plane_count {
      return Err(ResampleError::PlaneCount(PlaneCount::new(
        plane_count,
        frame.planes(),
      )));
    }

    let mut reserved: [Option<C::Reserved>; 8] = core::array::from_fn(|_| None);
    for (index, slot) in reserved.iter_mut().enumerate().take(plane_count) {
      // SAFETY: `frame` owns a live `AVFrame` this call just
      // allocated; `data` is a public field and `plane_count` is
      // within the eight slots `data` has.
      let data_ptr = unsafe { (*frame.as_ptr()).data[index] };
      if data_ptr.is_null() {
        return Err(ResampleError::OutputBuffer(OutputBuffer::new(index)));
      }
      // SAFETY: the frame is live, and the helper only reads `buf[]`'s
      // ranges to find the one containing `data_ptr`. Proving it here
      // is what lets the carry on the far side of the conversion be
      // unconditional — a copy on the owned lane, a view on the other,
      // and neither has a proof left to make.
      let backing =
        unsafe { crate::convert::find_audio_backing_buffer(frame.as_ptr(), data_ptr, plane_len) }
          .ok_or(ResampleError::OutputBuffer(OutputBuffer::new(index)))?;
      // The carrier's claim on the plane, taken **here** — before `swr`
      // consumes anything. See [`FfmpegCarrier::reserve`].
      //
      // SAFETY: `backing` is a live buffer of this frame's, proved just
      // above to cover `plane_len` bytes from `data_ptr`; the frame is
      // moved into `PreparedOutput` below and so outlives the commit.
      let offset = unsafe { (data_ptr as usize).wrapping_sub((*backing).data as usize) };
      // SAFETY: as above — the extent lies inside `backing`.
      *slot = Some(
        unsafe { C::reserve(backing, offset, plane_len) }
          .ok_or(ResampleError::OutputBuffer(OutputBuffer::new(index)))?,
      );
    }

    Ok(PreparedOutput {
      frame,
      reserved,
      plane_count,
      plane_len,
      per_sample,
    })
  }

  /// Turns a converted frame into a `mediaspine` one. **Infallible**,
  /// by construction: every check it could have made was made in
  /// [`Self::prepare_output`], and everything left here is arithmetic
  /// over values this type owns plus a copy that cannot be refused.
  ///
  /// `None` when the conversion produced nothing — the delay line
  /// swallowed the input, which is ordinary and not a failure.
  fn finish_output(&mut self, mut prepared: PreparedOutput<C>) -> Option<Frame<C>> {
    let produced = prepared.frame.samples();
    if produced == 0 {
      return None;
    }
    let pts = self.next_pts.unwrap_or(0);
    // `check_timeline` ran before `swr` did, against a capacity that is
    // never smaller than what came out, so this addition cannot leave
    // `i64`. It is stated rather than checked because a check here
    // would be an error path on the wrong side of the conversion —
    // exactly what this design exists to remove.
    debug_assert!(
      pts.checked_add(produced as i64).is_some(),
      "the timeline was preflighted against a capacity >= produced",
    );
    self.next_pts = Some(pts.saturating_add(produced as i64));

    let bytes = prepared
      .per_sample
      .saturating_mul(produced)
      .min(prepared.plane_len);
    let plane_count = prepared.plane_count;
    let mut planes: [Plane<C::Buffer>; 8] = core::array::from_fn(|_| Plane::new(C::empty(), 0));
    for (index, slot) in planes.iter_mut().enumerate().take(plane_count) {
      // Present for every index below `plane_count`: `prepare_output`
      // fills exactly that many and returns an error otherwise.
      let Some(reserved) = prepared.reserved[index].take() else {
        continue;
      };
      // **The valid prefix, not the plane.** `plane_len` is what
      // capacity was allocated for; `bytes` is what `swr` produced.
      // Both lanes stop at the latter, for the same reason the decode
      // road does: the tail is allocator memory nothing wrote, and a
      // carrier's span is a span a consumer may read.
      //
      // SAFETY: the reservation covers `plane_len` bytes inside one of
      // `frame`'s own buffers, and `bytes <= plane_len` by the `min`
      // above. `swr_convert_frame` has written those bytes and does not
      // replace the buffers; `frame` has been owned by `prepared` — and
      // so kept alive — across the whole conversion. A view committed
      // here outlives `prepared` by refcount, and the writing is over
      // before the sharing begins: this resampler allocates a fresh
      // output frame per conversion, so nothing ever writes into a
      // buffer a delivered frame is reading.
      // Infallible: the owned lane took its allocation in `reserve`,
      // before `swr` ran, so nothing here can refuse and no converted
      // sample can be lost to an allocator.
      *slot = Plane::new(unsafe { C::commit(reserved, bytes) }, bytes as u32);
    }

    Some(
      AudioFrame::new(
        self.target.rate,
        produced as u32,
        // Exact, not clipped: `check_spec` refused every spec outside
        // `1..=MAX_FRAME_CHANNELS` before this resampler existed.
        self.target.channels() as u8,
        self.target_format,
        self.target_layout.clone(),
        planes,
        plane_count as u8,
        AudioFrameExtra::default(),
      )
      .with_pts(Some(Timestamp::new(pts, self.target_timebase)))
      .with_duration(Some(Timestamp::new(produced as i64, self.target_timebase))),
    )
  }
}

/// Everything a converted frame needs, acquired before the conversion
/// runs. See [`FfmpegResampler::prepare_output`].
struct PreparedOutput<C: crate::FfmpegCarrier + crate::CarrierOps> {
  /// The output `AVFrame`. Owning it here is what keeps the plane
  /// pointers below valid across the conversion — moving this struct
  /// moves a pointer to the `AVFrame`, never the `AVFrame` or the
  /// buffers it addresses.
  frame: frame::Audio,
  /// One carrier claim per populated plane, taken before the
  /// conversion ran and settled at its true length after — which is
  /// what keeps this struct's whole reason for existing intact on the
  /// view lane too. `None` for every unpopulated slot.
  reserved: [Option<C::Reserved>; 8],
  plane_count: usize,
  /// Bytes one plane holds at full capacity — the ceiling every trim
  /// stays under.
  plane_len: usize,
  /// Bytes one plane holds per sample.
  per_sample: usize,
}

impl<C: crate::FfmpegCarrier + crate::CarrierOps> CarrierResampler<C> {
  /// **Always [`Sent::Accepted`] when it accepts at all.** The
  /// converted-frame queue this type keeps is unbounded — every frame a
  /// conversion produces is built before `swr` is asked and pushed
  /// straight onto it — so there is no state in which draining first
  /// would let a submission through that is refused now. A bounded
  /// implementation of the same face would answer
  /// [`Sent::MustDrain`] here; this one has nothing to say it about.
  ///
  /// [`ResampleError::AfterEof`] stays an error rather than becoming
  /// that arm, and the line is the same one the decoders draw: a
  /// resampler that has been told the stream ended will refuse this
  /// frame however much is drained first, so sending the caller into a
  /// drain loop would be sending it nowhere. `flush` is the way back,
  /// and the message says so.
  pub(crate) fn send_frame_impl(&mut self, frame: &Frame<C>) -> Result<Sent, ResampleError> {
    if self.eof {
      return Err(ResampleError::AfterEof);
    }
    self.check_source(frame)?;
    // A frame carrying no samples is a header and nothing else. There
    // is nothing to convert and nothing to stage: `av_frame_get_buffer`
    // refuses a zero-sample allocation, so staging one would hand `swr`
    // an unbacked `AVFrame` for no gain.
    if frame.nb_samples() == 0 {
      return Ok(Sent::Accepted);
    }

    // Nothing below touches the session's state until the conversion
    // has succeeded. A refused frame must leave the timeline exactly
    // where it was, or the next good frame inherits the rejected one's
    // timestamp.
    let anchor = self.anchor_of(frame)?;
    let input = self.stage_input(frame)?;
    let capacity = self.output_capacity(frame.nb_samples() as i64)?;
    self.check_timeline(anchor, capacity)?;
    let mut prepared = self.prepare_output(capacity)?;
    // The last fallible thing before the conversion: room for the frame
    // it will produce. `push_back` on a full queue allocates, and an
    // allocation failure there aborts the process rather than
    // unwinding — so the growth happens here, where it can be an error.
    self
      .ready
      .try_reserve(1)
      .map_err(|_| ResampleError::QueueAlloc)?;

    // The only mutation. Everything above could fail and cost nothing;
    // nothing below can fail at all.
    self
      .ctx
      .run(&input, &mut prepared.frame)
      .map_err(|e| ResampleError::Resample(CrossingError::Ffmpeg(e)))?;

    // The frame is inside the filter now, so the timeline may be
    // anchored on it. Anchored on *input* rather than on the first
    // output, because a call that produces nothing but fills the delay
    // line still fixes where the stream starts.
    if self.next_pts.is_none() {
      self.next_pts = anchor;
    }
    if let Some(converted) = self.finish_output(prepared) {
      self.ready.push_back(converted);
    }
    Ok(Sent::Accepted)
  }

  /// **No parked-frame slot here, and none is needed.** The queue holds
  /// frames that are already built: every fallible step of a
  /// conversion — the carrier's claim included — happens in
  /// [`Self::prepare_output`], before `swr` consumes anything, and
  /// `finish_output` is infallible by construction. There is no
  /// conversion left to fail after a frame has been taken out of the
  /// queue, so nothing can be lost between the two. That is the
  /// property the reserve-then-commit seam was built for, stated where
  /// the sibling roads state their slots.
  pub(crate) fn receive_frame_impl(
    &mut self,
    dst: &mut Frame<C>,
  ) -> Result<Received, ResampleError> {
    if let Some(frame) = self.ready.pop_front() {
      *dst = frame;
      return Ok(Received::Produced);
    }
    if !self.eof {
      return Ok(Received::NeedsInput);
    }
    if self.drained {
      return Ok(Received::Ended);
    }
    // EOF: drain the conversion tail. Without this every file loses the
    // tens of milliseconds sitting inside the filter.
    //
    // **This is where the two answers used to be one.** Pre-EOF nothing
    // ready and post-EOF tail exhausted both returned `Again`, so a
    // caller that did not itself remember whether it had called
    // `send_eof` could not tell "send more" from "there is no more" —
    // and a drain loop written against the seam alone spun forever on
    // a resampler that was already finished.
    let remaining = self.delay_impl();
    if remaining <= 0 {
      self.drained = true;
      return Ok(Received::Ended);
    }
    let capacity = remaining.min(i64::from(i32::MAX)) as usize;
    // Same discipline as `send_frame`, and for the same reason: the
    // tail is drained only once the timeline can hold it and every
    // reference the converted frame needs is already in hand, so a
    // failure leaves the delay line untouched instead of turning
    // samples into an error.
    self.check_timeline(None, capacity)?;
    let mut prepared = self.prepare_output(capacity)?;
    self
      .ctx
      .flush(&mut prepared.frame)
      .map_err(|e| ResampleError::Resample(CrossingError::Ffmpeg(e)))?;
    match self.finish_output(prepared) {
      Some(frame) => {
        *dst = frame;
        Ok(Received::Produced)
      }
      // The delay line reported samples and the flush produced none.
      // Another flush would report the same and produce the same — this
      // really happens, `swr_get_delay` standing at a residue the
      // converter will not emit — so this is the end of the tail and
      // not a pause in it. Saying otherwise is the spin this reform
      // removes.
      None => {
        self.drained = true;
        Ok(Received::Ended)
      }
    }
  }

  pub(crate) fn send_eof_impl(&mut self) -> Result<Sent, ResampleError> {
    self.eof = true;
    Ok(Sent::Accepted)
  }

  /// Resets the resampler for another stream on the same two specs.
  ///
  /// The `swr` context is **rebuilt**, not drained. `swresample` has no
  /// reset call, and draining it dry cannot be verified from outside: a
  /// `swr_convert_frame` that makes no progress reports no error, so a
  /// drain loop that gives up and a drain loop that finished are
  /// indistinguishable — and a flush that returned `Ok` with the old
  /// delay line still inside would let one stream's tail contaminate
  /// the next. A fresh context is the only reset whose success is a
  /// fact.
  ///
  /// The new context is built before the old one is dropped, so a
  /// failure leaves the resampler exactly as it was: this call either
  /// resets everything or changes nothing.
  pub(crate) fn flush_impl(&mut self) -> Result<(), ResampleError> {
    let ctx = open_context(
      &self.source,
      &self.target,
      self.staged_source_layout,
      self.staged_target_layout,
    )?;
    self.ctx = ctx;
    self.ready.clear();
    self.next_pts = None;
    self.eof = false;
    self.drained = false;
    debug_assert_eq!(self.delay_impl(), 0, "a fresh swr context holds nothing");
    Ok(())
  }
}

macro_rules! resampler_lane_face {
  ($($lane:ty),+ $(,)?) => { $(
    impl CarrierResampler<$lane> {
      /// Opens a resampler between two explicit specs. See
      /// [`CarrierResampler::new_impl`] for the full contract.
      pub fn new(
        source: ResampleSpec,
        target: ResampleSpec,
        limits: FrameLimits,
      ) -> Result<Self, ResampleError> {
        Self::new_impl(source, target, limits)
      }

      /// The spec frames must arrive in.
      pub const fn source(&self) -> &ResampleSpec {
        self.source_impl()
      }

      /// The spec frames leave in.
      pub const fn target(&self) -> &ResampleSpec {
        self.target_impl()
      }

      /// The wrapped `swr` context.
      pub const fn inner(&self) -> &resampling::Context {
        self.inner_impl()
      }

      /// Samples still inside the delay line, counted at the output
      /// rate.
      pub fn delay(&self) -> i64 {
        self.delay_impl()
      }
    }

    impl AudioResampler for CarrierResampler<$lane> {
      type Adapter = Ffmpeg;
      type Buffer = <$lane as crate::FfmpegCarrier>::Buffer;
      type Error = ResampleError;

      fn send_frame(&mut self, frame: &Frame<$lane>) -> Result<Sent, ResampleError> {
        self.send_frame_impl(frame)
      }

      fn receive_frame(&mut self, dst: &mut Frame<$lane>) -> Result<Received, ResampleError> {
        self.receive_frame_impl(dst)
      }

      fn send_eof(&mut self) -> Result<Sent, ResampleError> {
        self.send_eof_impl()
      }

      fn flush(&mut self) -> Result<(), ResampleError> {
        self.flush_impl()
      }
    }
  )+ };
}

resampler_lane_face!(crate::View, crate::Owned);

/// Payload for [`ResampleError::SourceChanged`].
///
/// A frame arrived whose shape is not the source spec this resampler
/// was built with — the mid-stream refusal.
///
/// The face never silently reconfigures: doing so would resample the
/// two halves of a stream on different terms and hand back a single
/// unbroken timeline built out of them. Build a new resampler for the
/// new source spec.
#[derive(thiserror::Error, Debug, Clone)]
#[error(
  "source format changed mid-stream: expected {expected_rate} Hz {expected_format:?}, \
   got {found_rate} Hz {found_format:?}"
)]
pub struct SourceChanged {
  expected_rate: u32,
  expected_format: SampleFormat,
  found_rate: u32,
  found_format: SampleFormat,
}

impl SourceChanged {
  /// Constructs a `SourceChanged` payload.
  #[inline]
  pub const fn new(
    expected_rate: u32,
    expected_format: SampleFormat,
    found_rate: u32,
    found_format: SampleFormat,
  ) -> Self {
    Self {
      expected_rate,
      expected_format,
      found_rate,
      found_format,
    }
  }
  /// Rate the resampler was built for.
  #[inline]
  pub const fn expected_rate(&self) -> u32 {
    self.expected_rate
  }
  /// Sample format the resampler was built for.
  #[inline]
  pub const fn expected_format(&self) -> SampleFormat {
    self.expected_format
  }
  /// Rate the offending frame carried.
  #[inline]
  pub const fn found_rate(&self) -> u32 {
    self.found_rate
  }
  /// Sample format the offending frame carried.
  #[inline]
  pub const fn found_format(&self) -> SampleFormat {
    self.found_format
  }
}

/// Payload for [`ResampleError::PlaneCount`].
///
/// A frame's planes do not hold what its header claims — too few
/// planes for the format, or a plane shorter than its sample count
/// requires.
#[derive(thiserror::Error, Debug, Clone)]
#[error("frame plane geometry mismatch: expected {expected}, found {found}")]
pub struct PlaneCount {
  expected: usize,
  found: usize,
}

impl PlaneCount {
  /// Constructs a `PlaneCount` payload.
  #[inline]
  pub const fn new(expected: usize, found: usize) -> Self {
    Self { expected, found }
  }
  /// What the format and sample count require.
  #[inline]
  pub const fn expected(&self) -> usize {
    self.expected
  }
  /// What the frame carries.
  #[inline]
  pub const fn found(&self) -> usize {
    self.found
  }
}

/// Payload for [`ResampleError::OutputTooLarge`].
///
/// The converted frame would be larger than
/// [`FrameLimits::max_frame_bytes`] allows.
///
/// Distinct from [`SampleCount`], which is about a count no `AVFrame`
/// can express at all. This one is about a frame FFmpeg would happily
/// allocate and the caller has said it does not want: the amplification
/// a hostile source rate buys — one second at a declared 1 Hz becomes
/// gigabytes at 192 kHz — lands here.
#[derive(thiserror::Error, Debug, Clone, Copy, PartialEq, Eq)]
#[error("a converted frame of {bytes} bytes exceeds the {limit}-byte ceiling")]
pub struct OutputTooLarge {
  bytes: usize,
  limit: usize,
}

impl OutputTooLarge {
  /// Constructs an `OutputTooLarge` payload.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn new(bytes: usize, limit: usize) -> Self {
    Self { bytes, limit }
  }
  /// The bytes the conversion would have produced.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn bytes(&self) -> usize {
    self.bytes
  }
  /// The ceiling in force.
  #[cfg_attr(not(tarpaulin), inline(always))]
  pub const fn limit(&self) -> usize {
    self.limit
  }
}

/// Payload for [`ResampleError::SampleCount`].
///
/// A sample count no frame can hold: one whose byte size overflows, or
/// one past the `c_int` `av_frame_get_buffer` takes.
#[derive(thiserror::Error, Debug, Clone)]
#[error("{requested} samples is not a frame size")]
pub struct SampleCount {
  requested: usize,
}

impl SampleCount {
  /// Constructs a `SampleCount` payload.
  #[inline]
  pub const fn new(requested: usize) -> Self {
    Self { requested }
  }
  /// The count that was asked for.
  #[inline]
  pub const fn requested(&self) -> usize {
    self.requested
  }
}

/// Payload for [`ResampleError::UnsupportedRate`].
///
/// One end of the conversion declares a sample rate `swr` cannot be
/// driven with — zero, or past `c_int`.
#[derive(thiserror::Error, Debug, Clone)]
#[error("the {end} rate {rate} is not a sample rate swr can use")]
pub struct UnsupportedRate {
  end: SpecEnd,
  rate: u32,
}

impl UnsupportedRate {
  /// Constructs an `UnsupportedRate` payload.
  #[inline]
  pub const fn new(end: SpecEnd, rate: u32) -> Self {
    Self { end, rate }
  }
  /// Which end of the conversion.
  #[inline]
  pub const fn end(&self) -> SpecEnd {
    self.end
  }
  /// The rate that was declared.
  #[inline]
  pub const fn rate(&self) -> u32 {
    self.rate
  }
}

/// Payload for [`ResampleError::UnsupportedFormat`].
///
/// One end of the conversion declares no sample format
/// (`AV_SAMPLE_FMT_NONE`) — the state a codec context is in before its
/// decoder opens.
#[derive(thiserror::Error, Debug, Clone)]
#[error("the {end} spec names no sample format")]
pub struct UnsupportedFormat {
  end: SpecEnd,
}

impl UnsupportedFormat {
  /// Constructs an `UnsupportedFormat` payload.
  #[inline]
  pub const fn new(end: SpecEnd) -> Self {
    Self { end }
  }
  /// Which end of the conversion.
  #[inline]
  pub const fn end(&self) -> SpecEnd {
    self.end
  }
}

/// Payload for [`ResampleError::UnsupportedLayout`].
///
/// One end of the conversion declares a channel layout this backend
/// will not carry.
///
/// Native and unspecified layouts are the two it does. A **custom** or
/// **ambisonic** `AVChannelLayout` owns a heap-allocated channel map,
/// and FFmpeg documents that such a layout must be copied with
/// `av_channel_layout_copy` rather than assigned — while
/// `ffmpeg_next::ChannelLayout` is a `Copy` wrapper with no destructor.
/// Every `AVFrame` this type stages or allocates receives the layout by
/// assignment, and `av_frame_free` runs `av_channel_layout_uninit` on
/// it: the first staged frame to be dropped would free a map the spec,
/// the decoder and every later frame still point at. Refusing at
/// construction is what keeps that use-after-free unreachable; a
/// resampler over those layouts is a separate design, not a silent
/// approximation.
#[derive(thiserror::Error, Debug, Clone)]
#[error("the {end} channel layout is not supported: order {order}, {channels} channels")]
pub struct UnsupportedLayout {
  end: SpecEnd,
  order: i32,
  channels: i32,
}

impl UnsupportedLayout {
  /// Constructs an `UnsupportedLayout` payload.
  #[inline]
  pub const fn new(end: SpecEnd, order: i32, channels: i32) -> Self {
    Self {
      end,
      order,
      channels,
    }
  }
  /// Which end of the conversion.
  #[inline]
  pub const fn end(&self) -> SpecEnd {
    self.end
  }
  /// `AVChannelOrder` as the raw integer it is on the wire.
  #[inline]
  pub const fn order(&self) -> i32 {
    self.order
  }
  /// The channel count the layout declares.
  #[inline]
  pub const fn channels(&self) -> i32 {
    self.channels
  }
}

/// Payload for [`ResampleError::TooManyPlanes`].
///
/// A planar spec with more channels than a decoded frame has plane
/// slots.
///
/// `mediaspine`'s `AudioFrame` carries a fixed eight planes
/// (`AV_NUM_DATA_POINTERS`); planar audio past that lives in
/// `AVFrame.extended_data[]`, which this crate does not plumb through.
/// As a **source** no valid frame could ever arrive; as a **target**
/// `swr` would produce one this crate cannot hand back — and it would
/// fail only after the input had been consumed, leaving a session that
/// cannot be retried. Both are refused at construction, where nothing
/// has happened yet.
#[derive(thiserror::Error, Debug, Clone)]
#[error("the {end} spec is planar with {channels} channels; a frame carries {limit} planes")]
pub struct TooManyPlanes {
  end: SpecEnd,
  channels: i32,
  limit: i32,
}

impl TooManyPlanes {
  /// Constructs a `TooManyPlanes` payload.
  #[inline]
  pub const fn new(end: SpecEnd, channels: i32, limit: i32) -> Self {
    Self {
      end,
      channels,
      limit,
    }
  }
  /// Which end of the conversion.
  #[inline]
  pub const fn end(&self) -> SpecEnd {
    self.end
  }
  /// The channel count the layout declares.
  #[inline]
  pub const fn channels(&self) -> i32 {
    self.channels
  }
  /// Plane slots a frame has.
  #[inline]
  pub const fn limit(&self) -> i32 {
    self.limit
  }
}

/// Payload for [`ResampleError::UnsupportedChannelCount`].
///
/// A spec declaring more channels than a frame's channel field can
/// carry. `mediaspine`'s `AudioFrame` states its channel count in a
/// `u8`, so 255 is the ceiling in both directions.
///
/// This is the **packed** sibling of [`TooManyPlanes`], which only ever
/// caught planar specs: packed audio declares one plane whatever its
/// channel count, so a 256-channel packed spec sailed past that check
/// and had its count clipped on the way into the frame — a frame whose
/// bytes were computed from 256 channels while advertising 255. Refused
/// here, at the same choke point, for both ends.
#[derive(thiserror::Error, Debug, Clone)]
#[error("the {end} spec declares {channels} channels; a frame carries at most {limit}")]
pub struct UnsupportedChannelCount {
  end: SpecEnd,
  channels: i32,
  limit: i32,
}

impl UnsupportedChannelCount {
  /// Constructs an `UnsupportedChannelCount` payload.
  #[inline]
  pub const fn new(end: SpecEnd, channels: i32, limit: i32) -> Self {
    Self {
      end,
      channels,
      limit,
    }
  }
  /// Which end of the conversion.
  #[inline]
  pub const fn end(&self) -> SpecEnd {
    self.end
  }
  /// The channel count the layout declares.
  #[inline]
  pub const fn channels(&self) -> i32 {
    self.channels
  }
  /// Channels a frame can state.
  #[inline]
  pub const fn limit(&self) -> i32 {
    self.limit
  }
}

/// Payload for [`ResampleError::TimestampOutOfRange`].
///
/// A frame's timestamp does not land on the output timeline: it does
/// not survive the rescale as an `i64`, or it is `AV_NOPTS_VALUE`,
/// which is a sentinel rather than a time.
///
/// Raised before anything is staged, so a refused frame leaves the
/// resampler exactly as it was.
#[derive(thiserror::Error, Debug, Clone)]
#[error("the frame timestamp {pts} does not land on the output timeline")]
pub struct TimestampOutOfRange {
  pts: i64,
}

impl TimestampOutOfRange {
  /// Constructs a `TimestampOutOfRange` payload.
  #[inline]
  pub const fn new(pts: i64) -> Self {
    Self { pts }
  }
  /// The timestamp the frame carried, in its own timebase.
  #[inline]
  pub const fn pts(&self) -> i64 {
    self.pts
  }
}

/// Payload for [`ResampleError::ChannelDropped`].
///
/// The conversion between these two layouts would silently drop a
/// source channel: FFmpeg's own mixing matrix routes it to no output.
///
/// `swr` mixes the channel positions its rematrix table knows and
/// processes the rest of the input as though it were absent — a log
/// line at most. Measured against FFmpeg 9, packed 22.2 → mono loses
/// fifteen of twenty-four channels and `cube` → stereo loses two of
/// eight, so this is not a matter of channel count. Installing an
/// explicit mix matrix is how such a conversion would be accepted
/// deliberately; until this crate has a place for one, the pair is
/// refused.
#[derive(thiserror::Error, Debug, Clone)]
#[error(
  "converting {source_channels} channels to {target_channels} would drop source channel \
   {channel}: FFmpeg's mixing matrix routes it to no output"
)]
pub struct ChannelDropped {
  source_channels: i32,
  target_channels: i32,
  channel: i32,
}

impl ChannelDropped {
  /// Constructs a `ChannelDropped` payload.
  #[inline]
  pub const fn new(source_channels: i32, target_channels: i32, channel: i32) -> Self {
    Self {
      source_channels,
      target_channels,
      channel,
    }
  }
  /// Channels the source layout declares.
  #[inline]
  pub const fn source_channels(&self) -> i32 {
    self.source_channels
  }
  /// Channels the target layout declares.
  #[inline]
  pub const fn target_channels(&self) -> i32 {
    self.target_channels
  }
  /// The first source channel that reaches no output channel.
  #[inline]
  pub const fn channel(&self) -> i32 {
    self.channel
  }
}

/// Payload for [`ResampleError::RematrixUnsupported`].
///
/// FFmpeg will not build a mixing matrix between these two layouts at
/// all.
#[derive(thiserror::Error, Debug, Clone)]
#[error("FFmpeg builds no mixing matrix from {source_channels} channels to {target_channels}")]
pub struct RematrixUnsupported {
  source_channels: i32,
  target_channels: i32,
}

impl RematrixUnsupported {
  /// Constructs a `RematrixUnsupported` payload.
  #[inline]
  pub const fn new(source_channels: i32, target_channels: i32) -> Self {
    Self {
      source_channels,
      target_channels,
    }
  }
  /// Channels the source layout declares.
  #[inline]
  pub const fn source_channels(&self) -> i32 {
    self.source_channels
  }
  /// Channels the target layout declares.
  #[inline]
  pub const fn target_channels(&self) -> i32 {
    self.target_channels
  }
}

/// Payload for [`ResampleError::TimestampOverflow`].
///
/// The output timeline would leave `i64`. Counted timestamps are exact
/// or they are nothing, so this is named rather than saturated.
#[derive(thiserror::Error, Debug, Clone)]
#[error("the output timeline overflows: {pts} + {samples} samples")]
pub struct TimestampOverflow {
  pts: i64,
  samples: i64,
}

impl TimestampOverflow {
  /// Constructs a `TimestampOverflow` payload.
  #[inline]
  pub const fn new(pts: i64, samples: i64) -> Self {
    Self { pts, samples }
  }
  /// Where the timeline stood.
  #[inline]
  pub const fn pts(&self) -> i64 {
    self.pts
  }
  /// How many samples were produced.
  #[inline]
  pub const fn samples(&self) -> i64 {
    self.samples
  }
}

/// Payload for [`ResampleError::OutputBuffer`].
///
/// A reference to one of the output frame's planes could not be taken.
///
/// Raised while preparing the conversion, never after it: that is the
/// point of preparing.
#[derive(thiserror::Error, Debug, Clone)]
#[error("the output frame's plane {plane} could not be referenced")]
pub struct OutputBuffer {
  plane: usize,
}

impl OutputBuffer {
  /// Constructs an `OutputBuffer` payload.
  #[inline]
  pub const fn new(plane: usize) -> Self {
    Self { plane }
  }
  /// Which plane slot.
  #[inline]
  pub const fn plane(&self) -> usize {
    self.plane
  }
}

/// Errors from [`FfmpegResampler`] — **faults and the two send-side
/// refusals** ([`Self::SourceChanged`], [`Self::AfterEof`]).
///
/// `Again` used to be here and meant two opposite things: pre-EOF
/// "nothing ready, send more" and post-EOF "the tail is exhausted".
/// A caller polling the seam could tell them apart only by remembering
/// whether it had itself called `send_eof`; one that did not spun
/// forever. Both are [`Received`] states now, and they are distinct.
///
/// **Open fault taxonomy, so it is `#[non_exhaustive]`.** New ways to
/// fail are discovered — a backend, a ceiling, a corruption a codec
/// learns to report — and a consumer that meets one it has never heard
/// of should take its generic-fault path. That is exactly what the
/// wildcard arm this attribute forces is for. The two status
/// vocabularies opposite it,
/// [`Sent`](mediaspine::Sent) and [`Received`](mediaspine::Received),
/// are exhaustive for the mirror-image reason: their arms are the
/// substrate's fixed state set, and there the wildcard would be dead
/// weight hiding a state a consumer forgot.
#[derive(thiserror::Error, Debug, IsVariant, Unwrap, TryUnwrap)]
#[unwrap(ref, ref_mut)]
#[try_unwrap(ref, ref_mut)]
#[non_exhaustive]
pub enum ResampleError {
  /// The conversion would produce a frame larger than the ceiling
  /// allows. Refused **before** the output frame is allocated.
  #[error(transparent)]
  OutputTooLarge(#[from] OutputTooLarge),

  /// A frame arrived whose shape is not the source spec this resampler
  /// was built with — the mid-stream refusal.
  #[error(transparent)]
  SourceChanged(#[from] SourceChanged),

  /// [`send_frame`](AudioResampler::send_frame) was called after
  /// [`send_eof`](AudioResampler::send_eof). Call
  /// [`flush`](AudioResampler::flush) first to reuse the resampler for
  /// another stream.
  #[error("send_frame after send_eof; flush() first to start another stream")]
  AfterEof,

  /// A frame's planes do not hold what its header claims — too few
  /// planes for the format, or a plane shorter than its sample count
  /// requires.
  #[error(transparent)]
  PlaneCount(#[from] PlaneCount),

  /// A sample count no frame can hold: one whose byte size overflows,
  /// or one past the `c_int` `av_frame_get_buffer` takes.
  #[error(transparent)]
  SampleCount(#[from] SampleCount),

  /// One end of the conversion declares a sample rate `swr` cannot be
  /// driven with — zero, or past `c_int`.
  #[error(transparent)]
  UnsupportedRate(#[from] UnsupportedRate),

  /// One end of the conversion declares no sample format
  /// (`AV_SAMPLE_FMT_NONE`) — the state a codec context is in before
  /// its decoder opens.
  #[error(transparent)]
  UnsupportedFormat(#[from] UnsupportedFormat),

  /// One end of the conversion declares a channel layout this backend
  /// will not carry.
  #[error(transparent)]
  UnsupportedLayout(#[from] UnsupportedLayout),

  /// A planar spec with more channels than a decoded frame has plane
  /// slots.
  #[error(transparent)]
  TooManyPlanes(#[from] TooManyPlanes),

  /// A spec with more channels than a frame's channel field can state.
  #[error(transparent)]
  UnsupportedChannelCount(#[from] UnsupportedChannelCount),

  /// A frame's timestamp does not land on the output timeline.
  #[error(transparent)]
  TimestampOutOfRange(#[from] TimestampOutOfRange),

  /// The conversion between these two layouts would silently drop a
  /// source channel.
  #[error(transparent)]
  ChannelDropped(#[from] ChannelDropped),

  /// FFmpeg will not build a mixing matrix between these two layouts at
  /// all.
  #[error(transparent)]
  RematrixUnsupported(#[from] RematrixUnsupported),

  /// The output timeline would leave `i64`. Counted timestamps are
  /// exact or they are nothing, so this is named rather than saturated.
  #[error(transparent)]
  TimestampOverflow(#[from] TimestampOverflow),

  /// The wrapped `swresample` call reported an error.
  #[error(transparent)]
  Resample(#[from] CrossingError),

  /// A reference to one of the output frame's planes could not be
  /// taken.
  #[error(transparent)]
  OutputBuffer(#[from] OutputBuffer),

  /// The queue of converted frames could not be grown to hold one more.
  #[error("out of memory reserving room for a converted frame")]
  QueueAlloc,
}

/// Which end of a conversion a refusal is about.
#[derive(Copy, Clone, Debug, PartialEq, Eq, IsVariant)]
pub enum SpecEnd {
  /// The spec frames must arrive in.
  Source,
  /// The spec frames leave in.
  Target,
}

impl core::fmt::Display for SpecEnd {
  fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
    f.write_str(match self {
      Self::Source => "source",
      Self::Target => "target",
    })
  }
}

/// Plane slots a `mediaspine::frame::AudioFrame` has — the fixed array
/// matching `AV_NUM_DATA_POINTERS`. Planar audio past this many
/// channels lives in `AVFrame.extended_data[]` / `extended_buf[]`,
/// which this crate does not plumb through: `convert` refuses such a
/// frame and `AudioFrame::new` will not build one.
const MAX_AUDIO_PLANES: i32 = 8;

/// Channels a `mediaspine::frame::AudioFrame` can state — its channel
/// field is a `u8`. A spec past this is refused rather than clipped.
const MAX_FRAME_CHANNELS: i32 = u8::MAX as i32;

/// Refuses a spec `swr` cannot be driven with, or whose channel layout
/// cannot be carried by value — see [`ResampleError::UnsupportedLayout`]
/// for that one, which is the whole reason this check exists at the
/// choke point rather than in the `const` constructor.
fn check_spec(spec: &ResampleSpec, end: SpecEnd) -> Result<(), ResampleError> {
  if spec.rate == 0 || spec.rate > i32::MAX as u32 {
    return Err(ResampleError::UnsupportedRate(UnsupportedRate::new(
      end, spec.rate,
    )));
  }
  if spec.format == Sample::None {
    return Err(ResampleError::UnsupportedFormat(UnsupportedFormat::new(
      end,
    )));
  }
  let order = layout_order(&spec.layout);
  let channels = spec.layout.channels();
  let carried = order == AVChannelOrder::AV_CHANNEL_ORDER_NATIVE as i32
    || order == AVChannelOrder::AV_CHANNEL_ORDER_UNSPEC as i32;
  if !carried || channels <= 0 {
    return Err(ResampleError::UnsupportedLayout(UnsupportedLayout::new(
      end, order, channels,
    )));
  }
  // A planar spec with more channels than the frame model has plane
  // slots is a resampler that cannot work in either direction, and
  // saying so here is the difference between a refusal at construction
  // and a refusal on every frame — the target one arriving *after*
  // `swr` has already consumed the input, which is not a state a caller
  // can retry from.
  if spec.format.is_planar() && channels > MAX_AUDIO_PLANES {
    return Err(ResampleError::TooManyPlanes(TooManyPlanes::new(
      end,
      channels,
      MAX_AUDIO_PLANES,
    )));
  }
  // And the packed sibling, which the plane check above cannot see: a
  // packed spec declares one plane at any channel count, so it reached
  // the frame with its count clipped to 255 instead of refused. That is
  // the same silent truncation the decode path was carrying, on the
  // other audio road — closed here, at the same choke point, so that
  // every channel count downstream is exact by construction.
  if channels > MAX_FRAME_CHANNELS {
    return Err(ResampleError::UnsupportedChannelCount(
      UnsupportedChannelCount::new(end, channels, MAX_FRAME_CHANNELS),
    ));
  }
  Ok(())
}

/// A layout's `AVChannelOrder` as the integer it is on the wire.
///
/// Read raw rather than matched as an `AVChannelOrder`, the discipline
/// this crate keeps everywhere it touches a bindgen enum: a value
/// outside this build's discriminant set would be undefined behaviour
/// the moment it existed as one.
/// Runs [`crate::channel_layout::layout_preflight`] over a caller's
/// layout and refuses a `CUSTOM` order outright.
///
/// The refusal is not squeamishness about custom layouts; it is what
/// makes the rest of this type's story true. A `CUSTOM` layout carries
/// a raw `u.map` pointer whose extent no signature here can establish —
/// the same wall [`crate::channel_layout::channel_layout_from_ffmpeg`]
/// meets — and a [`ResampleSpec`] holding one would be a struct with a
/// bare pointer inside, which is the thing that would have to be
/// reasoned about for every use of this value on another thread. Every
/// layout a live resampler holds is mask-only, and therefore a plain
/// value.
fn preflight_layout(layout: &ChannelLayout, end: SpecEnd) -> Result<(), ResampleError> {
  let unsupported = || {
    ResampleError::UnsupportedLayout(UnsupportedLayout::new(
      end,
      layout_order(layout),
      layout.channels(),
    ))
  };
  if layout_order(layout) == AVChannelOrder::AV_CHANNEL_ORDER_CUSTOM as i32 {
    return Err(unsupported());
  }
  // SAFETY: `layout` is a live `ChannelLayout` for the duration of this
  // call, and the custom order — the only one whose contract mentions a
  // map — was just refused.
  unsafe { crate::channel_layout::layout_preflight(&layout.0 as *const _) }
    .map_err(|_| unsupported())
}

/// **`Send`, and deliberately not `Sync`.**
///
/// # Safety
///
/// The `SwrContext` behind `ctx` is owned by this value: it is
/// allocated in [`open_context`], never shared, never cloned, and
/// dropped with the resampler. Every operation that touches it goes
/// through `&mut self` — `swr_convert_frame`, `swr_get_delay`,
/// `swr_init` on the rebuild — so no two threads can be inside
/// libswresample for this context at once, which is the whole of what
/// that library requires: it keeps no thread-local or process-global
/// state of its own, and a context is a plain heap allocation whose
/// address is meaningful on any thread.
///
/// The other fields are ordinary owned values. The two
/// `ffmpeg_next::ChannelLayout`s and the two [`ResampleSpec`]s wrap an
/// `AVChannelLayout`, which holds a pointer **only for
/// `AV_CHANNEL_ORDER_CUSTOM`** — and [`preflight_layout`] refuses that
/// order at the one construction choke point, so every layout a live
/// resampler holds is the `u.mask` arm: a `uint64_t`, a plain value.
/// The staged layouts come from [`initialized_layout`], which produces
/// `av_channel_layout_default`'s mask-only answer. `ready` holds
/// `Frame<C>` values, whose carriers are `Send` by the carrier
/// contract.
///
/// **Not `Sync`, and that is not an oversight.** `&self` would let two
/// threads hold the context at once, and nothing in this type
/// serialises that; the `&mut` receiver is the serialisation. Moving
/// the whole resampler to another thread is sound; sharing it is not
/// offered.
unsafe impl<C: crate::FfmpegCarrier> Send for CarrierResampler<C> {}

fn layout_order(layout: &ChannelLayout) -> i32 {
  // SAFETY: `layout` is a live `ChannelLayout` for the duration of this
  // call; `addr_of!` reaches its `order` field without forming a
  // reference to the enum.
  unsafe { read_unaligned(addr_of!(layout.0.order).cast::<i32>()) }
}

/// FFmpeg's `SWR_CH_MAX`: the square its own matrix builder writes,
/// whatever the two layouts' channel counts are.
///
/// Not a convenience. `swr_build_matrix2` copies its internal
/// `[SWR_CH_MAX][SWR_CH_MAX]` block out at the caller's stride, so a
/// buffer sized to the actual channel counts is written far past its
/// end — measured, and the measurement is a killed process.
const SWR_CH_MAX: usize = 64;

/// Refuses an **effective pair** whose rematrixing would silently drop
/// input channels.
///
/// Takes the layouts `swr` is configured with, not the ones the caller
/// declared. The two differ exactly where it matters: an unspecified
/// layout is resolved to FFmpeg's default for its channel count before
/// the context is opened, and twenty-four unspecified channels resolve
/// to 22.2 — so the declared pair says "unspecified, nothing to
/// rematrix" while the conversion that runs is the lossy one.
///
/// This is the second half of the crate's two-layout bookkeeping, and
/// the halves answer different questions. The **declared** layout is
/// what decoded frames carry (a WAV without a channel mask hands out
/// unspecified frames forever) and stays the yardstick for the
/// mid-stream refusal: *is this frame the stream I was built for?* The
/// **effective** layout is what `swr` and every staged `AVFrame` use,
/// and it is the one judged here: *what will `swr` actually do?*
///
/// Each end can be perfectly valid on its own and the conversion
/// between them still lose whole channels: `swr` mixes only the channel
/// positions its rematrix table knows, and quietly processes the rest
/// of the input as though it were not there. Measured against the
/// linked FFmpeg 9 with a tone isolated in each source channel: packed
/// 22.2 → mono drops fifteen of twenty-four (`swr` says as much in a
/// log line and converts anyway), `cube` → stereo drops two of *eight*
/// — so a channel-count threshold is both too strict and too loose to
/// be the rule.
///
/// The rule is asked of FFmpeg instead: build the mixing matrix its own
/// builder would use, and refuse when any input channel reaches no
/// output at all. `lfe_mix_level` is deliberately non-zero, so the
/// question is "can this channel reach the output" rather than "does
/// FFmpeg's default downmix policy include it" — the default leaves LFE
/// out of a downmix on purpose, and refusing an everyday 5.1 → stereo
/// over that would be absurd. The predicate matched the tone sweep
/// exactly on every pair measured.
///
/// A pair `swr` cannot matrix at all is refused too. Accepting these
/// deliberately is a *mix matrix* option on the spec — a real design, not
/// something to mint in passing; until it exists, refusal is the honest
/// answer.
fn check_pair(source: &ChannelLayout, target: &ChannelLayout) -> Result<(), ResampleError> {
  let native = AVChannelOrder::AV_CHANNEL_ORDER_NATIVE as i32;
  // A layout still unspecified *after* resolution — a channel count
  // FFmpeg has no default for — is mapped positionally by `swr` with no
  // rematrixing at all, and identical layouts need no matrix: neither
  // can drop a channel, and neither is what the builder describes.
  if layout_order(source) != native || layout_order(target) != native || source == target {
    return Ok(());
  }
  let source_channels = source.channels();
  let target_channels = target.channels();

  let mut matrix = vec![0f64; SWR_CH_MAX * SWR_CH_MAX];
  // SAFETY: both layouts are live for the call; `matrix` is the full
  // `SWR_CH_MAX` square the builder writes, passed with the matching
  // stride; the encoding is a compile-time constant of this build; and
  // a null log context is documented as allowed.
  let rc = unsafe {
    swr_build_matrix2(
      &source.0,
      &target.0,
      core::f64::consts::FRAC_1_SQRT_2,
      core::f64::consts::FRAC_1_SQRT_2,
      1.0,
      1.0,
      1.0,
      matrix.as_mut_ptr(),
      SWR_CH_MAX as isize,
      AVMatrixEncoding::AV_MATRIX_ENCODING_NONE,
      core::ptr::null_mut(),
    )
  };
  if rc < 0 {
    return Err(ResampleError::RematrixUnsupported(
      RematrixUnsupported::new(source_channels, target_channels),
    ));
  }
  for channel in 0..source_channels.min(SWR_CH_MAX as i32) {
    let index = channel as usize;
    if (0..target_channels.min(SWR_CH_MAX as i32) as usize)
      .all(|out| matrix[index + SWR_CH_MAX * out] == 0.0)
    {
      return Err(ResampleError::ChannelDropped(ChannelDropped::new(
        source_channels,
        target_channels,
        channel,
      )));
    }
  }
  Ok(())
}

/// Opens a `swr` context for the two specs. Shared by
/// [`FfmpegResampler::new`] and the rebuild
/// [`AudioResampler::flush`] performs.
fn open_context(
  source: &ResampleSpec,
  target: &ResampleSpec,
  staged_source_layout: ChannelLayout,
  staged_target_layout: ChannelLayout,
) -> Result<resampling::Context, ResampleError> {
  resampling::Context::get(
    source.format,
    staged_source_layout,
    source.rate,
    target.format,
    staged_target_layout,
    target.rate,
  )
  .map_err(|e| ResampleError::Resample(CrossingError::Ffmpeg(e)))
}

/// Allocates an audio `AVFrame`, checking every step the dependency's
/// own `frame::Audio::new` does not.
///
/// `ffmpeg_next`'s constructor dereferences `av_frame_alloc`'s result
/// without a null check and discards `av_frame_get_buffer`'s return
/// value, so an allocation failure there yields a frame whose planes
/// are not backed — which is then handed to FFI. Both are checked here;
/// on failure the caller gets a named error and no frame at all. The
/// null check is the crate's existing one
/// ([`crate::frame::alloc_av_audio_frame`], which the decoders already
/// allocate through), so there is one answer to `av_frame_alloc`
/// returning null rather than two.
fn new_audio_frame(
  format: Sample,
  samples: usize,
  rate: u32,
  layout: ChannelLayout,
) -> Result<frame::Audio, ResampleError> {
  if samples == 0 || samples > i32::MAX as usize {
    return Err(ResampleError::SampleCount(SampleCount::new(samples)));
  }
  let mut out = crate::frame::alloc_av_audio_frame()?;
  out.set_format(format);
  out.set_samples(samples);
  // The layout is assigned by value, which is sound only because
  // `check_spec` refused every layout that owns a heap channel map.
  out.set_channel_layout(layout);
  out.set_rate(rate);
  // SAFETY: `out` is a live `AVFrame` whose format, sample count and
  // layout were just set; `av_frame_get_buffer` allocates its planes
  // and reports failure in its return value, which is checked.
  let rc = unsafe { av_frame_get_buffer(out.as_mut_ptr(), 0) };
  if rc < 0 {
    return Err(ResampleError::Resample(CrossingError::Ffmpeg(
      ffmpeg_next::Error::from(rc),
    )));
  }
  Ok(out)
}

/// Bytes one plane holds for `samples` samples of `format`, or `None`
/// when that product does not fit a `usize`. Packed formats keep every
/// channel in the single plane; planar formats give each channel its
/// own.
fn plane_bytes(format: Sample, samples: usize, channels: i32) -> Option<usize> {
  // Refused, not floored. `check_spec` already proves the count is in
  // `1..=MAX_FRAME_CHANNELS` before any resampler exists, so this is a
  // restatement of an invariant rather than a live branch — but it is
  // stated as a refusal because the alternative was a substituted `1`,
  // which invents a channel the caller never declared and makes the
  // byte product disagree with the frame it sizes.
  let channels = usize::try_from(channels).ok().filter(|count| *count > 0)?;
  let bytes = samples.checked_mul(format.bytes())?;
  if format.is_planar() {
    Some(bytes)
  } else {
    bytes.checked_mul(channels)
  }
}

/// Builds a native-order [`ChannelLayout`] from a channel bitmask,
/// without ever forming an `AVChannelLayout` out of foreign memory:
/// the struct starts zeroed (`AV_CHANNEL_ORDER_UNSPEC` is `0`, a valid
/// discriminant) and FFmpeg fills it.
fn layout_from_mask(mask: u64) -> ChannelLayout {
  // SAFETY: a zeroed `AVChannelLayout` is a valid value — its `order`
  // field reads as `AV_CHANNEL_ORDER_UNSPEC`, the zero discriminant —
  // and `av_channel_layout_from_mask` overwrites it wholesale.
  unsafe {
    let mut layout = std::mem::zeroed();
    if av_channel_layout_from_mask(&mut layout, mask) < 0 {
      return ChannelLayout::default(mask.count_ones() as i32);
    }
    ChannelLayout(layout)
  }
}

/// The layout `swr` will actually be configured with.
///
/// `swr_init` replaces an unspecified input or output layout with
/// FFmpeg's default for that channel count, and from then on compares
/// every frame handed to it against *that* layout — a staged frame
/// still carrying the unspecified one is refused with
/// `AVERROR_INPUT_CHANGED`. Applying the same rule here, once, keeps
/// the frames this type builds in step with the context it built.
///
/// The declared layout is kept separately and is what the mid-stream
/// check compares against, because it is what decoded frames really
/// carry: a WAV without a channel mask hands out unspecified frames
/// forever, whatever `swr` decided internally.
fn initialized_layout(layout: ChannelLayout) -> ChannelLayout {
  if layout.is_empty() {
    ChannelLayout::default(layout.channels())
  } else {
    layout
  }
}

/// Reads an `AVChannelLayout` out of FFmpeg memory into a layout this
/// spec can own, or `None` for one it does not represent.
///
/// The `order` field is read as the integer it is on the wire: an
/// out-of-range value would be undefined behaviour the instant it
/// existed as an `AVChannelOrder`, which is the hazard this crate
/// keeps out everywhere it touches a bindgen enum.
///
/// A **custom** or **ambisonic** layout returns `None`. Both keep a
/// heap-allocated channel map inside the layout, and `ChannelLayout` is
/// a plain `Copy` wrapper with no destructor: owning one here would
/// either alias a map the decoder still frees or leak the copy. A
/// resampler over one of those layouts is a separate design, not a
/// silent approximation.
///
/// # Safety
///
/// `ptr` must be a live `*const AVChannelLayout` for the duration of
/// this call.
unsafe fn layout_from_raw(ptr: *const ffmpeg_next::ffi::AVChannelLayout) -> Option<ChannelLayout> {
  let order = unsafe { read_unaligned(addr_of!((*ptr).order).cast::<i32>()) };
  let channels = unsafe { (*ptr).nb_channels };
  // The union's `mask` arm is only read for the order that names it —
  // for `CUSTOM` those eight bytes are a pointer. `layout_from_parts`
  // refuses that order anyway, so a zero is the honest thing to hand
  // it and reading the pointer would be the dishonest one.
  let mask = if matches!(LayoutArm::of(order), LayoutArm::Mask) {
    // SAFETY: the arm names exactly the orders whose contract defines
    // `u.mask`. Through the shared dispatcher rather than a local
    // comparison, which had named `NATIVE` alone and so dropped an
    // ambisonic layout's non-diegetic channels on the floor.
    unsafe { (*ptr).u.mask }
  } else {
    0
  };
  layout_from_parts(order, channels, mask)
}

/// The layout roster, over parts rather than a pointer.
///
/// One function so the two roads into a source spec —
/// [`ResampleSpec::from_parameters`] over a live `AVCodecParameters`
/// and [`ResampleSpec::from_ticket`] over an owned mirror — cannot
/// drift into admitting different layouts from the same file.
fn layout_from_parts(order: i32, channels: i32, mask: u64) -> Option<ChannelLayout> {
  if channels <= 0 {
    return None;
  }
  if order == AVChannelOrder::AV_CHANNEL_ORDER_NATIVE as i32 {
    if mask != 0 {
      return Some(layout_from_mask(mask));
    }
    // Native in name with no channels named: unspecified in substance.
    return Some(ResampleSpec::unspecified_layout(channels));
  }
  if order == AVChannelOrder::AV_CHANNEL_ORDER_UNSPEC as i32 {
    return Some(ResampleSpec::unspecified_layout(channels));
  }
  None
}

/// Compile-time assurance that `SampleFormat`'s round trip through
/// FFmpeg's vocabulary is the identity on the closed set. Both
/// directions are hand-written tables, and a table that disagreed with
/// its inverse would silently mislabel every sample.
const _: () = {
  assert!(
    SampleFormat::from_raw(AVSampleFormat::AV_SAMPLE_FMT_NONE as i32)
      .to_ffmpeg()
      .is_none()
  );
};

#[cfg(test)]
mod tests {
  use super::*;

  use mediaspine::resampler::AudioResampler;

  // These exercise the conversion arithmetic — rates, layouts, the
  // counted timeline, the byte ceiling — which is lane-independent, so
  // they run on the owned lane exactly as they did before the second
  // lane existed. The view lane's own road through this type (reserve
  // before `swr`, commit after) is proved in `tests/view_carriers.rs`,
  // where a produced plane can be shown to point into the output
  // frame's buffer.
  use crate::{FfmpegBytes, FfmpegOwnedResampler as FfmpegResampler};

  type Frame = super::Frame<crate::Owned>;

  /// **The resampler moves between threads, and does not share.**
  ///
  /// Stated as a compile-time fact rather than a comment because the
  /// `unsafe impl Send` on this type rests on an invariant a later
  /// change could quietly break: every layout a live resampler holds is
  /// the mask arm of its union, because [`preflight_layout`] refuses a
  /// `CUSTOM` order at the one construction choke point. If that
  /// refusal is ever removed, a `ResampleSpec` starts carrying a raw
  /// `u.map` pointer and the argument behind the `unsafe impl` is no
  /// longer true — this lane will still compile, so the refusal's own
  /// lane below is what guards it, and this one guards the `Send`
  /// itself against being dropped.
  ///
  /// `Sync` is deliberately absent and deliberately not asserted here:
  /// the `SwrContext` is serialised by `&mut self`, which `&self` would
  /// not provide.
  #[test]
  fn the_resampler_is_send_on_both_lanes() {
    const fn assert_send<T: Send>() {}
    assert_send::<CarrierResampler<crate::Owned>>();
    assert_send::<CarrierResampler<crate::View>>();
  }

  /// **`==` on a spec never reaches FFmpeg — and this lane's *passing*
  /// is the proof.**
  ///
  /// `ResampleSpec` derived `PartialEq`, and `ffmpeg_next::ChannelLayout`
  /// implements it as `av_channel_layout_compare`, which walks
  /// `u.map[i]` for a `CUSTOM` order. So a safe caller could forge a
  /// layout declaring channels it has no map for, wrap it in two specs,
  /// and write `a == b` — reaching a dereference of null inside FFmpeg
  /// without constructing a resampler or touching a single line of this
  /// crate's own code. No preflight can dominate an operator.
  ///
  /// The layouts below are exactly that shape. If the hand-written
  /// comparison is ever replaced by a derive again, this lane does not
  /// fail — it **crashes**, because FFmpeg reads from the null map. That
  /// is the honest signal for this defect, and the same one the
  /// null-map lanes in `channel_layout` carry.
  #[test]
  fn spec_equality_never_follows_a_channel_map() {
    // SAFETY: a zeroed `AVChannelLayout` is a valid value; the fields
    // below declare two channels under a custom order and leave `u.map`
    // null — a shape safe Rust can build and FFmpeg would dereference.
    let forged = || {
      let mut inner: ffmpeg_next::ffi::AVChannelLayout = unsafe { std::mem::zeroed() };
      inner.order = AVChannelOrder::AV_CHANNEL_ORDER_CUSTOM;
      inner.nb_channels = 2;
      ChannelLayout(inner)
    };
    let format = Sample::I16(ffmpeg_next::format::sample::Type::Packed);
    let one = ResampleSpec::new(48_000, format, forged());
    let two = ResampleSpec::new(48_000, format, forged());

    // Two *separate* null maps: equal by every field this comparison
    // reads, and the answer is the conservative one because the maps
    // are compared by identity rather than by contents.
    assert!(one == two, "two null maps are the same map");
    // Reflexive, which `Eq` requires of it.
    assert!(one == one);

    // A different map pointer is a different layout, without either
    // being read. `data` outlives the comparison.
    let data = [0u8; 64];
    let mut other_inner: ffmpeg_next::ffi::AVChannelLayout = unsafe { std::mem::zeroed() };
    other_inner.order = AVChannelOrder::AV_CHANNEL_ORDER_CUSTOM;
    other_inner.nb_channels = 2;
    other_inner.u.map = data.as_ptr().cast_mut().cast();
    let three = ResampleSpec::new(48_000, format, ChannelLayout(other_inner));
    assert!(one != three, "two different maps are different layouts");

    // And the ordinary road still answers as it always did: mask-only
    // orders compare by their mask.
    let stereo = ResampleSpec::new(48_000, format, ChannelLayout::STEREO);
    assert!(stereo == ResampleSpec::new(48_000, format, ChannelLayout::STEREO));
    assert!(stereo != ResampleSpec::new(48_000, format, ChannelLayout::MONO));
    assert!(stereo != ResampleSpec::new(44_100, format, ChannelLayout::STEREO));
    assert!(stereo != one, "a native layout is not a custom one");
  }

  /// **An `UNSPEC` layout's union is undefined, so nothing reads it.**
  ///
  /// `AVChannelLayout`'s own header says the union must not be used for
  /// `AV_CHANNEL_ORDER_UNSPEC`, and this crate had been reading
  /// `u.mask` for every order that was not `CUSTOM`. Two valid
  /// unspecified layouts of the same width therefore compared
  /// **unequal** whenever whatever bytes happened to sit in that
  /// storage differed — a wrong answer derived from memory neither
  /// layout owns.
  ///
  /// The same was true of merely *printing* one: the derived `Debug`
  /// delegated to `ffmpeg_next`'s formatter, which prints `u.mask`
  /// unconditionally. Both are order-aware now; see `LayoutArm`.
  #[test]
  fn an_unspecified_layout_ignores_its_union() {
    let unspec = |channels: i32, junk: u64| {
      // SAFETY: a zeroed `AVChannelLayout` is a valid value. `UNSPEC`
      // is the zero discriminant, and the union is written with bytes
      // the order does not define — which is exactly the state under
      // test.
      let mut inner: ffmpeg_next::ffi::AVChannelLayout = unsafe { std::mem::zeroed() };
      inner.order = AVChannelOrder::AV_CHANNEL_ORDER_UNSPEC;
      inner.nb_channels = channels;
      inner.u.mask = junk;
      ChannelLayout(inner)
    };
    let format = Sample::I16(ffmpeg_next::format::sample::Type::Packed);

    let plain = ResampleSpec::new(48_000, format, unspec(6, 0));
    let junked = ResampleSpec::new(48_000, format, unspec(6, 0xDEAD_BEEF_DEAD_BEEF));
    assert!(
      plain == junked,
      "two unspecified layouts of one width are one layout, whatever sits in storage the \
       order does not define",
    );
    // The width is still the value, so a different one is a different
    // layout.
    assert!(plain != ResampleSpec::new(48_000, format, unspec(2, 0)));

    // And `Debug` says the union is undefined rather than printing it.
    let rendered = format!("{plain:?}");
    assert!(
      rendered.contains("undefined for this order"),
      "an unspecified layout must not have its union printed: {rendered}",
    );
    assert!(
      !rendered.contains("dead") && !rendered.contains("DEAD"),
      "the junk must not reach a log line: {rendered}",
    );
    // A native layout does print its mask, which is what makes the
    // line above a statement about the order rather than about `Debug`.
    let native = ResampleSpec::new(48_000, format, ChannelLayout::STEREO);
    assert!(format!("{native:?}").contains("mask"));
  }

  /// A `CUSTOM` layout is refused at construction — the invariant the
  /// `unsafe impl Send` above rests on, and the reason a live
  /// resampler's layouts are plain values.
  #[test]
  fn a_custom_layout_is_refused_at_construction() {
    // SAFETY: a zeroed `AVChannelLayout` is a valid value; the fields
    // below are set to a custom order declaring channels, and the
    // pointer arm stays null — a shape a safe caller can build.
    let mut inner: ffmpeg_next::ffi::AVChannelLayout = unsafe { std::mem::zeroed() };
    inner.order = AVChannelOrder::AV_CHANNEL_ORDER_CUSTOM;
    inner.nb_channels = 2;
    let custom = ChannelLayout(inner);

    let spec = ResampleSpec::new(
      48_000,
      Sample::I16(ffmpeg_next::format::sample::Type::Packed),
      custom,
    );
    let target = ResampleSpec::new(
      48_000,
      Sample::I16(ffmpeg_next::format::sample::Type::Packed),
      ChannelLayout::STEREO,
    );
    // Matched rather than formatted: the resampler is deliberately not
    // `Debug`, so only the error side can be printed.
    match FfmpegResampler::new(spec, target, FrameLimits::default()) {
      Err(ResampleError::UnsupportedLayout(fault)) => {
        assert_eq!(fault.end(), SpecEnd::Source);
        assert_eq!(fault.channels(), 2);
      }
      Err(other) => panic!("a custom layout must be an unsupported layout, got {other:?}"),
      Ok(_) => panic!("a custom layout must be refused at construction"),
    }
  }

  /// A 48 kHz packed-s16 stereo frame of silence, with the plane its
  /// header claims.
  fn stereo_frame(samples: u32) -> Frame {
    let plane = FfmpegBytes::copy_from_slice(&vec![0u8; samples as usize * 2 * 2]);
    let planes = std::array::from_fn(|index| {
      Plane::new(
        if index == 0 {
          plane.clone()
        } else {
          FfmpegBytes::empty()
        },
        0,
      )
    });
    AudioFrame::new(
      48_000,
      samples,
      2,
      SampleFormat::S16,
      crate::channel_layout::channel_layout_description_from_ffmpeg(&ChannelLayout::STEREO)
        .expect("a stereo layout describes"),
      planes,
      1,
      AudioFrameExtra::default(),
    )
    .with_pts(Some(Timestamp::new(
      0,
      Timebase::new(1, std::num::NonZeroI32::new(48_000).expect("a real rate")),
    )))
  }

  /// Takes every frame that is ready *right now* and stops — the
  /// pre-EOF half of a drain, written as the exhaustive match the face
  /// now requires. A `.is_ok()` loop here would never end: "needs
  /// input" is a success.
  fn drain_ready(resampler: &mut FfmpegResampler, dst: &mut Frame) {
    loop {
      match resampler.receive_frame(dst).expect("a fault-free drain") {
        Received::Produced => {}
        Received::NeedsInput | Received::Ended => return,
      }
    }
  }

  fn stereo_to_mono() -> FfmpegResampler {
    FfmpegResampler::new(
      ResampleSpec::new(
        48_000,
        Sample::I16(ffmpeg_next::format::sample::Type::Packed),
        ChannelLayout::STEREO,
      ),
      ResampleSpec::new(
        16_000,
        Sample::I16(ffmpeg_next::format::sample::Type::Packed),
        ChannelLayout::MONO,
      ),
      FrameLimits::default(),
    )
    .expect("open resampler")
  }

  #[test]
  fn resample_error_carries_the_derived_accessor_face() {
    // `IsVariant` / `Unwrap` / `TryUnwrap` — one arm per derive family,
    // mirroring the mediadecode-side proof for this crate's own
    // newly-wired `derive_more` dependency.
    let err = ResampleError::OutputBuffer(OutputBuffer::new(2));
    assert!(err.is_output_buffer());
    assert!(!err.is_queue_alloc());
    assert_eq!(err.unwrap_output_buffer_ref().plane(), 2);
    assert!(err.try_unwrap_queue_alloc().is_err());
  }

  #[test]
  fn an_allocation_fault_while_sending_leaves_the_session_untouched() {
    // The class this design exists to end: a failure on the far side of
    // `swr_convert_frame` leaves a session no caller can act on —
    // retrying feeds the same samples twice, continuing loses them, and
    // the delay line has moved either way. Every allocation the
    // conversion needs is taken before `swr` runs, so an allocator that
    // refuses everything can only produce an error that cost nothing.
    crate::fault_subprocess::in_subprocess(
      "resampler::tests::an_allocation_fault_while_sending_leaves_the_session_untouched",
      || {
        let mut resampler = stereo_to_mono();
        let frame = stereo_frame(4_800);
        let mut dst = crate::boundary::empty_owned_audio_frame();
        crate::accepted(resampler.send_frame(&frame), "a first frame");
        drain_ready(&mut resampler, &mut dst);
        let delay = resampler.delay();
        assert!(delay > 0, "the filter has to be holding something");

        // SAFETY: in `in_subprocess`'s child, alone. The capped window runs one
        // `send_frame`: libswresample and this crate's checked frame
        // allocation.
        unsafe { crate::fault_subprocess::cap_ffmpeg_allocations(1) };
        let refused = resampler.send_frame(&frame);
        crate::fault_subprocess::uncap_ffmpeg_allocations();

        assert!(
          refused.is_err(),
          "an allocator that refuses everything must not look like success",
        );
        assert_eq!(
          resampler.delay(),
          delay,
          "the frame went into the filter anyway",
        );
        assert!(
          matches!(resampler.receive_frame(&mut dst), Ok(Received::NeedsInput)),
          "a failed send left output ready",
        );

        // And the session is still a session: the same frame converts.
        crate::accepted(resampler.send_frame(&frame), "the failure cost nothing");
        assert!(matches!(
          resampler.receive_frame(&mut dst),
          Ok(Received::Produced)
        ));
      },
    );
  }

  #[test]
  fn an_allocation_fault_while_draining_keeps_the_tail() {
    // The same property one call along, where the samples at risk are
    // the ones already inside the filter: a drain that fails must leave
    // the tail where it was, not turn it into an error.
    crate::fault_subprocess::in_subprocess(
      "resampler::tests::an_allocation_fault_while_draining_keeps_the_tail",
      || {
        let mut resampler = stereo_to_mono();
        let frame = stereo_frame(4_800);
        let mut dst = crate::boundary::empty_owned_audio_frame();
        for _ in 0..3 {
          crate::accepted(resampler.send_frame(&frame), "send_frame");
          drain_ready(&mut resampler, &mut dst);
        }
        crate::accepted(resampler.send_eof(), "eof");
        let tail = resampler.delay();
        assert!(tail > 0, "there has to be a tail to lose");

        // SAFETY: in `in_subprocess`'s child, alone. The capped window runs one
        // `receive_frame`: libswresample and this crate's checked frame
        // allocation.
        unsafe { crate::fault_subprocess::cap_ffmpeg_allocations(1) };
        let refused = resampler.receive_frame(&mut dst);
        crate::fault_subprocess::uncap_ffmpeg_allocations();

        let refused = refused.expect_err("the drain cannot have succeeded");
        // No arm of this enum can say "send me more input" any more —
        // that answer left the error type — so an allocation failure has
        // nowhere to be mistaken for one.
        assert!(
          matches!(
            refused,
            ResampleError::Resample(_) | ResampleError::OutputBuffer(_)
          ),
          "an allocation failure surfaced as something else: {refused:?}",
        );
        assert_eq!(
          resampler.delay(),
          tail,
          "the tail was consumed by a drain that failed",
        );

        // And it is still drainable, which is the whole point.
        assert_eq!(
          resampler
            .receive_frame(&mut dst)
            .expect("the tail survived the failure"),
          Received::Produced,
        );
      },
    );
  }

  #[test]
  fn the_sample_format_table_round_trips() {
    for format in [
      SampleFormat::U8,
      SampleFormat::S16,
      SampleFormat::S32,
      SampleFormat::S64,
      SampleFormat::FLT,
      SampleFormat::DBL,
      SampleFormat::U8P,
      SampleFormat::S16P,
      SampleFormat::S32P,
      SampleFormat::S64P,
      SampleFormat::FLTP,
      SampleFormat::DBLP,
    ] {
      let ffmpeg = format.to_ffmpeg().expect("a named format");
      assert_eq!(
        SampleFormat::from_ffmpeg(ffmpeg),
        format,
        "{format:?} does not survive the round trip",
      );
      assert_eq!(ffmpeg.is_planar(), format.is_planar());
    }
    assert!(SampleFormat::NONE.to_ffmpeg().is_none());
    assert!(SampleFormat::from_raw(9999).to_ffmpeg().is_none());
  }

  #[test]
  fn a_mask_rebuilds_the_layout_it_names() {
    let stereo = layout_from_mask(ChannelLayout::STEREO.bits());
    assert_eq!(stereo.channels(), 2);
    assert_eq!(stereo.bits(), ChannelLayout::STEREO.bits());

    let five_one = layout_from_mask(ChannelLayout::_5POINT1.bits());
    assert_eq!(five_one.channels(), 6);
    assert_eq!(
      five_one.bits(),
      ChannelLayout::_5POINT1.bits(),
      "the side-vs-back distinction is exactly what a default layout would lose",
    );
  }

  #[test]
  fn plane_geometry_follows_packed_versus_planar() {
    use ffmpeg_next::format::sample::Type;
    // Packed: one plane holding every channel.
    assert_eq!(
      plane_bytes(Sample::I16(Type::Packed), 1024, 2),
      Some(1024 * 2 * 2)
    );
    // Planar: one plane per channel, so the count does not multiply in.
    assert_eq!(
      plane_bytes(Sample::I16(Type::Planar), 1024, 2),
      Some(1024 * 2)
    );
    assert_eq!(
      plane_bytes(Sample::F32(Type::Planar), 1024, 6),
      Some(1024 * 4)
    );
    // A sample count whose byte size does not fit is not a size. This
    // is the arithmetic that used to run before the allocation it
    // feeds, and it wrapped.
    assert_eq!(
      plane_bytes(Sample::F32(Type::Packed), usize::MAX / 2, 8),
      None,
      "an overflowing plane size is refused, not wrapped",
    );
  }

  #[test]
  fn the_target_timebase_is_one_tick_per_output_sample() {
    let spec = ResampleSpec::new(
      16_000,
      Sample::I16(ffmpeg_next::format::sample::Type::Packed),
      ChannelLayout::MONO,
    );
    let tb = spec.timebase();
    assert_eq!((tb.num(), tb.den().get()), (1, 16_000));
  }
}
