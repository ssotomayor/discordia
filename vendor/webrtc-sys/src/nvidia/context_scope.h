#pragma once

namespace webrtc::nvidia {
template <typename Enter, typename Leave, typename Operation>
void WithCurrentContext(Enter&& enter, Leave&& leave, Operation&& operation) {
  enter();
  try {
    operation();
  } catch (...) {
    leave(false);
    throw;
  }
  leave(true);
}
}
