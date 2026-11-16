#pragma once

#define MF_TRY(call) do { const HRESULT mf_call_result = (call); if (FAILED(mf_call_result)) { \
  LogFailure(#call, mf_call_result); return mf_call_result; } } while (false)
