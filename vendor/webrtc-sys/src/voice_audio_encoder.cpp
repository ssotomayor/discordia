#include "livekit/voice_audio_encoder.h"

#include <algorithm>
#include <string>
#include <utility>

#include "absl/strings/match.h"
#include "api/audio_codecs/builtin_audio_encoder_factory.h"
#include "api/audio_codecs/opus/audio_encoder_opus.h"
#include "api/environment/environment_factory.h"
#include "api/make_ref_counted.h"
#include "modules/audio_coding/codecs/opus/audio_encoder_opus.h"
#include "rtc_base/buffer.h"
#include "rtc_base/checks.h"
#include "webrtc-sys/src/voice_audio_encoder.rs.h"

namespace livekit_ffi {

namespace {

// 1275 bytes per Opus frame, and a packet holds at most 120 ms of them.
constexpr size_t kMaxEncodedBytes = 6 * 1275;

class VoiceAudioEncoderFactory : public webrtc::AudioEncoderFactory {
 public:
  VoiceAudioEncoderFactory()
      : builtin_(webrtc::CreateBuiltinAudioEncoderFactory()) {}

  std::vector<webrtc::AudioCodecSpec> GetSupportedEncoders() override {
    return builtin_->GetSupportedEncoders();
  }

  std::optional<webrtc::AudioCodecInfo> QueryAudioEncoder(
      const webrtc::SdpAudioFormat& format) override {
    return builtin_->QueryAudioEncoder(format);
  }

  std::unique_ptr<webrtc::AudioEncoder> Create(
      const webrtc::Environment& env,
      const webrtc::SdpAudioFormat& format,
      Options options) override {
    if (absl::EqualsIgnoreCase(format.name, "opus")) {
      auto config = webrtc::AudioEncoderOpus::SdpToConfig(format);
      if (config && config->num_channels == 1 && config->IsOk()) {
        return std::make_unique<VoiceAudioEncoder>(*config,
                                                   options.payload_type);
      }
    }
    return builtin_->Create(env, format, options);
  }

 private:
  webrtc::scoped_refptr<webrtc::AudioEncoderFactory> builtin_;
};

}  // namespace

VoiceAudioEncoder::VoiceAudioEncoder(webrtc::AudioEncoderOpusConfig config,
                                     int payload_type)
    : payload_type_(payload_type) {
  Recreate(std::move(config));
}

VoiceAudioEncoder::~VoiceAudioEncoder() {
  RTC_CHECK_EQ(0, WebRtcOpus_EncoderFree(inst_));
}

void VoiceAudioEncoder::Recreate(webrtc::AudioEncoderOpusConfig config) {
  RTC_CHECK(config.IsOk());
  if (inst_) {
    RTC_CHECK_EQ(0, WebRtcOpus_EncoderFree(inst_));
    inst_ = nullptr;
  }
  config_ = std::move(config);
  input_.clear();
  input_.reserve(FramesPerPacket() * SamplesPer10ms());
  RTC_CHECK_EQ(0, WebRtcOpus_EncoderCreate(
                      &inst_, config_.num_channels,
                      config_.application ==
                              webrtc::AudioEncoderOpusConfig::ApplicationMode::kVoip
                          ? 0
                          : 1,
                      config_.sample_rate_hz));
  RTC_CHECK_EQ(0, WebRtcOpus_SetBitRate(inst_, *config_.bitrate_bps));
  RTC_CHECK_EQ(0, config_.fec_enabled ? WebRtcOpus_EnableFec(inst_)
                                      : WebRtcOpus_DisableFec(inst_));
  RTC_CHECK_EQ(0, WebRtcOpus_SetMaxPlaybackRate(inst_,
                                                config_.max_playback_rate_hz));
  complexity_ = webrtc::AudioEncoderOpusImpl::GetNewComplexity(config_)
                    .value_or(config_.complexity);
  RTC_CHECK_EQ(0, WebRtcOpus_SetComplexity(inst_, complexity_));
  RTC_CHECK_EQ(0, config_.dtx_enabled ? WebRtcOpus_EnableDtx(inst_)
                                      : WebRtcOpus_DisableDtx(inst_));
  RTC_CHECK_EQ(0, WebRtcOpus_SetPacketLossRate(inst_, kExpectedLossPercent));
  RTC_CHECK_EQ(0, config_.cbr_enabled ? WebRtcOpus_EnableCbr(inst_)
                                      : WebRtcOpus_DisableCbr(inst_));
  consecutive_dtx_frames_ = 0;
}

size_t VoiceAudioEncoder::FramesPerPacket() const {
  return static_cast<size_t>(config_.frame_size_ms / 10);
}

size_t VoiceAudioEncoder::SamplesPer10ms() const {
  return static_cast<size_t>(config_.sample_rate_hz / 100) *
         config_.num_channels;
}

int VoiceAudioEncoder::SampleRateHz() const {
  return config_.sample_rate_hz;
}

size_t VoiceAudioEncoder::NumChannels() const {
  return config_.num_channels;
}

int VoiceAudioEncoder::RtpTimestampRateHz() const {
  return 48000;
}

size_t VoiceAudioEncoder::Num10MsFramesInNextPacket() const {
  return FramesPerPacket();
}

size_t VoiceAudioEncoder::Max10MsFramesInAPacket() const {
  return FramesPerPacket();
}

int VoiceAudioEncoder::GetTargetBitrate() const {
  return *config_.bitrate_bps;
}

void VoiceAudioEncoder::Reset() {
  Recreate(config_);
}

bool VoiceAudioEncoder::SetFec(bool enable) {
  RTC_CHECK_EQ(0, enable ? WebRtcOpus_EnableFec(inst_)
                         : WebRtcOpus_DisableFec(inst_));
  config_.fec_enabled = enable;
  return true;
}

bool VoiceAudioEncoder::SetDtx(bool enable) {
  RTC_CHECK_EQ(0, enable ? WebRtcOpus_EnableDtx(inst_)
                         : WebRtcOpus_DisableDtx(inst_));
  config_.dtx_enabled = enable;
  return true;
}

bool VoiceAudioEncoder::GetDtx() const {
  return config_.dtx_enabled;
}

bool VoiceAudioEncoder::SetApplication(Application application) {
  auto config = config_;
  config.application =
      application == Application::kSpeech
          ? webrtc::AudioEncoderOpusConfig::ApplicationMode::kVoip
          : webrtc::AudioEncoderOpusConfig::ApplicationMode::kAudio;
  Recreate(std::move(config));
  return true;
}

void VoiceAudioEncoder::SetMaxPlaybackRate(int frequency_hz) {
  auto config = config_;
  config.max_playback_rate_hz = frequency_hz;
  Recreate(std::move(config));
}

// Measured loss lags and averages away a burst; the FEC budget is fixed instead.
void VoiceAudioEncoder::OnReceivedUplinkPacketLossFraction(float /* fraction */) {}

void VoiceAudioEncoder::OnReceivedTargetAudioBitrate(int target_bps) {
  ApplyAudioBitrate(target_bps);
}

void VoiceAudioEncoder::OnReceivedUplinkBandwidth(
    int target_audio_bitrate_bps,
    std::optional<int64_t> /* bwe_period_ms */) {
  ApplyAudioBitrate(target_audio_bitrate_bps);
}

void VoiceAudioEncoder::OnReceivedUplinkAllocation(
    webrtc::BitrateAllocationUpdate update) {
  ApplyAudioBitrate(update.target_bitrate.bps());
}

void VoiceAudioEncoder::OnReceivedOverhead(size_t overhead_bytes_per_packet) {
  overhead_bytes_per_packet_ = overhead_bytes_per_packet;
}

// A target includes the packets' own overhead; until that is known the target
// says nothing about the payload, so the configured bitrate stands.
void VoiceAudioEncoder::ApplyAudioBitrate(int target_audio_bitrate_bps) {
  if (!overhead_bytes_per_packet_) {
    return;
  }
  const int overhead_bps = static_cast<int>(*overhead_bytes_per_packet_ * 8 *
                                            100 / FramesPerPacket());
  SetTargetBitrate(target_audio_bitrate_bps - overhead_bps);
}

void VoiceAudioEncoder::SetTargetBitrate(int target_bps) {
  const int bitrate =
      std::clamp(target_bps, webrtc::AudioEncoderOpusConfig::kMinBitrateBps,
                 webrtc::AudioEncoderOpusConfig::kMaxBitrateBps);
  if (*config_.bitrate_bps != bitrate) {
    config_.bitrate_bps = bitrate;
    RTC_CHECK_EQ(0, WebRtcOpus_SetBitRate(inst_, bitrate));
  }
  const auto complexity = webrtc::AudioEncoderOpusImpl::GetNewComplexity(config_);
  if (complexity && *complexity != complexity_) {
    complexity_ = *complexity;
    RTC_CHECK_EQ(0, WebRtcOpus_SetComplexity(inst_, complexity_));
  }
}

std::optional<std::pair<webrtc::TimeDelta, webrtc::TimeDelta>>
VoiceAudioEncoder::GetFrameLengthRange() const {
  const auto frame = webrtc::TimeDelta::Millis(config_.frame_size_ms);
  return {{frame, frame}};
}

webrtc::AudioEncoder::EncodedInfo VoiceAudioEncoder::EncodeImpl(
    uint32_t rtp_timestamp,
    std::span<const int16_t> audio,
    webrtc::Buffer* encoded) {
  if (input_.empty()) {
    first_timestamp_ = rtp_timestamp;
  }
  input_.insert(input_.end(), audio.begin(), audio.end());
  if (input_.size() < FramesPerPacket() * SamplesPer10ms()) {
    return EncodedInfo();
  }
  EncodedInfo info;
  info.encoded_bytes = encoded->AppendData(
      kMaxEncodedBytes, [&](std::span<uint8_t> out) {
        const int written =
            WebRtcOpus_Encode(inst_, input_.data(),
                              input_.size() / config_.num_channels,
                              out.size(), out.data());
        RTC_CHECK_GE(written, 0);
        return static_cast<size_t>(written);
      });
  input_.clear();

  // Opus DTX sends a one or two byte frame instead of silence, and every 20th
  // of those carries comfort noise, which is not speech either.
  const bool dtx_frame = info.encoded_bytes <= 2;
  info.encoded_timestamp = first_timestamp_;
  info.payload_type = payload_type_;
  info.send_even_if_empty = true;
  info.speech = !dtx_frame && consecutive_dtx_frames_ != 20;
  info.encoder_type = CodecType::kOpus;
  consecutive_dtx_frames_ = dtx_frame ? consecutive_dtx_frames_ + 1 : 0;
  return info;
}

webrtc::scoped_refptr<webrtc::AudioEncoderFactory>
CreateVoiceAudioEncoderFactory() {
  return webrtc::make_ref_counted<VoiceAudioEncoderFactory>();
}

VoiceEncoderProbe probe_voice_encoder(rust::Slice<const int16_t> pcm,
                                      bool stereo,
                                      int bitrate_bps) {
  webrtc::SdpAudioFormat format(
      "opus", 48000, 2,
      {{"minptime", "10"},
       {"useinbandfec", "1"},
       {"stereo", stereo ? "1" : "0"},
       {"maxaveragebitrate", std::to_string(bitrate_bps)}});
  auto encoder = CreateVoiceAudioEncoderFactory()->Create(
      webrtc::CreateEnvironment(), format, {.payload_type = 111});
  encoder->OnReceivedUplinkPacketLossFraction(0.0f);

  VoiceEncoderProbe probe{};
  probe.voice_encoder =
      dynamic_cast<VoiceAudioEncoder*>(encoder.get()) != nullptr;
  std::vector<int16_t> interleaved;
  webrtc::Buffer packet;
  uint32_t timestamp = 0;
  for (size_t at = 0; at + 480 <= pcm.size(); at += 480) {
    interleaved.clear();
    for (size_t i = 0; i < 480; ++i) {
      interleaved.insert(interleaved.end(), encoder->NumChannels(), pcm[at + i]);
    }
    packet.Clear();
    auto info = encoder->Encode(timestamp, interleaved, &packet);
    timestamp += 480;
    if (info.encoded_bytes == 0) {
      continue;
    }
    probe.packets += 1;
    probe.bytes += info.encoded_bytes;
    if (WebRtcOpus_PacketHasFec(packet.data(), packet.size())) {
      probe.with_fec += 1;
    }
  }
  return probe;
}

}  // namespace livekit_ffi
