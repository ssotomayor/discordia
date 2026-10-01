#pragma once
#include "rust/cxx.h"
#include "api/video/video_frame.h"
#include "api/video/video_sink_interface.h"
#ifdef WEBRTC_WIN
#include "modules/video_capture/video_capture.h"
#endif

namespace livekit_ffi {
struct CameraDevice;
struct CameraSinkWrapper;
class CameraCapture : public webrtc::VideoSinkInterface<webrtc::VideoFrame> {
 public:
  CameraCapture(rust::Str id, unsigned width, unsigned height, unsigned fps,
                rust::Box<CameraSinkWrapper> sink);
  ~CameraCapture() override;
  void OnFrame(const webrtc::VideoFrame& frame) override;
 private:
  rust::Box<CameraSinkWrapper> sink_;
#ifdef WEBRTC_WIN
  webrtc::scoped_refptr<webrtc::VideoCaptureModule> capture_;
#endif
};
rust::Vec<CameraDevice> camera_devices();
std::unique_ptr<CameraCapture> start_camera_capture(rust::Str id, unsigned width,
    unsigned height, unsigned fps, rust::Box<CameraSinkWrapper> sink);
}
