//! The resample seam — sample-rate, sample-format and channel-layout
//! conversion for decoded audio.
//!
//! Every consumer downstream of a decoder wants audio in *its* shape,
//! and the shapes disagree: speech models want 16 kHz mono, audio-event
//! and music models want 48 kHz, a mixer wants whatever the graph runs
//! at. The decoder produces whatever the file holds. [`AudioResampler`]
//! is the seam between them.
//!
//! The face is the `AudioStreamDecoder` push pair (the read face in
//! `mediadecode`), one tier along:
//! `send_frame` / `receive_frame` / `send_eof` / `flush`, answering the
//! same [`Sent`] and [`Received`] the decoders answer. One rhythm for
//! the whole chain: the caller pushes, drains, and schedules.
//!
//! No FFmpeg in the shape. `swresample` is merely the first
//! implementation; a pure-Rust polyphase resampler fits the same face.

use crate::{Received, Sent, adapter::AudioAdapter, frame::AudioFrame};

/// The [`AudioFrame`] an [`AudioAdapter`] carries over buffer `B`.
pub type AdapterAudioFrame<A, B> = AudioFrame<
  <A as AudioAdapter>::SampleFormat,
  <A as AudioAdapter>::ChannelLayout,
  <A as AudioAdapter>::FrameExtra,
  B,
>;

/// Push-style audio resampler: rate, sample format, and channel layout
/// conversion.
///
/// # Both specs are explicit at construction
///
/// An implementation is built knowing **both** ends: the source spec —
/// the rate, sample format and channel layout the frames coming in will
/// carry, which the caller reads off the track's
/// [`TrackInfo`](crate::demuxer::TrackInfo) — and the target spec,
/// which is the caller's own. The target is never a constant baked into
/// the seam: one consumer wants 16 kHz mono and the next wants 48 kHz
/// stereo, from the same file, at the same time. It is options.
///
/// Construction is therefore not on this trait, for the same reason it
/// is not on the decoder traits: each backend takes its own spelling of
/// those two specs.
///
/// # A mid-stream format change is a named refusal
///
/// If a frame arrives whose rate, sample format or channel layout is
/// not the source spec the implementation was built with,
/// [`send_frame`](Self::send_frame) **fails with a named error**. It
/// does not silently reconfigure. Silent reconfiguration would resample
/// the two halves of a stream on different terms and hand the caller a
/// single unbroken timeline built out of them, with nothing in the
/// output saying so. Refusing hands the decision back up: the caller
/// rebuilds the resampler for the new source spec, and the seam in the
/// data has a seam in the code to match.
///
/// # EOF drains the conversion tail
///
/// A rate converter holds samples back: its filter needs future input
/// to produce present output, so at any moment some tens of
/// milliseconds are inside it and not yet out. [`send_eof`](Self::send_eof)
/// followed by [`receive_frame`](Self::receive_frame) drains them.
/// Skipping the drain loses the end of every file.
///
/// The tail has an end and [`receive_frame`](Self::receive_frame) names
/// it: [`Received::Ended`] once the delay line is empty, never
/// [`Received::NeedsInput`]. The distinction is the whole reason a
/// drain loop can be written without the caller remembering whether it
/// called `send_eof` — one answer means *feed me*, the other means
/// *stop*, and a resampler that says the first when it means the second
/// is an infinite loop nothing in the caller can detect.
///
/// Output timestamps are kept by **delay-compensated accounting**: the
/// output timeline is anchored on the first input timestamp and then
/// advanced by the number of samples actually produced, so the frames
/// drained after EOF continue the same timeline rather than restarting
/// or repeating it.
pub trait AudioResampler {
  /// Backend vocabulary. The source and target specs are both
  /// expressed in it.
  type Adapter: AudioAdapter;
  /// Buffer type held by the frames this resampler accepts and
  /// produces.
  type Buffer: AsRef<[u8]>;
  /// Resampler-specific error type — **faults only**. The rhythm's own
  /// states leave through [`Sent`] and [`Received`]; what travels here
  /// is the mid-stream-change refusal out of
  /// [`send_frame`](Self::send_frame) and whatever else genuinely went
  /// wrong.
  type Error;

  /// Submits one decoded frame.
  ///
  /// Fails with a named error if the frame's rate, sample format or
  /// channel layout is not the source spec this resampler was built
  /// with — **that is a fault, not back pressure**: the same frame will
  /// be refused however much is drained first, so it travels in `Err`.
  ///
  /// [`Sent::MustDrain`] is reserved for an implementation whose
  /// converted-frame queue is bounded. The `swresample` backend's is
  /// not, so it answers [`Sent::Accepted`] whenever it accepts at all;
  /// the arm is on the face because a bounded implementation is the
  /// obvious next one and a caller written against the face must
  /// already handle it.
  fn send_frame(
    &mut self,
    frame: &AdapterAudioFrame<Self::Adapter, Self::Buffer>,
  ) -> Result<Sent, Self::Error>;

  /// Drains one converted frame into `dst`, answering the same
  /// [`Received`] that `AudioStreamDecoder::receive_frame` answers.
  fn receive_frame(
    &mut self,
    dst: &mut AdapterAudioFrame<Self::Adapter, Self::Buffer>,
  ) -> Result<Received, Self::Error>;

  /// Signals end of stream. The conversion tail becomes drainable
  /// through [`receive_frame`](Self::receive_frame).
  fn send_eof(&mut self) -> Result<Sent, Self::Error>;

  /// Flushes internal state — buffered input, the undrained tail, and
  /// the output timestamp anchor — leaving the resampler ready for a
  /// fresh stream on the same two specs.
  fn flush(&mut self) -> Result<(), Self::Error>;
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::frame::AudioFrame;

  struct ALoop;
  impl AudioAdapter for ALoop {
    type CodecId = u32;
    type SampleFormat = u32;
    type ChannelLayout = u32;
    type PacketExtra = ();
    type FrameExtra = ();
  }

  /// **Faults only.** The rhythm's two non-frame states are not here —
  /// they leave through [`Received`], which is what leaves this enum
  /// with exactly one arm.
  #[derive(Debug, PartialEq, Eq)]
  enum LoopError {
    /// The mid-stream refusal.
    SourceChanged,
    /// A frame offered after the stream was declared over.
    AfterEof,
  }

  /// Loopback resampler: accepts only 48 kHz, hands each frame straight
  /// back, refuses anything else by name, and holds a one-frame
  /// conversion tail so end-of-stream has something to drain.
  struct LoopResampler {
    pending: bool,
    eof: bool,
    /// Frames still inside the filter at `send_eof`, delivered by the
    /// post-EOF drain.
    tail: u8,
  }

  impl LoopResampler {
    const fn new() -> Self {
      Self {
        pending: false,
        eof: false,
        tail: 0,
      }
    }
  }

  impl AudioResampler for LoopResampler {
    type Adapter = ALoop;
    type Buffer = &'static [u8];
    type Error = LoopError;

    fn send_frame(
      &mut self,
      frame: &AudioFrame<u32, u32, (), &'static [u8]>,
    ) -> Result<Sent, LoopError> {
      // The terminal state outranks the slot: draining never makes a
      // post-end offer acceptable, so `MustDrain` would be a promise
      // this face cannot keep. Checked first for that reason.
      if self.eof {
        return Err(LoopError::AfterEof);
      }
      if frame.sample_rate() != 48_000 {
        return Err(LoopError::SourceChanged);
      }
      // A one-frame slot: a second frame before the first is drained
      // is back pressure, not a failure — the loopback is the smallest
      // implementation that can show the arm meaning something.
      if self.pending {
        return Ok(Sent::MustDrain);
      }
      self.pending = true;
      self.tail = 1;
      Ok(Sent::Accepted)
    }

    fn receive_frame(
      &mut self,
      _dst: &mut AudioFrame<u32, u32, (), &'static [u8]>,
    ) -> Result<Received, LoopError> {
      if self.pending {
        self.pending = false;
        return Ok(Received::Produced);
      }
      if !self.eof {
        return Ok(Received::NeedsInput);
      }
      if self.tail > 0 {
        self.tail -= 1;
        return Ok(Received::Produced);
      }
      Ok(Received::Ended)
    }

    fn send_eof(&mut self) -> Result<Sent, LoopError> {
      self.eof = true;
      Ok(Sent::Accepted)
    }

    fn flush(&mut self) -> Result<(), LoopError> {
      self.pending = false;
      self.eof = false;
      self.tail = 0;
      Ok(())
    }
  }

  fn frame(rate: u32) -> AudioFrame<u32, u32, (), &'static [u8]> {
    const EMPTY: &[u8] = &[];
    AudioFrame::new(
      rate,
      1024,
      2,
      0,
      0,
      [crate::frame::Plane::new(EMPTY, 0); 8],
      1,
      (),
    )
  }

  #[test]
  fn the_face_is_implementable_and_signals_needs_more_in_the_ok_arm() {
    fn _accepts<R: AudioResampler>() {}
    _accepts::<LoopResampler>();

    let mut r = LoopResampler::new();
    let mut dst = frame(48_000);
    assert_eq!(r.receive_frame(&mut dst), Ok(Received::NeedsInput));
    assert_eq!(r.send_frame(&frame(48_000)), Ok(Sent::Accepted));
    assert_eq!(r.receive_frame(&mut dst), Ok(Received::Produced));
    assert_eq!(r.receive_frame(&mut dst), Ok(Received::NeedsInput));
  }

  #[test]
  fn a_mid_stream_change_is_refused_by_name() {
    let mut r = LoopResampler::new();
    assert_eq!(r.send_frame(&frame(48_000)), Ok(Sent::Accepted));
    assert_eq!(
      r.send_frame(&frame(44_100)),
      Err(LoopError::SourceChanged),
      "the face never silently reconfigures",
    );
  }

  #[test]
  fn flush_drops_the_undrained_tail() {
    let mut r = LoopResampler::new();
    assert_eq!(r.send_frame(&frame(48_000)), Ok(Sent::Accepted));
    r.flush().expect("flush");
    let mut dst = frame(48_000);
    assert_eq!(r.receive_frame(&mut dst), Ok(Received::NeedsInput));
  }

  /// **The spin-forever regression, at the face.**
  ///
  /// The drain below is the loop a generic consumer writes: it does not
  /// know whether `send_eof` was called and has no input left to offer,
  /// so [`Received::NeedsInput`] would be a hang. It terminates only
  /// because the tail's end has its own word.
  ///
  /// Under the previous convention both states arrived as one unnamed
  /// `Err` — pre-EOF "send more" and post-EOF "there is no more" were
  /// literally the same value — so this loop could not be written at
  /// all, and the version that could be written spun. The iteration cap
  /// is what turns the hang into a failing test rather than a hanging
  /// one.
  #[test]
  fn the_drained_tail_ends_instead_of_asking_for_input_that_cannot_come() {
    let mut r = LoopResampler::new();
    assert_eq!(r.send_frame(&frame(48_000)), Ok(Sent::Accepted));
    assert_eq!(r.send_eof(), Ok(Sent::Accepted));

    let mut dst = frame(48_000);
    let mut frames = 0_u32;
    let mut ended = false;
    for _ in 0..64 {
      match r
        .receive_frame(&mut dst)
        .expect("no fault in a clean drain")
      {
        Received::Produced => frames += 1,
        Received::NeedsInput => panic!(
          "a resampler that has been told the stream is over asked for input it \
           can never get — the caller has nothing left to send, so this is the hang",
        ),
        Received::Ended => {
          ended = true;
          break;
        }
      }
    }
    assert!(ended, "the drain never reached the end of the tail");
    assert_eq!(frames, 2, "one queued frame plus the one-frame tail");

    // And the answer is stable: a caller that polls past the end keeps
    // being told the same thing rather than being sent back for input.
    assert_eq!(r.receive_frame(&mut dst), Ok(Received::Ended));
  }
}
