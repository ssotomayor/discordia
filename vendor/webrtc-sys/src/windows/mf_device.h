#pragma once

#include <d3d10_1.h>
#include <d3d11.h>
#include <wrl/client.h>

namespace webrtc::mf {
inline HRESULT ProtectDeviceContext(ID3D11Device* device) {
  if (!device) return E_POINTER;
  Microsoft::WRL::ComPtr<ID3D10Multithread> multithread;
  HRESULT hr = device->QueryInterface(IID_PPV_ARGS(&multithread));
  if (FAILED(hr)) return hr;
  // The MFT shares this immediate context with its asynchronous driver workers.
  multithread->SetMultithreadProtected(TRUE);
  return multithread->GetMultithreadProtected() ? S_OK : E_FAIL;
}
}
