#pragma once

namespace webrtc {
void UseH264PpsRetry();
}

namespace webrtc::h264 {
template <typename Encrypt, typename Matches, typename FreshIv>
bool EncryptPreservingPps(Encrypt encrypt, Matches matches, FreshIv fresh_iv) {
  for (unsigned attempt = 0; attempt < 32; ++attempt) {
    if (!encrypt()) return false;
    if (matches()) return true;
    if (attempt + 1 < 32) fresh_iv();
  }
  return false;
}
}
