#pragma once

#include <windows.h>
#include <vfwmsgs.h>

namespace webrtc::mf {

inline bool UnsupportedCodecControl(HRESULT result) {
  return result == E_NOTIMPL || result == E_PROP_ID_UNSUPPORTED ||
         result == E_PROP_SET_UNSUPPORTED ||
         result == HRESULT_FROM_WIN32(ERROR_NOT_SUPPORTED);
}

template <typename Supported, typename Set, typename Report>
HRESULT OptionalCodecControl(Supported supported, Set set, Report report) {
  HRESULT result = supported();
  if (result == S_FALSE) return S_OK;
  if (SUCCEEDED(result)) result = set();
  if (FAILED(result)) {
    report(result);
    if (UnsupportedCodecControl(result)) return S_OK;
  }
  return result;
}

}  // namespace webrtc::mf
