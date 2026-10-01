#include "livekit/camera_capture.h"
#include "webrtc-sys/src/camera_capture.rs.h"
#include <stdexcept>
#include <string>
#ifdef WEBRTC_WIN
#include "modules/video_capture/video_capture_factory.h"
#endif

namespace livekit_ffi {
rust::Vec<CameraDevice> camera_devices() {
  rust::Vec<CameraDevice> result;
#ifdef WEBRTC_WIN
  std::unique_ptr<webrtc::VideoCaptureModule::DeviceInfo> info(
      webrtc::VideoCaptureFactory::CreateDeviceInfo());
  if (!info) return result;
  for (unsigned index = 0; index < info->NumberOfDevices(); ++index) {
    char label[1024] = {}, id[4096] = {};
    if (info->GetDeviceName(index, label, sizeof(label), id, sizeof(id)) == 0)
      result.push_back(CameraDevice{rust::String(id), rust::String(label)});
  }
#endif
  return result;
}

CameraCapture::CameraCapture(rust::Str id, unsigned width, unsigned height,
    unsigned fps, rust::Box<CameraSinkWrapper> sink) : sink_(std::move(sink)) {
#ifdef WEBRTC_WIN
  std::string device(id);
  std::unique_ptr<webrtc::VideoCaptureModule::DeviceInfo> info(
      webrtc::VideoCaptureFactory::CreateDeviceInfo());
  if (!info) throw std::runtime_error("Camera enumeration is unavailable");
  webrtc::VideoCaptureCapability requested, selected;
  requested.width = width;
  requested.height = height;
  requested.maxFPS = fps;
  requested.videoType = webrtc::VideoType::kI420;
  if (info->GetBestMatchedCapability(device.c_str(), requested, selected) < 0)
    throw std::runtime_error("No supported format for this camera");
  capture_ = webrtc::VideoCaptureFactory::Create(device.c_str());
  if (!capture_) throw std::runtime_error("Couldn't open this camera");
  capture_->RegisterCaptureDataCallback(this);
  if (capture_->StartCapture(selected) != 0) {
    capture_->DeRegisterCaptureDataCallback();
    capture_ = nullptr;
    throw std::runtime_error("Couldn't start this camera; check camera privacy permissions");
  }
#else
  throw std::runtime_error("Native camera capture is unavailable on this platform");
#endif
}

CameraCapture::~CameraCapture() {
#ifdef WEBRTC_WIN
  if (capture_) {
    capture_->StopCapture();
    capture_->DeRegisterCaptureDataCallback();
  }
#endif
}

void CameraCapture::OnFrame(const webrtc::VideoFrame& frame) {
  auto buffer = frame.video_frame_buffer()->ToI420();
  if (!buffer) return;
  if (buffer->width() <= 0 || buffer->height() <= 0 ||
      buffer->width() > 8192 || buffer->height() > 8192 ||
      buffer->StrideY() < buffer->width() ||
      buffer->StrideU() < buffer->ChromaWidth() ||
      buffer->StrideV() < buffer->ChromaWidth()) return;
  sink_->on_frame(buffer->width(), buffer->height(), buffer->StrideY(),
      buffer->StrideU(), buffer->StrideV(),
      rust::Slice<const uint8_t>(buffer->DataY(), size_t(buffer->StrideY()) * buffer->height()),
      rust::Slice<const uint8_t>(buffer->DataU(), size_t(buffer->StrideU()) * buffer->ChromaHeight()),
      rust::Slice<const uint8_t>(buffer->DataV(), size_t(buffer->StrideV()) * buffer->ChromaHeight()),
      frame.timestamp_us());
}

std::unique_ptr<CameraCapture> start_camera_capture(rust::Str id, unsigned width,
    unsigned height, unsigned fps, rust::Box<CameraSinkWrapper> sink) {
  return std::make_unique<CameraCapture>(id, width, height, fps, std::move(sink));
}
}
