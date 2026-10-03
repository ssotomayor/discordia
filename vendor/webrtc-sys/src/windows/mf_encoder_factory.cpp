#include "mf_encoder_factory.h"
#include "realtime_encoder.h"

#include <windows.h>
#include <codecapi.h>
#include <strmif.h>
#include <d3d11.h>
#include <dxgi1_2.h>
#include <mfapi.h>
#include <mferror.h>
#include <mfidl.h>
#include <mftransform.h>
#include <wrl/client.h>

#include <algorithm>
#include <chrono>
#include <cmath>
#include <condition_variable>
#include <cstdlib>
#include <deque>
#include <map>
#include <mutex>
#include <optional>
#include <thread>

#include "api/video/encoded_image.h"
#include "api/video/i420_buffer.h"
#include "api/video_codecs/video_encoder.h"
#include "api/video_codecs/video_encoder_factory_template.h"
#include "api/video_codecs/video_encoder_factory_template_open_h264_adapter.h"
#include "api/video_codecs/video_encoder_software_fallback_wrapper.h"
#include "common_video/h264/h264_common.h"
#include "modules/video_coding/include/video_codec_interface.h"
#include "modules/video_coding/include/video_error_codes.h"
#include "rtc_base/logging.h"
#include "third_party/libyuv/include/libyuv/convert_from.h"

namespace webrtc {
namespace {
using Microsoft::WRL::ComPtr;

class Apartment {
 public:
  Apartment() : result_(CoInitializeEx(nullptr, COINIT_MULTITHREADED)) {}
  ~Apartment() { if (SUCCEEDED(result_)) CoUninitialize(); }
  bool valid() const { return SUCCEEDED(result_) || result_ == RPC_E_CHANGED_MODE; }
 private:
  HRESULT result_;
};

HRESULT StartMf() {
  // Media Foundation is shared with the WebView; keep its startup reference for the process lifetime.
  static const HRESULT result = [] {
    if (!LoadLibraryA("mfplat.dll")) return HRESULT_FROM_WIN32(ERROR_MOD_NOT_FOUND);
    return MFStartup(MF_VERSION, MFSTARTUP_FULL);
  }();
  return result;
}

std::string EncoderName(IMFActivate* activate) {
  wchar_t* value = nullptr;
  UINT32 length = 0;
  if (FAILED(activate->GetAllocatedString(MFT_FRIENDLY_NAME_Attribute, &value, &length)))
    return "Windows hardware encoder";
  int bytes = WideCharToMultiByte(CP_UTF8, 0, value, length, nullptr, 0, nullptr, nullptr);
  std::string name(bytes, '\0');
  if (bytes > 0) WideCharToMultiByte(CP_UTF8, 0, value, length, name.data(), bytes, nullptr, nullptr);
  CoTaskMemFree(value);
  return name;
}

std::vector<ComPtr<IMFActivate>> HardwareEncoders() {
  std::vector<ComPtr<IMFActivate>> result;
  if (FAILED(StartMf())) return result;
  MFT_REGISTER_TYPE_INFO input{MFMediaType_Video, MFVideoFormat_NV12};
  MFT_REGISTER_TYPE_INFO output{MFMediaType_Video, MFVideoFormat_H264};
  IMFActivate** items = nullptr;
  UINT32 count = 0;
  if (FAILED(MFTEnumEx(MFT_CATEGORY_VIDEO_ENCODER,
                      MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER,
                      &input, &output, &items, &count))) return result;
  const char* filter = std::getenv("DISCORDIA_MF_ENCODER_FILTER");
  for (UINT32 i = 0; i < count; ++i) {
    ComPtr<IMFActivate> item;
    item.Attach(items[i]);
    RTC_LOG(LS_INFO) << "Windows hardware H264 encoder: " << EncoderName(item.Get());
    if (!filter || EncoderName(item.Get()).find(filter) != std::string::npos)
      result.push_back(std::move(item));
  }
  CoTaskMemFree(items);
  return result;
}

#define MF_TRY(call) do { HRESULT hr = (call); if (FAILED(hr)) { \
  RTC_LOG(LS_WARNING) << #call << " failed: " << hr; return hr; } } while (false)

HRESULT VideoType(const GUID& subtype, const VideoCodec& settings,
                  IMFMediaType** result) {
  ComPtr<IMFMediaType> type;
  MF_TRY(MFCreateMediaType(&type));
  MF_TRY(type->SetGUID(MF_MT_MAJOR_TYPE, MFMediaType_Video));
  MF_TRY(type->SetGUID(MF_MT_SUBTYPE, subtype));
  MF_TRY(type->SetUINT32(MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive));
  MF_TRY(MFSetAttributeSize(type.Get(), MF_MT_FRAME_SIZE, settings.width, settings.height));
  MF_TRY(MFSetAttributeRatio(type.Get(), MF_MT_FRAME_RATE, settings.maxFramerate, 1));
  MF_TRY(MFSetAttributeRatio(type.Get(), MF_MT_PIXEL_ASPECT_RATIO, 1, 1));
  *result = type.Detach();
  return S_OK;
}

HRESULT SetCodecUint(ICodecAPI* codec, const GUID& key, ULONG value) {
  if (!codec || codec->IsSupported(&key) != S_OK) return E_NOTIMPL;
  VARIANT setting;
  VariantInit(&setting);
  setting.vt = VT_UI4;
  setting.ulVal = value;
  HRESULT hr = codec->SetValue(&key, &setting);
  if (FAILED(hr)) RTC_LOG(LS_WARNING) << "Hardware encoder rejected tuning property: " << hr;
  return hr;
}

struct Session {
  ComPtr<ID3D11Device> device;
  ComPtr<ID3D11DeviceContext> context;
  ComPtr<IMFDXGIDeviceManager> manager;
  ComPtr<IMFTransform> transform;
  ComPtr<IMFMediaEventGenerator> events;
  ComPtr<ICodecAPI> codec;
  MFT_OUTPUT_STREAM_INFO output_info{};
  DWORD input_id = 0;
  DWORD output_id = 0;
  unsigned input_requests = 0;
  mf::SampleClock clock;
  mf::OutputWatchdog watchdog;
  LONGLONG duration = 0;
  int width = 0;
  int height = 0;
  std::map<LONGLONG, VideoFrame> submitted;
  std::string name;

  ~Session() {
    if (transform) {
      ComPtr<IMFShutdown> shutdown;
      if (SUCCEEDED(transform.As(&shutdown))) {
        HRESULT hr = shutdown->Shutdown();
        if (FAILED(hr)) RTC_LOG(LS_WARNING) << "Hardware encoder shutdown failed: " << hr;
      }
    }
  }

  HRESULT Open(IMFActivate* activation, const VideoCodec& settings) {
    MF_TRY(activation->ActivateObject(IID_PPV_ARGS(&transform)));
    ComPtr<IMFAttributes> attributes;
    MF_TRY(transform->GetAttributes(&attributes));
    MF_TRY(attributes->SetUINT32(MF_TRANSFORM_ASYNC_UNLOCK, TRUE));
    UINT32 d3d11 = 0;
    if (SUCCEEDED(attributes->GetUINT32(MF_SA_D3D11_AWARE, &d3d11)) && d3d11) {
      ComPtr<IDXGIFactory1> factory;
      MF_TRY(CreateDXGIFactory1(IID_PPV_ARGS(&factory)));
      UINT64 luid = 0;
      bool has_luid = SUCCEEDED(activation->GetUINT64(MFT_ENUM_ADAPTER_LUID, &luid));
      std::string friendly = EncoderName(activation);
      UINT vendor = friendly.find("AMD") != std::string::npos ? 0x1002
          : friendly.find("NVIDIA") != std::string::npos ? 0x10de
          : friendly.find("Intel") != std::string::npos ? 0x8086 : 0;
      ComPtr<IDXGIAdapter1> selected;
      for (UINT i = 0; ; ++i) {
        ComPtr<IDXGIAdapter1> adapter;
        if (factory->EnumAdapters1(i, &adapter) == DXGI_ERROR_NOT_FOUND) break;
        DXGI_ADAPTER_DESC1 desc{};
        MF_TRY(adapter->GetDesc1(&desc));
        UINT64 adapter_luid = (static_cast<UINT64>(static_cast<UINT32>(desc.AdapterLuid.HighPart)) << 32)
            | desc.AdapterLuid.LowPart;
        if (!(desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE)
            && (has_luid ? adapter_luid == luid : !vendor || desc.VendorId == vendor)) {
          selected = adapter;
          break;
        }
      }
      if (!selected) return MF_E_UNSUPPORTED_D3D_TYPE;
      MF_TRY(D3D11CreateDevice(selected.Get(), D3D_DRIVER_TYPE_UNKNOWN, nullptr,
                              D3D11_CREATE_DEVICE_VIDEO_SUPPORT | D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                              nullptr, 0, D3D11_SDK_VERSION, &device, nullptr, &context));
      UINT token = 0;
      MF_TRY(MFCreateDXGIDeviceManager(&token, &manager));
      MF_TRY(manager->ResetDevice(device.Get(), token));
      MF_TRY(transform->ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER,
                                      reinterpret_cast<ULONG_PTR>(manager.Get())));
    }
    MF_TRY(transform.As(&events));
    HRESULT ids = transform->GetStreamIDs(1, &input_id, 1, &output_id);
    if (FAILED(ids) && ids != E_NOTIMPL) return ids;
    if (SUCCEEDED(transform.As(&codec))) {
      VARIANT low_latency;
      VariantInit(&low_latency);
      low_latency.vt = VT_BOOL;
      low_latency.boolVal = VARIANT_TRUE;
      if (codec->IsSupported(&CODECAPI_AVLowLatencyMode) == S_OK) {
        HRESULT hr = codec->SetValue(&CODECAPI_AVLowLatencyMode, &low_latency);
        if (FAILED(hr)) RTC_LOG(LS_WARNING) << "Hardware low-latency mode unavailable: " << hr;
      }
      SetCodecUint(codec.Get(), CODECAPI_AVEncCommonRateControlMode, eAVEncCommonRateControlMode_CBR);
      SetCodecUint(codec.Get(), CODECAPI_AVEncMPVDefaultBPictureCount, 0);
    }
    ComPtr<IMFMediaType> output;
    MF_TRY(VideoType(MFVideoFormat_H264, settings, &output));
    MF_TRY(output->SetUINT32(MF_MT_AVG_BITRATE, settings.startBitrate * 1000));
    MF_TRY(output->SetUINT32(MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_Base));
    MF_TRY(transform->SetOutputType(output_id, output.Get(), 0));
    ComPtr<IMFMediaType> input;
    MF_TRY(VideoType(MFVideoFormat_NV12, settings, &input));
    MF_TRY(input->SetUINT32(MF_MT_DEFAULT_STRIDE, settings.width));
    MF_TRY(transform->SetInputType(input_id, input.Get(), 0));
    // Some hardware MFTs ignore MF_MT_AVG_BITRATE unless ICodecAPI is set as well.
    MF_TRY(SetCodecUint(codec.Get(), CODECAPI_AVEncCommonMeanBitRate, settings.startBitrate * 1000));
    MF_TRY(transform->GetOutputStreamInfo(output_id, &output_info));
    MF_TRY(transform->ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0));
    MF_TRY(transform->ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0));
    duration = 10000000 / settings.maxFramerate;
    width = settings.width;
    height = settings.height;
    name = "Media Foundation H264 Encoder · " + EncoderName(activation);
    return S_OK;
  }

  HRESULT Input(const VideoFrame& frame, bool keyframe) {
    auto buffer = frame.video_frame_buffer()->ToI420();
    if (!buffer || buffer->width() != width || buffer->height() != height) return E_INVALIDARG;
    ComPtr<IMFMediaBuffer> memory;
    uint64_t allocation = static_cast<uint64_t>(width) * height * 3 / 2;
    if (allocation > MAXDWORD) return E_INVALIDARG;
    DWORD bytes = static_cast<DWORD>(allocation);
    MF_TRY(MFCreateMemoryBuffer(bytes, &memory));
    BYTE* pixels = nullptr;
    MF_TRY(memory->Lock(&pixels, nullptr, nullptr));
    int converted = libyuv::I420ToNV12(buffer->DataY(), buffer->StrideY(),
        buffer->DataU(), buffer->StrideU(), buffer->DataV(), buffer->StrideV(),
        pixels, buffer->width(), pixels + buffer->width() * buffer->height(),
        buffer->width(), buffer->width(), buffer->height());
    HRESULT unlocked = memory->Unlock();
    if (converted != 0) return E_INVALIDARG;
    MF_TRY(unlocked);
    MF_TRY(memory->SetCurrentLength(bytes));
    ComPtr<IMFSample> sample;
    MF_TRY(MFCreateSample(&sample));
    MF_TRY(sample->AddBuffer(memory.Get()));
    LONGLONG time = clock.Next(frame.timestamp_us());
    MF_TRY(sample->SetSampleTime(time));
    MF_TRY(sample->SetSampleDuration(duration));
    if (keyframe) MF_TRY(SetCodecUint(codec.Get(), CODECAPI_AVEncVideoForceKeyFrame, 1));
    MF_TRY(transform->ProcessInput(input_id, sample.Get(), 0));
    submitted.emplace(time, frame);
    --input_requests;
    return S_OK;
  }

  HRESULT Output(EncodedImageCallback* callback, bool renegotiated = false) {
    ComPtr<IMFSample> supplied;
    if (!(output_info.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES)) {
      MF_TRY(MFCreateSample(&supplied));
      ComPtr<IMFMediaBuffer> memory;
      MF_TRY(MFCreateAlignedMemoryBuffer(output_info.cbSize,
                                        output_info.cbAlignment ? output_info.cbAlignment - 1 : 0, &memory));
      MF_TRY(supplied->AddBuffer(memory.Get()));
    }
    MFT_OUTPUT_DATA_BUFFER output{};
    output.dwStreamID = output_id;
    output.pSample = supplied.Get();
    DWORD status = 0;
    HRESULT hr = transform->ProcessOutput(0, 1, &output, &status);
    if (output.pEvents) output.pEvents->Release();
    ComPtr<IMFSample> sample;
    if (output.pSample == supplied.Get()) sample = supplied;
    else sample.Attach(output.pSample);
    if (hr == MF_E_TRANSFORM_STREAM_CHANGE) {
      if (renegotiated) return hr;
      ComPtr<IMFMediaType> type;
      MF_TRY(transform->GetOutputAvailableType(output_id, 0, &type));
      MF_TRY(transform->SetOutputType(output_id, type.Get(), 0));
      MF_TRY(transform->GetOutputStreamInfo(output_id, &output_info));
      return Output(callback, true);
    }
    MF_TRY(hr);
    if (output.dwStatus & MFT_OUTPUT_DATA_BUFFER_NO_SAMPLE) return S_OK;
    if (!sample) return E_UNEXPECTED;
    LONGLONG time = 0;
    MF_TRY(sample->GetSampleTime(&time));
    auto metadata = submitted.find(time);
    if (metadata == submitted.end()) return E_UNEXPECTED;
    ComPtr<IMFMediaBuffer> memory;
    MF_TRY(sample->ConvertToContiguousBuffer(&memory));
    BYTE* bytes = nullptr;
    DWORD length = 0;
    MF_TRY(memory->Lock(&bytes, nullptr, &length));
    auto data = EncodedImageBuffer::Create(bytes, length);
    MF_TRY(memory->Unlock());
    EncodedImage image;
    image.SetEncodedData(data);
    image.set_size(length);
    image._encodedWidth = metadata->second.width();
    image._encodedHeight = metadata->second.height();
    image.SetRtpTimestamp(metadata->second.rtp_timestamp());
    image.ntp_time_ms_ = metadata->second.ntp_time_ms();
    image.capture_time_ms_ = metadata->second.render_time_ms();
    image.rotation_ = metadata->second.rotation();
    image.SetColorSpace(metadata->second.color_space());
    image.SetSimulcastIndex(0);
    image._frameType = VideoFrameType::kVideoFrameDelta;
    for (const auto& nalu : H264::FindNaluIndices(MakeArrayView(data->data(), length))) {
      auto type = H264::ParseNaluType(data->data()[nalu.payload_start_offset]);
      if ((type == H264::kIdr || type == H264::kSlice)
          && (nalu.payload_size < 2
              || !mf::FitsLiveKitH264Prefix(data->data()[nalu.payload_start_offset + 1]))) {
        RTC_LOG(LS_WARNING) << "Hardware H264 slice header is incompatible with LiveKit encryption; requesting software fallback";
        return E_FAIL;
      }
      if (type == H264::kIdr)
        image._frameType = VideoFrameType::kVideoFrameKey;
    }
    submitted.erase(metadata);
    watchdog.Produced(mf::OutputWatchdog::Clock::now());
    CodecSpecificInfo info{};
    info.codecType = kVideoCodecH264;
    info.codecSpecific.H264.packetization_mode = H264PacketizationMode::NonInterleaved;
    if (callback && callback->OnEncodedImage(image, &info).error != EncodedImageCallback::Result::OK)
      return E_FAIL;
    return S_OK;
  }
};

class WindowsMfEncoder : public VideoEncoder {
 public:
  ~WindowsMfEncoder() override { Release(); }
  int32_t InitEncode(const VideoCodec* settings, const Settings&) override {
    if (!settings || settings->codecType != kVideoCodecH264 || settings->maxFramerate == 0
        || settings->width == 0 || settings->height == 0
        || settings->width % 2 || settings->height % 2) return WEBRTC_VIDEO_CODEC_ERR_PARAMETER;
    Release();
    {
      std::lock_guard lock(mutex_);
      stop_ = false;
      initialized_ = false;
      failure_ = S_OK;
      bitrate_ = settings->startBitrate * 1000;
      framerate_ = settings->maxFramerate;
    }
    worker_ = std::thread([this, settings = *settings] { Run(settings); });
    std::unique_lock lock(mutex_);
    wake_.wait(lock, [this] { return initialized_; });
    return FAILED(failure_) ? WEBRTC_VIDEO_CODEC_ENCODER_FAILURE : WEBRTC_VIDEO_CODEC_OK;
  }
  int32_t RegisterEncodeCompleteCallback(EncodedImageCallback* callback) override {
    std::lock_guard lock(mutex_);
    callback_ = callback;
    return WEBRTC_VIDEO_CODEC_OK;
  }
  int32_t Release() override {
    { std::lock_guard lock(mutex_); stop_ = true; }
    wake_.notify_all();
    if (worker_.joinable()) worker_.join();
    { std::lock_guard lock(mutex_); queue_.Clear(); }
    return WEBRTC_VIDEO_CODEC_OK;
  }
  int32_t Encode(const VideoFrame& frame, const std::vector<VideoFrameType>* types) override {
    std::lock_guard lock(mutex_);
    if (stop_ || FAILED(failure_)) return WEBRTC_VIDEO_CODEC_FALLBACK_SOFTWARE;
    if (types && !types->empty()
        && std::all_of(types->begin(), types->end(), [](VideoFrameType type) {
          return type == VideoFrameType::kEmptyFrame;
        })) return WEBRTC_VIDEO_CODEC_NO_OUTPUT;
    if (bitrate_ == 0) return WEBRTC_VIDEO_CODEC_NO_OUTPUT;
    bool key = types && std::find(types->begin(), types->end(), VideoFrameType::kVideoFrameKey) != types->end();
    queue_.Push(frame, key);
    wake_.notify_all();
    return WEBRTC_VIDEO_CODEC_OK;
  }
  void SetRates(const RateControlParameters& parameters) override {
    std::lock_guard lock(mutex_);
    bitrate_ = parameters.bitrate.get_sum_bps();
    if (std::isfinite(parameters.framerate_fps) && parameters.framerate_fps > 0)
      framerate_ = parameters.framerate_fps;
    if (!bitrate_) queue_.Clear();
    wake_.notify_all();
  }
  EncoderInfo GetEncoderInfo() const override {
    std::lock_guard lock(mutex_);
    EncoderInfo info;
    info.implementation_name = name_;
    info.is_hardware_accelerated = true;
    info.supports_native_handle = false;
    info.supports_simulcast = false;
    info.scaling_settings = ScalingSettings::kOff;
    info.preferred_pixel_formats = {VideoFrameBuffer::Type::kI420};
    return info;
  }
 private:
  void Run(VideoCodec settings) {
    Apartment apartment;
    std::unique_ptr<Session> session;
    HRESULT failure = E_FAIL;
    if (apartment.valid()) {
      for (auto& activation : HardwareEncoders()) {
        auto candidate = std::make_unique<Session>();
        failure = candidate->Open(activation.Get(), settings);
        if (SUCCEEDED(failure)) { session = std::move(candidate); break; }
        RTC_LOG(LS_WARNING) << "Hardware encoder initialization failed: "
                            << EncoderName(activation.Get()) << " / " << failure;
      }
    }
    {
      std::lock_guard lock(mutex_);
      failure_ = failure;
      initialized_ = true;
      if (session) name_ = session->name;
    }
    wake_.notify_all();
    if (!session) return;
    ULONG previous_bitrate = settings.startBitrate * 1000;
    while (SUCCEEDED(failure)) {
      EncodedImageCallback* callback;
      ULONG bitrate;
      double framerate;
      bool queued;
      {
        std::lock_guard lock(mutex_);
        if (stop_) break;
        callback = callback_;
        bitrate = bitrate_;
        framerate = framerate_;
        queued = !queue_.Empty();
      }
      if (bitrate != previous_bitrate) {
        if (bitrate) {
          failure = SetCodecUint(session->codec.Get(), CODECAPI_AVEncCommonMeanBitRate, bitrate);
          if (FAILED(failure)) break;
        }
        previous_bitrate = bitrate;
      }
      if (framerate > 0) session->duration = static_cast<LONGLONG>(10000000 / framerate);
      for (;;) {
        ComPtr<IMFMediaEvent> event;
        HRESULT hr = session->events->GetEvent(MF_EVENT_FLAG_NO_WAIT, &event);
        if (hr == MF_E_NO_EVENTS_AVAILABLE) break;
        if (FAILED(hr)) { failure = hr; break; }
        MediaEventType type;
        hr = event->GetType(&type);
        if (FAILED(hr)) { failure = hr; break; }
        hr = event->GetStatus(&failure);
        if (FAILED(hr)) failure = hr;
        if (FAILED(failure)) break;
        if (type == METransformNeedInput) ++session->input_requests;
        if (type == METransformHaveOutput) failure = session->Output(callback);
        if (FAILED(failure)) break;
      }
      if (FAILED(failure)) break;
      if (session->watchdog.Stalled(bitrate && (queued || !session->submitted.empty()),
                                    mf::OutputWatchdog::Clock::now())) {
        RTC_LOG(LS_ERROR) << "Hardware encoder stopped producing frames; requesting software fallback";
        failure = MF_E_HW_MFT_FAILED_START_STREAMING;
        break;
      }
      std::optional<mf::LatestFrames<VideoFrame>::Entry> frame;
      {
        std::unique_lock lock(mutex_);
        if (stop_) break;
        if (bitrate && session->input_requests && !queue_.Empty() && session->submitted.size() < 8) {
          frame = queue_.Pop();
        } else if (queue_.Empty() && session->submitted.empty()) {
          wake_.wait(lock, [this, previous_bitrate] {
            return stop_ || !queue_.Empty() || bitrate_ != previous_bitrate;
          });
        } else {
          wake_.wait_for(lock, std::chrono::milliseconds(2));
        }
      }
      if (frame) failure = session->Input(frame->frame, frame->key);
    }
    if (FAILED(failure)) {
      RTC_LOG(LS_ERROR) << "Media Foundation hardware encoding failed: " << failure;
      std::lock_guard lock(mutex_);
      failure_ = failure;
    }
  }
  mutable std::mutex mutex_;
  std::condition_variable wake_;
  std::thread worker_;
  mf::LatestFrames<VideoFrame> queue_;
  EncodedImageCallback* callback_ = nullptr;
  bool stop_ = true;
  bool initialized_ = false;
  HRESULT failure_ = S_OK;
  ULONG bitrate_ = 0;
  double framerate_ = 0;
  std::string name_ = "Media Foundation H264 Encoder";
};
#undef MF_TRY
}

bool WindowsMfEncoderFactory::IsSupported() {
  Apartment apartment;
  return apartment.valid() && !HardwareEncoders().empty();
}

std::vector<SdpVideoFormat> WindowsMfEncoderFactory::GetSupportedFormats() const {
  return {SdpVideoFormat("H264", {{"profile-level-id", "42e01f"},
                                {"level-asymmetry-allowed", "1"},
                                {"packetization-mode", "1"}})};
}

std::unique_ptr<VideoEncoder> WindowsMfEncoderFactory::Create(
    const Environment& env, const SdpVideoFormat& format) {
  for (const auto& supported : GetSupportedFormats())
    if (format.IsSameCodec(supported)) {
      VideoEncoderFactoryTemplate<OpenH264EncoderTemplateAdapter> software_factory;
      auto software = software_factory.Create(env, format);
      if (!software) return std::make_unique<WindowsMfEncoder>();
      return CreateVideoEncoderSoftwareFallbackWrapper(
          env, std::move(software), std::make_unique<WindowsMfEncoder>(), false);
    }
  return nullptr;
}
}
