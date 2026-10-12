(() => {
  if (window.__discordiaActivityClock) return;
  window.__discordiaActivityClock = true;
  let focused = document.hasFocus();
  let clocks = [];
  let timer = null;

  function elapsed(start, now) {
    if (!Number.isFinite(start) || start <= 0 || now < start) return '';
    const total = Math.floor((now - start) / 1000);
    const h = Math.floor(total / 3600);
    const m = Math.floor(total / 60) % 60;
    const sec = String(total % 60).padStart(2, '0');
    return `${h ? `${h}:${String(m).padStart(2, '0')}` : String(m).padStart(2, '0')}:${sec} elapsed`;
  }

  function tick() {
    const now = Date.now();
    for (const clock of clocks) {
      const text = elapsed(Number(clock.getAttribute('data-activity-start')), now);
      if (clock.textContent !== text) {
        // Dioxus retains the text-node reference for later activity updates.
        if (clock.firstChild?.nodeType === 3) clock.firstChild.nodeValue = text;
        else clock.textContent = text;
      }
    }
  }

  function sync() {
    const active = focused && !document.hidden && clocks.length > 0;
    if (!active && timer !== null) {
      clearInterval(timer);
      timer = null;
    }
    if (active) {
      tick();
      if (timer === null) timer = setInterval(tick, 1000);
    }
  }

  function scan() {
    clocks = [...document.querySelectorAll('[data-activity-clock]')];
    sync();
  }

  window.addEventListener('focus', () => { focused = true; sync(); });
  window.addEventListener('blur', () => { focused = false; sync(); });
  document.addEventListener('visibilitychange', sync);
  new MutationObserver(records => {
    if (records.some(r => r.type === 'attributes' ||
      [...r.addedNodes].some(n => n.nodeType === 1 &&
        (n.matches('[data-activity-clock]') || n.querySelector('[data-activity-clock]'))) ||
      [...r.removedNodes].some(n => n.nodeType === 1 && clocks.some(c => n === c || n.contains(c))))) scan();
  }).observe(document.documentElement, {
    childList: true, subtree: true, attributes: true, attributeFilter: ['data-activity-start']
  });
  scan();
})();
