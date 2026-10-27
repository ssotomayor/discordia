#pragma once

#include <cstdint>
#include <memory>

#include "rust/cxx.h"

struct OpusDecoder;

namespace livekit_ffi {

class SoundboardDecoder {
 public:
  SoundboardDecoder(OpusDecoder* decoder, int channels);
  ~SoundboardDecoder();
  SoundboardDecoder(const SoundboardDecoder&) = delete;
  SoundboardDecoder& operator=(const SoundboardDecoder&) = delete;
  int32_t decode(rust::Slice<const uint8_t> packet, rust::Slice<float> pcm);

 private:
  OpusDecoder* decoder_;
  int channels_;
};

std::unique_ptr<SoundboardDecoder> new_soundboard_decoder(uint8_t channels);

}  // namespace livekit_ffi
