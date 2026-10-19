#pragma once

#include <memory>
#include <optional>
#include <vector>

#include "api/audio_codecs/audio_encoder.h"
#include "api/audio_codecs/audio_encoder_factory.h"
#include "api/audio_codecs/opus/audio_encoder_opus_config.h"
#include "api/scoped_refptr.h"
#include "modules/audio_coding/codecs/opus/opus_interface.h"
#include "rust/cxx.h"

namespace livekit_ffi {

struct VoiceEncoderProbe;

// Mono Opus that always expects 30% loss, so FEC rides every packet from the
// first. libwebrtc's own encoder smooths reported loss and clamps it to 20%.
class VoiceAudioEncoder final : public webrtc::AudioEncoder {
 public:
  static constexpr int kExpectedLossPercent = 30;

  VoiceAudioEncoder(webrtc::AudioEncoderOpusConfig config, int payload_type);
  ~VoiceAudioEncoder() override;

  int SampleRateHz() const override;
  size_t NumChannels() const override;
  int RtpTimestampRateHz() const override;
  size_t Num10MsFramesInNextPacket() const override;
  size_t Max10MsFramesInAPacket() const override;
  int GetTargetBitrate() const override;

  void Reset() override;
  bool SetFec(bool enable) override;
  bool SetDtx(bool enable) override;
  bool GetDtx() const override;
  bool SetApplication(Application application) override;
  void SetMaxPlaybackRate(int frequency_hz) override;
  void OnReceivedUplinkPacketLossFraction(float fraction) override;
  void OnReceivedTargetAudioBitrate(int target_bps) override;
  void OnReceivedUplinkBandwidth(int target_audio_bitrate_bps,
                                 std::optional<int64_t> bwe_period_ms) override;
  void OnReceivedUplinkAllocation(webrtc::BitrateAllocationUpdate update) override;
  void OnReceivedOverhead(size_t overhead_bytes_per_packet) override;
  std::optional<std::pair<webrtc::TimeDelta, webrtc::TimeDelta>>
  GetFrameLengthRange() const override;

 protected:
  EncodedInfo EncodeImpl(uint32_t rtp_timestamp,
                         std::span<const int16_t> audio,
                         webrtc::Buffer* encoded) override;

 private:
  void Recreate(webrtc::AudioEncoderOpusConfig config);
  void SetTargetBitrate(int target_bps) override;
  void ApplyAudioBitrate(int target_audio_bitrate_bps);
  size_t FramesPerPacket() const;
  size_t SamplesPer10ms() const;

  webrtc::AudioEncoderOpusConfig config_;
  const int payload_type_;
  OpusEncInst* inst_ = nullptr;
  std::vector<int16_t> input_;
  uint32_t first_timestamp_ = 0;
  int complexity_ = 0;
  int consecutive_dtx_frames_ = 0;
  std::optional<size_t> overhead_bytes_per_packet_;
};

webrtc::scoped_refptr<webrtc::AudioEncoderFactory>
CreateVoiceAudioEncoderFactory();

VoiceEncoderProbe probe_voice_encoder(rust::Slice<const int16_t> pcm,
                                      bool stereo,
                                      int bitrate_bps);

}  // namespace livekit_ffi
