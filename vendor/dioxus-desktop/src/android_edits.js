(() => {
  const interpreter = window.interpreter;
  interpreter.waitForRequest = function(editsPath, requiredServerKey) {
    const socket = new WebSocket(editsPath);
    this.edits = socket;
    // Blob.arrayBuffer adds asynchronous file-reading work to every DOM acknowledgement.
    // ArrayBuffer messages can apply immediately even when background tasks are throttled.
    socket.binaryType = 'arraybuffer';
    let authenticated = false;
    socket.onmessage = ({data}) => {
      if (this.edits !== socket) return;
      if (typeof data === 'string') {
        if (data === requiredServerKey) authenticated = true;
      } else if (authenticated && data instanceof ArrayBuffer) {
        this.rafEdits(data);
      }
    };
    socket.onclose = () => {
      setTimeout(() => {
        if (this.edits === socket) this.waitForRequest(editsPath, requiredServerKey);
      }, 100);
    };
  };
  const immediate = interpreter.rafEdits;
  interpreter.rafEdits = function(bytes) {
    if (this.headless) return immediate.call(this, bytes);
    this.enqueueBytes(bytes);
    let finished = false, frame, timer;
    const flush = () => {
      if (finished) return;
      finished = true;
      cancelAnimationFrame(frame);
      clearTimeout(timer);
      this.flushQueuedBytes();
      this.markEditsFinished();
    };
    // Android can suspend animation frames behind permission dialogs or other apps.
    // A pending DOM acknowledgement must not suspend gateway heartbeats too.
    frame = requestAnimationFrame(flush);
    timer = setTimeout(flush, 100);
  };
})();
