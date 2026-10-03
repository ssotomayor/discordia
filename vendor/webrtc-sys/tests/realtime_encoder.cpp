#include "../src/windows/realtime_encoder.h"

#include <cassert>
#include <iostream>

int main() {
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
  std::cout << "Encoder queue, keyframe, capture timing and stall recovery tests passed\n";
}
