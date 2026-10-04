#pragma once

#include <algorithm>
#include <chrono>
#include <cstdint>
#include <deque>
#include <optional>
#include <utility>

namespace webrtc::mf {
// LiveKit leaves the NAL byte and one slice-header byte clear before encrypting H.264.
inline bool FitsLiveKitH264Prefix(uint8_t header) {
  unsigned bit = 0;
  auto read = [&]() { return (header >> (7 - bit++)) & 1; };
  for (unsigned field = 0; field < 3; ++field) {
    unsigned zeros = 0;
    bool stop = false;
    while (bit < 8) {
      if (read()) { stop = true; break; }
      ++zeros;
    }
    if (!stop || bit + zeros > 8) return false;
    bit += zeros;
  }
  return true;
}

template <typename Frame>
class LatestFrames {
 public:
  struct Entry { Frame frame; bool key; };
  void Push(Frame frame, bool key) {
    if (frames_.size() == 3) {
      key |= frames_.front().key;
      frames_.pop_front();
      if (key && !frames_.empty()) {
        frames_.front().key = true;
        key = false;
      }
    }
    frames_.push_back({std::move(frame), key});
  }
  Entry Pop() {
    Entry entry = std::move(frames_.front());
    frames_.pop_front();
    return entry;
  }
  bool Empty() const { return frames_.empty(); }
  void Clear() { frames_.clear(); }
 private:
  std::deque<Entry> frames_;
};

class SampleClock {
 public:
  int64_t Next(int64_t capture_us) {
    if (!origin_) origin_ = capture_us;
    int64_t time = std::max<int64_t>(0, capture_us - *origin_) * 10;
    time = std::max(time, previous_ + 1);
    previous_ = time;
    return time;
  }
 private:
  std::optional<int64_t> origin_;
  int64_t previous_ = -1;
};

class OutputWatchdog {
 public:
  using Clock = std::chrono::steady_clock;
  bool Stalled(bool pending, Clock::time_point now) {
    if (!pending) { since_.reset(); return false; }
    if (!since_) since_ = now;
    return now - *since_ >= std::chrono::seconds(2);
  }
  void Produced(Clock::time_point now) { since_ = now; }
 private:
  std::optional<Clock::time_point> since_;
};
}
