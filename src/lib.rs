use std::num::{NonZeroU32, NonZeroU8};
use symphonia::core::{
    audio::{AudioBufferRef, Layout},
    conv::FromSample,
    errors::Error as SymphoniaError,
    io::{MediaSource, MediaSourceStream},
    sample::Sample,
};
use thiserror::Error;
use vorbis_rs::{VorbisBitrateManagementStrategy, VorbisEncoderBuilder, VorbisError};

pub use symphonia::core::sample::{i24, u24};

/// Enum representing a channel layout
#[derive(Clone, Copy, Debug)]
pub enum Channels {
    Mono = 1,
    Stereo = 2,
}

/// Buffer containing samples
#[derive(Clone, Debug)]
pub struct SampleBuffer<
    S: Sample
        + FromSample<u8>
        + FromSample<u16>
        + FromSample<u24>
        + FromSample<u32>
        + FromSample<i8>
        + FromSample<i16>
        + FromSample<i24>
        + FromSample<i32>
        + FromSample<f32>
        + FromSample<f64>,
> {
    buffer: Box<[S]>,
    written: usize,
    duration: usize,
    channels: Channels,
    sample_rate: u32,
}

impl<
        S: Sample
            + FromSample<u8>
            + FromSample<u16>
            + FromSample<u24>
            + FromSample<u32>
            + FromSample<i8>
            + FromSample<i16>
            + FromSample<i24>
            + FromSample<i32>
            + FromSample<f32>
            + FromSample<f64>,
    > SampleBuffer<S>
{
    /// Creates a buffer given parameters and fills it with silence
    pub fn new(duration: usize, channels: Channels, sample_rate: u32) -> Self {
        Self {
            buffer: vec![S::MID; channels as usize * duration].into_boxed_slice(),
            written: 0,
            duration,
            channels,
            sample_rate,
        }
    }

    /// Returns a reference to contained samples
    pub fn samples(&self) -> &[S] {
        &self.buffer
    }

    /// Returns buffer duration in samples
    pub fn duration(&self) -> usize {
        self.duration
    }

    /// Returns buffer's channel layout
    pub fn channels(&self) -> Channels {
        self.channels
    }

    /// Returns buffer's sample rate
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    fn copy_samples(&mut self, buffer: AudioBufferRef<'_>) {
        let mut buffer2 = buffer.make_equivalent();
        buffer.convert(&mut buffer2);

        let p = buffer2.planes();
        let planes = p.planes();
        let interleaved = interleave(planes, self.channels);

        self.buffer[self.written..self.written + interleaved.len()].copy_from_slice(&interleaved);
        self.written += interleaved.len();
    }

    /// Returns an equivalent buffer with the desired sample format
    pub fn converted<
        T: Sample
            + FromSample<S>
            + FromSample<u8>
            + FromSample<u16>
            + FromSample<u24>
            + FromSample<u32>
            + FromSample<i8>
            + FromSample<i16>
            + FromSample<i24>
            + FromSample<i32>
            + FromSample<f32>
            + FromSample<f64>,
    >(
        &self,
    ) -> SampleBuffer<T> {
        SampleBuffer {
            buffer: self
                .buffer
                .iter()
                .copied()
                .map(FromSample::from_sample)
                .collect::<Vec<T>>()
                .into_boxed_slice(),
            written: self.written,
            duration: self.duration,
            channels: self.channels,
            sample_rate: self.sample_rate,
        }
    }
}

fn interleave<T: Copy, V: AsRef<[T]>>(samples: &[V], channels: Channels) -> Vec<T> {
    match channels {
        Channels::Mono => samples[0].as_ref().to_vec(),
        Channels::Stereo => samples[0]
            .as_ref()
            .iter()
            .zip(samples[1].as_ref().iter())
            .flat_map(|(&l, &r)| [l, r])
            .collect(),
    }
}

fn deintereave<T: Copy>(samples: &[T], channels: Channels) -> Vec<Vec<T>> {
    match channels {
        Channels::Mono => vec![samples.to_vec()],
        Channels::Stereo => {
            let mut result = vec![
                Vec::with_capacity(samples.len() / 2),
                Vec::with_capacity(samples.len() / 2),
            ];

            for i in (0..samples.len()).step_by(2) {
                result[0].push(samples[i]);
                result[1].push(samples[i + 1]);
            }

            result
        }
    }
}

/// Decodes an audio file in source
/// Returns a tuple of the source bitrate and a buffer with decoded samples
pub fn decode<
    S: Sample
        + FromSample<u8>
        + FromSample<u16>
        + FromSample<u24>
        + FromSample<u32>
        + FromSample<i8>
        + FromSample<i16>
        + FromSample<i24>
        + FromSample<i32>
        + FromSample<f32>
        + FromSample<f64>,
>(
    source: impl MediaSource + 'static,
) -> Result<(u64, SampleBuffer<S>), DecodeError> {
    let len = source
        .byte_len()
        .ok_or(DecodeError::PropertyLacking("source length"))?;

    let stream = MediaSourceStream::new(Box::new(source), Default::default());

    let probed = symphonia::default::get_probe().format(
        &Default::default(),
        stream,
        &Default::default(),
        &Default::default(),
    )?;

    let mut reader = probed.format;

    let track = reader
        .default_track()
        .ok_or(DecodeError::PropertyLacking("default track"))?;
    let id = track.id;

    let n_frames = track
        .codec_params
        .n_frames
        .ok_or(DecodeError::PropertyLacking("n_frames"))?;
    let mut buffer = SampleBuffer::new(
        n_frames as _,
        track
            .codec_params
            .channel_layout
            .map(|l| match l {
                Layout::Mono => Channels::Mono,
                Layout::Stereo => Channels::Stereo,
                _ => panic!(),
            })
            .or(track.codec_params.channels.map(|c| {
                if c.count() > 1 {
                    Channels::Stereo
                } else {
                    Channels::Mono
                }
            }))
            .ok_or(DecodeError::PropertyLacking("channel layout"))?,
        track
            .codec_params
            .sample_rate
            .ok_or(DecodeError::PropertyLacking("sample rate"))? as _,
    );

    let mut decoder =
        symphonia::default::get_codecs().make(&track.codec_params, &Default::default())?;

    let bitrate = len
        / track
            .codec_params
            .time_base
            .ok_or(DecodeError::PropertyLacking("time base"))?
            .calc_time(n_frames)
            .seconds
        * 8;

    loop {
        let packet = match reader.next_packet() {
            Ok(p) => p,
            _ => break,
        };

        if packet.track_id() != id {
            continue;
        }

        match decoder.decode(&packet) {
            Ok(decoded) => buffer.copy_samples(decoded),
            Err(SymphoniaError::DecodeError(_)) => (),
            _ => break,
        }
    }

    Ok((bitrate, buffer))
}

/// Enum representing decoding errors
#[derive(Error, Debug)]
#[error(transparent)]
pub enum DecodeError {
    Symphonia(#[from] SymphoniaError),
    #[error("source lacks property: {0}")]
    PropertyLacking(&'static str),
}

/// Function for encoding a buffer using ogg vorbis given an average bitrate
pub fn encode_vorbis(samples: &SampleBuffer<f32>, bitrate: u64) -> Result<Vec<u8>, VorbisError> {
    let mut encoder = VorbisEncoderBuilder::new(
        NonZeroU32::new(samples.sample_rate).unwrap(),
        NonZeroU8::new(samples.channels as _).unwrap(),
        Vec::new(),
    )?
    .bitrate_management_strategy(VorbisBitrateManagementStrategy::Abr {
        average_bitrate: NonZeroU32::new(bitrate as u32).unwrap(),
    })
    .build()?;

    for chunk in samples.samples().chunks(2048) {
        encoder.encode_audio_block(deintereave(chunk, samples.channels))?;
    }

    encoder.finish()
}
