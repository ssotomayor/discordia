#include "livekit/soundboard_decoder.h"

#include <limits>

#include "third_party/opus/src/include/opus.h"

namespace livekit_ffi {

SoundboardDecoder::SoundboardDecoder(OpusDecoder* decoder, int channels)
    : decoder_(decoder), channels_(channels) {}

SoundboardDecoder::~SoundboardDecoder() {
  opus_decoder_destroy(decoder_);
}

int32_t SoundboardDecoder::decode(rust::Slice<const uint8_t> packet,
                                 rust::Slice<float> pcm) {
  constexpr int kMaxFrames = 5760;
  if (packet.empty() ||
      packet.size() > static_cast<size_t>(std::numeric_limits<int32_t>::max())) {
    return OPUS_BAD_ARG;
  }
  const auto length = static_cast<opus_int32>(packet.size());
  const int frames = opus_packet_get_nb_samples(packet.data(), length, 48000);
  if (frames <= 0 || frames > kMaxFrames) {
    return OPUS_INVALID_PACKET;
  }
  if (pcm.size() < static_cast<size_t>(kMaxFrames * channels_)) {
    return OPUS_BUFFER_TOO_SMALL;
  }
  return opus_decode_float(decoder_, packet.data(), length, pcm.data(),
                           kMaxFrames, 0);
}

std::unique_ptr<SoundboardDecoder> new_soundboard_decoder(uint8_t channels) {
  if (channels != 1 && channels != 2) {
    return nullptr;
  }
  int error = OPUS_OK;
  OpusDecoder* decoder = opus_decoder_create(48000, channels, &error);
  if (!decoder || error != OPUS_OK) {
    if (decoder) opus_decoder_destroy(decoder);
    return nullptr;
  }
  return std::make_unique<SoundboardDecoder>(decoder, channels);
}

}  // namespace livekit_ffi
