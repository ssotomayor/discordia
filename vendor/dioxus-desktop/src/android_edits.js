(() => {
  const interpreter = window.interpreter;
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
