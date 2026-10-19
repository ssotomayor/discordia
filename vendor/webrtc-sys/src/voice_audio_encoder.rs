#[cxx::bridge(namespace = "livekit_ffi")]
pub mod ffi {
    #[derive(Debug, Default)]
    pub struct VoiceEncoderProbe {
        pub voice_encoder: bool,
        pub packets: u32,
        pub with_fec: u32,
        pub bytes: u64,
    }

    unsafe extern "C++" {
        include!("livekit/voice_audio_encoder.h");

        /// Encodes 48 kHz mono `pcm` through the factory a call's tracks use,
        /// with the receiver reporting no loss at all.
        fn probe_voice_encoder(pcm: &[i16], stereo: bool, bitrate_bps: i32) -> VoiceEncoderProbe;
    }
}
