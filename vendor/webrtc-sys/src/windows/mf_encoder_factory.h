#pragma once

#include "api/video_codecs/video_encoder_factory.h"

namespace webrtc {
class WindowsMfEncoderFactory : public VideoEncoderFactory {
 public:
  static bool IsSupported();
  std::vector<SdpVideoFormat> GetSupportedFormats() const override;
  std::unique_ptr<VideoEncoder> Create(
      const Environment& env, const SdpVideoFormat& format) override;
};
}
