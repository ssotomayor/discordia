#include "../src/windows/realtime_encoder.h"

#include <cassert>
#include <iostream>
#include <memory>

#ifdef _WIN32
#include "../src/windows/mf_input_buffer.h"
#include <wrl/client.h>
#endif

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
  TestInputRetries();
#ifdef _WIN32
  TestInputBuffers();
#endif
  using namespace webrtc::mf;
  assert(FitsLiveKitH264Prefix(0xb8));
  assert(FitsLiveKitH264Prefix(0xe0));
  assert(!FitsLiveKitH264Prefix(0x88));
  assert(!FitsLiveKitH264Prefix(0));
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
