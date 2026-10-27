#[cxx::bridge(namespace = "livekit_ffi")]
pub mod ffi {
    // SAFETY: C++ owns the decoder; both slices carry lengths checked before decoding.
    unsafe extern "C++" {
        include!("livekit/soundboard_decoder.h");
        type SoundboardDecoder;
        fn new_soundboard_decoder(channels: u8) -> UniquePtr<SoundboardDecoder>;
        fn decode(self: Pin<&mut SoundboardDecoder>, packet: &[u8], pcm: &mut [f32]) -> i32;
    }
}
