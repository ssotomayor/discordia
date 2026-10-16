#pragma once

#include <windows.h>
#include <mfapi.h>
#include <mftransform.h>

#include <algorithm>

namespace webrtc::mf {
inline HRESULT CreateInputBuffer(const MFT_INPUT_STREAM_INFO& info, DWORD bytes,
                                IMFMediaBuffer** buffer) {
  return MFCreateAlignedMemoryBuffer((std::max)(bytes, info.cbSize),
                                    info.cbAlignment ? info.cbAlignment - 1 : 0,
                                    buffer);
}
}
