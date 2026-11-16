#include "../src/windows/realtime_encoder.h"
#include "../src/h264_pps_retry.h"
#include "../src/nvidia/context_scope.h"

#include <cassert>
#include <iostream>
#include <memory>
#include <stdexcept>

void TestContextCleanup() {
  for (int failure = -1; failure < 5; ++failure) {
    int pushes = 0, pops = 0, copies = 0;
    bool checked = false;
    try {
      webrtc::nvidia::WithCurrentContext(
          [&] { if (failure == 0) throw std::runtime_error("push"); ++pushes; },
          [&](bool normal) { ++pops; checked = normal; if (failure == 4) throw std::runtime_error("pop"); },
          [&] { for (int plane = 1; plane <= 3; ++plane) { if (failure == plane) throw std::runtime_error("copy"); ++copies; } });
      assert(failure == -1);
    } catch (const std::runtime_error&) {
      assert(failure != -1);
    }
    assert(pushes == (failure == 0 ? 0 : 1));
    assert(pops == pushes);
    assert(checked == (failure == -1 || failure == 4));
    if (failure == -1) assert(copies == 3);
  }
}

#ifdef _WIN32
#include "../src/windows/mf_input_buffer.h"
#include "../src/windows/mf_device.h"
#include "../src/windows/mf_result.h"
#include "../src/windows/mf_codec_control.h"
#include <wrl/client.h>
#endif

void TestRetiringSkippedInput() {
  std::map<int64_t, std::shared_ptr<int>> submitted;
  for (int64_t frame = 0; frame < 3000; frame += 3) {
    auto skipped = std::make_shared<int>(static_cast<int>(frame));
    std::weak_ptr<int> retained = skipped;
    submitted.emplace(frame, std::move(skipped));
    submitted.emplace(frame + 1, std::make_shared<int>(static_cast<int>(frame + 1)));
    submitted.emplace(frame + 2, std::make_shared<int>(static_cast<int>(frame + 2)));
    webrtc::mf::RetireSubmitted(submitted, frame + 1);
    assert(retained.expired());
    assert(submitted.size() == 1 && submitted.begin()->first == frame + 2);
  }
  webrtc::mf::RetireSubmitted(submitted, 3000);
  assert(submitted.empty());
}

void TestEncryptionRetriesUseFreshIvsAndStop() {
  for (unsigned mismatches : {0u, 2u, 31u, 32u, 100u}) {
    unsigned attempts = 0, nonce = 0;
    bool sent = webrtc::h264::EncryptPreservingPps(
        [&] { assert(nonce == attempts); ++attempts; return true; },
        [&] { return attempts > mismatches; },
        [&] { ++nonce; });
    assert(sent == (mismatches < 32));
    assert(attempts == (std::min)(mismatches + 1, 32u));
    assert(nonce == attempts - 1);
  }
  unsigned attempts = 0, checked = 0;
  assert(!webrtc::h264::EncryptPreservingPps(
      [&] { ++attempts; return false; },
      [&] { ++checked; return true; },
      [] { assert(false); }));
  assert(attempts == 1 && checked == 0);
}

#ifdef _WIN32
namespace result_test {
int failures = 0;
HRESULT last_failure = S_OK;
void LogFailure(const char*, HRESULT failure) { ++failures; last_failure = failure; }
HRESULT ReturnResult(HRESULT hr) {
  MF_TRY(hr);
  return S_OK;
}
}

void TestMfResultDoesNotShadowTheCall() {
  for (unsigned i = 0; i < 1000; ++i) assert(result_test::ReturnResult(S_OK) == S_OK);
  assert(result_test::failures == 0);
  assert(result_test::ReturnResult(E_ACCESSDENIED) == E_ACCESSDENIED);
  assert(result_test::failures == 1 && result_test::last_failure == E_ACCESSDENIED);
}

void TestOptionalCodecControlPreservesDeviceFailures() {
  using webrtc::mf::OptionalCodecControl;
  int sets = 0, reports = 0;
  auto apply = [&](HRESULT support, HRESULT result) {
    return OptionalCodecControl(
        [&] { return support; },
        [&] { ++sets; return result; },
        [&](HRESULT) { ++reports; });
  };
  assert(apply(S_FALSE, E_FAIL) == S_OK);
  assert(sets == 0 && reports == 0);
  assert(apply(E_NOTIMPL, S_OK) == S_OK);
  assert(sets == 0 && reports == 1);
  assert(apply(S_OK, E_PROP_ID_UNSUPPORTED) == S_OK);
  assert(sets == 1 && reports == 2);
  for (HRESULT failure : {DXGI_ERROR_DEVICE_REMOVED, DXGI_ERROR_DEVICE_RESET,
                          E_OUTOFMEMORY, E_ACCESSDENIED, E_INVALIDARG}) {
    assert(apply(failure, S_OK) == failure);
    assert(apply(S_OK, failure) == failure);
  }
  assert(apply(S_OK, S_OK) == S_OK);
}

void TestMultithreadDevice() {
  using Microsoft::WRL::ComPtr;
  ComPtr<ID3D11Device> device;
  assert(SUCCEEDED(D3D11CreateDevice(nullptr, D3D_DRIVER_TYPE_WARP, nullptr, 0,
      nullptr, 0, D3D11_SDK_VERSION, &device, nullptr, nullptr)));
  assert(webrtc::mf::ProtectDeviceContext(nullptr) == E_POINTER);
  assert(SUCCEEDED(webrtc::mf::ProtectDeviceContext(device.Get())));
  ComPtr<ID3D10Multithread> multithread;
  assert(SUCCEEDED(device.As(&multithread)));
  assert(multithread->GetMultithreadProtected());
  assert(SUCCEEDED(webrtc::mf::ProtectDeviceContext(device.Get())));
  assert(multithread->GetMultithreadProtected());
}
#endif

void TestSoftwareFallbackBudget() {
  using webrtc::mf::SoftwareFallbackWithinBudget;
  assert(SoftwareFallbackWithinBudget(1280, 720, 30));
  assert(SoftwareFallbackWithinBudget(720, 1280, 30));
  assert(SoftwareFallbackWithinBudget(640, 360, 60));
  assert(!SoftwareFallbackWithinBudget(1920, 1080, 60));
  assert(!SoftwareFallbackWithinBudget(1920, 1080, 30));
  assert(!SoftwareFallbackWithinBudget(1280, 720, 60));
  assert(!SoftwareFallbackWithinBudget(3840, 720, 30));
  assert(!SoftwareFallbackWithinBudget(UINT32_MAX, UINT32_MAX, UINT32_MAX));
  assert(!SoftwareFallbackWithinBudget(0, 720, 30));
  int initialized = 0;
  auto init = [&] { ++initialized; return 0; };
  auto denied = [] { return -7; };
  assert(webrtc::mf::InitializeSoftwareFallback(true, 1920, 1080, 60, init, denied) == -7);
  assert(initialized == 0);
  assert(webrtc::mf::InitializeSoftwareFallback(true, 1280, 720, 30, init, denied) == 0);
  assert(webrtc::mf::InitializeSoftwareFallback(false, 1920, 1080, 60, init, denied) == 0);
  assert(initialized == 2);
}

void TestInputRetries() {
  using namespace webrtc::mf;
  struct Metadata { int frame; int64_t timestamp; bool keyframe; };
  RetriableInput<Metadata, std::shared_ptr<int>> pending;
  auto sample = std::make_shared<int>(42);
  pending.Prepare(Metadata{7, 123456, true}, sample);
  int attempts = 0, accepted = 0;
  auto send = [&](const std::shared_ptr<int>& candidate) {
    assert(candidate == sample);
    return ++attempts < 4 ? -1 : 0;
  };
  auto accept = [&](const Metadata& metadata) {
    assert(metadata.frame == 7 && metadata.timestamp == 123456 && metadata.keyframe);
    ++accepted;
  };
  for (int i = 0; i < 3; ++i) {
    assert(pending.Submit(send, accept, -1) == -1);
    assert(!pending.Empty() && accepted == 0);
  }
  assert(pending.Submit(send, accept, -1) == 0);
  assert(pending.Empty() && accepted == 1);
  assert(pending.Submit(send, accept, -1) == 0);
  assert(attempts == 4 && accepted == 1);
  pending.Prepare(Metadata{8, 654321, false}, sample);
  assert(pending.Submit([](const auto&) { return -2; }, accept, -1) == -2);
  assert(!pending.Empty() && accepted == 1);
}

#ifdef _WIN32
void TestInputBuffers() {
  using Microsoft::WRL::ComPtr;
  assert(SUCCEEDED(MFStartup(MF_VERSION)));
  for (DWORD alignment : {0ul, 16ul, 32ul, 64ul, 256ul}) {
    MFT_INPUT_STREAM_INFO info{};
    info.cbAlignment = alignment;
    for (DWORD required : {0ul, 1024ul, 4096ul}) {
      info.cbSize = required;
      ComPtr<IMFMediaBuffer> buffer;
      assert(SUCCEEDED(webrtc::mf::CreateInputBuffer(info, 2048, &buffer)));
      BYTE* data = nullptr;
      DWORD capacity = 0;
      assert(SUCCEEDED(buffer->Lock(&data, &capacity, nullptr)));
      assert(capacity >= 2048 && capacity >= required);
      assert(!alignment || reinterpret_cast<uintptr_t>(data) % alignment == 0);
      assert(SUCCEEDED(buffer->Unlock()));
      assert(SUCCEEDED(buffer->SetCurrentLength(2048)));
      DWORD length = 0;
      assert(SUCCEEDED(buffer->GetCurrentLength(&length)) && length == 2048);
    }
  }
  assert(SUCCEEDED(MFShutdown()));
}
#endif

int main() {
  TestContextCleanup();
  TestEncryptionRetriesUseFreshIvsAndStop();
  TestRetiringSkippedInput();
  TestSoftwareFallbackBudget();
  TestInputRetries();
#ifdef _WIN32
  TestMfResultDoesNotShadowTheCall();
  TestOptionalCodecControlPreservesDeviceFailures();
  TestMultithreadDevice();
  TestInputBuffers();
#endif
  using namespace webrtc::mf;
  assert(FitsLiveKitH264Prefix(0xb8));
  assert(FitsLiveKitH264Prefix(0xe0));
  assert(!FitsLiveKitH264Prefix(0x88));
  assert(!FitsLiveKitH264Prefix(0));
  assert(RetriableLiveKitH264Prefix(0x88, 0x80));
  assert(RetriableLiveKitH264Prefix(0x88, 0xff));
  assert(!RetriableLiveKitH264Prefix(0x88, 0x40));
  assert(!RetriableLiveKitH264Prefix(0, 0xff));
  LatestFrames<int> queue;
  queue.Push(0, true);
  for (int i = 1; i < 1000; ++i) queue.Push(i, false);
  auto first = queue.Pop();
  assert(first.frame == 997 && first.key);
  assert(queue.Pop().frame == 998);
  assert(queue.Pop().frame == 999);
  assert(queue.Empty());

  queue.Push(1, false);
  queue.Push(2, false);
  queue.Push(3, false);
  queue.Push(4, true);
  assert(queue.Pop().key);
  assert(queue.Pop().frame == 3);
  assert(queue.Pop().frame == 4);
  queue.Push(5, true);
  queue.Clear();
  assert(queue.Empty());

  SampleClock clock;
  assert(clock.Next(1000000) == 0);
  assert(clock.Next(1033333) == 333330);
  assert(clock.Next(1200000) == 2000000);
  assert(clock.Next(1200000) == 2000001);
  assert(clock.Next(1100000) == 2000002);
  assert(clock.Next(1300000) == 3000000);

  OutputWatchdog watchdog;
  auto now = OutputWatchdog::Clock::time_point{};
  assert(!watchdog.Stalled(true, now));
  assert(!watchdog.Stalled(true, now + std::chrono::milliseconds(1999)));
  assert(watchdog.Stalled(true, now + std::chrono::seconds(2)));
  watchdog.Produced(now + std::chrono::seconds(2));
  assert(!watchdog.Stalled(true, now + std::chrono::seconds(3)));
  assert(!watchdog.Stalled(false, now + std::chrono::seconds(10)));
  assert(!watchdog.Stalled(true, now + std::chrono::seconds(20)));
  assert(watchdog.Stalled(true, now + std::chrono::seconds(22)));
  std::cout << "Encoder buffer, retry, queue, keyframe, timing and recovery tests passed\n";
}
