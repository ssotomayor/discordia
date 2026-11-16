#ifndef WEBRTC_NVIDIA_I420_BUFFER_CUDA_H_
#define WEBRTC_NVIDIA_I420_BUFFER_CUDA_H_

#include <cuda.h>

#include <cstdint>

#include "NvEncoder/NvEncoderCuda.h"
#include "api/video/i420_buffer.h"
#include "context_scope.h"
#include "rtc_base/logging.h"

namespace webrtc {

inline void CopyHostPlaneToDevice(CUdeviceptr dst,
                                  uint32_t dst_pitch,
                                  const uint8_t* src,
                                  uint32_t src_pitch,
                                  uint32_t width_bytes,
                                  uint32_t height) {
  CUDA_MEMCPY2D copy = {0};
  copy.srcMemoryType = CU_MEMORYTYPE_HOST;
  copy.srcHost = src;
  copy.srcPitch = src_pitch;
  copy.dstMemoryType = CU_MEMORYTYPE_DEVICE;
  copy.dstDevice = dst;
  copy.dstPitch = dst_pitch;
  copy.WidthInBytes = width_bytes;
  copy.Height = height;
  CUDA_DRVAPI_CALL(cuMemcpy2D(&copy));
}

inline void CopyI420BufferToDeviceFrame(CUcontext context,
                                        const I420BufferInterface& buffer,
                                        const NvEncInputFrame& input_frame) {
  if (input_frame.bufferFormat != NV_ENC_BUFFER_FORMAT_IYUV ||
      input_frame.numChromaPlanes != 2) {
    NVENC_THROW_ERROR("NVENC I420 upload requires an IYUV input frame",
                      NV_ENC_ERR_INVALID_PARAM);
  }

  nvidia::WithCurrentContext(
      [context] { CUDA_DRVAPI_CALL(cuCtxPushCurrent(context)); },
      [](bool checked) {
        const CUresult cuda_status = cuCtxPopCurrent(nullptr);
        if (checked) {
          CUDA_DRVAPI_CALL(cuda_status);
        } else if (cuda_status != CUDA_SUCCESS) {
          RTC_LOG(LS_ERROR) << "CUDA context cleanup after failed I420 upload: "
                            << static_cast<int>(cuda_status);
        }
      },
      [&] {
        const CUdeviceptr dst_y = reinterpret_cast<CUdeviceptr>(input_frame.inputPtr);
        const CUdeviceptr dst_u = dst_y + input_frame.chromaOffsets[0];
        const CUdeviceptr dst_v = dst_y + input_frame.chromaOffsets[1];
        CopyHostPlaneToDevice(dst_y, input_frame.pitch, buffer.DataY(),
                              buffer.StrideY(), buffer.width(), buffer.height());
        CopyHostPlaneToDevice(dst_u, input_frame.chromaPitch, buffer.DataU(),
                              buffer.StrideU(), buffer.ChromaWidth(), buffer.ChromaHeight());
        CopyHostPlaneToDevice(dst_v, input_frame.chromaPitch, buffer.DataV(),
                              buffer.StrideV(), buffer.ChromaWidth(), buffer.ChromaHeight());
      });
}

}  // namespace webrtc

#endif  // WEBRTC_NVIDIA_I420_BUFFER_CUDA_H_
