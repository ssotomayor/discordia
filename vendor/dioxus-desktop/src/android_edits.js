(() => {
  const interpreter = window.interpreter;
  interpreter.waitForRequest = function(editsPath, requiredServerKey) {
    const socket = new WebSocket(editsPath);
    const previous = this.edits;
    this.edits = socket;
    // The server keeps serving a socket the page still holds open, and this page ignores it.
    if (previous && previous !== socket) previous.close();
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
    let opened = false;
    socket.onopen = () => { opened = true; this.editFailures = 0; };
    socket.onclose = ({code, reason} = {}) => {
      if (this.edits !== socket) return;
      const failures = opened ? 0 : (this.editFailures = (this.editFailures || 0) + 1);
      if (opened || Number.isInteger(Math.log10(failures))) {
        try {
          this.sendIpcMessage('edit_socket_closed', {code, reason, opened, failures});
        } catch (_) {}
      }
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
      // Rust renders nothing until this batch is acknowledged, so a throwing edit froze the app.
      try {
        this.flushQueuedBytes();
      } catch (error) {
        try {
          this.sendIpcMessage('edit_error', {message: String(error), stack: String(error?.stack ?? '')});
        } catch (_) {}
      } finally {
        this.markEditsFinished();
      }
    };
    // Android can suspend animation frames behind permission dialogs or other apps.
    // A pending DOM acknowledgement must not suspend gateway heartbeats too.
    frame = requestAnimationFrame(flush);
    timer = setTimeout(flush, 100);
  };
})();
